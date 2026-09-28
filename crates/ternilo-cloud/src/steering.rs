use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use sha2::{Digest, Sha256};
use sqlx::Row;
use ternilo_protocol::{
    HarnessError, RunId, SessionId, SessionSubmission, SubmissionId, SubmissionPlacement, TenantId,
    UserId,
};
use ternilo_storage::{Json, Transaction, for_update};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, ExecutorCapability, ExecutorCommand,
    ExecutorCommandBody, ExecutorScope,
};

use crate::{
    ClaimedCloudSessionCommand, CloudStore, CloudWorkerIdentity, StartedRun, commands, inbox, store,
};

const STEERING_COMMAND_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
pub struct CloudSteeringTicket {
    pub submission: SessionSubmission,
    pub command_id: Option<CommandId>,
}

impl CloudStore {
    /// Reserves one queued submission for delivery to the Worker that currently
    /// owns the session run. The submission remains queued until the child
    /// explicitly ACKs the steering window.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep authorization, canonical state and audit changes in one atomic operation."
    )]
    pub async fn begin_session_steering(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        now_ms: u64,
    ) -> Result<CloudSteeringTicket, HarnessError> {
        let user_id = actor_id;
        tenant_id.validate()?;
        user_id.validate()?;
        session_id.validate()?;
        submission_id.validate()?;
        let expires_at_ms = now_ms
            .checked_add(
                u64::try_from(STEERING_COMMAND_TTL.as_millis())
                    .expect("steering command TTL fits u64"),
            )
            .ok_or_else(|| HarnessError::invalid("steering command expiry exceeds u64"))?;
        let now = store::to_i64(now_ms, "steering command timestamp")?;

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
        commands::set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let query = for_update(
            &transaction,
            "SELECT 1 FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
            "SELECT 1 FROM cloud_sessions WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 FOR UPDATE",
        );
        let owned = sqlx::query_scalar::<_, i32>(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(session_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store::database_error)?;
        if owned.is_none() {
            return Err(HarnessError::invalid(
                "cloud Session does not exist for this owner",
            ));
        }
        let query = for_update(
            &transaction,
            "SELECT submission_id, run_id, input_provenance, actor_user_id, content, submission_references, attachments,
                    placement, created_at_ms, updated_at_ms, steering_command_id
             FROM cloud_session_submissions
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND submission_id = $4",
            "SELECT submission_id, run_id, input_provenance, actor_user_id, content, submission_references, attachments,
                    placement, created_at_ms, updated_at_ms, steering_command_id
             FROM cloud_session_submissions
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND submission_id = $4 FOR UPDATE",
        );
        let row = sqlx::query(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(session_id.as_str())
            .bind(submission_id.as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store::database_error)?
            .ok_or_else(|| HarnessError::invalid("queued submission does not exist"))?;
        let submission = inbox::decode_submission(&row)?;
        let existing_command = row
            .try_get::<Option<String>, _>("steering_command_id")
            .map_err(store::database_error)?
            .map(CommandId::new);
        if submission.placement == SubmissionPlacement::Steering || existing_command.is_some() {
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
            transaction.commit().await.map_err(store::database_error)?;
            return Ok(CloudSteeringTicket {
                submission,
                command_id: existing_command,
            });
        }
        if submission.placement != SubmissionPlacement::Queued {
            return Err(HarnessError::invalid(
                "only a queued submission can enter the active steering window",
            ));
        }

        let query = for_update(
            &transaction,
            "SELECT run.run_id, run.session_fencing_token
             FROM cloud_runs AS run
             JOIN cloud_session_submissions AS active_submission
               ON active_submission.tenant_id = run.tenant_id
              AND active_submission.user_id = run.user_id
              AND active_submission.session_id = run.session_id
              AND active_submission.run_id = run.run_id
              AND active_submission.placement = 'running'
             JOIN cloud_session_writer_leases AS writer
               ON writer.tenant_id = run.tenant_id
              AND writer.session_id = run.session_id
              AND writer.run_id = run.run_id
              AND writer.fencing_token = run.session_fencing_token
             WHERE run.tenant_id = $1 AND run.user_id = $2 AND run.session_id = $3
               AND run.state IN ('running', 'cancel_requested')
               AND run.lease_owner IS NOT NULL
               AND writer.lease_owner = run.lease_owner
               AND writer.expires_at_ms > $4
",
            "SELECT run.run_id, run.session_fencing_token
             FROM cloud_runs AS run
             JOIN cloud_session_submissions AS active_submission
               ON active_submission.tenant_id = run.tenant_id
              AND active_submission.user_id = run.user_id
              AND active_submission.session_id = run.session_id
              AND active_submission.run_id = run.run_id
              AND active_submission.placement = 'running'
             JOIN cloud_session_writer_leases AS writer
               ON writer.tenant_id = run.tenant_id
              AND writer.session_id = run.session_id
              AND writer.run_id = run.run_id
              AND writer.fencing_token = run.session_fencing_token
             WHERE run.tenant_id = $1 AND run.user_id = $2 AND run.session_id = $3
               AND run.state IN ('running', 'cancel_requested')
               AND run.lease_owner IS NOT NULL
               AND writer.lease_owner = run.lease_owner
               AND writer.expires_at_ms > $4 FOR UPDATE OF run, writer",
        );
        let active = sqlx::query(query)
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(session_id.as_str())
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store::database_error)?;
        let Some(active) = active else {
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
            transaction.commit().await.map_err(store::database_error)?;
            return Ok(CloudSteeringTicket {
                submission,
                command_id: None,
            });
        };
        let target_run_id = RunId::new(
            active
                .try_get::<String, _>("run_id")
                .map_err(store::database_error)?,
        );
        if !compatible_steering_budget_in(
            &mut transaction,
            tenant_id,
            &submission.run_id,
            &target_run_id,
        )
        .await?
        {
            transaction.commit().await.map_err(store::database_error)?;
            return Ok(CloudSteeringTicket {
                submission,
                command_id: None,
            });
        }
        let target_fencing_token = store::from_i64(
            active
                .try_get::<i64, _>("session_fencing_token")
                .map_err(store::database_error)?,
            "steering target writer fencing token",
        )?;
        let command_id = random_command_id("steer");
        let command = ExecutorCommand {
            input_provenance: None,
            command_id: command_id.clone(),
            scope: ExecutorScope {
                tenant_id: tenant_id.clone(),
                user_id: user_id.clone(),
            },
            issued_at_ms: now_ms,
            expires_at_ms,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionQueueSteer {
                    session_id: session_id.clone(),
                    submission_id: submission_id.clone(),
                },
            },
        };
        command.validate(now_ms)?;
        let command_value = commands::canonical_command(&command)?;
        let command_digest =
            Sha256::digest(serde_json::to_vec(&command_value).map_err(|error| {
                HarnessError::execution(format!("encode steering command: {error}"))
            })?);
        let command_seq = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(command_seq), -1) + 1
             FROM cloud_session_commands
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        sqlx::query(
            "INSERT INTO cloud_session_commands (
                tenant_id, user_id, session_id, command_id, command_seq,
                command_json, command_digest, required_capability,
                required_catalog_revision, read_only, target_run_id,
                target_writer_fencing_token, state, issued_at_ms, expires_at_ms,
                created_at_ms, updated_at_ms, actor_user_id, contributor_user_id
             ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, 0, $10, $11,
                'pending', $12, $13, $12, $12, $14, $15
             )",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(command_id.as_str())
        .bind(command_seq)
        .bind(Json(&command))
        .bind(command_digest.as_slice())
        .bind(commands::capability_name(
            ExecutorCapability::SessionSteering,
        )?)
        .bind(crate::CLOUD_CATALOG_REVISION)
        .bind(target_run_id.as_str())
        .bind(store::to_i64(
            target_fencing_token,
            "steering target writer fencing token",
        )?)
        .bind(now)
        .bind(store::to_i64(
            expires_at_ms,
            "steering command expiry timestamp",
        )?)
        .bind(actor_id.as_str())
        .bind(
            row.try_get::<String, _>("actor_user_id")
                .map_err(store::database_error)?,
        )
        .execute(&mut *transaction)
        .await
        .map_err(store::database_error)?;
        sqlx::query(
            "UPDATE cloud_session_submissions
             SET steering_command_id = $5,
                 steering_target_run_id = $6,
                 steering_target_writer_fencing_token = $7,
                 updated_at_ms = CASE WHEN updated_at_ms >= $8 THEN updated_at_ms + 1 ELSE $8 END
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND submission_id = $4 AND placement = 'queued'",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(submission_id.as_str())
        .bind(command_id.as_str())
        .bind(target_run_id.as_str())
        .bind(store::to_i64(
            target_fencing_token,
            "steering target writer fencing token",
        )?)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(store::database_error)?;
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
        transaction.commit().await.map_err(store::database_error)?;
        Ok(CloudSteeringTicket {
            submission,
            command_id: Some(command_id),
        })
    }

    /// Stops an unclaimed steering request after the HTTP wait window. An
    /// inflight command is left to its Worker because its child ACK may already
    /// be on the wire.
    pub async fn expire_pending_session_steering(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        session_id: &SessionId,
        submission_id: &SubmissionId,
        command_id: &CommandId,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let now = store::to_i64(now_ms, "steering timeout timestamp")?;
        let reply = CommandReply::failure(
            command_id.clone(),
            now_ms,
            HarnessError::execution("steering timed out before a Worker claimed the command"),
        );
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
        commands::set_owner_scope(&mut transaction, tenant_id, user_id).await?;
        let expired = sqlx::query(
            "UPDATE cloud_session_commands
             SET state = 'expired', reply_json = $6, completed_at_ms = $7,
                 updated_at_ms = $7
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND command_id = $4 AND state = 'pending'
               AND EXISTS (
                   SELECT 1 FROM cloud_session_submissions AS submission
                   WHERE submission.tenant_id = $1 AND submission.user_id = $2
                     AND submission.session_id = $3 AND submission.submission_id = $5
                     AND submission.steering_command_id = $4
               )",
        )
        .bind(tenant_id.as_str())
        .bind(user_id.as_str())
        .bind(session_id.as_str())
        .bind(command_id.as_str())
        .bind(submission_id.as_str())
        .bind(Json(reply))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(store::database_error)?
        .rows_affected();
        if expired == 1 {
            sqlx::query(
                "UPDATE cloud_session_submissions
                 SET steering_command_id = NULL, steering_target_run_id = NULL,
                     steering_target_writer_fencing_token = NULL,
                     placement = 'queued', updated_at_ms = CASE WHEN updated_at_ms >= $6 THEN updated_at_ms + 1 ELSE $6 END
                 WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
                   AND submission_id = $4 AND steering_command_id = $5",
            )
            .bind(tenant_id.as_str())
            .bind(user_id.as_str())
            .bind(session_id.as_str())
            .bind(submission_id.as_str())
            .bind(command_id.as_str())
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(store::database_error)?;
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
        transaction.commit().await.map_err(store::database_error)
    }

    pub async fn steering_submission_for_worker(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        now_ms: u64,
    ) -> Result<Option<SessionSubmission>, HarnessError> {
        let mut transaction = self.begin().await?;
        if commands::worker_in(&mut transaction, identity, now_ms, true)
            .await?
            .is_none()
        {
            return Ok(None);
        }
        let Some(row) = commands::command_in(
            &mut transaction,
            &command.tenant_id,
            &command.user_id,
            &command.command.command_id,
        )
        .await?
        else {
            return Ok(None);
        };
        if !commands::command_owned(&row, identity, now_ms)?
            || row
                .try_get::<String, _>("required_capability")
                .map_err(store::database_error)?
                != "session_steering"
            || !commands::target_is_current(&mut transaction, &row, identity, now_ms).await?
        {
            return Ok(None);
        }
        let submission = sqlx::query("SELECT submission.* FROM cloud_session_submissions AS submission
            JOIN cloud_session_commands AS command ON command.tenant_id = submission.tenant_id AND command.user_id = submission.user_id AND command.session_id = submission.session_id
            WHERE command.tenant_id = $1 AND command.command_id = $2 AND submission.steering_command_id = command.command_id
                AND submission.steering_target_run_id = command.target_run_id
                AND submission.steering_target_writer_fencing_token = command.target_writer_fencing_token AND submission.placement = 'queued'")
            .bind(command.tenant_id.as_str()).bind(command.command.command_id.as_str())
            .fetch_optional(&mut *transaction).await.map_err(store::database_error)?;
        let submission = if let Some(row) = submission.as_ref() {
            let candidate = inbox::decode_submission(row)?;
            let target = RunId::new(
                row.try_get::<String, _>("steering_target_run_id")
                    .map_err(store::database_error)?,
            );
            if compatible_steering_budget_in(
                &mut transaction,
                &command.tenant_id,
                &candidate.run_id,
                &target,
            )
            .await?
            {
                Some(candidate)
            } else {
                None
            }
        } else {
            None
        };
        transaction.commit().await.map_err(store::database_error)?;
        Ok(submission)
    }

    pub async fn complete_steering_command(
        &self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        accepted: bool,
        now_ms: u64,
    ) -> Result<CommandReply, HarnessError> {
        let reply = CommandReply::success(
            command.command.command_id.clone(),
            now_ms,
            serde_json::json!({ "accepted": accepted }),
        );
        let mut transaction = self.begin().await?;
        commands::require_worker_in(&mut transaction, identity, now_ms).await?;
        let row = commands::required_command_in(&mut transaction, command).await?;
        if commands::completed_reply_matches(&row, &reply)? {
            return Ok(reply);
        }
        if !commands::command_owned(&row, identity, now_ms)?
            || row
                .try_get::<String, _>("required_capability")
                .map_err(store::database_error)?
                != "session_steering"
            || !commands::target_is_current(&mut transaction, &row, identity, now_ms).await?
        {
            return Err(HarnessError::policy(
                "steering command lost its Worker run or writer fence",
            ));
        }
        let changed = sqlx::query("UPDATE cloud_session_submissions SET placement = $3,
                steering_command_id = CASE WHEN $4 = 1 THEN steering_command_id ELSE NULL END,
                steering_target_run_id = CASE WHEN $4 = 1 THEN steering_target_run_id ELSE NULL END,
                steering_target_writer_fencing_token = CASE WHEN $4 = 1 THEN steering_target_writer_fencing_token ELSE NULL END,
                updated_at_ms = CASE WHEN updated_at_ms >= $5 THEN updated_at_ms + 1 ELSE $5 END
             WHERE tenant_id = $1 AND steering_command_id = $2 AND placement = 'queued'
                AND steering_target_run_id = $6 AND steering_target_writer_fencing_token = $7")
            .bind(command.tenant_id.as_str()).bind(command.command.command_id.as_str())
            .bind(if accepted { "steering" } else { "queued" }).bind(i64::from(accepted))
            .bind(store::to_i64(now_ms, "steering completion time")?)
            .bind(row.try_get::<Option<String>, _>("target_run_id").map_err(store::database_error)?)
            .bind(row.try_get::<Option<i64>, _>("target_writer_fencing_token").map_err(store::database_error)?)
            .execute(&mut *transaction).await.map_err(store::database_error)?.rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy(
                "steering submission no longer matches the claimed command",
            ));
        }
        commands::finish_command_in(&mut transaction, &row, "completed", Some(&reply), now_ms)
            .await?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(reply)
    }

    pub async fn requeue_steering_for_run(
        &self,
        identity: &CloudWorkerIdentity,
        run: &StartedRun,
        now_ms: u64,
    ) -> Result<u32, HarnessError> {
        let mut transaction = self.begin().await?;
        if commands::worker_in(&mut transaction, identity, now_ms, true)
            .await?
            .is_none()
        {
            return Ok(0);
        }
        commands::set_owner_scope(
            &mut transaction,
            &run.claim.tenant_id,
            &run.claim.spec.metadata.user_id,
        )
        .await?;
        let valid: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_runs AS run JOIN cloud_session_writer_leases AS writer
            ON writer.tenant_id = run.tenant_id AND writer.session_id = run.session_id AND writer.run_id = run.run_id
            WHERE run.tenant_id = $1 AND run.run_id = $2 AND run.state IN ('running', 'cancel_requested')
                AND run.lease_owner = $3 AND run.lease_token = $4 AND run.lease_expires_at_ms > $6
                AND run.session_fencing_token = $5 AND writer.lease_owner = $3 AND writer.fencing_token = $5 AND writer.expires_at_ms > $6")
            .bind(run.claim.tenant_id.as_str()).bind(run.claim.run_id.as_str()).bind(identity.worker_id.as_str())
            .bind(store::to_i64(run.claim.lease_token, "run lease token")?).bind(store::to_i64(run.fencing_token, "writer fence")?)
            .bind(store::to_i64(now_ms, "steering requeue time")?).fetch_one(&mut *transaction).await.map_err(store::database_error)?;
        if valid != 1 {
            return Ok(0);
        }
        let changed = requeue_steering_in(
            &mut transaction,
            &run.claim.tenant_id,
            &run.claim.run_id,
            now_ms,
        )
        .await?;
        transaction.commit().await.map_err(store::database_error)?;
        Ok(changed)
    }
}

/// The caller holds the run fence and owner scope in the same transaction.
pub(crate) async fn requeue_steering_in(
    transaction: &mut Transaction,
    tenant_id: &TenantId,
    run_id: &RunId,
    now_ms: u64,
) -> Result<u32, HarnessError> {
    let changed = sqlx::query("UPDATE cloud_session_submissions SET placement = 'queued', steering_command_id = NULL, batch_run_id = NULL,
        steering_target_run_id = NULL, steering_target_writer_fencing_token = NULL, updated_at_ms = CASE WHEN updated_at_ms >= $3 THEN updated_at_ms + 1 ELSE $3 END
        WHERE tenant_id = $1 AND (steering_target_run_id = $2 OR batch_run_id = $2) AND placement = 'steering'")
        .bind(tenant_id.as_str()).bind(run_id.as_str()).bind(store::to_i64(now_ms, "steering requeue time")?)
        .execute(&mut **transaction).await.map_err(store::database_error)?.rows_affected();
    u32::try_from(changed)
        .map_err(|_| HarnessError::execution("steering requeue count exceeds u32"))
}

fn random_command_id(prefix: &str) -> CommandId {
    CommandId::new(format!(
        "{prefix}_{}",
        URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
    ))
}

/// A steering receipt can only transfer an unused reservation within the same budget month.
pub(crate) async fn compatible_steering_budget_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    candidate: &RunId,
    target: &RunId,
) -> Result<bool, HarnessError> {
    let row = sqlx::query(
        "SELECT candidate.spec AS candidate_spec, target.spec AS target_spec,
                candidate_budget.period_start AS candidate_month, target_budget.period_start AS target_month
         FROM cloud_runs AS candidate JOIN cloud_runs AS target ON target.tenant_id=candidate.tenant_id
         JOIN control_quota_reservations AS candidate_budget ON candidate_budget.tenant_id=candidate.tenant_id
           AND candidate_budget.reservation_id=candidate.quota_reservation_id
         JOIN control_quota_reservations AS target_budget ON target_budget.tenant_id=target.tenant_id
           AND target_budget.reservation_id=target.quota_reservation_id
         WHERE candidate.tenant_id=$1 AND candidate.run_id=$2 AND target.run_id=$3
           AND candidate.user_id=target.user_id AND candidate.state='queued'
           AND candidate_budget.state='active' AND target_budget.state='active'"
    ).bind(tenant.as_str()).bind(candidate.as_str()).bind(target.as_str())
        .fetch_optional(&mut **tx).await.map_err(store::database_error)?;
    let Some(row) = row else {
        return Ok(false);
    };
    let candidate = row
        .try_get::<Json<ternilo_protocol::RunSpec>, _>("candidate_spec")
        .map_err(store::database_error)?
        .0;
    let target = row
        .try_get::<Json<ternilo_protocol::RunSpec>, _>("target_spec")
        .map_err(store::database_error)?
        .0;
    Ok(row
        .try_get::<String, _>("candidate_month")
        .map_err(store::database_error)?
        == row
            .try_get::<String, _>("target_month")
            .map_err(store::database_error)?
        && crate::profile_model_snapshot(&candidate.profile)?
            == crate::profile_model_snapshot(&target.profile)?)
}
