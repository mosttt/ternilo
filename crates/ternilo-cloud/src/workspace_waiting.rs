//! Queue activity is scheduling metadata, not an event from a model turn that never began.

use sqlx::Row;
use ternilo_protocol::{
    HarnessError, RunId, SessionExecutionActivity, SessionExecutionPhase, SessionId, TenantId,
    UserId,
};
use ternilo_storage::{Backend, Json, Transaction, for_update, set_tenant_scope, set_user_scope};

use crate::store::{database_error, to_i64};

/// The caller holds the storage gate. Each changed session is locked without waiting.
pub(crate) async fn refresh_in(
    tx: &mut Transaction,
    storage: &str,
    now: u64,
) -> Result<(), HarnessError> {
    let sql = match ternilo_storage::backend(tx) {
        Backend::Postgres => "SELECT * FROM ternilo_cloud_workspace_waiting($1,$2)",
        Backend::Sqlite => include_str!("queries/workspace_waiting.sql"),
    };
    let rows = sqlx::query(sql)
        .bind(storage)
        .bind(to_i64(now, "queue activity time")?)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?;
    for row in rows {
        let tenant = TenantId::new(
            row.try_get::<String, _>("tenant_id")
                .map_err(database_error)?,
        );
        let owner = UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        let session = SessionId::new(
            row.try_get::<String, _>("session_id")
                .map_err(database_error)?,
        );
        let run = RunId::new(row.try_get::<String, _>("run_id").map_err(database_error)?);
        let reason: String = row.try_get("next_reason").map_err(database_error)?;
        set_tenant_scope(tx, &tenant).await?;
        set_user_scope(tx, &owner).await?;
        let locked = sqlx::query(for_update(tx,
            "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 AND state='queued' AND current_run_id IS NULL",
            "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 AND state='queued' AND current_run_id IS NULL FOR UPDATE SKIP LOCKED"))
            .bind(tenant.as_str()).bind(session.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
        if locked.is_none() {
            continue;
        }
        let changed = sqlx::query(
            "UPDATE cloud_runs SET queue_wait_reason=$3 WHERE tenant_id=$1 AND run_id=$2 AND state='queued'
             AND EXISTS (SELECT 1 FROM cloud_session_submissions submission JOIN cloud_session_inboxes inbox
                ON inbox.tenant_id=submission.tenant_id AND inbox.session_id=submission.session_id AND inbox.user_id=submission.user_id
                WHERE submission.tenant_id=$1 AND submission.run_id=$2 AND submission.placement='running' AND inbox.paused=0)",
        ).bind(tenant.as_str()).bind(run.as_str()).bind(&reason)
            .execute(&mut **tx).await.map_err(database_error)?.rows_affected();
        if changed == 0 {
            continue;
        }
        let execution = SessionExecutionActivity {
            run_id: run,
            phase: if reason == "workspace" {
                SessionExecutionPhase::WaitingForWorkspace
            } else {
                SessionExecutionPhase::WaitingForCapacity
            },
        };
        sqlx::query("UPDATE cloud_sessions SET execution=$3,updated_at_ms=$4 WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant.as_str()).bind(session.as_str()).bind(Json(&execution))
            .bind(to_i64(now, "queue activity time")?).execute(&mut **tx).await.map_err(database_error)?;
    }
    Ok(())
}
