use super::{JsonlEventStore, LiveSessions, LocalApplication, LocalState};
use std::sync::{Arc, Weak};
use ternilo_protocol::{
    HarnessError, InputAuthor, RunId, SessionEvent, SessionEventKind, SessionId,
};

pub(super) struct InputOrigins {
    state: Arc<LocalState>,
    live: Weak<LiveSessions>,
}

impl LocalApplication {
    pub async fn model_input_origin(
        &self,
        id: &str,
        run_id: &RunId,
    ) -> Result<crate::ModelInputOrigin, HarnessError> {
        InputOrigins::new(Arc::clone(&self.state), Arc::downgrade(&self.live))
            .resolve(id, run_id)
            .await
    }
}

impl InputOrigins {
    pub(super) fn new(state: Arc<LocalState>, live: Weak<LiveSessions>) -> Self {
        Self { state, live }
    }

    pub(super) async fn events(&self, id: &str) -> Result<Vec<SessionEvent>, HarnessError> {
        let live = self
            .live
            .upgrade()
            .ok_or_else(|| HarnessError::cancelled("local session host is shutting down"))?;
        let managed = live.read().await.get(id).cloned();
        if let Some(managed) = managed {
            return Ok(managed.harness.events().await);
        }
        JsonlEventStore::new(&self.state.sessions_dir(), &SessionId::new(id))
            .load_events()
            .await
    }

    pub(super) async fn resolve(
        &self,
        id: &str,
        run_id: &RunId,
    ) -> Result<crate::ModelInputOrigin, HarnessError> {
        let mut id = id.to_owned();
        let mut run_id = run_id.clone();
        let mut schedule_origins = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert((id.clone(), run_id.clone())) || visited.len() > 64 {
                return Err(HarnessError::policy("model input lineage contains a cycle"));
            }
            let session = self
                .state
                .session(&id)
                .await
                .ok_or_else(|| HarnessError::invalid("unknown account model session"))?;
            let events = self.events(&id).await?;
            let (input, source) = events
                .iter()
                .find_map(|event| {
                    if event.run_id == run_id
                        && let SessionEventKind::UserMessage {
                            provenance, source, ..
                        } = &event.kind
                    {
                        Some((provenance.clone(), source.as_ref()))
                    } else {
                        None
                    }
                })
                .ok_or_else(|| HarnessError::policy("model request has no input for its run"))?;
            if matches!(
                input.as_ref().map(|provenance| &provenance.author),
                Some(InputAuthor::Automation {
                    source: ternilo_protocol::AutomatedInputSource::Schedule
                })
            ) {
                let Some(ternilo_protocol::UserMessageSource::Schedule {
                    schedule_id,
                    created_seq,
                    dispatched_seq,
                }) = source
                else {
                    return Err(HarnessError::policy(
                        "scheduled model request has no origin events",
                    ));
                };
                let origin = ternilo_protocol::ScheduleModelOrigin {
                    session_id: session.identity.session_id.clone(),
                    created_seq: *created_seq,
                    dispatched_seq: *dispatched_seq,
                };
                origin.validate()?;
                let created = events.iter().find(|event| event.seq == *created_seq)
                    .filter(|event| matches!(&event.kind, SessionEventKind::ScheduleChanged { change: ternilo_protocol::ScheduleChange::Create { schedule } } if schedule.id == *schedule_id))
                    .ok_or_else(|| HarnessError::policy("scheduled model creation is missing"))?;
                if !events.iter().any(|event| event.seq == *dispatched_seq && matches!(&event.kind, SessionEventKind::ScheduleChanged { change: ternilo_protocol::ScheduleChange::Dispatch { id, run_id: Some(dispatched_run), .. } } if id == schedule_id && dispatched_run == &run_id)) {
                    return Err(HarnessError::policy("scheduled model dispatch differs from its run"));
                }
                schedule_origins.push(origin);
                run_id = created.run_id.clone();
                continue;
            }
            if matches!(
                input.as_ref().map(|provenance| &provenance.author),
                Some(InputAuthor::Automation {
                    source: ternilo_protocol::AutomatedInputSource::Subagent
                })
            ) && session.subagent.is_some()
                && let Some(parent) = session.parent_session_id
            {
                parent.as_str().clone_into(&mut id);
            } else {
                return Ok(crate::ModelInputOrigin {
                    session_id: session.identity.session_id,
                    provenance: input,
                    schedule_origins,
                });
            }
        }
    }
}
