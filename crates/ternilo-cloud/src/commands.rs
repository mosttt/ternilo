use std::{collections::BTreeSet, time::Duration};

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, RunId, SessionId, TenantId, UserId};
use ternilo_storage::{Backend, Json, Transaction, backend, for_update};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, ExecutorCapabilities, ExecutorCapability,
    ExecutorCommand, ExecutorCommandBody, ExecutorHello, ExecutorId, ExecutorKind,
};

use crate::{CloudSessionRecord, CloudStore, store};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudWorkerIdentity {
    pub worker_id: ExecutorId,
    pub instance_nonce: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudWorkerRecord {
    pub capacity: crate::WorkerCapacity,
    pub identity: CloudWorkerIdentity,
    pub hello: ExecutorHello,
    pub registered_at_ms: u64,
    pub last_seen_at_ms: u64,
    pub lease_expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CloudSessionCommandState {
    Pending,
    Inflight,
    Completed,
    Expired,
    Indeterminate,
}

impl CloudSessionCommandState {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "pending" => Ok(Self::Pending),
            "inflight" => Ok(Self::Inflight),
            "completed" => Ok(Self::Completed),
            "expired" => Ok(Self::Expired),
            "indeterminate" => Ok(Self::Indeterminate),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown cloud Session command state {value:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CloudCommandDelivery {
    ReadOnly,
    TargetRun {
        run_id: RunId,
        writer_fencing_token: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloudSessionCommandDraft {
    pub session_id: SessionId,
    pub command: ExecutorCommand,
    pub required_capability: ExecutorCapability,
    pub required_catalog_revision: Option<String>,
    pub delivery: CloudCommandDelivery,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CloudSessionCommandRecord {
    pub contributor_user_id: Option<UserId>,
    pub actor_user_id: UserId,
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub session_id: SessionId,
    pub command_seq: u64,
    pub command: ExecutorCommand,
    pub required_capability: ExecutorCapability,
    pub required_catalog_revision: Option<String>,
    pub delivery: CloudCommandDelivery,
    pub state: CloudSessionCommandState,
    pub attempt_count: u32,
    pub reply: Option<CommandReply>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaimedCloudSessionCommand {
    pub actor_user_id: UserId,
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub session_id: SessionId,
    pub command_seq: u64,
    pub command: ExecutorCommand,
    pub required_capability: ExecutorCapability,
    pub delivery: CloudCommandDelivery,
    pub attempt_count: u32,
}

impl CloudSessionCommandDraft {
    fn validate(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        self.session_id.validate()?;
        self.command.validate(now_ms)?;
        if &self.command.scope.tenant_id != tenant_id || &self.command.scope.user_id != user_id {
            return Err(HarnessError::invalid(
                "cloud Session command scope does not match its owner",
            ));
        }
        if self
            .required_catalog_revision
            .as_ref()
            .is_some_and(|revision| revision.trim().is_empty())
        {
            return Err(HarnessError::invalid(
                "required catalog revision must not be empty",
            ));
        }
        self.validate_delivery()
    }

    fn validate_delivery(&self) -> Result<(), HarnessError> {
        match (&self.command.body, &self.delivery) {
            (
                ExecutorCommandBody::Application {
                    request:
                        ApplicationOperation::SessionSkills { session_id }
                        | ApplicationOperation::SessionSkillResolve { session_id, .. },
                },
                CloudCommandDelivery::ReadOnly,
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::Skills => {}
            (
                ExecutorCommandBody::Application {
                    request:
                        ApplicationOperation::SessionCommands { session_id }
                        | ApplicationOperation::SessionServices { session_id },
                },
                CloudCommandDelivery::ReadOnly,
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::AddressedSessionCommands => {}
            (
                ExecutorCommandBody::Application {
                    request:
                        ApplicationOperation::SessionServiceStart { session_id, .. }
                        | ApplicationOperation::SessionServiceStop { session_id, .. },
                },
                CloudCommandDelivery::TargetRun { .. },
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::AddressedSessionCommands => {}
            (
                ExecutorCommandBody::Application {
                    request:
                        ApplicationOperation::SessionReferenceCandidates { session_id, .. }
                        | ApplicationOperation::SessionWorkspace {
                            session_id,
                            request:
                                ternilo_protocol::WorkspaceRequest::Info
                                | ternilo_protocol::WorkspaceRequest::List { .. }
                                | ternilo_protocol::WorkspaceRequest::Read { .. },
                        },
                },
                CloudCommandDelivery::ReadOnly,
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::WorkspaceFiles => {}
            (
                ExecutorCommandBody::Application {
                    request: ApplicationOperation::SessionQueueSteer { session_id, .. },
                },
                CloudCommandDelivery::TargetRun { .. },
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::SessionSteering => {}
            (
                ExecutorCommandBody::Application {
                    request:
                        ApplicationOperation::SessionSubagentFollowup { session_id, .. }
                        | ApplicationOperation::SessionSubagentInterrupt { session_id, .. },
                },
                CloudCommandDelivery::TargetRun { .. },
            ) if session_id == &self.session_id
                && self.required_capability == ExecutorCapability::AddressedSubagents => {}
            (
                ExecutorCommandBody::CancelRun { session_id, run_id },
                CloudCommandDelivery::TargetRun {
                    run_id: target_run_id,
                    ..
                },
            ) if session_id == &self.session_id
                && run_id == target_run_id
                && self.required_capability == ExecutorCapability::RunCancellation => {}
            _ => {
                return Err(HarnessError::invalid(
                    "command is not a supported cloud Session command or its delivery metadata does not match",
                ));
            }
        }
        if let CloudCommandDelivery::TargetRun {
            writer_fencing_token,
            ..
        } = &self.delivery
            && *writer_fencing_token == 0
        {
            return Err(HarnessError::invalid(
                "target run writer fencing token must be positive",
            ));
        }
        Ok(())
    }
}

impl CloudStore {
    pub async fn session_runtime_target(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        action: ternilo_control::ResourceAction,
        now_ms: u64,
    ) -> Result<Option<CloudCommandDelivery>, HarnessError> {
        let mut transaction = self.begin().await?;
        let owner = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            action,
        )
        .await?;
        let row = sqlx::query(
            "SELECT run.run_id, writer.fencing_token FROM cloud_runs AS run
             JOIN cloud_session_writer_leases AS writer
               ON writer.tenant_id=run.tenant_id AND writer.session_id=run.session_id
             WHERE run.tenant_id=$1 AND run.session_id=$2 AND run.user_id=$3
               AND run.state IN ('running','cancel_requested')
               AND run.lease_expires_at_ms>$4 AND writer.expires_at_ms>$4
               AND writer.run_id=run.run_id AND writer.lease_owner=run.lease_owner
               AND writer.fencing_token=run.session_fencing_token",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(owner.as_str())
        .bind(store::to_i64(now_ms, "runtime target time")?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        let target = row
            .map(|row| -> Result<_, HarnessError> {
                Ok(CloudCommandDelivery::TargetRun {
                    run_id: RunId::new(
                        row.try_get::<String, _>("run_id")
                            .map_err(store::database_error)?,
                    ),
                    writer_fencing_token: store::from_i64(
                        row.try_get("fencing_token")
                            .map_err(store::database_error)?,
                        "runtime writer fence",
                    )?,
                })
            })
            .transpose()?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(target)
    }

    pub async fn register_cloud_worker(
        &self,
        hello: &ExecutorHello,
        lease_ttl: Duration,
        now_ms: u64,
    ) -> Result<CloudWorkerIdentity, HarnessError> {
        let mut transaction = self.begin().await?;
        let identity = register_worker_in(&mut transaction, hello, lease_ttl, now_ms).await?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(identity)
    }

    pub async fn heartbeat_cloud_worker(
        &self,
        identity: &CloudWorkerIdentity,
        lease_ttl: Duration,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_worker_identity(identity)?;
        let expiry = lease_expiry(now_ms, lease_ttl, "cloud Worker lease")?;
        let mut transaction = self.begin().await?;
        let changed = sqlx::query("UPDATE cloud_workers SET last_seen_at_ms = $4, lease_expires_at_ms = $5 WHERE worker_id = $1 AND instance_nonce = $2 AND generation = $3 AND lease_expires_at_ms > $4")
            .bind(identity.worker_id.as_str()).bind(&identity.instance_nonce).bind(store::to_i64(identity.generation, "worker generation")?)
            .bind(store::to_i64(now_ms, "worker heartbeat time")?).bind(store::to_i64(expiry, "worker expiry")?)
            .execute(&mut *transaction).await.map_err(store::database_error)?.rows_affected();
        if changed == 1 {
            transaction.commit().await.map_err(store::database_error)?;
            Ok(())
        } else {
            Err(HarnessError::policy(
                "cloud Worker identity lease was lost or fenced",
            ))
        }
    }

    pub async fn cloud_worker(
        &self,
        worker_id: &ExecutorId,
    ) -> Result<Option<CloudWorkerRecord>, HarnessError> {
        worker_id.validate()?;
        let row = sqlx::query(
            "SELECT worker_id, instance_nonce, generation, hello_json,
                    max_active_runs, max_resident_runs, registered_at_ms, last_seen_at_ms, lease_expires_at_ms
             FROM cloud_workers WHERE worker_id = $1",
        )
        .bind(worker_id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(store::database_error)?;
        row.map(|row| decode_worker(&row)).transpose()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn enqueue_session_command(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        draft: &CloudSessionCommandDraft,
        now_ms: u64,
    ) -> Result<CloudSessionCommandRecord, HarnessError> {
        let command_value = canonical_command(&draft.command)?;
        let command_digest =
            Sha256::digest(serde_json::to_vec(&command_value).map_err(|error| {
                HarnessError::execution(format!("encode cloud Session command: {error}"))
            })?);
        let required_capability = capability_name(draft.required_capability)?;
        let (read_only, target_run_id, target_writer_fencing_token) =
            delivery_columns(&draft.delivery)?;
        let mut transaction = self.begin().await?;
        let action = match &draft.command.body {
            ExecutorCommandBody::CancelRun { .. }
            | ExecutorCommandBody::Application {
                request:
                    ApplicationOperation::SessionSubagentInterrupt { .. }
                    | ApplicationOperation::SessionServiceStop { .. },
            } => ternilo_control::ResourceAction::Stop,
            _ if read_only => ternilo_control::ResourceAction::View,
            _ => ternilo_control::ResourceAction::Submit,
        };
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            &draft.session_id,
            action,
        )
        .await?;
        let user_id = &owner_id;
        draft.validate(tenant_id, user_id, now_ms)?;
        if matches!(
            draft.command.body,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionWorkspace { .. }
            }
        ) {
            crate::sharing::require_workspace_read_in(
                &mut transaction,
                tenant_id,
                actor_id,
                &draft.session_id,
            )
            .await?;
        }
        let query = for_update(
            &transaction,
            "SELECT 1 FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
            "SELECT 1 FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 FOR UPDATE",
        );
        let owned_session = sqlx::query_scalar::<_, i32>(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(draft.session_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store::database_error)?;
        if owned_session.is_none() {
            return Err(HarnessError::invalid(
                "cloud Session does not exist for this owner",
            ));
        }
        let command_seq = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(command_seq), -1) + 1
             FROM cloud_session_commands
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(draft.session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        let inserted = sqlx::query(
            "INSERT INTO cloud_session_commands (
                tenant_id, user_id, session_id, command_id, command_seq,
                command_json, command_digest, required_capability,
                required_catalog_revision, read_only, target_run_id,
                target_writer_fencing_token, state, issued_at_ms, expires_at_ms,
                created_at_ms, updated_at_ms, actor_user_id
             ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                'pending', $13, $14, $15, $15, $16
             )
             ON CONFLICT (tenant_id, command_id) DO NOTHING
             RETURNING tenant_id, user_id, actor_user_id, contributor_user_id, session_id, command_seq, command_json,
                       required_capability, required_catalog_revision, read_only,
                       target_run_id, target_writer_fencing_token, state,
                       attempt_count, reply_json, created_at_ms, updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(draft.session_id.as_str())
        .bind(draft.command.command_id.as_str())
        .bind(command_seq)
        .bind(Json(&draft.command))
        .bind(command_digest.as_slice())
        .bind(&required_capability)
        .bind(&draft.required_catalog_revision)
        .bind(i64::from(read_only))
        .bind(target_run_id.as_deref())
        .bind(target_writer_fencing_token)
        .bind(store::to_i64(
            draft.command.issued_at_ms,
            "cloud Session command issue timestamp",
        )?)
        .bind(store::to_i64(
            draft.command.expires_at_ms,
            "cloud Session command expiry",
        )?)
        .bind(store::to_i64(now_ms, "cloud Session command timestamp")?)
        .bind(actor_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        let row = if let Some(row) = inserted {
            row
        } else {
            sqlx::query(
                "SELECT tenant_id, user_id, actor_user_id, contributor_user_id, session_id, command_seq, command_json,
                        required_capability, required_catalog_revision, read_only,
                        target_run_id, target_writer_fencing_token, state,
                        attempt_count, reply_json, created_at_ms, updated_at_ms
                 FROM cloud_session_commands
                 WHERE tenant_id = $1 AND command_id = $2",
            )
            .bind(tenant_id.as_str())
            .bind(draft.command.command_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store::database_error)?
            .ok_or_else(|| {
                HarnessError::conflict(
                    "cloud Session command id already belongs to another command",
                )
            })?
        };
        let record = decode_command_record(&row)?;
        if record.actor_user_id != *actor_id
            || record.user_id != *user_id
            || record.session_id != draft.session_id
            || record.command != draft.command
            || record.required_capability != draft.required_capability
            || record.required_catalog_revision != draft.required_catalog_revision
            || record.delivery != draft.delivery
        {
            return Err(HarnessError::conflict(
                "cloud Session command id was reused with a different body or delivery target",
            ));
        }
        if !read_only {
            crate::sharing::audit_session_in(
                &mut transaction,
                tenant_id,
                actor_id,
                &draft.session_id,
                user_id,
                action,
                now_ms,
            )
            .await?;
        }
        transaction.commit().await.map_err(store::database_error)?;
        Ok(record)
    }

    pub async fn session_command(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        command_id: &CommandId,
    ) -> Result<Option<CloudSessionCommandRecord>, HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        command_id.validate()?;
        let mut transaction = self.begin().await?;
        set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let row = sqlx::query(
            "SELECT tenant_id, user_id, actor_user_id, contributor_user_id, session_id, command_seq, command_json,
                    required_capability, required_catalog_revision, read_only,
                    target_run_id, target_writer_fencing_token, state,
                    attempt_count, reply_json, created_at_ms, updated_at_ms
             FROM cloud_session_commands
             WHERE tenant_id = $1 AND command_id = $2 AND user_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(command_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        transaction.commit().await.map_err(store::database_error)?;
        row.map(|row| decode_command_record(&row)).transpose()
    }

    pub async fn wait_for_session_command_reply(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        command_id: &CommandId,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Option<CommandReply>, HarnessError> {
        if poll_interval.is_zero() {
            return Err(HarnessError::invalid(
                "command reply poll interval must be positive",
            ));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let record = self.session_command(tenant_id, user_id, command_id).await?;
            let Some(record) = record else {
                return Ok(None);
            };
            if let Some(reply) = record.reply {
                return Ok(Some(reply));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(poll_interval.min(deadline - now)).await;
        }
    }

    pub async fn claim_session_commands(
        &self,
        identity: &CloudWorkerIdentity,
        capabilities: &ExecutorCapabilities,
        dispatch_lease_ttl: Duration,
        limit: u32,
        now_ms: u64,
    ) -> Result<Vec<ClaimedCloudSessionCommand>, HarnessError> {
        validate_worker_identity(identity)?;
        if limit == 0 || limit > 100 {
            return Err(HarnessError::invalid(
                "cloud Session command claim limit must be between 1 and 100",
            ));
        }
        let dispatch_expiry = lease_expiry(now_ms, dispatch_lease_ttl, "command dispatch lease")?;
        let now = store::to_i64(now_ms, "command claim time")?;
        let mut transaction = self.begin().await?;
        let Some(worker) = worker_in(&mut transaction, identity, now_ms, true).await? else {
            return Ok(Vec::new());
        };
        let storage_id = Self::worker_storage_in(&mut transaction, &identity.worker_id).await?;
        let gate = if backend(&transaction) == Backend::Postgres {
            "SELECT ternilo_cloud_claim_gate()"
        } else {
            "SELECT 1-claims_paused FROM cloud_runtime_control WHERE singleton=1"
        };
        let claims_allowed = sqlx::query_scalar::<_, i64>(gate)
            .fetch_one(&mut *transaction)
            .await
            .map_err(store::database_error)?
            != 0;
        if !capabilities
            .iter()
            .any(|value| *value == ExecutorCapability::AddressedSessionCommands)
        {
            return Ok(Vec::new());
        }
        let candidates = command_scopes_in(
            &mut transaction,
            "claim",
            identity.worker_id.as_str(),
            identity.generation,
            now_ms,
        )
        .await?;
        let mut claimed = Vec::new();
        for candidate in candidates {
            let Some(row) =
                command_in(&mut transaction, &candidate.0, &candidate.1, &candidate.2).await?
            else {
                continue;
            };
            let record = decode_command_record(&row)?;
            let expires: i64 = row
                .try_get("expires_at_ms")
                .map_err(store::database_error)?;
            let dispatch: Option<i64> = row
                .try_get("dispatch_lease_until_ms")
                .map_err(store::database_error)?;
            let read_only = matches!(record.delivery, CloudCommandDelivery::ReadOnly);
            if (!claims_allowed && read_only)
                || expires <= store::to_i64(now_ms, "command claim time")?
                || !(record.state == CloudSessionCommandState::Pending
                    || (record.state == CloudSessionCommandState::Inflight
                        && read_only
                        && dispatch.is_some_and(|expiry| expiry <= now)))
                || !capabilities
                    .iter()
                    .any(|value| *value == record.required_capability)
                || record
                    .required_catalog_revision
                    .as_ref()
                    .is_some_and(|revision| revision != &worker.hello.catalog_revision)
                || !claim_target_available(&mut transaction, &row, identity, now_ms).await?
            {
                continue;
            }
            if !Self::ensure_tenant_storage_in(
                &mut transaction,
                &record.tenant_id,
                &storage_id,
                now_ms,
            )
            .await?
            {
                continue;
            }
            let updated = sqlx::query("UPDATE cloud_session_commands SET state = 'inflight', lease_owner = $3, worker_generation = $4, dispatch_lease_until_ms = $5, attempt_count = attempt_count + 1, updated_at_ms = CASE WHEN updated_at_ms > $6 THEN updated_at_ms ELSE $6 END WHERE tenant_id = $1 AND command_id = $2 RETURNING *")
                .bind(candidate.0.as_str()).bind(candidate.2.as_str()).bind(identity.worker_id.as_str())
                .bind(store::to_i64(identity.generation, "worker generation")?).bind(store::to_i64(dispatch_expiry, "dispatch expiry")?)
                .bind(store::to_i64(now_ms, "command claim time")?).fetch_one(&mut *transaction).await.map_err(store::database_error)?;
            claimed.push(decode_claimed_command(&updated)?);
            if claimed.len() == usize::try_from(limit).expect("command claim limit is at most 100")
            {
                break;
            }
        }
        transaction.commit().await.map_err(store::database_error)?;
        Ok(claimed)
    }

    pub async fn complete_session_command(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        reply: &CommandReply,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_worker_identity(identity)?;
        if reply.command_id != command.command.command_id || reply.completed_at_ms != now_ms {
            return Err(HarnessError::invalid(
                "cloud Session command reply does not match the claimed command or completion time",
            ));
        }
        let mut transaction = self.begin().await?;
        require_worker_in(&mut transaction, identity, now_ms).await?;
        let row = required_command_in(&mut transaction, command).await?;
        if completed_reply_matches(&row, reply)? {
            return Ok(());
        }
        if !command_owned(&row, identity, now_ms)?
            || !target_is_current(&mut transaction, &row, identity, now_ms).await?
        {
            return Err(HarnessError::policy(
                "cloud Session command is no longer owned by this Worker generation",
            ));
        }
        finish_command_in(&mut transaction, &row, "completed", Some(reply), now_ms).await?;
        transaction.commit().await.map_err(store::database_error)
    }

    pub async fn inspection_session_for_worker(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        now_ms: u64,
    ) -> Result<CloudSessionRecord, HarnessError> {
        let mut transaction = self.begin().await?;
        require_inspection_in(&mut transaction, identity, command, now_ms).await?;
        let row = sqlx::query("SELECT * FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3")
            .bind(command.tenant_id.as_str()).bind(command.user_id.as_str()).bind(command.session_id.as_str())
            .fetch_one(&mut *transaction).await.map_err(store::database_error)?;
        let session = store::decode_session(&row)?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(session)
    }

    pub async fn extensions_for_inspection_command(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        profile: &ternilo_protocol::Profile,
        policy: &ternilo_extension::ExtensionHostPolicy,
        now_ms: u64,
    ) -> Result<Vec<ternilo_extension::ExtensionDistribution>, HarnessError> {
        validate_worker_identity(identity)?;
        let references = ternilo_extension::extension_mounts(profile)?
            .into_iter()
            .map(|mount| (mount.package_id, mount.version))
            .collect::<BTreeSet<_>>();
        let mut transaction = self.begin().await?;
        require_inspection_in(&mut transaction, identity, command, now_ms).await?;
        let mut distributions = Vec::with_capacity(references.len());
        for (package_id, version) in references {
            let row = sqlx::query(
                "SELECT publisher.trust, plugin.install_request FROM control_extension_packages AS plugin
                 JOIN control_extension_publishers AS publisher ON publisher.tenant_id = plugin.tenant_id AND publisher.key_id = plugin.publisher_key_id
                 WHERE plugin.tenant_id = $1 AND plugin.package_id = $2 AND plugin.version = $3
                   AND plugin.enabled = 1 AND plugin.revoked = 0 AND publisher.revoked = 0")
            .bind(command.tenant_id.as_str()).bind(&package_id).bind(&version)
            .fetch_optional(&mut *transaction).await.map_err(store::database_error)?
            .ok_or_else(|| {
                HarnessError::policy(format!(
                    "cloud extension package {package_id}@{version} is unavailable or revoked",
                ))
            })?;
            let publisher = row
                .try_get::<Json<ternilo_extension::PublisherTrust>, _>("trust")
                .map_err(store::database_error)?
                .0;
            let install = row
                .try_get::<Json<ternilo_extension::ExtensionInstallRequest>, _>("install_request")
                .map_err(store::database_error)?
                .0;
            if install.bundle.manifest.package_id != package_id
                || install.bundle.manifest.version != version
            {
                return Err(HarnessError::policy(
                    "cloud extension inspection distribution does not match the requested package",
                ));
            }
            policy.validate_install(&install.bundle.manifest, &install.granted_capabilities)?;
            ternilo_extension::verify_bundle(
                &install.bundle,
                &publisher,
                policy.max_payload_bytes,
            )?;
            distributions.push(ternilo_extension::ExtensionDistribution { publisher, install });
        }
        transaction.commit().await.map_err(store::database_error)?;
        Ok(distributions)
    }

    pub async fn defer_session_command(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.begin().await?;
        require_worker_in(&mut transaction, identity, now_ms).await?;
        let row = required_command_in(&mut transaction, command).await?;
        let expiry: i64 = row
            .try_get("expires_at_ms")
            .map_err(store::database_error)?;
        if !command_owned(&row, identity, now_ms)?
            || expiry <= store::to_i64(now_ms, "command defer time")?
            || row
                .try_get::<Option<String>, _>("target_run_id")
                .map_err(store::database_error)?
                .is_none()
            || !target_is_current(&mut transaction, &row, identity, now_ms).await?
        {
            return Err(HarnessError::policy(
                "cloud Session command can no longer be deferred by this Worker generation",
            ));
        }
        finish_command_in(&mut transaction, &row, "pending", None, now_ms).await?;
        transaction.commit().await.map_err(store::database_error)
    }

    pub async fn release_session_commands(
        &self,
        identity: &CloudWorkerIdentity,
        now_ms: u64,
    ) -> Result<u32, HarnessError> {
        let mut transaction = self.begin().await?;
        let changed = release_commands_in(&mut transaction, identity, now_ms).await?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(changed)
    }

    pub async fn reap_session_commands(&self, now_ms: u64) -> Result<u32, HarnessError> {
        let mut transaction = self.begin().await?;
        let candidates = command_scopes_in(&mut transaction, "reap", "", 0, now_ms).await?;
        let mut changed = 0;
        let now = store::to_i64(now_ms, "command reaper time")?;
        for (tenant, user, command) in candidates {
            let Some(row) = command_in(&mut transaction, &tenant, &user, &command).await? else {
                continue;
            };
            let state: String = row.try_get("state").map_err(store::database_error)?;
            let expiry: i64 = row
                .try_get("expires_at_ms")
                .map_err(store::database_error)?;
            let dispatch: Option<i64> = row
                .try_get("dispatch_lease_until_ms")
                .map_err(store::database_error)?;
            if !((state == "pending" && expiry <= now)
                || (state == "inflight"
                    && (expiry <= now || dispatch.is_some_and(|value| value <= now))))
            {
                continue;
            }
            let read_only = row
                .try_get::<i64, _>("read_only")
                .map_err(store::database_error)?
                != 0;
            let next = if expiry <= now {
                "expired"
            } else if read_only {
                "pending"
            } else {
                "indeterminate"
            };
            let reply = (next != "pending").then(|| {
                CommandReply::failure(
                    command.clone(),
                    now_ms,
                    HarnessError::execution(if next == "expired" {
                        "cloud Session command expired before completion"
                    } else {
                        "cloud Session mutation command lost its dispatch owner"
                    }),
                )
            });
            finish_command_in(&mut transaction, &row, next, reply.as_ref(), now_ms).await?;
            if next != "pending" {
                clear_steering_command_in(&mut transaction, &tenant, &command, now_ms).await?;
            }
            changed += 1;
        }
        transaction.commit().await.map_err(store::database_error)?;
        Ok(changed)
    }

    pub async fn drain_cloud_worker(
        &self,
        identity: &CloudWorkerIdentity,
        now_ms: u64,
    ) -> Result<u32, HarnessError> {
        let mut transaction = self.begin().await?;
        let count = drain_worker_in(&mut transaction, identity, now_ms).await?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(count)
    }
}

pub(crate) async fn worker_in(
    transaction: &mut Transaction,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
    require_live: bool,
) -> Result<Option<CloudWorkerRecord>, HarnessError> {
    validate_worker_identity(identity)?;
    let query = for_update(
        transaction,
        "SELECT * FROM cloud_workers WHERE worker_id = $1",
        "SELECT * FROM cloud_workers WHERE worker_id = $1 FOR UPDATE",
    );
    let row = sqlx::query(query)
        .bind(identity.worker_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store::database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let worker = decode_worker(&row)?;
    Ok(
        (worker.identity == *identity && (!require_live || worker.lease_expires_at_ms > now_ms))
            .then_some(worker),
    )
}

pub(crate) async fn require_worker_in(
    transaction: &mut Transaction,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<(), HarnessError> {
    worker_in(transaction, identity, now_ms, true)
        .await?
        .ok_or_else(|| HarnessError::policy("cloud Worker identity lease was lost or fenced"))?;
    Ok(())
}

pub(crate) async fn command_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    user: &UserId,
    command: &CommandId,
) -> Result<Option<AnyRow>, HarnessError> {
    set_owner_scope(transaction, tenant, user).await?;
    let query = for_update(
        transaction,
        "SELECT * FROM cloud_session_commands WHERE tenant_id = $1 AND user_id = $2 AND command_id = $3",
        "SELECT * FROM cloud_session_commands WHERE tenant_id = $1 AND user_id = $2 AND command_id = $3 FOR UPDATE",
    );
    sqlx::query(query)
        .bind(tenant.as_str())
        .bind(user.as_str())
        .bind(command.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store::database_error)
}

pub(crate) async fn required_command_in(
    transaction: &mut Transaction,
    command: &ClaimedCloudSessionCommand,
) -> Result<AnyRow, HarnessError> {
    let row = command_in(
        transaction,
        &command.tenant_id,
        &command.user_id,
        &command.command.command_id,
    )
    .await?
    .ok_or_else(|| HarnessError::policy("cloud Session command does not belong to this owner"))?;
    let stored = decode_command_record(&row)?;
    if stored.session_id != command.session_id
        || stored.command != command.command
        || stored.required_capability != command.required_capability
        || stored.delivery != command.delivery
    {
        return Err(HarnessError::policy(
            "claimed command does not match its stored scope and body",
        ));
    }
    Ok(row)
}

pub(crate) fn command_owned(
    row: &AnyRow,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<bool, HarnessError> {
    let now = store::to_i64(now_ms, "command ownership time")?;
    Ok(row
        .try_get::<String, _>("state")
        .map_err(store::database_error)?
        == "inflight"
        && row
            .try_get::<Option<String>, _>("lease_owner")
            .map_err(store::database_error)?
            .as_deref()
            == Some(identity.worker_id.as_str())
        && row
            .try_get::<Option<i64>, _>("worker_generation")
            .map_err(store::database_error)?
            == Some(store::to_i64(identity.generation, "worker generation")?)
        && row
            .try_get::<Option<i64>, _>("dispatch_lease_until_ms")
            .map_err(store::database_error)?
            .is_some_and(|expiry| expiry > now))
}

pub(crate) fn completed_reply_matches(
    row: &AnyRow,
    reply: &CommandReply,
) -> Result<bool, HarnessError> {
    let stored: Option<Json<CommandReply>> =
        row.try_get("reply_json").map_err(store::database_error)?;
    Ok(row
        .try_get::<String, _>("state")
        .map_err(store::database_error)?
        == "completed"
        && stored.is_some_and(|stored| stored.0 == *reply))
}

pub(crate) async fn target_is_current(
    transaction: &mut Transaction,
    row: &AnyRow,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<bool, HarnessError> {
    let target: Option<String> = row
        .try_get("target_run_id")
        .map_err(store::database_error)?;
    let Some(target) = target else {
        return Ok(true);
    };
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_runs AS run JOIN cloud_session_writer_leases AS writer
         ON writer.tenant_id = run.tenant_id AND writer.session_id = run.session_id
         WHERE run.tenant_id = $1 AND run.session_id = $2 AND run.run_id = $3
           AND run.state IN ('running', 'cancel_requested') AND run.lease_owner = $4
           AND run.lease_expires_at_ms > $6 AND run.session_fencing_token = $5
           AND writer.run_id = run.run_id AND writer.lease_owner = $4
           AND writer.fencing_token = $5 AND writer.expires_at_ms > $6",
    )
    .bind(
        row.try_get::<String, _>("tenant_id")
            .map_err(store::database_error)?,
    )
    .bind(
        row.try_get::<String, _>("session_id")
            .map_err(store::database_error)?,
    )
    .bind(target)
    .bind(identity.worker_id.as_str())
    .bind(
        row.try_get::<Option<i64>, _>("target_writer_fencing_token")
            .map_err(store::database_error)?,
    )
    .bind(store::to_i64(now_ms, "target lease time")?)
    .fetch_one(&mut **transaction)
    .await
    .map_err(store::database_error)?;
    Ok(count == 1)
}

async fn claim_target_available(
    transaction: &mut Transaction,
    row: &AnyRow,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<bool, HarnessError> {
    if row
        .try_get::<Option<String>, _>("target_run_id")
        .map_err(store::database_error)?
        .is_some()
    {
        return target_is_current(transaction, row, identity, now_ms).await;
    }
    let owners: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT lease_owner FROM cloud_runs WHERE tenant_id = $1 AND session_id = $2
         AND state IN ('running', 'cancel_requested') AND lease_expires_at_ms > $3",
    )
    .bind(
        row.try_get::<String, _>("tenant_id")
            .map_err(store::database_error)?,
    )
    .bind(
        row.try_get::<String, _>("session_id")
            .map_err(store::database_error)?,
    )
    .bind(store::to_i64(now_ms, "run lease time")?)
    .fetch_all(&mut **transaction)
    .await
    .map_err(store::database_error)?;
    Ok(owners.is_empty()
        || owners
            .iter()
            .any(|owner| owner.as_deref() == Some(identity.worker_id.as_str())))
}

pub(crate) async fn finish_command_in(
    transaction: &mut Transaction,
    row: &AnyRow,
    state: &str,
    reply: Option<&CommandReply>,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let now = store::to_i64(now_ms, "command completion time")?;
    sqlx::query(
        "UPDATE cloud_session_commands SET state = $3,
        lease_owner = CASE WHEN $3='completed' THEN lease_owner ELSE NULL END,
        worker_generation = CASE WHEN $3='completed' THEN worker_generation ELSE NULL END,
        dispatch_lease_until_ms = NULL, reply_json = $4, completed_at_ms = $5, updated_at_ms = $6
        WHERE tenant_id = $1 AND command_id = $2",
    )
    .bind(
        row.try_get::<String, _>("tenant_id")
            .map_err(store::database_error)?,
    )
    .bind(
        row.try_get::<String, _>("command_id")
            .map_err(store::database_error)?,
    )
    .bind(state)
    .bind(reply.map(Json))
    .bind(reply.map(|_| now))
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(store::database_error)?;
    Ok(())
}

pub(crate) async fn clear_steering_command_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    command: &CommandId,
    now_ms: u64,
) -> Result<(), HarnessError> {
    sqlx::query("UPDATE cloud_session_submissions SET steering_command_id = NULL, steering_target_run_id = NULL,
        steering_target_writer_fencing_token = NULL, placement = 'queued', updated_at_ms = CASE WHEN updated_at_ms >= $3 THEN updated_at_ms + 1 ELSE $3 END
        WHERE tenant_id = $1 AND steering_command_id = $2")
        .bind(tenant.as_str()).bind(command.as_str()).bind(store::to_i64(now_ms, "steering recovery time")?)
        .execute(&mut **transaction).await.map_err(store::database_error)?;
    Ok(())
}

async fn require_inspection_in(
    transaction: &mut Transaction,
    identity: &CloudWorkerIdentity,
    command: &ClaimedCloudSessionCommand,
    now_ms: u64,
) -> Result<(), HarnessError> {
    require_worker_in(transaction, identity, now_ms).await?;
    let row = required_command_in(transaction, command).await?;
    if row
        .try_get::<i64, _>("read_only")
        .map_err(store::database_error)?
        == 0
        || !command_owned(&row, identity, now_ms)?
    {
        return Err(HarnessError::policy(
            "cloud inspection command lost its Worker lease or Session binding",
        ));
    }
    Ok(())
}

const COMMAND_SCOPES: &str = "SELECT tenant_id, user_id, command_id FROM cloud_session_commands
 WHERE ($1 = 'claim' AND expires_at_ms > $4 AND (state = 'pending' OR (state = 'inflight' AND read_only = 1 AND dispatch_lease_until_ms <= $4)))
    OR ($1 = 'release' AND state = 'inflight' AND lease_owner = $2 AND worker_generation = $3)
    OR ($1 = 'reap' AND ((state = 'pending' AND expires_at_ms <= $4) OR (state = 'inflight' AND (expires_at_ms <= $4 OR dispatch_lease_until_ms <= $4))))
 ORDER BY issued_at_ms, command_seq, tenant_id, command_id";

async fn command_scopes_in(
    transaction: &mut Transaction,
    operation: &str,
    worker: &str,
    generation: u64,
    now_ms: u64,
) -> Result<Vec<(TenantId, UserId, CommandId)>, HarnessError> {
    let query = if backend(transaction) == Backend::Postgres {
        "SELECT tenant_id, user_id, command_id FROM ternilo_cloud_command_scopes($1, $2, $3, $4)"
    } else {
        COMMAND_SCOPES
    };
    let rows = sqlx::query(query)
        .bind(operation)
        .bind(worker)
        .bind(store::to_i64(generation, "worker generation")?)
        .bind(store::to_i64(now_ms, "command discovery time")?)
        .fetch_all(&mut **transaction)
        .await
        .map_err(store::database_error)?;
    rows.iter()
        .map(|row| {
            Ok((
                TenantId::new(
                    row.try_get::<String, _>("tenant_id")
                        .map_err(store::database_error)?,
                ),
                UserId::new(
                    row.try_get::<String, _>("user_id")
                        .map_err(store::database_error)?,
                ),
                CommandId::new(
                    row.try_get::<String, _>("command_id")
                        .map_err(store::database_error)?,
                ),
            ))
        })
        .collect()
}

async fn release_commands_in(
    transaction: &mut Transaction,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<u32, HarnessError> {
    if worker_in(transaction, identity, now_ms, false)
        .await?
        .is_none()
    {
        return Ok(0);
    }
    let candidates = command_scopes_in(
        transaction,
        "release",
        identity.worker_id.as_str(),
        identity.generation,
        now_ms,
    )
    .await?;
    let mut changed = 0;
    for (tenant, user, command) in candidates {
        let Some(row) = command_in(transaction, &tenant, &user, &command).await? else {
            continue;
        };
        if row
            .try_get::<String, _>("state")
            .map_err(store::database_error)?
            != "inflight"
            || row
                .try_get::<Option<String>, _>("lease_owner")
                .map_err(store::database_error)?
                .as_deref()
                != Some(identity.worker_id.as_str())
            || row
                .try_get::<Option<i64>, _>("worker_generation")
                .map_err(store::database_error)?
                != Some(store::to_i64(identity.generation, "worker generation")?)
        {
            continue;
        }
        let read_only = row
            .try_get::<i64, _>("read_only")
            .map_err(store::database_error)?
            != 0;
        let reply = (!read_only).then(|| {
            CommandReply::failure(
                command.clone(),
                now_ms,
                HarnessError::execution(
                    "cloud Worker released a mutation command with an unknown outcome",
                ),
            )
        });
        finish_command_in(
            transaction,
            &row,
            if read_only {
                "pending"
            } else {
                "indeterminate"
            },
            reply.as_ref(),
            now_ms,
        )
        .await?;
        if !read_only {
            clear_steering_command_in(transaction, &tenant, &command, now_ms).await?;
        }
        changed += 1;
    }
    Ok(changed)
}

pub(crate) async fn set_owner_scope(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
) -> Result<(), HarnessError> {
    ternilo_storage::set_tenant_scope(transaction, tenant_id).await?;
    ternilo_storage::set_user_scope(transaction, user_id).await?;
    Ok(())
}

fn validate_worker_identity(identity: &CloudWorkerIdentity) -> Result<(), HarnessError> {
    identity.worker_id.validate()?;
    if identity.instance_nonce.trim().is_empty() || identity.generation == 0 {
        return Err(HarnessError::invalid(
            "cloud Worker identity requires an instance nonce and positive generation",
        ));
    }
    Ok(())
}

fn lease_expiry(now_ms: u64, ttl: Duration, label: &str) -> Result<u64, HarnessError> {
    if ttl.is_zero() || ttl > Duration::from_mins(5) {
        return Err(HarnessError::invalid(format!(
            "{label} TTL must be positive and at most 5 minutes"
        )));
    }
    let ttl_ms = u64::try_from(ttl.as_millis())
        .map_err(|_| HarnessError::invalid(format!("{label} TTL exceeds u64 milliseconds")))?;
    now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| HarnessError::invalid(format!("{label} expiry exceeds u64")))
}

pub(crate) fn capability_name(capability: ExecutorCapability) -> Result<String, HarnessError> {
    let value = serde_json::to_value(capability)
        .map_err(|error| HarnessError::execution(format!("encode capability: {error}")))?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| HarnessError::execution("executor capability did not encode as a string"))
}

fn capability_parse(value: &str) -> Result<ExecutorCapability, HarnessError> {
    serde_json::from_value(Value::String(value.to_owned())).map_err(|error| {
        HarnessError::execution(format!(
            "database contains unknown executor capability {value:?}: {error}"
        ))
    })
}

pub(crate) fn canonical_command(command: &ExecutorCommand) -> Result<Value, HarnessError> {
    let mut value = serde_json::to_value(command)
        .map_err(|error| HarnessError::execution(format!("encode command: {error}")))?;
    canonicalize_json(&mut value);
    Ok(value)
}

fn canonicalize_json(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(canonicalize_json),
        Value::Object(values) => {
            let mut entries = std::mem::take(values).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (_, value) in &mut entries {
                canonicalize_json(value);
            }
            values.extend(entries);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn delivery_columns(
    delivery: &CloudCommandDelivery,
) -> Result<(bool, Option<String>, Option<i64>), HarnessError> {
    match delivery {
        CloudCommandDelivery::ReadOnly => Ok((true, None, None)),
        CloudCommandDelivery::TargetRun {
            run_id,
            writer_fencing_token,
        } => {
            run_id.validate()?;
            Ok((
                false,
                Some(run_id.as_str().to_owned()),
                Some(store::to_i64(
                    *writer_fencing_token,
                    "target writer fencing token",
                )?),
            ))
        }
    }
}

fn delivery_from_row(row: &AnyRow) -> Result<CloudCommandDelivery, HarnessError> {
    let read_only = row
        .try_get::<i64, _>("read_only")
        .map_err(store::database_error)?;
    let target_run_id = row
        .try_get::<Option<String>, _>("target_run_id")
        .map_err(store::database_error)?;
    let target_writer_fencing_token = row
        .try_get::<Option<i64>, _>("target_writer_fencing_token")
        .map_err(store::database_error)?;
    match (read_only != 0, target_run_id, target_writer_fencing_token) {
        (true, None, None) => Ok(CloudCommandDelivery::ReadOnly),
        (false, Some(run_id), Some(fencing_token)) => Ok(CloudCommandDelivery::TargetRun {
            run_id: RunId::new(run_id),
            writer_fencing_token: store::from_i64(fencing_token, "target writer fencing token")?,
        }),
        _ => Err(HarnessError::execution(
            "database contains inconsistent cloud Session command delivery metadata",
        )),
    }
}

fn decode_worker(row: &AnyRow) -> Result<CloudWorkerRecord, HarnessError> {
    let hello = row
        .try_get::<Json<ExecutorHello>, _>("hello_json")
        .map_err(store::database_error)?
        .0;
    hello.validate()?;
    let worker_id = ExecutorId::new(
        row.try_get::<String, _>("worker_id")
            .map_err(store::database_error)?,
    );
    let instance_nonce = row
        .try_get::<String, _>("instance_nonce")
        .map_err(store::database_error)?;
    if hello.executor_id != worker_id || hello.instance_nonce != instance_nonce {
        return Err(HarnessError::execution(
            "cloud Worker registry row does not match its hello",
        ));
    }
    Ok(CloudWorkerRecord {
        capacity: crate::WorkerCapacity {
            max_active_runs: u32::try_from(
                row.try_get::<i64, _>("max_active_runs")
                    .map_err(store::database_error)?,
            )
            .map_err(|_| HarnessError::execution("invalid Worker active capacity"))?,
            max_resident_runs: u32::try_from(
                row.try_get::<i64, _>("max_resident_runs")
                    .map_err(store::database_error)?,
            )
            .map_err(|_| HarnessError::execution("invalid Worker resident capacity"))?,
        },
        identity: CloudWorkerIdentity {
            worker_id,
            instance_nonce,
            generation: store::from_i64(
                row.try_get("generation").map_err(store::database_error)?,
                "cloud Worker generation",
            )?,
        },
        hello,
        registered_at_ms: store::from_i64(
            row.try_get("registered_at_ms")
                .map_err(store::database_error)?,
            "cloud Worker registration timestamp",
        )?,
        last_seen_at_ms: store::from_i64(
            row.try_get("last_seen_at_ms")
                .map_err(store::database_error)?,
            "cloud Worker heartbeat timestamp",
        )?,
        lease_expires_at_ms: store::from_i64(
            row.try_get("lease_expires_at_ms")
                .map_err(store::database_error)?,
            "cloud Worker lease expiry",
        )?,
    })
}

pub(crate) fn decode_command_record(
    row: &AnyRow,
) -> Result<CloudSessionCommandRecord, HarnessError> {
    let command = row
        .try_get::<Json<ExecutorCommand>, _>("command_json")
        .map_err(store::database_error)?
        .0;
    let tenant_id = TenantId::new(
        row.try_get::<String, _>("tenant_id")
            .map_err(store::database_error)?,
    );
    let user_id = UserId::new(
        row.try_get::<String, _>("user_id")
            .map_err(store::database_error)?,
    );
    if command.scope.tenant_id != tenant_id || command.scope.user_id != user_id {
        return Err(HarnessError::execution(
            "cloud Session command row does not match its encoded scope",
        ));
    }
    Ok(CloudSessionCommandRecord {
        contributor_user_id: row
            .try_get::<Option<String>, _>("contributor_user_id")
            .map_err(store::database_error)?
            .map(UserId::new),
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(store::database_error)?,
        ),
        tenant_id,
        user_id,
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(store::database_error)?,
        ),
        command_seq: store::from_i64(
            row.try_get("command_seq").map_err(store::database_error)?,
            "cloud Session command sequence",
        )?,
        command,
        required_capability: capability_parse(
            &row.try_get::<String, _>("required_capability")
                .map_err(store::database_error)?,
        )?,
        required_catalog_revision: row
            .try_get("required_catalog_revision")
            .map_err(store::database_error)?,
        delivery: delivery_from_row(row)?,
        state: CloudSessionCommandState::parse(
            &row.try_get::<String, _>("state")
                .map_err(store::database_error)?,
        )?,
        attempt_count: u32::try_from(
            row.try_get::<i32, _>("attempt_count")
                .map_err(store::database_error)?,
        )
        .map_err(|_| HarnessError::execution("command attempt count is negative"))?,
        reply: row
            .try_get::<Option<Json<CommandReply>>, _>("reply_json")
            .map_err(store::database_error)?
            .map(|reply| reply.0),
        created_at_ms: store::from_i64(
            row.try_get("created_at_ms")
                .map_err(store::database_error)?,
            "cloud Session command creation timestamp",
        )?,
        updated_at_ms: store::from_i64(
            row.try_get("updated_at_ms")
                .map_err(store::database_error)?,
            "cloud Session command update timestamp",
        )?,
    })
}

pub(crate) fn decode_claimed_command(
    row: &AnyRow,
) -> Result<ClaimedCloudSessionCommand, HarnessError> {
    let command = row
        .try_get::<Json<ExecutorCommand>, _>("command_json")
        .map_err(store::database_error)?
        .0;
    let tenant_id = TenantId::new(
        row.try_get::<String, _>("tenant_id")
            .map_err(store::database_error)?,
    );
    let user_id = UserId::new(
        row.try_get::<String, _>("user_id")
            .map_err(store::database_error)?,
    );
    if command.scope.tenant_id != tenant_id || command.scope.user_id != user_id {
        return Err(HarnessError::execution(
            "claimed cloud Session command does not match its scope",
        ));
    }
    Ok(ClaimedCloudSessionCommand {
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(store::database_error)?,
        ),
        tenant_id,
        user_id,
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(store::database_error)?,
        ),
        command_seq: store::from_i64(
            row.try_get("command_seq").map_err(store::database_error)?,
            "cloud Session command sequence",
        )?,
        command,
        required_capability: capability_parse(
            &row.try_get::<String, _>("required_capability")
                .map_err(store::database_error)?,
        )?,
        delivery: delivery_from_row(row)?,
        attempt_count: u32::try_from(
            row.try_get::<i32, _>("attempt_count")
                .map_err(store::database_error)?,
        )
        .map_err(|_| HarnessError::execution("command attempt count is negative"))?,
    })
}

pub(crate) async fn register_worker_in(
    transaction: &mut Transaction,
    hello: &ExecutorHello,
    lease_ttl: Duration,
    now_ms: u64,
) -> Result<CloudWorkerIdentity, HarnessError> {
    hello.validate()?;
    if hello.executor_kind != ExecutorKind::CloudWorker {
        return Err(HarnessError::invalid(
            "only a cloud Worker can register in the cloud Worker registry",
        ));
    }
    let expiry = lease_expiry(now_ms, lease_ttl, "cloud Worker lease")?;
    let generation: i64 = sqlx::query_scalar(
            "INSERT INTO cloud_workers (worker_id, instance_nonce, generation, hello_json, registered_at_ms, last_seen_at_ms, lease_expires_at_ms)
             VALUES ($1, $2, 1, $3, $4, $4, $5)
             ON CONFLICT (worker_id) DO UPDATE SET instance_nonce = excluded.instance_nonce,
                generation = CASE WHEN cloud_workers.instance_nonce = excluded.instance_nonce THEN cloud_workers.generation ELSE cloud_workers.generation + 1 END,
                hello_json = excluded.hello_json,
                registered_at_ms = CASE WHEN cloud_workers.instance_nonce = excluded.instance_nonce THEN cloud_workers.registered_at_ms ELSE excluded.registered_at_ms END,
                last_seen_at_ms = excluded.last_seen_at_ms, lease_expires_at_ms = excluded.lease_expires_at_ms
             RETURNING generation")
            .bind(hello.executor_id.as_str()).bind(&hello.instance_nonce).bind(Json(hello))
            .bind(store::to_i64(now_ms, "worker registration time")?).bind(store::to_i64(expiry, "worker expiry")?)
            .fetch_one(&mut **transaction).await.map_err(store::database_error)?;
    Ok(CloudWorkerIdentity {
        worker_id: hello.executor_id.clone(),
        instance_nonce: hello.instance_nonce.clone(),
        generation: store::from_i64(generation, "worker generation")?,
    })
}

pub(crate) async fn drain_worker_in(
    transaction: &mut Transaction,
    identity: &CloudWorkerIdentity,
    now_ms: u64,
) -> Result<u32, HarnessError> {
    if worker_in(transaction, identity, now_ms, false)
        .await?
        .is_none()
    {
        return Ok(0);
    }
    let released = release_commands_in(transaction, identity, now_ms).await?;
    let telemetry = crate::telemetry::release_telemetry_in(transaction, identity, now_ms).await?;
    let reaped =
        store::drain_worker_runs_in(transaction, identity.worker_id.as_str(), now_ms).await?;
    released
        .checked_add(telemetry)
        .and_then(|count| count.checked_add(reaped))
        .ok_or_else(|| HarnessError::execution("worker drain count overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use ternilo_transport::ExecutorScope;

    #[test]
    fn draft_accepts_only_the_cloud_session_subset() {
        let tenant_id = TenantId::new("tenant");
        let user_id = UserId::new("user");
        let session_id = SessionId::new("session");
        let draft = CloudSessionCommandDraft {
            session_id: session_id.clone(),
            command: ExecutorCommand {
                input_provenance: None,
                command_id: CommandId::new("command"),
                scope: ExecutorScope {
                    tenant_id: tenant_id.clone(),
                    user_id: user_id.clone(),
                },
                issued_at_ms: 10,
                expires_at_ms: 20,
                body: ExecutorCommandBody::Application {
                    request: ApplicationOperation::SessionSkills { session_id },
                },
            },
            required_capability: ExecutorCapability::Skills,
            required_catalog_revision: Some("catalog".to_owned()),
            delivery: CloudCommandDelivery::ReadOnly,
        };
        draft.validate(&tenant_id, &user_id, 10).unwrap();

        let mut commands = draft.clone();
        commands.command.body = ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionCommands {
                session_id: commands.session_id.clone(),
            },
        };
        commands.required_capability = ExecutorCapability::AddressedSessionCommands;
        commands.validate(&tenant_id, &user_id, 10).unwrap();

        let mut resolve = draft;
        resolve.command.body = ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionSkillResolve {
                session_id: resolve.session_id.clone(),
                name: "release-check".to_owned(),
                input: "inspect".to_owned(),
            },
        };
        resolve.validate(&tenant_id, &user_id, 10).unwrap();
    }

    #[test]
    fn runtime_service_mutations_require_a_target_writer_fence_and_are_not_replayable_reads() {
        let tenant = TenantId::new("tenant");
        let owner = UserId::new("owner");
        let session = SessionId::new("session");
        for request in [
            ApplicationOperation::SessionServiceStart {
                session_id: session.clone(),
                service_id: "mcp:tools".to_owned(),
            },
            ApplicationOperation::SessionServiceStop {
                session_id: session.clone(),
                service_id: "mcp:tools".to_owned(),
            },
        ] {
            let mut draft = CloudSessionCommandDraft {
                session_id: session.clone(),
                command: ExecutorCommand {
                    input_provenance: None,
                    command_id: CommandId::new("service-command"),
                    scope: ExecutorScope {
                        tenant_id: tenant.clone(),
                        user_id: owner.clone(),
                    },
                    issued_at_ms: 10,
                    expires_at_ms: 20,
                    body: ExecutorCommandBody::Application { request },
                },
                required_capability: ExecutorCapability::AddressedSessionCommands,
                required_catalog_revision: None,
                delivery: CloudCommandDelivery::ReadOnly,
            };
            assert!(draft.validate(&tenant, &owner, 10).is_err());
            draft.delivery = CloudCommandDelivery::TargetRun {
                run_id: RunId::new("active-run"),
                writer_fencing_token: 0,
            };
            assert!(draft.validate(&tenant, &owner, 10).is_err());
            draft.delivery = CloudCommandDelivery::TargetRun {
                run_id: RunId::new("active-run"),
                writer_fencing_token: 7,
            };
            draft.validate(&tenant, &owner, 10).unwrap();
            draft.session_id = SessionId::new("another-session");
            assert!(draft.validate(&tenant, &owner, 10).is_err());
        }
    }

    #[test]
    fn capability_names_match_the_postgres_wire() {
        assert_eq!(
            capability_name(ExecutorCapability::SessionSteering).unwrap(),
            "session_steering"
        );
        assert_eq!(
            capability_parse("session_steering").unwrap(),
            ExecutorCapability::SessionSteering
        );
        let capabilities = BTreeSet::from([ExecutorCapability::SessionSteering]);
        assert_eq!(capabilities.len(), 1);
    }
}
