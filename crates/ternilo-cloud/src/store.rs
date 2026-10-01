use std::{collections::BTreeSet, fmt::Write as _, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use sha2::{Digest, Sha256};
use sqlx::{AnyPool, Row};
use ternilo_protocol::{
    Attachment, FeedbackRating, HarnessError, PermissionPreset, ProviderProfile, RunId, RunOutcome,
    RunSpec, SessionCommandOutcome, SessionCommandOutcomeKind, SessionCommandReceipt, SessionEvent,
    SessionEventKind, SessionId, SessionMode, TenantId, UserAnswer, UserId, UserQuestion,
    WorkspaceId,
};
use ternilo_storage::Database;
use ternilo_transport::{
    CommandId, ExecutorCapability, ExecutorCommand, ExecutorCommandBody, ExecutorScope,
};

use crate::{
    CloudPendingQuestion, CloudRunClaim, CloudRunRecord, CloudRunState, StartedRun, TerminalState,
};

mod execution;
mod recovery;
mod sessions;
pub(crate) use execution::{
    drain_worker_runs_in, fail_queued_capacity_in, lock_session_in, require_writer_in,
};
pub(crate) use sessions::decode_session;

#[derive(Clone)]
pub struct CloudStore {
    pub(crate) database: Database,
    pub(crate) pool: AnyPool,
    pub(crate) worker_access: Option<crate::worker_access::WorkerAccess>,
}

#[derive(Clone, Debug)]
pub struct UserProviderRoute {
    pub provider: ProviderProfile,
    pub credential: Option<EncryptedUserProviderCredential>,
}

#[derive(Clone, Debug)]
pub struct EncryptedUserProviderCredential {
    pub name: String,
    pub version: u64,
    pub nonce: [u8; 24],
    pub ciphertext: Vec<u8>,
}

impl CloudStore {
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }

    pub async fn connect(
        database_url: &str,
        migration_database_url: Option<&str>,
        max_connections: u32,
    ) -> Result<Self, HarnessError> {
        if let Some(url) = migration_database_url {
            let owner = Database::connect(url, 1).await?;
            initialize_database(&owner).await?;
            owner.close().await;
            return Self::connect_without_migrations(database_url, max_connections).await;
        }
        let database = Database::connect(database_url, max_connections).await?;
        Self::from_database(database).await
    }

    pub async fn from_database(database: Database) -> Result<Self, HarnessError> {
        initialize_database(&database).await?;
        Ok(Self {
            pool: database.pool().clone(),
            worker_access: None,
            database,
        })
    }

    pub async fn connect_without_migrations(
        database_url: &str,
        max_connections: u32,
    ) -> Result<Self, HarnessError> {
        let database = Database::connect(database_url, max_connections).await?;
        Ok(Self {
            pool: database.pool().clone(),
            worker_access: None,
            database,
        })
    }

    pub async fn health(&self) -> Result<(), HarnessError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(database_error)?;
        Ok(())
    }

    pub async fn user_provider_route_for_worker(
        &self,
        tenant_id: &TenantId,
        user_id: &UserId,
        provider_id: &str,
    ) -> Result<Option<UserProviderRoute>, HarnessError> {
        tenant_id.validate()?;
        user_id.validate()?;
        let mut transaction = self.owner_transaction(tenant_id, user_id).await?;
        let query = if self.database.backend() == ternilo_storage::Backend::Postgres {
            "SELECT provider.provider_json, (provider.provider_json::jsonb) ->> 'api_key_ref' AS credential_name, credential.version AS credential_version, credential.nonce AS credential_nonce, credential.ciphertext AS credential_ciphertext FROM control_user_provider_profiles AS provider LEFT JOIN control_user_credentials AS credential ON credential.tenant_id=provider.tenant_id AND credential.user_id=provider.user_id AND credential.name=(provider.provider_json::jsonb) ->> 'api_key_ref' WHERE provider.tenant_id=$1 AND provider.user_id=$2 AND provider.provider_id=$3"
        } else {
            "SELECT provider.provider_json, provider.provider_json ->> 'api_key_ref' AS credential_name, credential.version AS credential_version, credential.nonce AS credential_nonce, credential.ciphertext AS credential_ciphertext FROM control_user_provider_profiles AS provider LEFT JOIN control_user_credentials AS credential ON credential.tenant_id=provider.tenant_id AND credential.user_id=provider.user_id AND credential.name=provider.provider_json ->> 'api_key_ref' WHERE provider.tenant_id=$1 AND provider.user_id=$2 AND provider.provider_id=$3"
        };
        let row = sqlx::query(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let provider = row
            .try_get::<ternilo_storage::Json<ProviderProfile>, _>("provider_json")
            .map_err(database_error)?
            .0;
        provider.validate()?;
        let credential_name = row
            .try_get::<Option<String>, _>("credential_name")
            .map_err(database_error)?;
        let credential_version = row
            .try_get::<Option<i64>, _>("credential_version")
            .map_err(database_error)?;
        let credential_nonce = row
            .try_get::<Option<Vec<u8>>, _>("credential_nonce")
            .map_err(database_error)?;
        let credential_ciphertext = row
            .try_get::<Option<Vec<u8>>, _>("credential_ciphertext")
            .map_err(database_error)?;
        let credential = match (
            credential_name,
            credential_version,
            credential_nonce,
            credential_ciphertext,
        ) {
            (Some(name), Some(version), Some(nonce), Some(ciphertext)) => {
                Some(EncryptedUserProviderCredential {
                    name,
                    version: u64::try_from(version).map_err(|_| {
                        HarnessError::execution("user Provider credential version is invalid")
                    })?,
                    nonce: nonce.try_into().map_err(|_| {
                        HarnessError::execution("user Provider credential nonce is invalid")
                    })?,
                    ciphertext,
                })
            }
            (_, None, None, None) => None,
            _ => {
                return Err(HarnessError::execution(
                    "user Provider credential record is incomplete",
                ));
            }
        };
        Ok(Some(UserProviderRoute {
            provider,
            credential,
        }))
    }

    pub async fn active_session_runs(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Vec<RunId>, HarnessError> {
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT run.run_id
             FROM cloud_runs AS run
             JOIN cloud_sessions AS session
               ON session.tenant_id = run.tenant_id AND session.session_id = run.session_id
             WHERE run.tenant_id = $1 AND run.session_id = $2 AND session.user_id = $3
               AND session.archived_at_ms IS NULL
               AND run.state IN ('queued', 'leased', 'running', 'cancel_requested')
             ORDER BY run.created_at_ms, run.run_id",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(rows.into_iter().map(RunId::new).collect())
    }

    #[expect(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn record_session_feedback(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        target_seq: u64,
        expected_revision: u64,
        rating: Option<FeedbackRating>,
        note: Option<String>,
        now_ms: u64,
    ) -> Result<SessionEvent, HarnessError> {
        let note = note
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if note
            .as_ref()
            .is_some_and(|value| value.chars().count() > 8_192)
        {
            return Err(HarnessError::invalid(
                "feedback note must not exceed 8192 characters",
            ));
        }
        let now = to_i64(now_ms, "cloud feedback timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let last_seq = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
            &transaction,
            "SELECT last_seq FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM cloud_runs
                   WHERE tenant_id = $1 AND session_id = $2
                     AND state IN ('queued', 'leased', 'running', 'cancel_requested')
               )",
            "SELECT last_seq FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM cloud_runs
                   WHERE tenant_id = $1 AND session_id = $2
                     AND state IN ('queued', 'leased', 'running', 'cancel_requested')
               )
             FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud feedback requires an idle, active session"))?;
        let target_row = sqlx::query(
            "SELECT event, writer_fencing_token
             FROM cloud_session_events
             WHERE tenant_id = $1 AND session_id = $2 AND seq = $3",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(to_i64(target_seq, "cloud feedback target sequence")?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid(format!("unknown event seq {target_seq}")))?;
        let target = target_row
            .try_get::<ternilo_storage::Json<SessionEvent>, _>("event")
            .map_err(database_error)?
            .0;
        if !matches!(target.kind, SessionEventKind::AssistantMessage { .. }) {
            return Err(HarnessError::invalid(
                "feedback target must be an assistant message",
            ));
        }
        let query = if self.database.backend() == ternilo_storage::Backend::Postgres {
            "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 AND (event::jsonb)->>'type'='feedback_recorded' AND CAST((event::jsonb)->>'target_seq' AS BIGINT)=$3 ORDER BY seq DESC LIMIT 1"
        } else {
            "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 AND event->>'type'='feedback_recorded' AND CAST(event->>'target_seq' AS BIGINT)=$3 ORDER BY seq DESC LIMIT 1"
        };
        let current_feedback = sqlx::query_scalar::<_, ternilo_storage::Json<SessionEvent>>(query)
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .bind(to_i64(target_seq, "cloud feedback target sequence")?)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
        let current_revision = current_feedback
            .and_then(|event| match event.0.kind {
                SessionEventKind::FeedbackRecorded { revision, .. } => Some(revision),
                _ => None,
            })
            .unwrap_or(0);
        if current_revision != expected_revision {
            return Err(HarnessError::conflict(format!(
                "feedback revision conflict: expected {expected_revision}, current {current_revision}"
            )));
        }
        let seq = from_i64(last_seq, "cloud session sequence")?.saturating_add(1);
        let event = SessionEvent {
            seq,
            occurred_at_ms: now_ms,
            run_id: target.run_id,
            kind: SessionEventKind::FeedbackRecorded {
                target_seq,
                revision: current_revision.saturating_add(1),
                rating,
                note,
            },
        };
        sqlx::query(
            "INSERT INTO cloud_session_events
                (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(to_i64(seq, "cloud feedback event sequence")?)
        .bind(event.run_id.as_str())
        .bind(ternilo_storage::Json(&event))
        .bind(
            target_row
                .try_get::<i64, _>("writer_fencing_token")
                .map_err(database_error)?,
        )
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        crate::telemetry::capture_event_in(&mut transaction, tenant_id, session_id, &event, now_ms)
            .await?;
        sqlx::query(
            "UPDATE cloud_sessions SET last_seq = $3, updated_at_ms = $4
             WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(to_i64(seq, "cloud feedback event sequence")?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(event)
    }

    /// Append `/feedback` command facts directly to an idle cloud session.
    /// This command plane is deliberately independent from worker/model runs.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn record_command_feedback(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        text: String,
        now_ms: u64,
    ) -> Result<SessionCommandReceipt, HarnessError> {
        let user_id = actor_id;
        session_id.validate()?;
        user_id.validate()?;
        let now = to_i64(now_ms, "cloud feedback command timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        let last_seq = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
            &transaction,
            "SELECT COALESCE(last_seq, -1)
             FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM cloud_runs
                   WHERE tenant_id = $1 AND session_id = $2
                     AND state IN ('queued', 'leased', 'running', 'cancel_requested')
               )",
            "SELECT COALESCE(last_seq, -1)
             FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND archived_at_ms IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM cloud_runs
                   WHERE tenant_id = $1 AND session_id = $2
                     AND state IN ('queued', 'leased', 'running', 'cancel_requested')
               )
             FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            HarnessError::invalid("cloud feedback command requires an idle, active session")
        })?;
        let fencing_token = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(writer_fencing_token), 0) + 1
             FROM cloud_session_events
             WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let command_id = format!("feedback_{}", URL_SAFE_NO_PAD.encode(random::<[u8; 16]>()));
        let run_id = RunId::new(format!("command-{command_id}"));
        run_id.validate()?;
        let normalized = text.trim();
        let mut kinds = vec![SessionEventKind::CommandStarted {
            command_id: command_id.clone(),
            command_name: "feedback".to_owned(),
        }];
        let outcome = if normalized.is_empty() {
            SessionCommandOutcome {
                kind: SessionCommandOutcomeKind::Error,
                code: "feedback_text_required".to_owned(),
                parameters: std::collections::BTreeMap::from([(
                    "usage".to_owned(),
                    "/feedback <text>".to_owned(),
                )]),
            }
        } else {
            kinds.push(SessionEventKind::FeedbackSubmitted {
                command_id: command_id.clone(),
                text: normalized.to_owned(),
            });
            SessionCommandOutcome {
                kind: SessionCommandOutcomeKind::Success,
                code: "feedback_recorded".to_owned(),
                parameters: std::collections::BTreeMap::from([(
                    "session_id".to_owned(),
                    session_id.as_str().to_owned(),
                )]),
            }
        };
        kinds.push(SessionEventKind::CommandFinished {
            command_id: command_id.clone(),
            outcome,
        });
        let mut events = Vec::with_capacity(kinds.len());
        for (offset, kind) in kinds.into_iter().enumerate() {
            let offset = i64::try_from(offset)
                .map_err(|_| HarnessError::execution("feedback command event count exceeds i64"))?;
            let seq = last_seq
                .checked_add(offset + 1)
                .ok_or_else(|| HarnessError::execution("cloud session sequence exceeds i64"))?;
            let event = SessionEvent {
                seq: from_i64(seq, "cloud feedback command event sequence")?,
                occurred_at_ms: now_ms,
                run_id: run_id.clone(),
                kind,
            };
            sqlx::query(
                "INSERT INTO cloud_session_events
                    (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .bind(seq)
            .bind(run_id.as_str())
            .bind(ternilo_storage::Json(&event))
            .bind(fencing_token)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
            crate::telemetry::capture_event_in(
                &mut transaction,
                tenant_id,
                session_id,
                &event,
                now_ms,
            )
            .await?;
            events.push(event);
        }
        let last = events
            .last()
            .ok_or_else(|| HarnessError::execution("feedback command produced no events"))?;
        sqlx::query(
            "UPDATE cloud_sessions SET last_seq = $3, updated_at_ms = $4
             WHERE tenant_id = $1 AND session_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(to_i64(last.seq, "cloud feedback command event sequence")?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            ternilo_control::ResourceAction::Submit,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(SessionCommandReceipt { command_id, events })
    }

    pub async fn pending_questions(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<Vec<CloudPendingQuestion>, HarnessError> {
        let user_id = actor_id;
        session_id.validate()?;
        user_id.validate()?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::View,
        )
        .await?;
        let user_id = &owner_id;
        set_tenant(&mut transaction, tenant_id).await?;
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        let query = if self.database.backend() == ternilo_storage::Backend::Postgres {
            "SELECT pending.question\n             FROM cloud_session_questions AS pending\n             JOIN cloud_session_events AS asked\n               ON asked.tenant_id = pending.tenant_id\n              AND asked.session_id = pending.session_id\n              AND asked.run_id = pending.run_id\n              AND (asked.event::jsonb)->>'type' = 'user_question_asked'\n              AND (asked.event::jsonb)->'question'->>'id' = pending.question_id\n             JOIN cloud_sessions AS session\n               ON session.tenant_id = pending.tenant_id\n              AND session.session_id = pending.session_id\n             JOIN cloud_runs AS run\n               ON run.tenant_id = pending.tenant_id AND run.run_id = pending.run_id\n             WHERE pending.tenant_id = $1 AND pending.user_id = $2\n               AND pending.session_id = $3 AND pending.state = 'pending'\n               AND session.archived_at_ms IS NULL\n               AND run.state IN ('running', 'cancel_requested')\n             ORDER BY asked.seq"
        } else {
            "SELECT pending.question
             FROM cloud_session_questions AS pending
             JOIN cloud_session_events AS asked
               ON asked.tenant_id = pending.tenant_id
              AND asked.session_id = pending.session_id
              AND asked.run_id = pending.run_id
              AND asked.event->>'type' = 'user_question_asked'
              AND asked.event->'question'->>'id' = pending.question_id
             JOIN cloud_sessions AS session
               ON session.tenant_id = pending.tenant_id
              AND session.session_id = pending.session_id
             JOIN cloud_runs AS run
               ON run.tenant_id = pending.tenant_id AND run.run_id = pending.run_id
             WHERE pending.tenant_id = $1 AND pending.user_id = $2
               AND pending.session_id = $3 AND pending.state = 'pending'
               AND session.archived_at_ms IS NULL
               AND run.state IN ('running', 'cancel_requested')
             ORDER BY asked.seq"
        };
        let rows = sqlx::query(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(session_id.as_str())
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(CloudPendingQuestion {
                    session_id: session_id.clone(),
                    question: row
                        .try_get::<ternilo_storage::Json<UserQuestion>, _>("question")
                        .map_err(database_error)?
                        .0,
                })
            })
            .collect()
    }

    pub async fn answer_question(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        answer: &UserAnswer,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let user_id = actor_id;
        session_id.validate()?;
        user_id.validate()?;
        require_identifier(&answer.question_id, "cloud question id")?;
        let mut transaction = self.begin().await?;
        let access = ternilo_control::resource_access_in(
            &mut transaction,
            actor_id,
            tenant_id,
            ternilo_control::ResourceKind::Session,
            session_id.as_str(),
        )
        .await?;
        access.require(ternilo_control::ResourceAction::View)?;
        let user_id = &access.storage_user_id;
        set_tenant(&mut transaction, tenant_id).await?;
        ternilo_storage::set_user_scope(&mut transaction, user_id).await?;
        let question = sqlx::query_scalar::<_, ternilo_storage::Json<UserQuestion>>(
            "SELECT question FROM cloud_session_questions
             WHERE tenant_id=$1 AND session_id=$2 AND user_id=$3
               AND question_id=$4 AND state='pending'",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(&answer.question_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            HarnessError::invalid("unknown, already answered, or inactive cloud question")
        })?
        .0;
        let action = ternilo_control::question_resource_action(&question);
        access.require(action)?;
        let changed = sqlx::query(
            "UPDATE cloud_session_questions AS pending
             SET answer = $5, state = 'answered', answered_at_ms = $6
             WHERE pending.tenant_id = $1 AND pending.user_id = $2
               AND pending.session_id = $3 AND pending.question_id = $4
               AND pending.state = 'pending'
               AND EXISTS (
                   SELECT 1 FROM cloud_sessions AS session
                   WHERE session.tenant_id = pending.tenant_id
                     AND session.session_id = pending.session_id
                     AND session.user_id = pending.user_id
                     AND session.archived_at_ms IS NULL
               )
               AND EXISTS (
                   SELECT 1 FROM cloud_runs AS run
                   WHERE run.tenant_id = pending.tenant_id
                     AND run.run_id = pending.run_id
                     AND run.state IN ('running', 'cancel_requested')
               )",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(&answer.question_id)
        .bind(ternilo_storage::Json(answer))
        .bind(to_i64(now_ms, "cloud question answer timestamp")?)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(
                "unknown, already answered, or inactive cloud question",
            ));
        }
        crate::sharing::audit_session_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            user_id,
            action,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(())
    }

    pub async fn get_run(
        &self,
        tenant_id: &TenantId,
        run_id: &RunId,
    ) -> Result<CloudRunRecord, HarnessError> {
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let row = select_run(&mut transaction, tenant_id, run_id).await?;
        transaction.commit().await.map_err(database_error)?;
        decode_run(&row)
    }

    pub async fn list_runs(
        &self,
        tenant_id: &TenantId,
        limit: u32,
    ) -> Result<Vec<CloudRunRecord>, HarnessError> {
        if limit == 0 || limit > 500 {
            return Err(HarnessError::invalid(
                "cloud run list limit must be between 1 and 500",
            ));
        }
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        let rows = sqlx::query(
            "SELECT tenant_id, run_id, user_id, actor_user_id, authorization_session_id, project_id, workspace_id, agent_id, session_id,
                    spec_digest, state, attempt, max_attempts, created_at_ms,
                    updated_at_ms, started_at_ms, finished_at_ms, outcome, error
             FROM cloud_runs WHERE tenant_id = $1
             ORDER BY created_at_ms DESC, run_id DESC LIMIT $2",
        )
        .bind(tenant_id.as_str())
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.iter().map(decode_run).collect()
    }

    pub async fn cancel_run(
        &self,
        tenant_id: &TenantId,
        run_id: &RunId,
        now_ms: u64,
    ) -> Result<CloudRunState, HarnessError> {
        let mut transaction = self.begin().await?;
        let state = Self::cancel_run_in(&mut transaction, tenant_id, run_id, None, now_ms).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(state)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn cancel_run_in(
        transaction: &mut ternilo_storage::Transaction,
        tenant_id: &TenantId,
        run_id: &RunId,
        actor_id: Option<&UserId>,
        now_ms: u64,
    ) -> Result<CloudRunState, HarnessError> {
        let now = to_i64(now_ms, "cloud cancellation timestamp")?;
        set_tenant(transaction, tenant_id).await?;
        let canonical_session = sqlx::query_scalar::<_, String>(
            "SELECT session_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(run_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud run does not exist"))?;
        execution::lock_session_in(transaction, tenant_id, &SessionId::new(canonical_session))
            .await?;
        let row = sqlx::query(ternilo_storage::for_update(
            transaction,
            "SELECT state, session_id, user_id, actor_user_id, quota_reservation_id,
                    lease_owner, lease_token, session_fencing_token
             FROM cloud_runs WHERE tenant_id = $1 AND run_id = $2",
            "SELECT state, session_id, user_id, actor_user_id, quota_reservation_id,
                    lease_owner, lease_token, session_fencing_token
             FROM cloud_runs WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(run_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("cloud run does not exist"))?;
        let current =
            CloudRunState::parse(&row.try_get::<String, _>("state").map_err(database_error)?)?;
        if current.terminal() {
            return Ok(current);
        }
        let session_id: String = row.try_get("session_id").map_err(database_error)?;
        let user_id: String = row.try_get("user_id").map_err(database_error)?;
        let accepted_actor = UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        );
        let cancellation_actor = actor_id.unwrap_or(&accepted_actor);
        ternilo_storage::set_user_scope(transaction, &UserId::new(user_id.clone())).await?;
        sqlx::query(ternilo_storage::for_update(
            transaction,
            "SELECT 1 FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2",
            "SELECT 1 FROM cloud_sessions
             WHERE tenant_id = $1 AND session_id = $2 FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(&session_id)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let reservation_id: String = row
            .try_get("quota_reservation_id")
            .map_err(database_error)?;
        let next = if matches!(current, CloudRunState::Queued | CloudRunState::Leased) {
            crate::steering::requeue_steering_in(transaction, tenant_id, run_id, now_ms).await?;
            sqlx::query(
                "UPDATE cloud_runs
                 SET state = 'cancelled', cancel_requested_at_ms = $3,
                     finished_at_ms = $3, updated_at_ms = $3,
                     lease_expires_at_ms = NULL
                 WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(tenant_id.as_str())
            .bind(run_id.as_str())
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            sqlx::query(
                "DELETE FROM cloud_session_submissions
                 WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 AND run_id = $4",
            )
            .bind(tenant_id.as_str())
            .bind(&user_id)
            .bind(&session_id)
            .bind(run_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            crate::inbox::promote_head(
                transaction,
                tenant_id,
                &UserId::new(&user_id),
                &SessionId::new(&session_id),
                now,
            )
            .await?;
            sqlx::query(
                "UPDATE control_quota_reservations SET state = 'released'
                 WHERE tenant_id = $1 AND reservation_id = $2 AND state = 'active'",
            )
            .bind(tenant_id.as_str())
            .bind(&reservation_id)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            sqlx::query(
                "UPDATE cloud_sessions SET state = 'cancelled', execution=NULL, updated_at_ms = $3
                 WHERE tenant_id = $1 AND session_id = $2 AND current_run_id IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM cloud_runs AS other
                       WHERE other.tenant_id = $1 AND other.session_id = $2
                         AND other.run_id != $4
                         AND other.state IN ('queued', 'leased', 'running', 'cancel_requested')
                   )",
            )
            .bind(tenant_id.as_str())
            .bind(&session_id)
            .bind(now)
            .bind(run_id.as_str())
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            CloudRunState::Cancelled
        } else {
            sqlx::query(
                "UPDATE cloud_runs
                 SET state = 'cancel_requested', cancel_requested_at_ms = $3,
                     updated_at_ms = $3
                 WHERE tenant_id = $1 AND run_id = $2",
            )
            .bind(tenant_id.as_str())
            .bind(run_id.as_str())
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(database_error)?;
            let lease_owner = row
                .try_get::<Option<String>, _>("lease_owner")
                .map_err(database_error)?
                .ok_or_else(|| HarnessError::execution("active cloud run has no lease owner"))?;
            let lease_token = from_i64(
                row.try_get::<Option<i64>, _>("lease_token")
                    .map_err(database_error)?
                    .ok_or_else(|| {
                        HarnessError::execution("active cloud run has no lease token")
                    })?,
                "cloud cancellation run lease token",
            )?;
            let writer_fencing_token = from_i64(
                row.try_get::<Option<i64>, _>("session_fencing_token")
                    .map_err(database_error)?
                    .ok_or_else(|| {
                        HarnessError::execution("active cloud run has no writer fencing token")
                    })?,
                "cloud cancellation writer fencing token",
            )?;
            enqueue_run_cancellation_command(
                transaction,
                tenant_id,
                &UserId::new(user_id.clone()),
                cancellation_actor,
                &SessionId::new(session_id.clone()),
                run_id,
                &lease_owner,
                lease_token,
                writer_fencing_token,
                now_ms,
            )
            .await?;
            CloudRunState::CancelRequested
        };
        Ok(next)
    }

    pub async fn session_events(
        &self,
        tenant_id: &TenantId,
        session_id: &SessionId,
        after_seq: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.session_events_with_access(tenant_id, None, session_id, after_seq, limit)
            .await
    }

    pub async fn session_events_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        after_seq: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.session_events_with_access(tenant_id, Some(actor_id), session_id, after_seq, limit)
            .await
    }

    pub async fn session_history_as(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Result<ternilo_protocol::SessionEventPage, HarnessError> {
        query.validate()?;
        session_id.validate()?;
        let before = query
            .before_seq
            .map(|seq| to_i64(seq, "history cursor"))
            .transpose()?;
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        {
            crate::sharing::session_owner_in(
                &mut transaction,
                tenant_id,
                actor_id,
                session_id,
                ternilo_control::ResourceAction::View,
            )
            .await?;
        }
        let rows = sqlx::query(
            "SELECT event FROM cloud_session_events
             WHERE tenant_id = $1 AND session_id = $2 AND (CAST($3 AS BIGINT) IS NULL OR seq < $3)
             ORDER BY seq DESC LIMIT $4",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(before)
        .bind(i64::from(query.limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let mut events = rows
            .into_iter()
            .map(|row| {
                row.try_get::<ternilo_storage::Json<SessionEvent>, _>("event")
                    .map(|event| event.0)
                    .map_err(database_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        Ok(ternilo_protocol::SessionEventPage::new(events))
    }

    async fn session_events_with_access(
        &self,
        tenant_id: &TenantId,
        actor_id: Option<&UserId>,
        session_id: &SessionId,
        after_seq: Option<u64>,
        limit: u32,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        if limit == 0 || limit > 1_000 {
            return Err(HarnessError::invalid(
                "cloud event limit must be between 1 and 1000",
            ));
        }
        session_id.validate()?;
        let after = after_seq
            .map(|value| to_i64(value, "cloud event cursor"))
            .transpose()?
            .unwrap_or(-1);
        let mut transaction = self.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        if let Some(actor_id) = actor_id {
            crate::sharing::session_owner_in(
                &mut transaction,
                tenant_id,
                actor_id,
                session_id,
                ternilo_control::ResourceAction::View,
            )
            .await?;
        }
        let rows = sqlx::query(
            "SELECT event FROM cloud_session_events
             WHERE tenant_id = $1 AND session_id = $2 AND seq > $3
             ORDER BY seq LIMIT $4",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<ternilo_storage::Json<SessionEvent>, _>("event")
                    .map(|event| event.0)
                    .map_err(database_error)
            })
            .collect()
    }

    /// Copies a content-addressed object produced inside a cloud workspace into
    /// PostgreSQL while the worker still owns the active run lease. Session
    /// events are appended only after every referenced object is durable here.
    pub async fn store_attachment_object(
        &self,
        run: &StartedRun,
        worker_id: &str,
        attachment: &Attachment,
        content: &[u8],
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_started_run(run, worker_id)?;
        attachment.validate()?;
        let digest = attachment.reference_digest().ok_or_else(|| {
            HarnessError::invalid("cloud attachment object requires a retained reference")
        })?;
        if content.is_empty() || content.len() > 64 * 1024 * 1024 {
            return Err(HarnessError::invalid(
                "cloud attachment object must contain 1 byte to 64 MiB",
            ));
        }
        if hex_digest(content) != digest {
            return Err(HarnessError::invalid(
                "cloud attachment object does not match its SHA-256 reference",
            ));
        }
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        let job = require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await?;
        let workspace: String = job.try_get("workspace_id").map_err(database_error)?;
        let changed=sqlx::query("INSERT INTO cloud_attachment_objects(tenant_id,workspace_id,digest,content,created_at_ms) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,workspace_id,digest) DO NOTHING")
            .bind(run.claim.tenant_id.as_str()).bind(&workspace).bind(digest).bind(content).bind(to_i64(now_ms,"cloud attachment timestamp")?).execute(&mut *transaction).await.map_err(database_error)?.rows_affected();
        if changed == 0 {
            let existing=sqlx::query_scalar::<_,Vec<u8>>("SELECT content FROM cloud_attachment_objects WHERE tenant_id=$1 AND workspace_id=$2 AND digest=$3")
                .bind(run.claim.tenant_id.as_str()).bind(workspace).bind(digest).fetch_one(&mut *transaction).await.map_err(database_error)?;
            if existing != content {
                return Err(HarnessError::policy(
                    "cloud attachment replay differs from the persisted object",
                ));
            }
        }
        transaction.commit().await.map_err(database_error)
    }

    pub async fn extensions_for_run(
        &self,
        run: &StartedRun,
        worker_id: &str,
        policy: &ternilo_extension::ExtensionHostPolicy,
        now_ms: u64,
    ) -> Result<Vec<ternilo_extension::ExtensionDistribution>, HarnessError> {
        validate_started_run(run, worker_id)?;
        let references = ternilo_extension::extension_mounts(&run.claim.spec.profile)?
            .into_iter()
            .map(|mount| (mount.package_id, mount.version))
            .collect::<BTreeSet<_>>();
        let mut distributions = Vec::with_capacity(references.len());
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        let job = require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await?;
        if job
            .try_get::<i64, _>("lease_expires_at_ms")
            .map_err(database_error)?
            <= to_i64(now_ms, "cloud plugin timestamp")?
        {
            return Err(HarnessError::policy("cloud run lease has expired"));
        }
        for reference in references {
            let row = sqlx::query("SELECT publisher.trust,plugin.install_request FROM control_extension_packages AS plugin JOIN control_extension_publishers AS publisher ON publisher.tenant_id=plugin.tenant_id AND publisher.key_id=plugin.publisher_key_id WHERE plugin.tenant_id=$1 AND plugin.package_id=$2 AND plugin.version=$3 AND plugin.enabled=1 AND plugin.revoked=0 AND publisher.revoked=0")
                .bind(run.claim.tenant_id.as_str()).bind(&reference.0).bind(&reference.1)
                .fetch_optional(&mut *transaction).await.map_err(database_error)?
            .ok_or_else(|| {
                HarnessError::policy(format!(
                    "cloud extension package {}@{} is unavailable or revoked",
                    reference.0, reference.1
                ))
            })?;
            let publisher = row
                .try_get::<ternilo_storage::Json<ternilo_extension::PublisherTrust>, _>("trust")
                .map_err(database_error)?
                .0;
            let install = row
                .try_get::<ternilo_storage::Json<ternilo_extension::ExtensionInstallRequest>, _>(
                    "install_request",
                )
                .map_err(database_error)?
                .0;
            if install.bundle.manifest.package_id != reference.0
                || install.bundle.manifest.version != reference.1
            {
                return Err(HarnessError::policy(
                    "cloud extension distribution does not match the requested package",
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
        transaction.commit().await.map_err(database_error)?;
        Ok(distributions)
    }

    pub async fn extensions_active(
        &self,
        run: &StartedRun,
        worker_id: &str,
        now_ms: u64,
    ) -> Result<bool, HarnessError> {
        validate_started_run(run, worker_id)?;
        let references = ternilo_extension::extension_mounts(&run.claim.spec.profile)?
            .into_iter()
            .map(|mount| (mount.package_id, mount.version))
            .collect::<BTreeSet<_>>();
        if references.is_empty() {
            return Ok(true);
        }
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        let job = match require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await {
            Ok(job) => job,
            Err(error) if error.code == ternilo_protocol::ErrorCode::PolicyDenied => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        if job
            .try_get::<i64, _>("lease_expires_at_ms")
            .map_err(database_error)?
            <= to_i64(now_ms, "cloud plugin timestamp")?
        {
            return Ok(false);
        }
        for reference in references {
            let active=sqlx::query_scalar::<_,i64>("SELECT CAST(EXISTS(SELECT 1 FROM control_extension_packages AS plugin JOIN control_extension_publishers AS publisher ON publisher.tenant_id=plugin.tenant_id AND publisher.key_id=plugin.publisher_key_id WHERE plugin.tenant_id=$1 AND plugin.package_id=$2 AND plugin.version=$3 AND plugin.enabled=1 AND plugin.revoked=0 AND publisher.revoked=0) AS INTEGER)")
                .bind(run.claim.tenant_id.as_str()).bind(&reference.0).bind(&reference.1).fetch_one(&mut *transaction).await.map_err(database_error)?;
            if active == 0 {
                return Ok(false);
            }
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(true)
    }
}

async fn initialize_database(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "worker_access",
            1,
            include_str!("schema/worker_access.sql"),
            include_str!("schema/worker_access_postgres.sql"),
        )
        .await?;
    crate::workspace_storage::initialize_database(database).await?;
    let schema = match database.backend() {
        ternilo_storage::Backend::Sqlite => concat!(
            include_str!("schema/sqlite.sql"),
            include_str!("schema/cloud_live_sqlite.sql"),
            include_str!("schema/execution_admission.sql"),
            include_str!("schema/workspace_occupancy.sql"),
            include_str!("schema/execution_families.sql")
        ),
        ternilo_storage::Backend::Postgres => concat!(
            include_str!("schema/postgres.sql"),
            include_str!("schema/cloud_live_postgres.sql"),
            include_str!("schema/execution_admission.sql"),
            include_str!("schema/workspace_occupancy.sql"),
            include_str!("schema/execution_families.sql")
        ),
    };
    database
        .initialize(
            "cloud",
            15,
            schema,
            concat!(
                include_str!("schema/postgres_access.sql"),
                include_str!("schema/commands_postgres_access.sql"),
                include_str!("schema/telemetry_postgres_access.sql"),
                include_str!("schema/grants_postgres.sql"),
                include_str!("schema/execution_admission_postgres.sql"),
                include_str!("schema/workspace_occupancy_postgres.sql"),
                include_str!("schema/workspace_recovery_postgres.sql"),
                include_str!("schema/workspace_waiting_postgres.sql"),
                include_str!("schema/execution_families_postgres.sql"),
            ),
        )
        .await?;
    database
        .initialize(
            "cloud_queue_batches",
            1,
            include_str!("schema/queue_batches.sql"),
            "",
        )
        .await?;
    database
        .initialize(
            "cloud_account_cleanup",
            1,
            include_str!("schema/account_cleanup.sql"),
            include_str!("schema/account_cleanup_postgres.sql"),
        )
        .await?;
    let resource_notifications = match database.backend() {
        ternilo_storage::Backend::Sqlite => include_str!("schema/resource_live_sqlite.sql"),
        ternilo_storage::Backend::Postgres => include_str!("schema/resource_live_postgres.sql"),
    };
    database
        .initialize("resource_live", 1, resource_notifications, "")
        .await?;
    database
        .initialize(
            "resource_ownership_live",
            1,
            match database.backend() {
                ternilo_storage::Backend::Sqlite => {
                    include_str!("schema/ownership_live_sqlite.sql")
                }
                ternilo_storage::Backend::Postgres => {
                    include_str!("schema/ownership_live_postgres.sql")
                }
            },
            "",
        )
        .await?;
    crate::maintenance::initialize_database(database).await
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_run_cancellation_command(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    actor_id: &UserId,
    session_id: &SessionId,
    run_id: &RunId,
    lease_owner: &str,
    lease_token: u64,
    writer_fencing_token: u64,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let command_seed = format!(
        "{}\0{}\0{}\0{lease_token}",
        tenant_id.as_str(),
        run_id.as_str(),
        lease_owner,
    );
    let command_hash = Sha256::digest(command_seed.as_bytes());
    let command_id = CommandId::new(format!(
        "cancel_{}",
        URL_SAFE_NO_PAD.encode(&command_hash[..16])
    ));
    let expires_at_ms = now_ms
        .checked_add(5 * 60 * 1_000)
        .ok_or_else(|| HarnessError::invalid("cloud cancel command expiry exceeds u64"))?;
    let command = ExecutorCommand {
        input_provenance: None,
        command_id: command_id.clone(),
        scope: ExecutorScope {
            tenant_id: tenant_id.clone(),
            user_id: user_id.clone(),
        },
        input_authorization: None,
        issued_at_ms: now_ms,
        expires_at_ms,
        body: ExecutorCommandBody::CancelRun {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        },
    };
    command.validate(now_ms)?;
    let command_value = crate::commands::canonical_command(&command)?;
    let command_digest = Sha256::digest(serde_json::to_vec(&command_value).map_err(|error| {
        HarnessError::execution(format!("encode cloud cancellation command: {error}"))
    })?);
    let command_seq = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(command_seq), -1) + 1
         FROM cloud_session_commands
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        "INSERT INTO cloud_session_commands (
            tenant_id, user_id, session_id, command_id, command_seq,
            command_json, command_digest, required_capability,
            required_catalog_revision, read_only, target_run_id,
            target_writer_fencing_token, state, issued_at_ms, expires_at_ms,
            created_at_ms, updated_at_ms, actor_user_id
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, 0, $10, $11,
            'pending', $12, $13, $12, $12, $14
         )
         ON CONFLICT (tenant_id, command_id) DO NOTHING",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .bind(command_id.as_str())
    .bind(command_seq)
    .bind(ternilo_storage::Json(&command))
    .bind(command_digest.as_slice())
    .bind(crate::commands::capability_name(
        ExecutorCapability::RunCancellation,
    )?)
    .bind(crate::CLOUD_CATALOG_REVISION)
    .bind(run_id.as_str())
    .bind(to_i64(
        writer_fencing_token,
        "cloud cancellation writer fencing token",
    )?)
    .bind(to_i64(now_ms, "cloud cancellation command timestamp")?)
    .bind(to_i64(
        expires_at_ms,
        "cloud cancellation command expiry timestamp",
    )?)
    .bind(actor_id.as_str())
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(crate) async fn select_run(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    run_id: &RunId,
) -> Result<sqlx::any::AnyRow, HarnessError> {
    sqlx::query(
        "SELECT tenant_id, run_id, user_id, actor_user_id, authorization_session_id, project_id, workspace_id, agent_id, session_id,
                spec_digest, state, attempt, max_attempts, created_at_ms,
                updated_at_ms, started_at_ms, finished_at_ms, outcome, error
         FROM cloud_runs WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(run_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud run does not exist"))
}

pub(crate) fn decode_run(row: &sqlx::any::AnyRow) -> Result<CloudRunRecord, HarnessError> {
    let outcome = row
        .try_get::<Option<ternilo_storage::Json<RunOutcome>>, _>("outcome")
        .map_err(database_error)?
        .map(|value| value.0);
    let error = row
        .try_get::<Option<ternilo_storage::Json<HarnessError>>, _>("error")
        .map_err(database_error)?
        .map(|value| value.0);
    Ok(CloudRunRecord {
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        ),
        authorization_session_id: SessionId::new(
            row.try_get::<String, _>("authorization_session_id")
                .map_err(database_error)?,
        ),
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        run_id: RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?),
        user_id: ternilo_protocol::UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        project_id: row.try_get("project_id").map_err(database_error)?,
        workspace_id: WorkspaceId::new(
            row.try_get::<String, _>("workspace_id")
                .map_err(database_error)?,
        ),
        agent_id: ternilo_protocol::AgentId::new(
            row.try_get::<String, _>("agent_id")
                .map_err(database_error)?,
        ),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        spec_digest_hex: hex(&row
            .try_get::<Vec<u8>, _>("spec_digest")
            .map_err(database_error)?),
        state: CloudRunState::parse(&row.try_get::<String, _>("state").map_err(database_error)?)?,
        attempt: from_i32(
            row.try_get("attempt").map_err(database_error)?,
            "cloud attempt",
        )?,
        max_attempts: from_i32(
            row.try_get("max_attempts").map_err(database_error)?,
            "cloud maximum attempts",
        )?,
        created_at_ms: from_i64(
            row.try_get("created_at_ms").map_err(database_error)?,
            "cloud creation timestamp",
        )?,
        updated_at_ms: from_i64(
            row.try_get("updated_at_ms").map_err(database_error)?,
            "cloud update timestamp",
        )?,
        started_at_ms: optional_u64(row, "started_at_ms", "cloud start timestamp")?,
        finished_at_ms: optional_u64(row, "finished_at_ms", "cloud finish timestamp")?,
        outcome,
        error,
    })
}

fn decode_claim(
    row: &sqlx::any::AnyRow,
    workspace_use: crate::WorkspaceUseTicket,
) -> Result<CloudRunClaim, HarnessError> {
    workspace_use.validate()?;
    let spec = row
        .try_get::<ternilo_storage::Json<RunSpec>, _>("spec")
        .map_err(database_error)?
        .0;
    spec.validate_shape()?;
    let stored_digest: Vec<u8> = row.try_get("spec_digest").map_err(database_error)?;
    let stored_digest: [u8; 32] = stored_digest
        .try_into()
        .map_err(|_| HarnessError::execution("cloud RunSpec digest has invalid length"))?;
    if spec_digest(&spec)? != stored_digest {
        return Err(HarnessError::policy(
            "cloud RunSpec digest verification failed",
        ));
    }
    let claim = CloudRunClaim {
        provenance: crate::input_provenance::stored_provenance(row)?,
        actor_user_id: UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        ),
        authorization_session_id: SessionId::new(
            row.try_get::<String, _>("authorization_session_id")
                .map_err(database_error)?,
        ),
        tenant_id: TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        ),
        run_id: RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?),
        session_id: SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        ),
        workspace_use,
        lease_token: from_i64(
            row.try_get("lease_token").map_err(database_error)?,
            "cloud claim token",
        )?,
        spec,
        spec_digest: stored_digest,
    };
    if claim.spec.metadata.tenant_id != claim.tenant_id
        || claim.spec.metadata.run_id != claim.run_id
        || claim.spec.metadata.session_id != claim.session_id
    {
        return Err(HarnessError::policy(
            "cloud claim columns do not match its immutable RunSpec",
        ));
    }
    Ok(claim)
}

fn validate_claim(
    claim: &CloudRunClaim,
    worker_id: &str,
    lease_ttl: Duration,
) -> Result<(), HarnessError> {
    claim.tenant_id.validate()?;
    claim.run_id.validate()?;
    claim.session_id.validate()?;
    if claim.lease_token == 0 {
        return Err(HarnessError::invalid("cloud claim token must be positive"));
    }
    validate_lease(worker_id, lease_ttl)
}

fn validate_started_run(run: &StartedRun, worker_id: &str) -> Result<(), HarnessError> {
    run.claim.tenant_id.validate()?;
    run.claim.run_id.validate()?;
    run.claim.session_id.validate()?;
    require_identifier(worker_id, "cloud worker id")?;
    if run.claim.lease_token == 0 || run.fencing_token == 0 {
        return Err(HarnessError::invalid(
            "cloud claim and writer fencing tokens must be positive",
        ));
    }
    Ok(())
}

fn validate_lease(worker_id: &str, lease_ttl: Duration) -> Result<(), HarnessError> {
    require_identifier(worker_id, "cloud worker id")?;
    if lease_ttl < Duration::from_secs(5) || lease_ttl > Duration::from_mins(5) {
        return Err(HarnessError::invalid(
            "cloud lease TTL must be between 5 seconds and 5 minutes",
        ));
    }
    Ok(())
}

fn validate_event_history(events: &[SessionEvent]) -> Result<(), HarnessError> {
    for (expected, event) in events.iter().enumerate() {
        let expected = u64::try_from(expected)
            .map_err(|_| HarnessError::execution("cloud session sequence exceeds u64"))?;
        if event.seq != expected {
            return Err(HarnessError::policy(format!(
                "cloud session event history is discontinuous at sequence {expected}"
            )));
        }
    }
    Ok(())
}

pub fn spec_digest(spec: &RunSpec) -> Result<[u8; 32], HarnessError> {
    let mut value = serde_json::to_value(spec)
        .map_err(|error| HarnessError::execution(format!("encode cloud RunSpec: {error}")))?;
    canonicalize_json(&mut value)?;
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| HarnessError::execution(format!("encode cloud RunSpec: {error}")))?;
    Ok(Sha256::digest(encoded).into())
}

pub(crate) fn hex_digest(content: &[u8]) -> String {
    Sha256::digest(content)
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        })
}

fn canonicalize_json(value: &mut serde_json::Value) -> Result<(), HarnessError> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                canonicalize_json(value)?;
            }
        }
        serde_json::Value::Object(values) => {
            let mut entries = std::mem::take(values).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (_, value) in &mut entries {
                canonicalize_json(value)?;
            }
            values.extend(entries);
        }
        serde_json::Value::Number(number) if !number.is_i64() && !number.is_u64() => {
            let normalized = number
                .as_f64()
                .and_then(serde_json::Number::from_f64)
                .ok_or_else(|| {
                    HarnessError::invalid("RunSpec contains a non-finite JSON number")
                })?;
            *number = normalized;
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
    Ok(())
}

pub(crate) async fn set_tenant(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    ternilo_storage::set_tenant_scope(transaction, tenant_id).await
}

fn optional_u64(
    row: &sqlx::any::AnyRow,
    column: &str,
    label: &str,
) -> Result<Option<u64>, HarnessError> {
    row.try_get::<Option<i64>, _>(column)
        .map_err(database_error)?
        .map(|value| from_i64(value, label))
        .transpose()
}

fn require_identifier(value: &str, label: &str) -> Result<(), HarnessError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        Err(HarnessError::invalid(format!(
            "{label} must contain 1 to 128 bytes without whitespace or control characters"
        )))
    } else {
        Ok(())
    }
}

pub(crate) const fn permission_str(value: PermissionPreset) -> &'static str {
    match value {
        PermissionPreset::ReadOnly => "read_only",
        PermissionPreset::WorkspaceWrite => "workspace_write",
        PermissionPreset::FullAccess => "full_access",
    }
}

pub(crate) const fn mode_str(value: SessionMode) -> &'static str {
    match value {
        SessionMode::Execute => "execute",
        SessionMode::Plan => "plan",
    }
}

fn duration_ms(duration: Duration) -> Result<u64, HarnessError> {
    duration
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::invalid("cloud duration exceeds u64 milliseconds"))
}

pub(crate) fn to_i64(value: u64, label: &str) -> Result<i64, HarnessError> {
    i64::try_from(value)
        .map_err(|_| HarnessError::invalid(format!("{label} exceeds PostgreSQL bigint")))
}

pub(crate) fn from_i64(value: i64, label: &str) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

fn from_i32(value: i32, label: &str) -> Result<u32, HarnessError> {
    u32::try_from(value).map_err(|_| HarnessError::execution(format!("{label} is negative")))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn database_error(error: sqlx::Error) -> HarnessError {
    HarnessError::execution(format!("cloud database error: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn json_error(error: serde_json::Error) -> HarnessError {
    HarnessError::execution(format!("cloud JSON error: {error}"))
}
