use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use sqlx::Row;
use ternilo_protocol::{
    HarnessError, QueueEditRequest, RunId, SessionId, SessionInboxSnapshot, SessionSubmission,
    SessionSubmissionRequest, SubmissionContent, SubmissionDelivery, SubmissionId,
    SubmissionPlacement, TenantId, UserId,
};

use crate::{
    CloudRunRecord, CloudStore, CompiledRun, StartedRun,
    store::{database_error, decode_run, select_run, spec_digest, to_i64},
};

mod batches;

#[derive(Clone, Debug, PartialEq)]
pub struct CloudSubmissionReceipt {
    pub run: CloudRunRecord,
    pub submission: SessionSubmission,
}

impl CloudStore {
    pub async fn run_submission_for_worker(
        &self,
        worker_id: &str,
        run: &StartedRun,
        now_ms: u64,
    ) -> Result<Option<SessionSubmission>, HarnessError> {
        if worker_id.trim().is_empty() {
            return Err(HarnessError::invalid("cloud Worker id must not be empty"));
        }
        let mut transaction = self.tenant_transaction(&run.claim.tenant_id).await?;
        if let Err(error) =
            crate::store::require_writer_in(&mut transaction, run, worker_id, Some(now_ms)).await
        {
            if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                return Ok(None);
            }
            return Err(error);
        }
        let row = sqlx::query("SELECT submission_id, run_id, input_provenance, content, submission_references, attachments, placement, created_at_ms, updated_at_ms FROM cloud_session_submissions WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3 AND placement='running'")
            .bind(run.claim.tenant_id.as_str()).bind(run.claim.session_id.as_str()).bind(run.claim.run_id.as_str())
            .fetch_optional(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        row.as_ref().map(decode_submission).transpose()
    }

    pub async fn submit_run(
        &self,
        compiled: &CompiledRun,
        quota_reservation_id: &str,
        now_ms: u64,
    ) -> Result<CloudRunRecord, HarnessError> {
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(compiled.spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: compiled.spec.input.clone(),
            },
            references: compiled.spec.references.clone(),
            attachments: compiled.spec.attachments.clone(),
        };
        self.enqueue_session_submission(compiled, quota_reservation_id, &request, now_ms)
            .await
            .map(|receipt| receipt.run)
    }

    pub async fn enqueue_session_submission(
        &self,
        compiled: &CompiledRun,
        quota_reservation_id: &str,
        request: &SessionSubmissionRequest,
        now_ms: u64,
    ) -> Result<CloudSubmissionReceipt, HarnessError> {
        let mut transaction = self.begin().await?;
        let receipt = Self::enqueue_session_submission_in(
            &mut transaction,
            compiled,
            quota_reservation_id,
            request,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(receipt)
    }

    pub async fn enqueue_session_submission_in(
        transaction: &mut ternilo_storage::Transaction,
        compiled: &CompiledRun,
        quota_reservation_id: &str,
        request: &SessionSubmissionRequest,
        now_ms: u64,
    ) -> Result<CloudSubmissionReceipt, HarnessError> {
        Self::enqueue_session_submission_with_parent_in(
            transaction,
            compiled,
            quota_reservation_id,
            request,
            None,
            now_ms,
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn enqueue_session_submission_with_parent_in(
        transaction: &mut ternilo_storage::Transaction,
        compiled: &CompiledRun,
        quota_reservation_id: &str,
        request: &SessionSubmissionRequest,
        parent: Option<&StartedRun>,
        now_ms: u64,
    ) -> Result<CloudSubmissionReceipt, HarnessError> {
        validate_enqueue(compiled, quota_reservation_id, request)?;
        crate::account_cleanup::require_active_actor_in(transaction, &compiled.actor_user_id)
            .await?;
        let spec = &compiled.spec;
        let tenant_id = &spec.metadata.tenant_id;
        let user_id = &spec.metadata.user_id;
        let session_id = &spec.metadata.session_id;
        let project_id = spec
            .metadata
            .project_id
            .as_deref()
            .ok_or_else(|| HarnessError::invalid("cloud submission requires a project"))?;
        let digest = spec_digest(spec)?;
        let now = to_i64(now_ms, "cloud submission timestamp")?;
        let max_attempts = i32::try_from(compiled.max_attempts)
            .map_err(|_| HarnessError::invalid("cloud max attempts exceeds PostgreSQL integer"))?;
        let reserved_tokens = to_i64(compiled.reserved_model_tokens, "reserved model tokens")?;
        let submission_id = random_submission_id();
        let author = if let Some(source) = compiled.automated_input {
            ternilo_protocol::InputAuthor::Automation { source }
        } else {
            let username: String =
                sqlx::query_scalar("SELECT username FROM control_users WHERE user_id=$1")
                    .bind(compiled.actor_user_id.as_str())
                    .fetch_one(&mut **transaction)
                    .await
                    .map_err(database_error)?;
            ternilo_protocol::InputAuthor::Account {
                user_id: compiled.actor_user_id.clone(),
                username,
            }
        };
        let provenance = ternilo_protocol::InputProvenance {
            run_id: None,
            input_id: submission_id.clone(),
            author,
        };
        provenance.validate()?;

        set_scope(transaction, tenant_id, user_id).await?;

        let inserted = sqlx::query(
            "INSERT INTO cloud_sessions
                (tenant_id, session_id, user_id, project_id, workspace_id, agent_id, state,
                 created_at_ms, updated_at_ms, model_snapshot)
             VALUES ($1, $2, $3, $4, $5, $6, 'queued', $7, $7, $8)
             ON CONFLICT (tenant_id, session_id) DO NOTHING",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(project_id)
        .bind(spec.metadata.workspace_id.as_str())
        .bind(spec.metadata.agent_id.as_str())
        .bind(now)
        .bind(crate::profile_model_snapshot(&spec.profile)?.map(ternilo_storage::Json))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if inserted == 1 {
            crate::execution_families::ensure_root_in(
                transaction,
                tenant_id,
                session_id,
                user_id,
                &spec.metadata.workspace_id,
                now_ms,
            )
            .await?;
        }
        require_session_identity(transaction, compiled).await?;
        if let Some(target) = request.content.regeneration_target() {
            let active: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM cloud_runs WHERE tenant_id=$1 AND session_id=$2
                 AND state NOT IN ('succeeded', 'failed', 'cancelled', 'indeterminate')",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
            if active != 0 {
                return Err(HarnessError::conflict(
                    "stop the current run and clear queued inputs before replacing a turn",
                ));
            }
            let records: Vec<ternilo_storage::Json<ternilo_protocol::SessionEvent>> = sqlx::query_scalar(
                "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 ORDER BY seq",
            )
            .bind(tenant_id.as_str())
            .bind(session_id.as_str())
            .fetch_all(&mut **transaction)
            .await
            .map_err(database_error)?;
            let events: Vec<_> = records.into_iter().map(|record| record.0).collect();
            ternilo_protocol::validate_regeneration(&events, target)?;
        }

        let reservation = sqlx::query(ternilo_storage::for_update(
            transaction,
            "SELECT reserved_model_tokens, run_id, expires_at_ms
             FROM control_quota_reservations
             WHERE tenant_id = $1 AND reservation_id = $2 AND user_id = $3
               AND state = 'active'",
            "SELECT reserved_model_tokens, run_id, expires_at_ms
             FROM control_quota_reservations
             WHERE tenant_id = $1 AND reservation_id = $2 AND user_id = $3
               AND state = 'active' FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(quota_reservation_id)
        .bind(user_id.as_str())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("active cloud quota reservation does not exist"))?;
        let reservation_tokens: i64 = reservation
            .try_get("reserved_model_tokens")
            .map_err(database_error)?;
        let reservation_run: Option<String> =
            reservation.try_get("run_id").map_err(database_error)?;
        let reservation_expiry: i64 = reservation
            .try_get("expires_at_ms")
            .map_err(database_error)?;
        if reservation_tokens != reserved_tokens
            || reservation_run
                .as_deref()
                .is_some_and(|run_id| run_id != spec.metadata.run_id.as_str())
            || reservation_expiry <= now
        {
            return Err(HarnessError::policy(
                "quota reservation does not match this cloud run",
            ));
        }

        sqlx::query(
            "INSERT INTO cloud_runs
                (tenant_id, run_id, user_id, project_id, workspace_id, agent_id, session_id,
                 spec, spec_digest, quota_reservation_id, state, priority,
                 max_attempts, available_at_ms, created_at_ms, updated_at_ms, actor_user_id, authorization_session_id, input_provenance)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'queued',
                     $11, $12, $13, $13, $13, $14, $15, $16)",
        )
        .bind(tenant_id.as_str())
        .bind(spec.metadata.run_id.as_str())
        .bind(user_id.as_str())
        .bind(project_id)
        .bind(spec.metadata.workspace_id.as_str())
        .bind(spec.metadata.agent_id.as_str())
        .bind(session_id.as_str())
        .bind(ternilo_storage::Json(spec))
        .bind(digest.to_vec())
        .bind(quota_reservation_id)
        .bind(compiled.priority)
        .bind(max_attempts)
        .bind(now)
        .bind(compiled.actor_user_id.as_str())
        .bind(compiled.authorization_session_id.as_str())
        .bind(ternilo_storage::Json(&provenance))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        crate::run_lineage::record_in(transaction, compiled, parent, now_ms).await?;
        crate::telemetry::sync_session_telemetry_in(
            transaction,
            tenant_id,
            user_id,
            session_id,
            now_ms,
        )
        .await?;

        sqlx::query(
            "INSERT INTO cloud_session_inboxes
                (tenant_id, user_id, session_id, paused, error, next_position, updated_at_ms)
             VALUES ($1, $2, $3, 0, NULL, 0, $4)
             ON CONFLICT (tenant_id, user_id, session_id) DO UPDATE
             SET paused = 0, error = NULL, updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let fifo_position = sqlx::query_scalar::<_, i64>(
            "UPDATE cloud_session_inboxes
             SET next_position = next_position + 1, updated_at_ms = $4
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
             RETURNING next_position - 1",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(now)
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "INSERT INTO cloud_session_submissions
                (tenant_id, user_id, session_id, submission_id, run_id,
                 requested_delivery, content, submission_references, attachments, placement, fifo_position,
                 created_at_ms, updated_at_ms, actor_user_id, input_provenance)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12, $13, $14)",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .bind(spec.metadata.run_id.as_str())
        .bind(delivery_str(request.delivery))
        .bind(ternilo_storage::Json(&request.content))
        .bind(ternilo_storage::Json(&request.references))
        .bind(ternilo_storage::Json(&request.attachments))
        .bind(placement_str(SubmissionPlacement::Queued))
        .bind(fifo_position)
        .bind(now)
        .bind(compiled.actor_user_id.as_str())
        .bind(ternilo_storage::Json(&provenance))
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        crate::shared_attachments::retain_submission_uploads_in(
            transaction,
            tenant_id,
            session_id,
            &submission_id,
            &spec.metadata.run_id,
            &request.attachments,
            now_ms,
        )
        .await?;
        let promoted = promote_head(transaction, tenant_id, user_id, session_id, now).await?;
        let placement = if promoted.as_ref() == Some(&spec.metadata.run_id) {
            SubmissionPlacement::Running
        } else {
            SubmissionPlacement::Queued
        };
        sqlx::query(
            "UPDATE cloud_sessions SET state = 'queued', updated_at_ms = $4
             WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND current_run_id IS NULL",
        )
        .bind(tenant_id.as_str())
        .bind(session_id.as_str())
        .bind(user_id.as_str())
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        let row = select_run(transaction, tenant_id, &spec.metadata.run_id).await?;

        let submission = SessionSubmission {
            provenance: Some(provenance),
            id: submission_id,
            run_id: spec.metadata.run_id.clone(),
            content: request.content.clone(),
            references: request.references.clone(),
            attachments: request.attachments.clone(),
            placement,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        submission.validate()?;
        Ok(CloudSubmissionReceipt {
            run: decode_run(&row)?,
            submission,
        })
    }

    pub async fn session_inbox(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
    ) -> Result<SessionInboxSnapshot, HarnessError> {
        let user_id = actor_id;
        validate_owner_scope(tenant_id, user_id, session_id)?;
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
        set_scope(&mut transaction, tenant_id, user_id).await?;
        require_owned_session(&mut transaction, tenant_id, user_id, session_id).await?;
        let header = sqlx::query(
            "SELECT paused, error FROM cloud_session_inboxes
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let rows = submission_rows(&mut transaction, tenant_id, user_id, session_id).await?;
        transaction.commit().await.map_err(database_error)?;
        let items = rows
            .iter()
            .map(decode_submission)
            .collect::<Result<Vec<_>, _>>()?;
        let active_run_id = items
            .iter()
            .find(|item| item.placement == SubmissionPlacement::Running)
            .map(|item| item.run_id.clone());
        Ok(SessionInboxSnapshot {
            session_id: session_id.clone(),
            active_run_id,
            paused: header
                .as_ref()
                .is_some_and(|row| row.get::<i64, _>("paused") != 0),
            error: header
                .as_ref()
                .and_then(|row| row.get::<Option<String>, _>("error")),
            items,
        })
    }

    pub async fn queued_submission_run(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
    ) -> Result<CompiledRun, HarnessError> {
        let mut tx = self.begin().await?;
        let owner = crate::sharing::session_owner_in(
            &mut tx,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        set_scope(&mut tx, tenant_id, &owner).await?;
        let row = sqlx::query("SELECT run.spec, run.actor_user_id, run.authorization_session_id, run.priority, run.max_attempts,
            reservation.reserved_model_tokens FROM cloud_session_submissions AS submission
            JOIN cloud_runs AS run ON run.tenant_id=submission.tenant_id AND run.run_id=submission.run_id
            JOIN control_quota_reservations AS reservation ON reservation.tenant_id=run.tenant_id
                AND reservation.reservation_id=run.quota_reservation_id
            WHERE submission.tenant_id=$1 AND submission.session_id=$2 AND submission.submission_id=$3
                AND submission.user_id=$4 AND submission.placement='queued' AND run.state='queued'")
            .bind(tenant_id.as_str()).bind(session_id.as_str()).bind(submission_id.as_str()).bind(owner.as_str())
            .fetch_optional(&mut *tx).await.map_err(database_error)?.ok_or_else(unknown_submission)?;
        let compiled = CompiledRun {
            automated_input: None,
            spec: row
                .try_get::<ternilo_storage::Json<ternilo_protocol::RunSpec>, _>("spec")
                .map_err(database_error)?
                .0,
            actor_user_id: UserId::new(
                row.try_get::<String, _>("actor_user_id")
                    .map_err(database_error)?,
            ),
            authorization_session_id: SessionId::new(
                row.try_get::<String, _>("authorization_session_id")
                    .map_err(database_error)?,
            ),
            reserved_model_tokens: u64::try_from(
                row.try_get::<i64, _>("reserved_model_tokens")
                    .map_err(database_error)?,
            )
            .map_err(|_| HarnessError::execution("queued task token limit is negative"))?,
            priority: row.try_get("priority").map_err(database_error)?,
            max_attempts: u32::try_from(
                row.try_get::<i32, _>("max_attempts")
                    .map_err(database_error)?,
            )
            .map_err(|_| HarnessError::execution("queued task attempts are negative"))?,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(compiled)
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn edit_queued_session_submission(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        request: QueueEditRequest,
        replacement: &CompiledRun,
        now_ms: u64,
    ) -> Result<SessionSubmission, HarnessError> {
        let user_id = actor_id;
        validate_owner_scope(tenant_id, user_id, session_id)?;
        submission_id.validate()?;
        request.validate()?;
        replacement.spec.validate_shape()?;
        let digest = spec_digest(&replacement.spec)?;
        let mut transaction = self.begin().await?;
        crate::account_cleanup::require_active_actor_in(&mut transaction, actor_id).await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Submit,
        )
        .await?;
        let user_id = &owner_id;
        set_scope(&mut transaction, tenant_id, user_id).await?;
        crate::store::lock_session_in(&mut transaction, tenant_id, session_id).await?;
        if actor_id != user_id {
            crate::shared_attachments::require_session_attachment_references_in(
                &mut transaction,
                tenant_id,
                session_id,
                &replacement.spec.attachments,
            )
            .await?;
        }
        let row = sqlx::query(ternilo_storage::for_update(
            &transaction,
            "SELECT submission.submission_id, submission.run_id, submission.input_provenance, submission.content,
                    submission.attachments, submission.placement, submission.steering_command_id,
                    submission.created_at_ms, submission.updated_at_ms,
                    run.project_id, run.workspace_id, run.agent_id,
                    run.priority, run.max_attempts, run.quota_reservation_id,
                    reservation.reserved_model_tokens
             FROM cloud_session_submissions AS submission
             JOIN cloud_runs AS run
               ON run.tenant_id = submission.tenant_id AND run.run_id = submission.run_id
             JOIN control_quota_reservations AS reservation
               ON reservation.tenant_id = run.tenant_id
              AND reservation.reservation_id = run.quota_reservation_id
             WHERE submission.tenant_id = $1 AND submission.user_id = $2
               AND submission.session_id = $3 AND submission.submission_id = $4",
            "SELECT submission.submission_id, submission.run_id, submission.input_provenance, submission.content,
                    submission.attachments, submission.placement, submission.steering_command_id,
                    submission.created_at_ms, submission.updated_at_ms,
                    run.project_id, run.workspace_id, run.agent_id,
                    run.priority, run.max_attempts, run.quota_reservation_id,
                    reservation.reserved_model_tokens
             FROM cloud_session_submissions AS submission
             JOIN cloud_runs AS run
               ON run.tenant_id = submission.tenant_id AND run.run_id = submission.run_id
             JOIN control_quota_reservations AS reservation
               ON reservation.tenant_id = run.tenant_id
              AND reservation.reservation_id = run.quota_reservation_id
             WHERE submission.tenant_id = $1 AND submission.user_id = $2
               AND submission.session_id = $3 AND submission.submission_id = $4
             FOR UPDATE OF submission, run",
        ))
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(unknown_submission)?;
        if row.get::<String, _>("placement") != "queued" {
            return Err(HarnessError::invalid(
                "only queued submissions can be edited",
            ));
        }
        if row
            .try_get::<Option<String>, _>("steering_command_id")
            .map_err(database_error)?
            .is_some()
        {
            return Err(HarnessError::conflict(
                "submission is being delivered into the active turn; wait until steering finishes before editing",
            ));
        }
        if &replacement.actor_user_id != actor_id
            || replacement.authorization_session_id != *session_id
        {
            return Err(HarnessError::policy(
                "queued replacement must retain its authenticated actor and session authority",
            ));
        }
        let previous = from_database_timestamp(&row, "updated_at_ms")?;
        if previous != request.expected_updated_at_ms {
            return Err(HarnessError::conflict(
                "queued submission has been modified; load the latest version before editing",
            ));
        }
        let updated_at_ms = now_ms.max(previous + 1);
        let now = to_i64(updated_at_ms, "cloud submission edit timestamp")?;
        let original_attachments = row
            .get::<ternilo_storage::Json<Vec<ternilo_protocol::Attachment>>, _>("attachments")
            .0;
        if replacement.spec.attachments != original_attachments {
            return Err(HarnessError::invalid(
                "queued edits must preserve accepted attachments",
            ));
        }
        let run_id = RunId::new(row.get::<String, _>("run_id"));
        let project_id: String = row.get("project_id");
        let workspace_id: String = row.get("workspace_id");
        let agent_id: String = row.get("agent_id");
        validate_replacement(
            replacement,
            tenant_id,
            user_id,
            session_id,
            &run_id,
            &project_id,
            &workspace_id,
            &agent_id,
            row.get("priority"),
            row.get("max_attempts"),
            row.get("reserved_model_tokens"),
        )?;
        let mut content = row
            .get::<ternilo_storage::Json<SubmissionContent>, _>("content")
            .0;
        match &mut content {
            SubmissionContent::Prompt { input }
            | SubmissionContent::Skill { input, .. }
            | SubmissionContent::Regenerate { input, .. } => {
                input.clone_from(&request.input);
            }
        }
        sqlx::query(
            "UPDATE cloud_session_submissions
             SET content = $5, submission_references = $6, attachments = $7, updated_at_ms = $8, actor_user_id = $9
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND submission_id = $4",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .bind(ternilo_storage::Json(&content))
        .bind(ternilo_storage::Json(&replacement.spec.references))
        .bind(ternilo_storage::Json(&replacement.spec.attachments))
        .bind(now)
        .bind(actor_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        let updated_run = sqlx::query(
            "UPDATE cloud_runs SET spec = $3, spec_digest = $4, updated_at_ms = $5, actor_user_id = $6, authorization_session_id = $7
             WHERE tenant_id = $1 AND run_id = $2 AND state = 'queued'",
        )
        .bind(tenant_id.as_str())
        .bind(run_id.as_str())
        .bind(ternilo_storage::Json(&replacement.spec))
        .bind(digest.to_vec())
        .bind(now)
        .bind(actor_id.as_str())
        .bind(session_id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        if updated_run != 1 {
            return Err(HarnessError::invalid(
                "queued submission run is unavailable",
            ));
        }
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
        let submission = SessionSubmission {
            provenance: crate::input_provenance::stored_provenance(&row)?,
            id: submission_id.clone(),
            run_id,
            content,
            references: replacement.spec.references.clone(),
            attachments: replacement.spec.attachments.clone(),
            placement: SubmissionPlacement::Queued,
            created_at_ms: from_database_timestamp(&row, "created_at_ms")?,
            updated_at_ms,
        };
        submission.validate()?;
        Ok(submission)
    }

    pub async fn remove_queued_session_submission(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        now_ms: u64,
    ) -> Result<SessionSubmission, HarnessError> {
        let user_id = actor_id;
        validate_owner_scope(tenant_id, user_id, session_id)?;
        submission_id.validate()?;
        let now = to_i64(now_ms, "cloud submission removal timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Stop,
        )
        .await?;
        let user_id = &owner_id;
        set_scope(&mut transaction, tenant_id, user_id).await?;
        crate::store::lock_session_in(&mut transaction, tenant_id, session_id).await?;
        let row = select_submission_for_update(
            &mut transaction,
            tenant_id,
            user_id,
            session_id,
            submission_id,
        )
        .await?;
        let submission = decode_submission(&row)?;
        if submission.placement != SubmissionPlacement::Queued {
            return Err(HarnessError::invalid(
                "only queued submissions can be removed",
            ));
        }
        let reservation_id: String = sqlx::query_scalar(ternilo_storage::for_update(
            &transaction,
            "SELECT quota_reservation_id FROM cloud_runs
             WHERE tenant_id = $1 AND run_id = $2 AND state = 'queued'",
            "SELECT quota_reservation_id FROM cloud_runs
             WHERE tenant_id = $1 AND run_id = $2 AND state = 'queued' FOR UPDATE",
        ))
        .bind(tenant_id.as_str())
        .bind(submission.run_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("queued submission run is unavailable"))?;
        sqlx::query("DELETE FROM cloud_runs WHERE tenant_id = $1 AND run_id = $2")
            .bind(tenant_id.as_str())
            .bind(submission.run_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        sqlx::query(
            "UPDATE control_quota_reservations SET state = 'released'
             WHERE tenant_id = $1 AND reservation_id = $2 AND state = 'active'",
        )
        .bind(tenant_id.as_str())
        .bind(reservation_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE cloud_session_inboxes SET updated_at_ms = $4
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
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
            ternilo_control::ResourceAction::Stop,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(submission)
    }

    pub async fn strict_steering_candidate(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        let user_id = actor_id;
        validate_owner_scope(tenant_id, user_id, session_id)?;
        submission_id.validate()?;
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
        set_scope(&mut transaction, tenant_id, user_id).await?;
        let row = sqlx::query(
            "SELECT submission_id, run_id, input_provenance, content, submission_references, attachments, placement,
                    created_at_ms, updated_at_ms
             FROM cloud_session_submissions
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND submission_id = $4",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(unknown_submission)?;
        let submission = decode_submission(&row)?;
        if submission.placement != SubmissionPlacement::Queued {
            return Err(HarnessError::invalid(
                "strict steering requires a queued submission",
            ));
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(submission)
    }

    pub async fn pause_session_inbox(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        error: Option<&str>,
        now_ms: u64,
    ) -> Result<SessionInboxSnapshot, HarnessError> {
        let user_id = actor_id;
        validate_owner_scope(tenant_id, user_id, session_id)?;
        let now = to_i64(now_ms, "cloud inbox pause timestamp")?;
        let mut transaction = self.begin().await?;
        let owner_id = crate::sharing::session_owner_in(
            &mut transaction,
            tenant_id,
            actor_id,
            session_id,
            ternilo_control::ResourceAction::Stop,
        )
        .await?;
        let user_id = &owner_id;
        set_scope(&mut transaction, tenant_id, user_id).await?;
        require_owned_session(&mut transaction, tenant_id, user_id, session_id).await?;
        sqlx::query(
            "INSERT INTO cloud_session_inboxes
                (tenant_id, user_id, session_id, paused, error, next_position, updated_at_ms)
             VALUES ($1, $2, $3, 1, $4, 0, $5)
             ON CONFLICT (tenant_id, user_id, session_id) DO UPDATE
             SET paused = 1, error = EXCLUDED.error, updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(error)
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
            ternilo_control::ResourceAction::Stop,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        self.session_inbox(tenant_id, actor_id, session_id).await
    }
}

fn validate_enqueue(
    compiled: &CompiledRun,
    quota_reservation_id: &str,
    request: &SessionSubmissionRequest,
) -> Result<(), HarnessError> {
    compiled.spec.validate_shape()?;
    request.validate()?;
    require_identifier(quota_reservation_id, "quota reservation id")?;
    if request
        .run_id
        .as_ref()
        .is_some_and(|run_id| run_id != &compiled.spec.metadata.run_id)
        || request.attachments != compiled.spec.attachments
        || (request.content.skill_name().is_none()
            && request.content.input() != compiled.spec.input)
    {
        return Err(HarnessError::invalid(
            "submission request does not match its compiled cloud run",
        ));
    }
    Ok(())
}

async fn require_session_identity(
    transaction: &mut ternilo_storage::Transaction,
    compiled: &CompiledRun,
) -> Result<(), HarnessError> {
    let spec = &compiled.spec;
    let session = sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT user_id, project_id, workspace_id, agent_id
         FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2",
        "SELECT user_id, project_id, workspace_id, agent_id
         FROM cloud_sessions WHERE tenant_id = $1 AND session_id = $2 FOR UPDATE",
    ))
    .bind(spec.metadata.tenant_id.as_str())
    .bind(spec.metadata.session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    let session_user: String = session.try_get("user_id").map_err(database_error)?;
    let session_project: String = session.try_get("project_id").map_err(database_error)?;
    let session_workspace: String = session.try_get("workspace_id").map_err(database_error)?;
    let session_agent: String = session.try_get("agent_id").map_err(database_error)?;
    if session_user != spec.metadata.user_id.as_str()
        || Some(session_project.as_str()) != spec.metadata.project_id.as_deref()
        || session_workspace != spec.metadata.workspace_id.as_str()
        || session_agent != spec.metadata.agent_id.as_str()
    {
        return Err(HarnessError::policy(
            "cloud session identity cannot change between submissions",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_replacement(
    replacement: &CompiledRun,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
    run_id: &RunId,
    project_id: &str,
    workspace_id: &str,
    agent_id: &str,
    priority: i32,
    max_attempts: i32,
    reserved_tokens: i64,
) -> Result<(), HarnessError> {
    let expected_attempts = i32::try_from(replacement.max_attempts)
        .map_err(|_| HarnessError::invalid("cloud max attempts exceeds PostgreSQL integer"))?;
    if replacement.spec.metadata.tenant_id != *tenant_id
        || replacement.spec.metadata.user_id != *user_id
        || replacement.spec.metadata.session_id != *session_id
        || replacement.spec.metadata.run_id != *run_id
        || replacement.spec.metadata.project_id.as_deref() != Some(project_id)
        || replacement.spec.metadata.workspace_id.as_str() != workspace_id
        || replacement.spec.metadata.agent_id.as_str() != agent_id
        || replacement.priority != priority
        || expected_attempts != max_attempts
        || to_i64(replacement.reserved_model_tokens, "reserved model tokens")? != reserved_tokens
    {
        return Err(HarnessError::invalid(
            "replacement cloud run must preserve queued submission identity and reservation",
        ));
    }
    Ok(())
}

async fn require_owned_session(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT CAST(EXISTS(
            SELECT 1 FROM cloud_sessions
            WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
         ) AS INTEGER)",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?
        != 0;
    if exists {
        Ok(())
    } else {
        Err(HarnessError::invalid("cloud session does not exist"))
    }
}

async fn submission_rows(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
) -> Result<Vec<sqlx::any::AnyRow>, HarnessError> {
    sqlx::query(
        "SELECT submission_id, run_id, input_provenance, content, submission_references, attachments,
                CASE WHEN batch_run_id IS NOT NULL THEN 'running' ELSE placement END AS placement,
                created_at_ms, updated_at_ms
         FROM cloud_session_submissions
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
         ORDER BY fifo_position",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)
}

pub(crate) async fn promote_head(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
    now: i64,
) -> Result<Option<RunId>, HarnessError> {
    let run_id = sqlx::query_scalar::<_, String>(
        "UPDATE cloud_session_submissions AS submission
         SET placement = 'running', updated_at_ms = CASE WHEN updated_at_ms >= $4 THEN updated_at_ms + 1 ELSE $4 END
         WHERE (submission.tenant_id, submission.user_id, submission.session_id,
                submission.submission_id) = (
             SELECT queued.tenant_id, queued.user_id, queued.session_id,
                    queued.submission_id
             FROM cloud_session_submissions AS queued
             JOIN cloud_session_inboxes AS inbox
               ON inbox.tenant_id = queued.tenant_id
              AND inbox.user_id = queued.user_id
              AND inbox.session_id = queued.session_id
             WHERE queued.tenant_id = $1 AND queued.user_id = $2
               AND queued.session_id = $3 AND queued.placement = 'queued'
               AND inbox.paused = 0
               AND NOT EXISTS (
                   SELECT 1 FROM cloud_session_submissions AS active
                   WHERE active.tenant_id = queued.tenant_id
                     AND active.user_id = queued.user_id
                     AND active.session_id = queued.session_id
                     AND active.placement = 'running'
               )
             ORDER BY queued.fifo_position
             LIMIT 1
         )
         RETURNING run_id",
    )
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .bind(now)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let run_id = run_id.map(RunId::new);
    if let Some(run_id) = &run_id {
        batches::claim_batch_in(transaction, tenant_id, user_id, session_id, run_id, now).await?;
    }
    Ok(run_id)
}

async fn select_submission_for_update(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
    submission_id: &SubmissionId,
) -> Result<sqlx::any::AnyRow, HarnessError> {
    sqlx::query(ternilo_storage::for_update(
        transaction,
        "SELECT submission_id, run_id, input_provenance, content, submission_references, attachments, placement,
                created_at_ms, updated_at_ms
         FROM cloud_session_submissions
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
           AND submission_id = $4",
        "SELECT submission_id, run_id, input_provenance, content, submission_references, attachments, placement,
                created_at_ms, updated_at_ms
         FROM cloud_session_submissions
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
           AND submission_id = $4
         FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(user_id.as_str())
    .bind(session_id.as_str())
    .bind(submission_id.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(unknown_submission)
}

pub(crate) fn decode_submission(
    row: &sqlx::any::AnyRow,
) -> Result<SessionSubmission, HarnessError> {
    let placement = match row.get::<String, _>("placement").as_str() {
        "queued" => SubmissionPlacement::Queued,
        "steering" => SubmissionPlacement::Steering,
        "running" => SubmissionPlacement::Running,
        value => {
            return Err(HarnessError::execution(format!(
                "database contains unknown submission placement {value:?}",
            )));
        }
    };
    let submission = SessionSubmission {
        provenance: crate::input_provenance::stored_provenance(row)?,
        id: SubmissionId::new(row.get::<String, _>("submission_id")),
        run_id: RunId::new(row.get::<String, _>("run_id")),
        content: row
            .get::<ternilo_storage::Json<SubmissionContent>, _>("content")
            .0,
        references: row
            .get::<ternilo_storage::Json<Vec<ternilo_protocol::SubmissionReference>>, _>(
                "submission_references",
            )
            .0,
        attachments: row
            .get::<ternilo_storage::Json<Vec<ternilo_protocol::Attachment>>, _>("attachments")
            .0,
        placement,
        created_at_ms: from_database_timestamp(row, "created_at_ms")?,
        updated_at_ms: from_database_timestamp(row, "updated_at_ms")?,
    };
    submission.validate()?;
    Ok(submission)
}

fn from_database_timestamp(row: &sqlx::any::AnyRow, column: &str) -> Result<u64, HarnessError> {
    let value = row.try_get::<i64, _>(column).map_err(database_error)?;
    u64::try_from(value).map_err(|_| HarnessError::execution("negative cloud inbox timestamp"))
}

async fn set_scope(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    user_id: &UserId,
) -> Result<(), HarnessError> {
    ternilo_storage::set_tenant_scope(transaction, tenant_id).await?;
    ternilo_storage::set_user_scope(transaction, user_id).await?;
    Ok(())
}

fn validate_owner_scope(
    tenant_id: &TenantId,
    user_id: &UserId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    tenant_id.validate()?;
    user_id.validate()?;
    session_id.validate()
}

fn require_identifier(value: &str, label: &str) -> Result<(), HarnessError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        Err(HarnessError::invalid(format!(
            "{label} must contain 1 to 128 bytes without whitespace or control characters",
        )))
    } else {
        Ok(())
    }
}

fn random_submission_id() -> SubmissionId {
    SubmissionId::new(format!(
        "sub_{}",
        URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
    ))
}

const fn delivery_str(delivery: SubmissionDelivery) -> &'static str {
    match delivery {
        SubmissionDelivery::Queue => "queue",
        SubmissionDelivery::Steer => "steer",
    }
}

const fn placement_str(placement: SubmissionPlacement) -> &'static str {
    match placement {
        SubmissionPlacement::Queued => "queued",
        SubmissionPlacement::Steering => "steering",
        SubmissionPlacement::Running => "running",
    }
}

fn unknown_submission() -> HarnessError {
    HarnessError::invalid("cloud submission does not exist")
}
