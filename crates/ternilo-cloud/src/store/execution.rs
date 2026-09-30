use super::{
    CloudRunClaim, CloudStore, Duration, HarnessError, Row, RunId, RunOutcome, SessionEvent,
    SessionEventKind, SessionId, StartedRun, TenantId, TerminalState, UserAnswer, UserId,
    UserQuestion, database_error, decode_claim, duration_ms, from_i64, set_tenant, to_i64,
    validate_claim, validate_lease, validate_started_run,
};
use sqlx::any::AnyRow;
use ternilo_storage::{Backend, Json, Transaction, for_update, set_user_scope};

impl CloudStore {
    pub async fn claim_run(
        &self,
        worker_id: &str,
        lease_ttl: Duration,
        now_ms: u64,
    ) -> Result<Option<CloudRunClaim>, HarnessError> {
        validate_lease(worker_id, lease_ttl)?;
        let now = to_i64(now_ms, "cloud claim timestamp")?;
        let expiry = to_i64(
            now_ms.saturating_add(duration_ms(lease_ttl)?),
            "cloud claim expiry",
        )?;
        self.resolve_execution_pressure(worker_id, now_ms).await?;
        let mut discovery = self.admission_transaction().await?;
        let candidates = run_scopes(&mut discovery, "claim", worker_id, now).await?;
        discovery.commit().await.map_err(database_error)?;
        for candidate in candidates {
            let tenant = TenantId::new(
                candidate
                    .try_get::<String, _>("tenant_id")
                    .map_err(database_error)?,
            );
            let user = UserId::new(
                candidate
                    .try_get::<String, _>("user_id")
                    .map_err(database_error)?,
            );
            let run_id: String = candidate.try_get("run_id").map_err(database_error)?;
            let session = SessionId::new(
                candidate
                    .try_get::<String, _>("session_id")
                    .map_err(database_error)?,
            );
            let mut tx = self.admission_transaction().await?;
            let storage_id = crate::execution_admission::worker_pool(&mut tx, worker_id).await?;
            crate::execution_admission::pool_gate(&mut tx, &storage_id).await?;
            crate::execution_admission::reconcile_pool_in(&mut tx, &storage_id, now_ms).await?;
            let gate = if self.database.backend() == Backend::Postgres {
                "SELECT ternilo_cloud_claim_gate()"
            } else {
                "SELECT 1-claims_paused FROM cloud_runtime_control WHERE singleton=1"
            };
            let allowed: i64 = sqlx::query_scalar(gate)
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
            if allowed == 0 {
                tx.commit().await.map_err(database_error)?;
                return Ok(None);
            }
            set_tenant(&mut tx, &tenant).await?;
            set_user_scope(&mut tx, &user).await?;
            lock_session_in(&mut tx, &tenant, &session).await?;
            let row = claimable_run_in(&mut tx, &tenant, &run_id, now).await?;
            let Some(row) = row else {
                tx.commit().await.map_err(database_error)?;
                continue;
            };
            if !Self::ensure_tenant_storage_in(&mut tx, &tenant, &storage_id, now_ms).await?
                || !crate::execution_admission::claim_in(
                    &mut tx,
                    &row,
                    worker_id,
                    &storage_id,
                    now_ms,
                )
                .await?
            {
                tx.rollback().await.map_err(database_error)?;
                continue;
            }
            let row = sqlx::query(
                "UPDATE cloud_runs SET state = 'leased', queue_wait_reason=NULL, attempt = attempt + 1, lease_owner = $3,
                lease_token = lease_token + 1, lease_expires_at_ms = $4, updated_at_ms = $5
                WHERE tenant_id = $1 AND run_id = $2 RETURNING tenant_id, run_id, session_id,
                lease_token, spec, spec_digest, actor_user_id, authorization_session_id, input_provenance",
            ).bind(tenant.as_str()).bind(&run_id).bind(worker_id).bind(expiry).bind(now)
                .fetch_one(&mut *tx).await.map_err(database_error)?;
            let workspace_use = crate::workspace_occupancy::ticket_for_run_in(
                &mut tx,
                &crate::RunLease {
                    tenant_id: tenant.clone(),
                    run_id: RunId::new(run_id.clone()),
                    lease_token: from_i64(
                        row.try_get("lease_token").map_err(database_error)?,
                        "cloud claim token",
                    )?,
                    writer_fencing_token: 0,
                },
            )
            .await?
            .ok_or_else(|| HarnessError::execution("cloud claim has no workspace occupancy"))?;
            let claim = decode_claim(&row, workspace_use)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(Some(claim));
        }
        Ok(None)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep actor admission, canonical leases and writer fencing in one atomic start transaction."
    )]
    pub async fn start_run(
        &self,
        claim: CloudRunClaim,
        worker_id: &str,
        lease_ttl: Duration,
        now_ms: u64,
    ) -> Result<Option<StartedRun>, HarnessError> {
        validate_claim(&claim, worker_id, lease_ttl)?;
        let now = to_i64(now_ms, "cloud start timestamp")?;
        let expiry = to_i64(
            now_ms.saturating_add(duration_ms(lease_ttl)?),
            "cloud writer lease expiry",
        )?;
        let mut tx = self.admission_transaction().await?;
        let storage_id = crate::execution_admission::worker_pool(&mut tx, worker_id).await?;
        crate::execution_admission::pool_gate(&mut tx, &storage_id).await?;
        set_tenant(&mut tx, &claim.tenant_id).await?;
        lock_session_in(&mut tx, &claim.tenant_id, &claim.session_id).await?;
        let row = sqlx::query(for_update(
            &tx,
            "SELECT user_id, actor_user_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND session_id=$3
            AND state='leased' AND lease_owner=$4 AND lease_token=$5 AND lease_expires_at_ms>$6",
            "SELECT user_id, actor_user_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND session_id=$3
            AND state='leased' AND lease_owner=$4 AND lease_token=$5 AND lease_expires_at_ms>$6
            FOR UPDATE",
        ))
        .bind(claim.tenant_id.as_str())
        .bind(claim.run_id.as_str())
        .bind(claim.session_id.as_str())
        .bind(worker_id)
        .bind(to_i64(claim.lease_token, "cloud claim token")?)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let actor_id = UserId::new(
            row.try_get::<String, _>("actor_user_id")
                .map_err(database_error)?,
        );
        if let Err(error) =
            crate::account_cleanup::require_active_actor_in(&mut tx, &actor_id).await
        {
            if matches!(
                error.code,
                ternilo_protocol::ErrorCode::PolicyDenied | ternilo_protocol::ErrorCode::Conflict
            ) {
                return Ok(None);
            }
            return Err(error);
        }
        set_user_scope(
            &mut tx,
            &UserId::new(
                row.try_get::<String, _>("user_id")
                    .map_err(database_error)?,
            ),
        )
        .await?;
        let fence = sqlx::query_scalar::<_,i64>(
            "INSERT INTO cloud_session_writer_leases (tenant_id, session_id, run_id,
            lease_owner, fencing_token, expires_at_ms, renewed_at_ms) VALUES ($1, $2,
            $3, $4, 1, $5, $6) ON CONFLICT (tenant_id, session_id) DO UPDATE SET run_id=EXCLUDED.run_id,
            lease_owner=EXCLUDED.lease_owner, fencing_token=cloud_session_writer_leases.fencing_token+1,
            expires_at_ms=EXCLUDED.expires_at_ms, renewed_at_ms=EXCLUDED.renewed_at_ms
            WHERE cloud_session_writer_leases.expires_at_ms <= $6 RETURNING fencing_token",
        )
            .bind(claim.tenant_id.as_str())
            .bind(claim.session_id.as_str())
            .bind(claim.run_id.as_str())
            .bind(worker_id)
            .bind(expiry)
            .bind(now)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
        let Some(fence) = fence else {
            return Ok(None);
        };
        sqlx::query(
            "UPDATE cloud_runs SET state='running', session_fencing_token=$3, lease_expires_at_ms=$4,
            started_at_ms=COALESCE(started_at_ms, $5), updated_at_ms=$5 WHERE tenant_id=$1
            AND run_id=$2",
        )
            .bind(claim.tenant_id.as_str())
            .bind(claim.run_id.as_str())
            .bind(fence)
            .bind(expiry)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        sqlx::query(
            "UPDATE cloud_sessions SET state='running', execution=NULL, current_run_id=$3, updated_at_ms=$4
            WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(claim.tenant_id.as_str())
        .bind(claim.session_id.as_str())
        .bind(claim.run_id.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        crate::execution_admission::started_in(&mut tx, &claim, worker_id, fence, now_ms).await?;
        let prior_events = super::recovery::load_repaired_history_in(
            &mut tx,
            &claim.tenant_id,
            &claim.session_id,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(Some(StartedRun {
            claim,
            fencing_token: from_i64(fence, "cloud writer fencing token")?,
            prior_events,
        }))
    }

    pub async fn release_claim(
        &self,
        claim: &CloudRunClaim,
        worker_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.admission_transaction().await?;
        let storage_id = crate::execution_admission::worker_pool(&mut tx, worker_id).await?;
        crate::execution_admission::pool_gate(&mut tx, &storage_id).await?;
        set_tenant(&mut tx, &claim.tenant_id).await?;
        let changed = sqlx::query(
            "UPDATE cloud_runs SET state='queued', lease_owner=NULL, lease_expires_at_ms=NULL,
            available_at_ms=$5, updated_at_ms=$5 WHERE tenant_id=$1 AND run_id=$2 AND
            state='leased' AND lease_owner=$3 AND lease_token=$4",
        )
        .bind(claim.tenant_id.as_str())
        .bind(claim.run_id.as_str())
        .bind(worker_id)
        .bind(to_i64(claim.lease_token, "cloud claim token")?)
        .bind(to_i64(now_ms, "cloud claim release timestamp")?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::policy("cloud claim is no longer owned"));
        }
        crate::execution_admission::released_in(
            &mut tx,
            &claim.tenant_id,
            &claim.run_id,
            claim.lease_token,
            &claim.spec.metadata.user_id,
            now_ms,
        )
        .await?;
        crate::workspace_occupancy::release_unstarted_in(
            &mut tx,
            &crate::RunLease::from(claim),
            worker_id,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn renew_run(
        &self,
        run: &StartedRun,
        worker_id: &str,
        lease_ttl: Duration,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_claim(&run.claim, worker_id, lease_ttl)?;
        let mut tx = self.admission_transaction().await?;
        let storage = crate::execution_admission::worker_pool(&mut tx, worker_id).await?;
        crate::execution_admission::pool_gate(&mut tx, &storage).await?;
        require_writer_in(&mut tx, run, worker_id, Some(now_ms)).await?;
        let now = to_i64(now_ms, "cloud lease renewal timestamp")?;
        let expiry = to_i64(
            now_ms.saturating_add(duration_ms(lease_ttl)?),
            "cloud lease renewal expiry",
        )?;
        sqlx::query(
            "UPDATE cloud_session_writer_leases SET expires_at_ms=$3, renewed_at_ms=$4
            WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.session_id.as_str())
        .bind(expiry)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "UPDATE cloud_runs SET lease_expires_at_ms=$3, updated_at_ms=$4 WHERE tenant_id=$1
            AND run_id=$2",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(expiry)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn cancel_requested(
        &self,
        run: &StartedRun,
        worker_id: &str,
    ) -> Result<bool, HarnessError> {
        let mut tx = self.tenant_transaction(&run.claim.tenant_id).await?;
        let found = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(SELECT 1 FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2
            AND state='cancel_requested' AND lease_owner=$3 AND lease_token=$4) AS
            INTEGER)",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(worker_id)
        .bind(to_i64(run.claim.lease_token, "cloud claim token")?)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(found != 0)
    }

    pub async fn append_event(
        &self,
        run: &StartedRun,
        worker_id: &str,
        event: &SessionEvent,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if event.run_id != run.claim.run_id {
            return Err(HarnessError::policy(
                "worker event run id does not match its claim",
            ));
        }
        let mut tx = self.tenant_transaction(&run.claim.tenant_id).await?;
        let canonical_run = require_writer_in(&mut tx, run, worker_id, Some(now_ms)).await?;
        let tenant = &run.claim.tenant_id;
        let session = &run.claim.session_id;
        let seq = to_i64(event.seq, "cloud event sequence")?;
        let existing = sqlx::query_scalar::<_, Json<SessionEvent>>(
            "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2
            AND seq=$3",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(seq)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if let Some(existing) = existing {
            if existing.0 != *event {
                return Err(HarnessError::policy(
                    "cloud event replay differs from the persisted event",
                ));
            }
            tx.commit().await.map_err(database_error)?;
            return Ok(());
        }
        let last = lock_session_in(&mut tx, tenant, session).await?;
        if last.checked_add(1) != Some(seq) {
            return Err(HarnessError::policy(
                "cloud event sequence does not follow the session cursor",
            ));
        }
        crate::input_provenance::validate_user_message_in(
            &mut tx,
            run,
            &canonical_run,
            &event.kind,
        )
        .await?;
        consume_steering_in(&mut tx, run, event, now_ms).await?;
        sqlx::query(
            "INSERT INTO cloud_session_events(tenant_id, session_id, seq, run_id, event,
            writer_fencing_token, created_at_ms) VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(seq)
        .bind(run.claim.run_id.as_str())
        .bind(Json(event))
        .bind(to_i64(run.fencing_token, "cloud writer fencing token")?)
        .bind(to_i64(now_ms, "cloud event timestamp")?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        let title = match &event.kind {
            SessionEventKind::SessionTitleGenerated { title } => {
                Some(title.trim().chars().take(256).collect::<String>())
                    .filter(|value| !value.is_empty())
            }
            _ => None,
        };
        sqlx::query(
            "UPDATE cloud_sessions SET last_seq=$3, title=CASE WHEN title='New session'
            THEN COALESCE($4, title) ELSE title END, updated_at_ms=$5 WHERE tenant_id=$1
            AND session_id=$2",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(seq)
        .bind(title)
        .bind(to_i64(now_ms, "cloud event timestamp")?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        project_execution_activity_in(&mut tx, tenant, session, event).await?;
        crate::telemetry::capture_event_in(&mut tx, tenant, session, event, now_ms).await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn model_budget(
        &self,
        run: &StartedRun,
        worker_id: &str,
    ) -> Result<(u64, u64), HarnessError> {
        let mut tx = self.tenant_transaction(&run.claim.tenant_id).await?;
        let job = require_writer_in(&mut tx, run, worker_id, None).await?;
        let reservation: String = job
            .try_get("quota_reservation_id")
            .map_err(database_error)?;
        let budget = ternilo_control::ControlStore::workload_model_budget_in(
            &mut tx,
            &run.claim.tenant_id,
            &reservation,
        )
        .await?;
        if budget.state != "active" {
            return Err(HarnessError::policy(
                "cloud model quota reservation is no longer active",
            ));
        }
        tx.commit().await.map_err(database_error)?;
        Ok((budget.reserved_model_tokens, budget.used_model_tokens))
    }

    pub async fn record_question(
        &self,
        run: &StartedRun,
        worker_id: &str,
        question: &UserQuestion,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_started_run(run, worker_id)?;
        let mut tx = self.tenant_transaction(&run.claim.tenant_id).await?;
        let job = require_writer_in(&mut tx, run, worker_id, Some(now_ms)).await?;
        let changed = sqlx::query(
            "INSERT INTO cloud_session_questions(tenant_id, user_id, session_id, run_id,
            question_id, question, created_at_ms) VALUES($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT(tenant_id, session_id, question_id) DO NOTHING",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(
            job.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        )
        .bind(run.claim.session_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(&question.id)
        .bind(Json(question))
        .bind(to_i64(now_ms, "cloud question timestamp")?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?
        .rows_affected();
        if changed == 0 {
            let existing = sqlx::query_scalar::<_, Json<UserQuestion>>(
                "SELECT question FROM cloud_session_questions WHERE tenant_id=$1 AND
                session_id=$2 AND question_id=$3 AND run_id=$4",
            )
            .bind(run.claim.tenant_id.as_str())
            .bind(run.claim.session_id.as_str())
            .bind(&question.id)
            .bind(run.claim.run_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?;
            if existing.is_none_or(|value| value.0 != *question) {
                return Err(HarnessError::policy(
                    "cloud question replay differs from the persisted question",
                ));
            }
        }
        tx.commit().await.map_err(database_error)
    }

    pub async fn question_answer_for_worker(
        &self,
        run: &StartedRun,
        worker_id: &str,
        question_id: &str,
        now_ms: u64,
    ) -> Result<Option<UserAnswer>, HarnessError> {
        validate_started_run(run, worker_id)?;
        let mut tx = self.tenant_transaction(&run.claim.tenant_id).await?;
        // A stale worker receives no answer, matching the prior polling contract.
        if let Err(error) = require_writer_in(&mut tx, run, worker_id, Some(now_ms)).await {
            if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                return Ok(None);
            }
            return Err(error);
        }
        let answer = sqlx::query_scalar::<_, Option<Json<UserAnswer>>>(
            "SELECT answer FROM cloud_session_questions WHERE tenant_id=$1 AND session_id=$2
            AND run_id=$3 AND question_id=$4 AND state='answered'",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.session_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(question_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .flatten()
        .map(|value| value.0);
        tx.commit().await.map_err(database_error)?;
        Ok(answer)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_run(
        &self,
        run: &StartedRun,
        worker_id: &str,
        state: TerminalState,
        outcome: Option<&RunOutcome>,
        error: Option<&HarnessError>,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        if state == TerminalState::Succeeded && outcome.is_none() {
            return Err(HarnessError::invalid(
                "a successful cloud run requires an outcome",
            ));
        }
        let mut tx = self.admission_transaction().await?;
        let storage_id = crate::execution_admission::worker_pool(&mut tx, worker_id).await?;
        crate::execution_admission::pool_gate(&mut tx, &storage_id).await?;
        set_tenant(&mut tx, &run.claim.tenant_id).await?;
        let job = require_writer_in(&mut tx, run, worker_id, Some(now_ms)).await?;
        let reservation: String = job
            .try_get("quota_reservation_id")
            .map_err(database_error)?;
        if settle_usage_in(
            &mut tx,
            &run.claim.tenant_id,
            &run.claim.run_id,
            &reservation,
            now_ms,
        )
        .await?
        .is_none()
        {
            return Err(HarnessError::policy(
                "cloud run could not finish because its quota changed",
            ));
        }
        let now = to_i64(now_ms, "cloud finish timestamp")?;
        sqlx::query(
            "UPDATE cloud_runs SET state=$3, outcome=$4, error=$5, finished_at_ms=$6,
            updated_at_ms=$6, lease_expires_at_ms=NULL WHERE tenant_id=$1 AND run_id=$2",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(state.as_str())
        .bind(outcome.map(Json))
        .bind(error.map(Json))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        super::recovery::repair_run_history_in(
            &mut tx,
            &run.claim.tenant_id,
            &run.claim.session_id,
            &run.claim.run_id,
            now_ms,
        )
        .await?;
        sqlx::query(
            "DELETE FROM cloud_session_writer_leases WHERE tenant_id=$1 AND session_id=$2
            AND fencing_token=$3",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.session_id.as_str())
        .bind(to_i64(run.fencing_token, "cloud writer fencing token")?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        let next = complete_submission_in(&mut tx, &run.claim.tenant_id, &run.claim.run_id, now_ms)
            .await?;
        finish_session_in(&mut tx, run, state, outcome, next.is_some(), now).await?;
        crate::execution_admission::mark_cleanup_in(
            &mut tx,
            &run.claim.tenant_id,
            &run.claim.run_id,
            run.claim.lease_token,
            &run.claim.spec.metadata.user_id,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn reap_expired(&self, now_ms: u64) -> Result<u32, HarnessError> {
        let mut tx = self.begin().await?;
        let count = reap_runs_in(&mut tx, now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(count)
    }
}

pub(crate) async fn lock_session_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
) -> Result<i64, HarnessError> {
    sqlx::query_scalar(for_update(
        tx,
        "SELECT last_seq FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        "SELECT last_seq FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR
        UPDATE",
    ))
    .bind(tenant.as_str())
    .bind(session.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))
}

pub(crate) async fn require_writer_in(
    tx: &mut Transaction,
    run: &StartedRun,
    worker_id: &str,
    now_ms: Option<u64>,
) -> Result<AnyRow, HarnessError> {
    validate_started_run(run, worker_id)?;
    set_tenant(tx, &run.claim.tenant_id).await?;
    lock_session_in(tx, &run.claim.tenant_id, &run.claim.session_id).await?;
    let row = sqlx::query(for_update(
        tx,
        "SELECT run.*, writer.expires_at_ms AS writer_expires_at_ms FROM cloud_runs
        AS run JOIN cloud_session_writer_leases AS writer ON writer.tenant_id=run.tenant_id
        AND writer.session_id=run.session_id AND writer.run_id=run.run_id WHERE run.tenant_id=$1
        AND run.run_id=$2 AND run.session_id=$3 AND run.state IN ('running', 'cancel_requested')
        AND run.lease_owner=$4 AND run.lease_token=$5 AND run.session_fencing_token=$6
        AND writer.lease_owner=$4 AND writer.fencing_token=$6",
        "SELECT run.*, writer.expires_at_ms AS writer_expires_at_ms FROM cloud_runs
        AS run JOIN cloud_session_writer_leases AS writer ON writer.tenant_id=run.tenant_id
        AND writer.session_id=run.session_id AND writer.run_id=run.run_id WHERE run.tenant_id=$1
        AND run.run_id=$2 AND run.session_id=$3 AND run.state IN ('running', 'cancel_requested')
        AND run.lease_owner=$4 AND run.lease_token=$5 AND run.session_fencing_token=$6
        AND writer.lease_owner=$4 AND writer.fencing_token=$6 FOR UPDATE OF run, writer",
    ))
    .bind(run.claim.tenant_id.as_str())
    .bind(run.claim.run_id.as_str())
    .bind(run.claim.session_id.as_str())
    .bind(worker_id)
    .bind(to_i64(run.claim.lease_token, "cloud claim token")?)
    .bind(to_i64(run.fencing_token, "cloud writer fencing token")?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?
    .ok_or_else(|| HarnessError::policy("cloud run lease or writer fence is no longer current"))?;
    if let Some(now) = now_ms {
        let now = to_i64(now, "cloud writer lease timestamp")?;
        if row
            .try_get::<i64, _>("writer_expires_at_ms")
            .map_err(database_error)?
            <= now
            || row
                .try_get::<Option<i64>, _>("lease_expires_at_ms")
                .map_err(database_error)?
                .is_none_or(|expiry| expiry <= now)
        {
            return Err(HarnessError::policy("cloud writer lease has expired"));
        }
    }
    set_user_scope(
        tx,
        &UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
    )
    .await?;
    crate::execution_admission::require_current_in(tx, run, worker_id, now_ms).await?;
    Ok(row)
}

async fn settle_usage_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    _run: &RunId,
    reservation: &str,
    now_ms: u64,
) -> Result<Option<i64>, HarnessError> {
    let summary = ternilo_control::ControlStore::finalize_workload_reservation_in(
        tx,
        tenant,
        reservation,
        now_ms,
    )
    .await?;
    Ok(Some(to_i64(
        summary.used_model_tokens,
        "settled workload tokens",
    )?))
}

async fn complete_submission_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    run: &RunId,
    now_ms: u64,
) -> Result<Option<RunId>, HarnessError> {
    crate::steering::requeue_steering_in(tx, tenant, run, now_ms).await?;
    let item = sqlx::query(
        "DELETE FROM cloud_session_submissions WHERE tenant_id=$1 AND run_id=$2 RETURNING
        user_id, session_id",
    )
    .bind(tenant.as_str())
    .bind(run.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some(item) = item else {
        return Ok(None);
    };
    let user = UserId::new(
        item.try_get::<String, _>("user_id")
            .map_err(database_error)?,
    );
    let session = SessionId::new(
        item.try_get::<String, _>("session_id")
            .map_err(database_error)?,
    );
    crate::inbox::promote_head(
        tx,
        tenant,
        &user,
        &session,
        to_i64(now_ms, "cloud completion timestamp")?,
    )
    .await
}

async fn run_scopes(
    tx: &mut Transaction,
    operation: &str,
    worker: &str,
    now: i64,
) -> Result<Vec<AnyRow>, HarnessError> {
    let query = if ternilo_storage::backend(tx) == Backend::Postgres {
        "SELECT tenant_id,user_id,session_id,run_id FROM ternilo_cloud_run_scopes($1,$2,$3)"
    } else {
        include_str!("../queries/run_scopes.sql")
    };
    sqlx::query(query)
        .bind(operation)
        .bind(worker)
        .bind(now)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)
}

pub(crate) async fn drain_worker_runs_in(
    tx: &mut Transaction,
    worker: &str,
    now_ms: u64,
) -> Result<u32, HarnessError> {
    let now = to_i64(now_ms, "cloud worker drain timestamp")?;
    for row in run_scopes(tx, "drain", worker, now).await? {
        let tenant = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let user = UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        let run: String = row.try_get("run_id").map_err(database_error)?;
        let session = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        set_tenant(tx, &tenant).await?;
        set_user_scope(tx, &user).await?;
        lock_session_in(tx, &tenant, &session).await?;
        let changed=sqlx::query(
            "UPDATE cloud_runs SET lease_expires_at_ms=$4, updated_at_ms=$4 WHERE tenant_id=$1
            AND run_id=$2 AND lease_owner=$3 AND state IN ('leased', 'running', 'cancel_requested')",
        )
            .bind(tenant.as_str())
            .bind(&run)
            .bind(worker)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?
            .rows_affected();
        if changed == 1 {
            sqlx::query(
                "UPDATE cloud_session_writer_leases SET expires_at_ms=$4, renewed_at_ms=$4
                WHERE tenant_id=$1 AND run_id=$2 AND lease_owner=$3",
            )
            .bind(tenant.as_str())
            .bind(run)
            .bind(worker)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
        }
    }
    reap_runs_in(tx, now_ms).await
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep lease recovery, quota settlement, queue promotion and writer cleanup in one transaction."
)]
async fn reap_runs_in(tx: &mut Transaction, now_ms: u64) -> Result<u32, HarnessError> {
    let now = to_i64(now_ms, "cloud reaper timestamp")?;
    let mut count = 0_u32;
    for row in run_scopes(tx, "reap", "", now).await? {
        let tenant = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let user = UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        let run = RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?);
        let session = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        set_tenant(tx, &tenant).await?;
        set_user_scope(tx, &user).await?;
        if !try_lock_recovery_session_in(tx, &tenant, &session).await? {
            continue;
        }
        let job=sqlx::query(for_update(tx,"SELECT state, quota_reservation_id FROM cloud_runs WHERE tenant_id=$1 AND
        run_id=$2 AND lease_expires_at_ms <= $3 AND (state IN ('running', 'cancel_requested')
        OR (state='leased' AND attempt>=max_attempts))","SELECT state, quota_reservation_id FROM cloud_runs WHERE tenant_id=$1
            AND run_id=$2 AND lease_expires_at_ms <= $3 AND (state IN ('running', 'cancel_requested')
            OR (state='leased' AND attempt>=max_attempts)) FOR UPDATE"))
            .bind(tenant.as_str())
            .bind(run.as_str())
            .bind(now)
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
        let Some(job) = job else {
            continue;
        };
        let leased = job.try_get::<String, _>("state").map_err(database_error)? == "leased";
        let state = if leased { "failed" } else { "indeterminate" };
        let message = if leased {
            "worker claim lease expired and retry budget was exhausted"
        } else {
            "worker lease expired while the run was executing"
        };
        sqlx::query(
            "UPDATE cloud_runs SET state=$3, finished_at_ms=$4, updated_at_ms=$4, error=$5
            WHERE tenant_id=$1 AND run_id=$2",
        )
        .bind(tenant.as_str())
        .bind(run.as_str())
        .bind(state)
        .bind(now)
        .bind(Json(HarnessError::execution(message)))
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        super::recovery::repair_run_history_in(tx, &tenant, &session, &run, now_ms).await?;
        if !leased {
            crate::steering::requeue_steering_in(tx, &tenant, &run, now_ms).await?;
        }
        settle_usage_in(
            tx,
            &tenant,
            &run,
            &job.try_get::<String, _>("quota_reservation_id")
                .map_err(database_error)?,
            now_ms,
        )
        .await?;
        let next = complete_submission_in(tx, &tenant, &run, now_ms).await?;
        sqlx::query(
            "UPDATE cloud_sessions SET state=$3, execution=NULL, current_run_id=NULL, updated_at_ms=$4
            WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(if next.is_some() { "queued" } else { state })
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        sqlx::query(
            "DELETE FROM cloud_session_writer_leases WHERE tenant_id=$1 AND session_id=$2
            AND expires_at_ms <= $3",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
        count += 1;
    }
    for row in run_scopes(tx, "writers", "", now).await? {
        let tenant = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        set_tenant(tx, &tenant).await?;
        let session = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        if !try_lock_recovery_session_in(tx, &tenant, &session).await? {
            continue;
        }
        sqlx::query(
            "DELETE FROM cloud_session_writer_leases WHERE tenant_id=$1 AND session_id=$2
            AND expires_at_ms<=$3",
        )
        .bind(tenant.as_str())
        .bind(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        )
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    }
    Ok(count)
}

#[expect(
    clippy::too_many_lines,
    reason = "Consume the steering receipt and transfer its quota in one indivisible transaction."
)]
async fn consume_steering_in(
    tx: &mut Transaction,
    run: &StartedRun,
    event: &SessionEvent,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let SessionEventKind::UserMessage {
        source:
            Some(ternilo_protocol::UserMessageSource::Submission {
                submission_id,
                delivery,
                ..
            }),
        ..
    } = &event.kind
    else {
        return Ok(());
    };
    if *delivery == ternilo_protocol::SubmissionDelivery::Queue {
        let batched: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM cloud_session_submissions
            WHERE tenant_id=$1 AND submission_id=$2 AND batch_run_id=$3",
        )
        .bind(run.claim.tenant_id.as_str())
        .bind(submission_id.as_str())
        .bind(run.claim.run_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if batched == 0 {
            return Ok(());
        }
    }
    let active_reservation: String = sqlx::query_scalar(
        "SELECT quota_reservation_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2",
    )
    .bind(run.claim.tenant_id.as_str())
    .bind(run.claim.run_id.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    ternilo_control::ControlStore::workload_model_budget_in(
        tx,
        &run.claim.tenant_id,
        &active_reservation,
    )
    .await?;
    let query = "SELECT submission.run_id, candidate.quota_reservation_id AS candidate_reservation,
    active.quota_reservation_id AS active_reservation, reservation.reserved_model_tokens
    FROM cloud_session_submissions AS submission JOIN cloud_runs AS candidate ON candidate.tenant_id=submission.tenant_id
    AND candidate.run_id=submission.run_id JOIN cloud_runs AS active ON active.tenant_id=submission.tenant_id
    AND active.run_id=COALESCE(submission.batch_run_id, submission.steering_target_run_id) JOIN control_quota_reservations
    AS reservation ON reservation.tenant_id=candidate.tenant_id AND reservation.reservation_id=candidate.quota_reservation_id
    WHERE submission.tenant_id=$1 AND submission.session_id=$2 AND submission.submission_id=$3
    AND submission.placement='steering' AND (submission.batch_run_id=$4 OR (submission.steering_target_run_id=$4 AND
    submission.steering_target_writer_fencing_token=$5)) AND candidate.state='queued'
    AND active.state IN ('running', 'cancel_requested') AND active.session_fencing_token=$5
    AND reservation.state='active'";
    let postgres = "SELECT submission.run_id, candidate.quota_reservation_id AS candidate_reservation,
    active.quota_reservation_id AS active_reservation, reservation.reserved_model_tokens
    FROM cloud_session_submissions AS submission JOIN cloud_runs AS candidate ON candidate.tenant_id=submission.tenant_id
    AND candidate.run_id=submission.run_id JOIN cloud_runs AS active ON active.tenant_id=submission.tenant_id
    AND active.run_id=COALESCE(submission.batch_run_id, submission.steering_target_run_id) JOIN control_quota_reservations
    AS reservation ON reservation.tenant_id=candidate.tenant_id AND reservation.reservation_id=candidate.quota_reservation_id
    WHERE submission.tenant_id=$1 AND submission.session_id=$2 AND submission.submission_id=$3
    AND submission.placement='steering' AND (submission.batch_run_id=$4 OR (submission.steering_target_run_id=$4 AND
    submission.steering_target_writer_fencing_token=$5)) AND candidate.state='queued'
    AND active.state IN ('running', 'cancel_requested') AND active.session_fencing_token=$5
    AND reservation.state='active' FOR UPDATE OF submission, candidate, active, reservation";
    let item = sqlx::query(for_update(tx, query, postgres))
        .bind(run.claim.tenant_id.as_str())
        .bind(run.claim.session_id.as_str())
        .bind(submission_id.as_str())
        .bind(run.claim.run_id.as_str())
        .bind(to_i64(run.fencing_token, "cloud writer fencing token")?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            HarnessError::policy("steering submission no longer targets the active run")
        })?;
    let candidate_run = RunId::new(
        item.try_get::<String, _>("run_id")
            .map_err(database_error)?,
    );
    if !crate::steering::compatible_steering_budget_in(
        tx,
        &run.claim.tenant_id,
        &candidate_run,
        &run.claim.run_id,
    )
    .await?
    {
        return Err(HarnessError::policy(
            "steering cannot change the active model binding or budget month",
        ));
    }
    let tenant = &run.claim.tenant_id;
    let changed = sqlx::query(
        "UPDATE control_quota_reservations SET reserved_model_tokens=reserved_model_tokens+$3
        WHERE tenant_id=$1 AND reservation_id=$2 AND state='active'",
    )
    .bind(tenant.as_str())
    .bind(
        item.try_get::<String, _>("active_reservation")
            .map_err(database_error)?,
    )
    .bind(
        item.try_get::<i64, _>("reserved_model_tokens")
            .map_err(database_error)?,
    )
    .execute(&mut **tx)
    .await
    .map_err(database_error)?
    .rows_affected();
    if changed != 1 {
        return Err(HarnessError::policy(
            "active steering quota reservation is no longer current",
        ));
    }
    let changed = sqlx::query(
        "UPDATE control_quota_reservations SET state='released' WHERE tenant_id=$1
        AND reservation_id=$2 AND state='active'",
    )
    .bind(tenant.as_str())
    .bind(
        item.try_get::<String, _>("candidate_reservation")
            .map_err(database_error)?,
    )
    .execute(&mut **tx)
    .await
    .map_err(database_error)?
    .rows_affected();
    if changed != 1 {
        return Err(HarnessError::policy(
            "steering candidate quota reservation is no longer current",
        ));
    }
    sqlx::query("DELETE FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2")
        .bind(tenant.as_str())
        .bind(
            item.try_get::<String, _>("run_id")
                .map_err(database_error)?,
        )
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    sqlx::query(
        "UPDATE cloud_session_inboxes SET updated_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(tenant.as_str())
    .bind(run.claim.session_id.as_str())
    .bind(to_i64(now_ms, "steering consumption timestamp")?)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(crate) async fn fail_queued_capacity_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    run: &RunId,
    now_ms: u64,
) -> Result<bool, HarnessError> {
    lock_session_in(tx, tenant, session).await?;
    let row=sqlx::query(for_update(tx,
        "SELECT user_id,quota_reservation_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND session_id=$3 AND state='queued'",
        "SELECT user_id,quota_reservation_id FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2 AND session_id=$3 AND state='queued' FOR UPDATE"))
        .bind(tenant.as_str()).bind(run.as_str()).bind(session.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    let Some(row) = row else {
        return Ok(false);
    };
    set_user_scope(
        tx,
        &UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
    )
    .await?;
    settle_usage_in(
        tx,
        tenant,
        run,
        &row.try_get::<String, _>("quota_reservation_id")
            .map_err(database_error)?,
        now_ms,
    )
    .await?;
    let error = HarnessError::policy(
        "capacity_exhausted: all compatible Worker resident slots are held by runs waiting for queued dependencies",
    );
    sqlx::query("UPDATE cloud_runs SET state='failed',error=$3,finished_at_ms=$4,updated_at_ms=$4 WHERE tenant_id=$1 AND run_id=$2 AND state='queued'")
        .bind(tenant.as_str()).bind(run.as_str()).bind(Json(&error)).bind(to_i64(now_ms,"capacity failure time")?).execute(&mut **tx).await.map_err(database_error)?;
    super::recovery::repair_run_history_in(tx, tenant, session, run, now_ms).await?;
    let next = complete_submission_in(tx, tenant, run, now_ms).await?;
    sqlx::query("UPDATE cloud_sessions SET state=$3,execution=NULL,current_run_id=NULL,updated_at_ms=$4 WHERE tenant_id=$1 AND session_id=$2")
        .bind(tenant.as_str()).bind(session.as_str()).bind(if next.is_some(){"queued"}else{"failed"}).bind(to_i64(now_ms,"capacity failure time")?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(true)
}

pub(super) async fn project_execution_activity_in(
    tx: &mut ternilo_storage::Transaction,
    tenant: &TenantId,
    session: &SessionId,
    event: &SessionEvent,
) -> Result<(), HarnessError> {
    if !matches!(
        event.kind,
        SessionEventKind::TurnStarted
            | SessionEventKind::ExecutionActivityChanged { .. }
            | SessionEventKind::WorkspaceExecutionWaiting
            | SessionEventKind::WorkspaceExecutionAcquired
            | SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled
    ) {
        return Ok(());
    }
    let mut current =
        sqlx::query_scalar::<_, Option<Json<ternilo_protocol::SessionExecutionActivity>>>(
            "SELECT execution FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?
        .map(|value| value.0);
    if ternilo_protocol::update_execution_activity(&mut current, event) {
        sqlx::query("UPDATE cloud_sessions SET execution=$3 WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant.as_str())
            .bind(session.as_str())
            .bind(current.as_ref().map(Json))
            .execute(&mut **tx)
            .await
            .map_err(database_error)?;
    }
    Ok(())
}

async fn try_lock_recovery_session_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
) -> Result<bool, HarnessError> {
    // Recovery can already hold another session's quota lock. Never wait on a
    // session whose submitter may need that quota; a later sweep will retry it.
    let present = sqlx::query(for_update(tx,
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE SKIP LOCKED"))
        .bind(tenant.as_str()).bind(session.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    Ok(present.is_some())
}

async fn claimable_run_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    run_id: &str,
    now: i64,
) -> Result<Option<AnyRow>, HarnessError> {
    sqlx::query(for_update(
        tx,
        "SELECT run.* FROM cloud_runs AS run JOIN cloud_session_submissions
                AS submission ON submission.tenant_id = run.tenant_id AND submission.run_id
                = run.run_id AND submission.placement = 'running' JOIN cloud_session_inboxes
                AS inbox ON inbox.tenant_id = submission.tenant_id AND inbox.user_id
                = submission.user_id AND inbox.session_id = submission.session_id WHERE
                run.tenant_id = $1 AND run.run_id = $2 AND inbox.paused = 0 AND ((run.state
                = 'queued' AND run.available_at_ms <= $3) OR (run.state = 'leased'
                AND run.lease_expires_at_ms <= $3 AND run.attempt < run.max_attempts))",
        "SELECT run.* FROM cloud_runs AS run JOIN cloud_session_submissions
                AS submission ON submission.tenant_id = run.tenant_id AND submission.run_id
                = run.run_id AND submission.placement = 'running' JOIN cloud_session_inboxes
                AS inbox ON inbox.tenant_id = submission.tenant_id AND inbox.user_id
                = submission.user_id AND inbox.session_id = submission.session_id WHERE
                run.tenant_id = $1 AND run.run_id = $2 AND inbox.paused = 0 AND ((run.state
                = 'queued' AND run.available_at_ms <= $3) OR (run.state = 'leased'
                AND run.lease_expires_at_ms <= $3 AND run.attempt < run.max_attempts))
                FOR UPDATE OF run, submission, inbox SKIP LOCKED",
    ))
    .bind(tenant.as_str())
    .bind(run_id)
    .bind(now)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)
}

async fn finish_session_in(
    tx: &mut Transaction,
    run: &StartedRun,
    state: TerminalState,
    outcome: Option<&RunOutcome>,
    has_next: bool,
    now: i64,
) -> Result<(), HarnessError> {
    let title = outcome
        .filter(|_| state == TerminalState::Succeeded)
        .and_then(|value| value.generated_title.as_deref())
        .map(|value| value.trim().chars().take(256).collect::<String>())
        .filter(|value| !value.is_empty());
    let approved = state == TerminalState::Succeeded
        && outcome.is_some_and(|value| {
            value.events.iter().any(|event| {
                matches!(
                    event.kind,
                    SessionEventKind::PlanReviewCompleted { approved: true, .. }
                )
            })
        });
    sqlx::query(
        "UPDATE cloud_sessions SET state=$3, execution=NULL, current_run_id=NULL, title=CASE WHEN
            title='New session' THEN COALESCE($4, title) ELSE title END, mode=CASE
            WHEN $5=1 AND mode='plan' THEN 'execute' ELSE mode END, updated_at_ms=$6
            WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(run.claim.tenant_id.as_str())
    .bind(run.claim.session_id.as_str())
    .bind(if has_next { "queued" } else { state.as_str() })
    .bind(title)
    .bind(i64::from(approved))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map(|_| ())
    .map_err(database_error)
}
