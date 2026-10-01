use super::{
    CloudStore, HarnessError, RunId, SessionId, SessionSubmission, SubmissionId,
    SubmissionPlacement, TenantId, UserId, database_error, decode_submission, promote_head,
    require_owned_session, select_submission_for_update, set_scope, to_i64,
};
use ternilo_control::ResourceAction;
use ternilo_storage::Transaction;

impl CloudStore {
    /// Persist stop-and-send before acknowledging; completion resumes the queue in its transaction.
    pub async fn restart_session_inbox(
        &self,
        tenant: &TenantId,
        actor: &UserId,
        session: &SessionId,
        submission: &SubmissionId,
        now_ms: u64,
    ) -> Result<SessionSubmission, HarnessError> {
        let mut tx = self.begin().await?;
        let owner =
            crate::sharing::session_owner_in(&mut tx, tenant, actor, session, ResourceAction::Stop)
                .await?;
        crate::sharing::session_owner_in(&mut tx, tenant, actor, session, ResourceAction::Submit)
            .await?;
        set_scope(&mut tx, tenant, &owner).await?;
        require_owned_session(&mut tx, tenant, &owner, session).await?;
        crate::store::lock_session_in(&mut tx, tenant, session).await?;
        let item = decode_submission(
            &select_submission_for_update(&mut tx, tenant, &owner, session, submission).await?,
        )?;
        if item.placement != SubmissionPlacement::Queued {
            tx.commit().await.map_err(database_error)?;
            return Ok(item);
        }
        let active = sqlx::query_scalar::<_, String>(
            "SELECT run_id FROM cloud_session_submissions WHERE tenant_id=$1 AND user_id=$2
                AND session_id=$3 AND placement='running' AND batch_run_id IS NULL",
        )
        .bind(tenant.as_str())
        .bind(owner.as_str())
        .bind(session.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        let now = to_i64(now_ms, "queue restart time")?;
        sqlx::query(
            "UPDATE cloud_session_inboxes SET paused=1, restart_after_run_id=$4,
                error=NULL, updated_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(tenant.as_str())
        .bind(session.as_str())
        .bind(now)
        .bind(active.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        let stopping = if let Some(active) = active {
            let run = RunId::new(active);
            !Self::cancel_run_in(&mut tx, tenant, &run, Some(actor), now_ms)
                .await?
                .terminal()
        } else {
            false
        };
        if !stopping {
            sqlx::query(
                "UPDATE cloud_session_inboxes SET paused=0, restart_after_run_id=NULL,
                    updated_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2",
            )
            .bind(tenant.as_str())
            .bind(session.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            promote_head(&mut tx, tenant, &owner, session, now).await?;
        }
        crate::sharing::audit_session_in(
            &mut tx,
            tenant,
            actor,
            session,
            &owner,
            ResourceAction::Stop,
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(item)
    }
}

pub(crate) async fn complete_restart_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    run: &RunId,
    now: i64,
) -> Result<(), HarnessError> {
    sqlx::query(
        "UPDATE cloud_session_inboxes SET paused=0, restart_after_run_id=NULL,
            error=NULL, updated_at_ms=$4
         WHERE tenant_id=$1 AND session_id=$2 AND restart_after_run_id=$3",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .bind(run.as_str())
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}
