use super::{LocalApplication, model_origins::InputOrigins};
use std::{future::Future, pin::Pin, sync::Arc};
use ternilo_kernel::{RunAuthorization, SessionEventStore};
use ternilo_protocol::{ErrorCode, HarnessError, InputProvenance, RunId};
use ternilo_transport::{
    NodeCleanupReceipt, NodeCleanupRequest, NodeCleanupSnapshot, NodeCleanupState,
    NodeInputAuthorization,
};

pub(super) struct AccountRunAuthorization {
    pub(super) origins: InputOrigins,
    pub(super) accounts: Arc<crate::account_authorizations::AccountAuthorizations>,
    pub(super) session_id: String,
}

impl RunAuthorization for AccountRunAuthorization {
    fn check<'a>(
        &'a self,
        run_id: &'a RunId,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if self.accounts.binding().await.is_none() {
                return Ok(());
            }
            let origin = self.origins.resolve(&self.session_id, run_id).await?;
            self.accounts.check(origin.provenance.as_ref()).await
        })
    }
}

impl LocalApplication {
    pub async fn node_storage_instance_id(&self) -> String {
        self.account_authorizations.storage_instance_id().await
    }
    pub async fn account_server_binding(&self) -> Option<crate::LocalServerBinding> {
        self.account_authorizations.binding().await
    }
    pub async fn record_account_input_authorization(
        &self,
        provenance: &InputProvenance,
        authorization: &NodeInputAuthorization,
    ) -> Result<(), HarnessError> {
        self.account_authorizations
            .accept(provenance, authorization)
            .await
    }

    pub async fn disconnect_account_authorizations(&self) {
        self.account_authorizations.disconnect().await;
    }

    pub async fn synchronize_account_authorizations(
        self: &Arc<Self>,
        snapshot: &NodeCleanupSnapshot,
    ) -> Result<Vec<NodeCleanupReceipt>, HarnessError> {
        self.account_authorizations.synchronize(snapshot).await?;
        self.execution_resources.prune().await?;
        // Repeated snapshots retry cleanup after an interrupted local pass.
        for session in self.state.snapshot().await.sessions {
            let id = session.identity.session_id.as_str();
            let managed = self.live.read().await.get(id).cloned();
            if let Some(managed) = managed
                && let Some(run_id) = managed.harness.active_run().await
            {
                let provenance = self.cleanup_input_provenance(id, &run_id).await?;
                if self
                    .account_authorizations
                    .check(provenance.as_ref())
                    .await
                    .is_err_and(|error| error.code == ErrorCode::PolicyDenied)
                {
                    managed.harness.cancel(run_id).await?;
                    self.interaction.cancel_session(id).await;
                }
            }
            for item in self.inbox.snapshot(id).await?.items {
                if self
                    .account_authorizations
                    .check(item.provenance.as_ref())
                    .await
                    .is_err_and(|error| error.code == ErrorCode::PolicyDenied)
                {
                    self.inbox.finish(id, &item.id).await?;
                }
            }
        }
        let mut receipts = Vec::new();
        for request in &snapshot.requests {
            if request.state == NodeCleanupState::Confirmed {
                continue;
            }
            let previous = self
                .account_authorizations
                .receipt(&request.request_id)
                .await;
            let receipt = match previous.as_ref() {
                Some(receipt) if receipt.state == NodeCleanupState::Confirmed => receipt.clone(),
                _ => {
                    let result = self.finish_account_cleanup(request).await;
                    let detail = result.as_ref().err().map(cleanup_issue);
                    if let Err(error) = &result
                        && previous
                            .as_ref()
                            .and_then(|receipt| receipt.detail.as_ref())
                            != detail.as_ref()
                    {
                        eprintln!("Node cleanup {}: {error}", request.request_id);
                    }
                    NodeCleanupReceipt {
                        storage_instance_id: self.node_storage_instance_id().await,
                        request_id: request.request_id.clone(),
                        status_revision: request.status_revision,
                        state: if result.is_ok() {
                            NodeCleanupState::Confirmed
                        } else {
                            NodeCleanupState::Pending
                        },
                        detail,
                    }
                }
            };
            self.account_authorizations
                .save_receipt(receipt.clone())
                .await?;
            if receipt.state != request.state || receipt.detail != request.detail {
                receipts.push(receipt);
            }
        }
        self.resume_pending_submissions().await?;
        self.resume_server_schedules().await?;
        Ok(receipts)
    }
}

impl LocalApplication {
    async fn cleanup_input_provenance(
        &self,
        id: &str,
        run_id: &RunId,
    ) -> Result<Option<InputProvenance>, HarnessError> {
        // A claimed input can own the active run before UserMessage is committed.
        // Its durable inbox row already carries the authenticated author evidence.
        if let Some(item) = self
            .inbox
            .snapshot(id)
            .await?
            .items
            .into_iter()
            .find(|item| item.run_id == *run_id)
            && item.provenance.as_ref().is_some_and(|input| {
                !matches!(
                    input.author,
                    ternilo_protocol::InputAuthor::Automation { .. }
                )
            })
        {
            return Ok(item.provenance);
        }
        Ok(self.model_input_origin(id, run_id).await?.provenance)
    }

    async fn affected_cleanup_run(
        &self,
        request: &NodeCleanupRequest,
        session_id: &str,
        run_id: &RunId,
    ) -> Result<bool, HarnessError> {
        let Some(provenance) = self.cleanup_input_provenance(session_id, run_id).await? else {
            return Ok(false);
        };
        if !matches!(&provenance.author,ternilo_protocol::InputAuthor::Account {user_id,..} if user_id==&request.user_id)
        {
            return Ok(false);
        }
        Ok(self
            .account_authorizations
            .check(Some(&provenance))
            .await
            .is_err_and(|error| error.code == ErrorCode::PolicyDenied))
    }

    async fn finish_account_cleanup(
        &self,
        request: &NodeCleanupRequest,
    ) -> Result<(), HarnessError> {
        for (id, owner) in self.execution_resources.owners().await {
            if self
                .affected_cleanup_run(request, &owner.session_id, &owner.run_id)
                .await?
            {
                self.execution_resources.stop(&id).await?;
            }
        }
        for session in self.state.snapshot().await.sessions {
            let id = session.identity.session_id.as_str();
            let managed = self.live.read().await.get(id).cloned();
            if let Some(managed) = managed
                && let Some(run_id) = managed.harness.active_run().await
                && self.affected_cleanup_run(request, id, &run_id).await?
            {
                managed.harness.cancel(run_id.clone()).await?;
                self.interaction.cancel_session(id).await;
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    while managed.harness.active_run().await.as_ref() == Some(&run_id) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                })
                .await
                .map_err(|_| {
                    HarnessError::unavailable("revoked account driver has not finished cleanup")
                })?;
            }
        }
        for session in self.state.snapshot().await.sessions {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.cleanup_session_work(request, session.identity.session_id.as_str()),
            )
            .await
            .map_err(|_| {
                HarnessError::unavailable(
                    "session changed while account cleanup was being recorded",
                )
            })??;
        }
        self.execution_resources.prune().await?;
        for (_, owner) in self.execution_resources.owners().await {
            if self
                .affected_cleanup_run(request, &owner.session_id, &owner.run_id)
                .await?
            {
                return Err(HarnessError::unavailable(
                    "revoked account still owns execution resources",
                ));
            }
        }
        Ok(())
    }

    async fn cleanup_session_work(
        &self,
        request: &NodeCleanupRequest,
        id: &str,
    ) -> Result<(), HarnessError> {
        use ternilo_protocol::{GoalStatus, ScheduleChange, SessionEvent, SessionEventKind};
        let lifecycle = self.session_lifecycle(id).await;
        let _lifecycle = lifecycle.lock().await;
        let Some(session) = self.state.session(id).await else {
            return Ok(());
        };
        let managed = self.live.read().await.get(id).cloned();
        let store =
            super::JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
                .with_index(
                    self.session_archive.index(),
                    &session.identity.session_id,
                    &session.workspace_id,
                )
                .with_notifications(
                    self.event_notifications.clone(),
                    &session.identity.session_id,
                )
                .with_execution_activity(
                    Arc::clone(&self.execution_activity),
                    &session.identity.session_id,
                );
        loop {
            let events = match &managed {
                Some(managed) => managed.harness.events().await,
                None => store.load_events().await?,
            };
            let mut change = None;
            for schedule in ternilo_builtins::pending_schedules(&events)? {
                let created = events.iter().find(|event| matches!(&event.kind,
                    SessionEventKind::ScheduleChanged { change: ScheduleChange::Create { schedule: record } } if record.id == schedule.id))
                    .ok_or_else(|| HarnessError::policy("scheduled cleanup has no creation event"))?;
                if self
                    .affected_cleanup_run(request, id, &created.run_id)
                    .await?
                {
                    change = Some((
                        created.run_id.clone(),
                        SessionEventKind::ScheduleChanged {
                            change: ScheduleChange::Delete { id: schedule.id },
                        },
                    ));
                    break;
                }
            }
            if change.is_none()
                && let Some(goal) = events
                    .iter()
                    .rev()
                    .find(|event| matches!(event.kind, SessionEventKind::GoalUpdated { .. }))
                && let SessionEventKind::GoalUpdated {
                    objective,
                    status: GoalStatus::Active,
                } = &goal.kind
                && self.affected_cleanup_run(request, id, &goal.run_id).await?
            {
                change = Some((
                    goal.run_id.clone(),
                    SessionEventKind::GoalUpdated {
                        objective: objective.clone(),
                        status: GoalStatus::Blocked,
                    },
                ));
            }
            let Some((run_id, kind)) = change else {
                return Ok(());
            };
            let seq = events
                .len()
                .try_into()
                .map_err(|_| HarnessError::execution("session sequence exceeds u64"))?;
            if let Some(managed) = &managed {
                // Live writers can publish between inspection and this append. Re-read
                // instead of deleting a newer schedule or blocking another author's goal.
                managed
                    .harness
                    .append_event_if_next_seq(seq, run_id, kind)
                    .await?;
            } else {
                store
                    .append(SessionEvent {
                        seq,
                        occurred_at_ms: super::now_ms()?,
                        run_id,
                        kind,
                    })
                    .await?;
            }
        }
    }
}

fn cleanup_issue(error: &HarnessError) -> String {
    if error
        .message
        .starts_with("Node restarted before execution resource completion")
    {
        "process_state_unknown"
    } else if error.message == "process group has not finished cleanup" {
        "process_exit_pending"
    } else if error.message == "revoked account driver has not finished cleanup"
        || error.message == "session changed while account cleanup was being recorded"
    {
        "session_busy"
    } else {
        "cleanup_failed"
    }
    .to_owned()
}
