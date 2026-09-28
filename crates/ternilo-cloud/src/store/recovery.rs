use std::collections::BTreeSet;

use sqlx::Row;
use ternilo_protocol::{HarnessError, RunId, SessionEvent, SessionId, TenantId};
use ternilo_storage::{Json, Transaction, database_error, for_update};

use super::{execution::lock_session_in, from_i64, to_i64, validate_event_history};

pub(super) async fn repair_run_history_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    run: &RunId,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let mut last = lock_session_in(transaction, tenant, session).await?;
    let terminal = sqlx::query_scalar::<_, Option<i64>>(for_update(
        transaction,
        "SELECT session_fencing_token FROM cloud_runs WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3 AND state IN ('succeeded','failed','cancelled','indeterminate')",
        "SELECT session_fencing_token FROM cloud_runs WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3 AND state IN ('succeeded','failed','cancelled','indeterminate') FOR UPDATE",
    )).bind(tenant.as_str()).bind(session.as_str()).bind(run.as_str())
        .fetch_optional(&mut **transaction).await.map_err(database_error)?;
    let Some(Some(fence)) = terminal else {
        return Ok(());
    };
    let events = sqlx::query_scalar::<_, Json<SessionEvent>>(
        "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3 ORDER BY seq",
    ).bind(tenant.as_str()).bind(session.as_str()).bind(run.as_str())
        .fetch_all(&mut **transaction).await.map_err(database_error)?
        .into_iter().map(|event| event.0).collect::<Vec<_>>();
    for (run_id, kind) in ternilo_builtins::interrupted_history_events(&events) {
        last = last
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("session sequence exceeds i64"))?;
        let event = SessionEvent {
            seq: from_i64(last, "recovery sequence")?,
            occurred_at_ms: now_ms,
            run_id,
            kind,
        };
        sqlx::query("INSERT INTO cloud_session_events(tenant_id,session_id,seq,run_id,event,writer_fencing_token,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(tenant.as_str()).bind(session.as_str()).bind(last).bind(run.as_str()).bind(Json(&event))
            .bind(fence).bind(to_i64(now_ms,"history recovery time")?)
            .execute(&mut **transaction).await.map_err(database_error)?;
        sqlx::query("UPDATE cloud_sessions SET last_seq=$3,updated_at_ms=$4 WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant.as_str()).bind(session.as_str()).bind(last).bind(to_i64(now_ms,"history recovery time")?)
            .execute(&mut **transaction).await.map_err(database_error)?;
        super::execution::project_execution_activity_in(transaction, tenant, session, &event)
            .await?;
        crate::telemetry::capture_event_in(transaction, tenant, session, &event, now_ms).await?;
    }
    Ok(())
}

pub(super) async fn load_repaired_history_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
    now_ms: u64,
) -> Result<Vec<SessionEvent>, HarnessError> {
    let mut events = load_history_in(transaction, tenant, session).await?;
    validate_event_history(&events)?;
    let unfinished = ternilo_builtins::interrupted_history_events(&events)
        .into_iter()
        .map(|(run_id, _)| run_id)
        .collect::<BTreeSet<_>>();
    if !unfinished.is_empty() {
        for run in unfinished {
            repair_run_history_in(transaction, tenant, session, &run, now_ms).await?;
        }
        events = load_history_in(transaction, tenant, session).await?;
    }
    Ok(events)
}

async fn load_history_in(
    transaction: &mut Transaction,
    tenant: &TenantId,
    session: &SessionId,
) -> Result<Vec<SessionEvent>, HarnessError> {
    let rows = sqlx::query(
        "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 ORDER BY seq",
    )
    .bind(tenant.as_str())
    .bind(session.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    rows.into_iter()
        .map(|row| {
            row.try_get::<Json<SessionEvent>, _>("event")
                .map(|event| event.0)
                .map_err(database_error)
        })
        .collect()
}
