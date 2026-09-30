use std::sync::Arc;

use ternilo_protocol::{
    HarnessError, InputProvenance, QueueEditRequest, SessionEvent, SessionEventKind,
    SessionInboxSnapshot, SessionSubmission, SessionSubmissionRequest, SubmissionContent,
    SubmissionDelivery, SubmissionId, SubmissionPlacement, UserMessageSource,
};
use tokio::sync::Mutex;

use super::{JsonlEventStore, LocalApplication, TurnInput, TurnRequest, now_ms};

impl LocalApplication {
    pub(super) async fn recover_session_work(&self) -> Result<(), HarnessError> {
        for session in self.state.snapshot().await.sessions {
            let session_id = session.identity.session_id.as_str();
            let events =
                JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
                    .load_events()
                    .await?;
            self.inbox
                .recover(session_id, &consumed_submission_ids(&events))
                .await?;
            // Timers need a live Harness; reading history must remain read-only.
            if session.archived_at_ms.is_none()
                && session.server_model.is_none()
                && ternilo_builtins::has_pending_schedules(&events)?
            {
                self.ensure_session_locked(session_id).await?;
            }
        }
        Ok(())
    }

    pub async fn resume_server_schedules(&self) -> Result<(), HarnessError> {
        if !self.server_models.installed() {
            return Ok(());
        }
        for session in self.state.snapshot().await.sessions {
            if session.archived_at_ms.is_some() || session.server_model.is_none() {
                continue;
            }
            let session_id = session.identity.session_id.as_str();
            let events = self.events_after(session_id, None).await?;
            if ternilo_builtins::has_pending_schedules(&events)? {
                self.addressable_session(session_id).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn settle_session_inbox(
        &self,
        session_id: &str,
        events: &[SessionEvent],
        cancelled: bool,
    ) -> Result<(), HarnessError> {
        self.inbox
            .settle_run(
                session_id,
                &consumed_submission_ids(events),
                cancelled,
                now_ms()?,
            )
            .await
    }

    async fn submission_driver(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut drivers = self.submission_drivers.lock().await;
        Arc::clone(
            drivers
                .entry(session_id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    fn spawn_submission_driver(self: &Arc<Self>, session_id: String) {
        let mut tasks = self.submission_tasks.lock().expect("submission task lock");
        if self.stopping.is_cancelled() {
            return;
        }
        while tasks.try_join_next().is_some() {}
        let application = Arc::clone(self);
        tasks.spawn(async move {
            if let Err(error) = application.drive_session_inbox(&session_id).await
                && !error.is_cancelled()
            {
                let _ = application
                    .inbox
                    .record_error(&session_id, error.to_string())
                    .await;
            }
        });
    }

    pub async fn resume_pending_submissions(self: &Arc<Self>) -> Result<(), HarnessError> {
        self.stopping.check()?;
        for session in self.state.snapshot().await.sessions {
            let session_id = session.identity.session_id.as_str().to_owned();
            if self.inbox.first_queued(&session_id).await?.is_some() {
                self.spawn_submission_driver(session_id);
            }
        }
        Ok(())
    }

    pub async fn submit_session(
        self: &Arc<Self>,
        session_id: &str,
        request: SessionSubmissionRequest,
    ) -> Result<SessionSubmission, HarnessError> {
        self.submit_session_with_provenance(session_id, request, self.local_input_provenance()?)
            .await
    }

    /// Accept an input authored by the authenticated caller of a trusted transport.
    pub async fn submit_session_with_provenance(
        self: &Arc<Self>,
        session_id: &str,
        request: SessionSubmissionRequest,
        provenance: InputProvenance,
    ) -> Result<SessionSubmission, HarnessError> {
        self.stopping.check()?;
        request.validate()?;
        provenance.validate()?;
        self.account_authorizations.check(Some(&provenance)).await?;
        if self.state.session(session_id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown session {session_id:?}"
            )));
        }
        if let Some(target) = request.content.regeneration_target() {
            let inbox = self.session_inbox(session_id).await?;
            if inbox.active_run_id.is_some() || !inbox.items.is_empty() {
                return Err(HarnessError::conflict(
                    "stop the current run and clear queued inputs before replacing a turn",
                ));
            }
            ternilo_protocol::validate_regeneration(&self.events(session_id).await?, target)?;
        }
        let input = turn_input(&request.content);
        self.validate_device_input(session_id, Some(&provenance))
            .await?;
        self.validate_turn_model(session_id, &input).await?;
        let attachments = self.attachments.save_many(request.attachments).await?;
        let now = now_ms()?;
        let submission_id = provenance.input_id.clone();
        let run_id = request
            .run_id
            .unwrap_or(ternilo_protocol::RunId::new(self.next_id("run")?));
        let item = SessionSubmission {
            id: submission_id.clone(),
            provenance: Some(provenance),
            run_id,
            content: request.content,
            references: request.references,
            attachments,
            placement: SubmissionPlacement::Queued,
            created_at_ms: now,
            updated_at_ms: now,
        };
        {
            let lifecycle = self.session_lifecycle(session_id).await;
            let _lifecycle = lifecycle.lock().await;
            let _admission = self.account_authorizations.input_admission().await;
            self.account_authorizations
                .check(item.provenance.as_ref())
                .await?;
            self.inbox.enqueue(session_id, item.clone()).await?;
            self.state.mark_session_started(session_id, now).await?;
            self.invalidate(
                Some(session_id),
                crate::LocalInvalidationCategory::Activity,
                None,
            );
        }
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid("submission session no longer exists"))?;
        self.session_archive
            .index()
            .append_uploads_fail_soft(
                session_id.to_owned(),
                session.workspace_id.as_str().to_owned(),
                item.attachments
                    .iter()
                    .enumerate()
                    .map(
                        |(index, attachment)| ternilo_protocol::AcceptedSessionUpload {
                            submission_id: item.id.clone(),
                            attachment_index: u32::try_from(index)
                                .expect("validated attachment count"),
                            created_at_ms: item.created_at_ms,
                            submitted_run_id: item.run_id.clone(),
                            attachment: attachment.clone(),
                        },
                    )
                    .collect(),
            )
            .await;
        let accepted = if request.delivery == SubmissionDelivery::Steer {
            self.steer_session_queue_item(session_id, &submission_id)
                .await?
        } else {
            item
        };
        self.spawn_submission_driver(session_id.to_owned());
        Ok(accepted)
    }

    pub async fn session_inbox(
        &self,
        session_id: &str,
    ) -> Result<SessionInboxSnapshot, HarnessError> {
        let session = self
            .state
            .session(session_id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {session_id:?}")))?;
        let active_run_id = match self.live.read().await.get(session_id).cloned() {
            Some(managed) => managed.harness.active_run().await,
            None => None,
        };
        let inbox = self.inbox.snapshot(session_id).await?;
        Ok(SessionInboxSnapshot {
            session_id: session.identity.session_id,
            active_run_id,
            paused: inbox.paused,
            error: inbox.error,
            items: inbox.items,
        })
    }

    pub async fn edit_session_queue_item(
        &self,
        session_id: &str,
        item_id: SubmissionId,
        request: QueueEditRequest,
    ) -> Result<SessionSubmission, HarnessError> {
        if self.state.session(session_id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown session {session_id:?}"
            )));
        }
        item_id.validate()?;
        self.inbox
            .edit(session_id, &item_id, request, now_ms()?)
            .await
    }

    pub async fn remove_session_queue_item(
        &self,
        session_id: &str,
        item_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        if self.state.session(session_id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown session {session_id:?}"
            )));
        }
        item_id.validate()?;
        self.inbox.remove_queued(session_id, &item_id).await
    }

    pub async fn steer_session_queue_item(
        self: &Arc<Self>,
        session_id: &str,
        item_id: &SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        item_id.validate()?;
        let snapshot = self.session_inbox(session_id).await?;
        let item = snapshot
            .items
            .into_iter()
            .find(|item| &item.id == item_id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown submission {item_id}")))?;
        if item.placement != SubmissionPlacement::Queued {
            return Ok(item);
        }
        self.inbox.pause(session_id).await?;
        if let Some(run_id) = snapshot.active_run_id {
            self.cancel_turn(session_id, run_id.as_str()).await?;
        }
        let driver = self.submission_driver(session_id).await;
        let _driver_guard = driver.lock().await;
        let managed = self.addressable_session(session_id).await?;
        let _turn_guard = managed.gate.lock().await;
        self.inbox.resume(session_id).await?;
        Ok(item)
    }

    pub async fn steer_queued_session_item(
        self: &Arc<Self>,
        session_id: &str,
        item_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        if self.state.session(session_id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown session {session_id:?}"
            )));
        }
        let item = self.steer_session_queue_item(session_id, &item_id).await?;
        self.spawn_submission_driver(session_id.to_owned());
        Ok(item)
    }

    async fn drive_session_inbox(self: &Arc<Self>, session_id: &str) -> Result<(), HarnessError> {
        let driver = self.submission_driver(session_id).await;
        let _driver_guard = driver.lock().await;
        loop {
            self.stopping.check()?;
            if let Some(item) = self.inbox.first_queued(session_id).await?
                && let Err(error) = self
                    .account_authorizations
                    .check(item.provenance.as_ref())
                    .await
            {
                if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                    self.inbox.remove_queued(session_id, &item.id).await?;
                    continue;
                }
                return Ok(());
            }
            // Profile changes remove the runtime before committing the new selection.
            // A queue driver must acquire the lifecycle lock before booting a replacement.
            let managed = self.addressable_session(session_id).await?;
            // A task created by a busy submission waits for that turn before
            // deciding whether cancellation parked the durable FIFO.
            let current_turn = Arc::clone(&managed.gate).lock_owned().await;
            drop(current_turn);
            self.stopping.check()?;
            let mut batch = self
                .inbox
                .claim_batch(session_id, now_ms()?)
                .await?
                .into_iter();
            let Some(item) = batch.next() else {
                return Ok(());
            };
            let result = self
                .run_turn_input(
                    session_id,
                    TurnRequest {
                        run_id: Some(item.run_id.as_str().to_owned()),
                        input: turn_input(&item.content),
                        references: item.references.clone(),
                        attachments: item.attachments.clone(),
                        source_override: Some(submission_source(&item, SubmissionDelivery::Queue)),
                        provenance: item.provenance.clone(),
                        require_unpaused_inbox: true,
                        additional_submissions: batch.collect(),
                    },
                )
                .await;
            let revoked = self
                .account_authorizations
                .check(item.provenance.as_ref())
                .await
                .is_err_and(|error| error.code == ternilo_protocol::ErrorCode::PolicyDenied);
            if result.is_err() {
                self.settle_session_inbox(
                    session_id,
                    &managed.harness.events().await,
                    result.as_ref().is_err_and(HarnessError::is_cancelled) && !revoked,
                )
                .await?;
            }
            match result {
                Ok(_) => {
                    self.inbox.finish(session_id, &item.id).await?;
                }
                Err(_) if revoked => {
                    self.inbox.finish(session_id, &item.id).await?;
                }
                Err(error) if error.is_cancelled() => {
                    if self
                        .inbox
                        .snapshot(session_id)
                        .await?
                        .items
                        .iter()
                        .any(|current| current.id == item.id)
                    {
                        self.inbox.requeue(session_id, &item.id, now_ms()?).await?;
                    }
                    self.inbox.pause(session_id).await?;
                    return Ok(());
                }
                Err(error) => {
                    let still_pending = self
                        .inbox
                        .snapshot(session_id)
                        .await?
                        .items
                        .iter()
                        .any(|current| current.id == item.id);
                    if still_pending {
                        self.inbox.requeue(session_id, &item.id, now_ms()?).await?;
                        self.inbox
                            .record_error(session_id, error.to_string())
                            .await?;
                        return Ok(());
                    }
                    // Once user/message is durable the failed turn consumed
                    // this occurrence; continue with the next queued turn.
                }
            }
        }
    }
}

pub(super) fn turn_input(content: &SubmissionContent) -> TurnInput {
    match content.skill_name() {
        None => TurnInput::Prompt(content.input().to_owned()),
        Some(name) => TurnInput::Skill {
            name: name.to_owned(),
            request: content.input().to_owned(),
        },
    }
}

pub(super) fn submission_source(
    item: &SessionSubmission,
    delivery: SubmissionDelivery,
) -> UserMessageSource {
    UserMessageSource::Submission {
        regenerate_from: item.content.regeneration_target(),
        submission_id: item.id.clone(),
        created_at_ms: item.created_at_ms,
        delivery,
        skill_name: item.content.skill_name().map(str::to_owned),
    }
}

fn consumed_submission_ids(events: &[SessionEvent]) -> Vec<SubmissionId> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                source: Some(UserMessageSource::Submission { submission_id, .. }),
                ..
            } => Some(submission_id.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
