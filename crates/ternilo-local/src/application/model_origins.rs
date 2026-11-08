use super::{JsonlEventStore, LiveSessions, LocalApplication, LocalState, ManagedSession};
use std::sync::{Arc, Weak};
use ternilo_protocol::{
    HarnessError, InputAuthor, RunId, SessionEvent, SessionEventKind, SessionId, SessionInputFacts,
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

    async fn live(&self, id: &str) -> Result<Option<Arc<ManagedSession>>, HarnessError> {
        let live = self
            .live
            .upgrade()
            .ok_or_else(|| HarnessError::cancelled("local session host is shutting down"))?;
        Ok(live.read().await.get(id).cloned())
    }

    pub(super) async fn latest_input_run(&self, id: &str) -> Result<Option<RunId>, HarnessError> {
        if let Some(managed) = self.live(id).await? {
            return Ok(managed.harness.summary().await.latest_input_run);
        }
        let reader = JsonlEventStore::new(&self.state.sessions_dir(), &SessionId::new(id))
            .reader()
            .await;
        tokio::task::spawn_blocking(move || {
            let mut latest = None;
            reader.scan(|event| {
                if matches!(event.kind, SessionEventKind::UserMessage { .. }) {
                    latest = Some(event.run_id);
                }
                Ok(())
            })?;
            Ok(latest)
        })
        .await
        .map_err(|error| HarnessError::execution(format!("read latest input: {error}")))?
    }

    async fn input(
        &self,
        id: &str,
        run_id: &RunId,
    ) -> Result<Option<SessionInputFacts>, HarnessError> {
        if let Some(managed) = self.live(id).await? {
            return Ok(managed
                .harness
                .run_facts(run_id.clone())
                .await
                .and_then(|run| run.input));
        }
        let reader = JsonlEventStore::new(&self.state.sessions_dir(), &SessionId::new(id))
            .reader()
            .await;
        let run_id = run_id.clone();
        tokio::task::spawn_blocking(move || {
            let mut input = None;
            reader.scan(|event| {
                if input.is_none()
                    && event.run_id == run_id
                    && let SessionEventKind::UserMessage {
                        provenance, source, ..
                    } = event.kind
                {
                    input = Some(SessionInputFacts {
                        seq: event.seq,
                        provenance,
                        source,
                    });
                }
                Ok(())
            })?;
            Ok(input)
        })
        .await
        .map_err(|error| HarnessError::execution(format!("read run input: {error}")))?
    }

    async fn event_at(&self, id: &str, seq: u64) -> Result<Option<SessionEvent>, HarnessError> {
        if let Some(managed) = self.live(id).await? {
            return Ok(managed.harness.event_at(seq).await);
        }
        JsonlEventStore::new(&self.state.sessions_dir(), &SessionId::new(id))
            .event_at(seq)
            .await
    }

    pub(super) async fn resolve(
        &self,
        id: &str,
        run_id: &RunId,
    ) -> Result<crate::ModelInputOrigin, HarnessError> {
        let mut id = id.to_owned();
        let mut run_id = run_id.clone();
        let mut run_origins = Vec::new();
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
            let SessionInputFacts {
                seq,
                provenance: input,
                source,
            } = self
                .input(&id, &run_id)
                .await?
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
                }) = source.as_ref()
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
                let created = self.event_at(&id, *created_seq).await?
                    .filter(|event| matches!(&event.kind, SessionEventKind::ScheduleChanged { change: ternilo_protocol::ScheduleChange::Create { schedule } } if schedule.id == *schedule_id))
                    .ok_or_else(|| HarnessError::policy("scheduled model creation is missing"))?;
                if !self.event_at(&id, *dispatched_seq).await?.is_some_and(|event| matches!(&event.kind, SessionEventKind::ScheduleChanged { change: ternilo_protocol::ScheduleChange::Dispatch { id, run_id: Some(dispatched_run), .. } } if id == schedule_id && dispatched_run == &run_id)) {
                    return Err(HarnessError::policy("scheduled model dispatch differs from its run"));
                }
                run_origins.push(ternilo_protocol::ModelRunOrigin::Schedule(origin));
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
                if let Some(ternilo_protocol::UserMessageSource::Subagent { parent_run_id }) =
                    source
                {
                    parent_run_id.validate()?;
                    run_origins.push(ternilo_protocol::ModelRunOrigin::Subagent {
                        session_id: session.identity.session_id.clone(),
                        input_seq: seq,
                    });
                    run_id = parent_run_id;
                }
                parent.as_str().clone_into(&mut id);
            } else {
                return Ok(crate::ModelInputOrigin {
                    session_id: session.identity.session_id,
                    provenance: input,
                    run_origins,
                });
            }
        }
    }
}
