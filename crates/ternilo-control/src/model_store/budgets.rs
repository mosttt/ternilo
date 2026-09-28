use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId};
use ternilo_storage::{Transaction, database_error, for_update, set_tenant_scope};

use super::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, WorkloadModelPrincipal,
    WorkloadReservationSummary, number, read_number, scope,
};

pub(crate) async fn lock_budget(
    tx: &mut Transaction,
    tenant: &TenantId,
) -> Result<(), HarnessError> {
    set_tenant_scope(tx, tenant).await?;
    sqlx::query(for_update(
        tx,
        "SELECT tenant_id FROM control_quotas WHERE tenant_id=$1",
        "SELECT tenant_id FROM control_quotas WHERE tenant_id=$1 FOR UPDATE",
    ))
    .bind(tenant.as_str())
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn require_run_budget(
    tx: &mut Transaction,
    principal: &WorkloadModelPrincipal,
    additional: u64,
    now: u64,
) -> Result<String, ModelAccessError> {
    lock_budget(tx, &principal.tenant_id).await?;
    let row = sqlx::query("SELECT user_id,run_id,state,period_start,reserved_model_tokens,expires_at_ms FROM control_quota_reservations WHERE tenant_id=$1 AND reservation_id=$2")
        .bind(principal.tenant_id.as_str()).bind(&principal.execution_reservation_id).fetch_optional(&mut **tx).await.map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("workload budget reservation does not exist"))?;
    if row
        .try_get::<String, _>("user_id")
        .map_err(database_error)?
        != principal.execution_owner_user_id.as_str()
        || row
            .try_get::<Option<String>, _>("run_id")
            .map_err(database_error)?
            .as_deref()
            != Some(principal.run_id.as_str())
        || row.try_get::<String, _>("state").map_err(database_error)? != "active"
        || read_number(&row, "expires_at_ms")? <= now
    {
        return Err(HarnessError::policy(
            "workload budget no longer belongs to this active execution",
        )
        .into());
    }
    let ceiling = read_number(&row, "reserved_model_tokens")?;
    if ceiling != principal.run_token_limit {
        return Err(HarnessError::policy(
            "canonical workload token ceiling differs from its budget reservation",
        )
        .into());
    }
    let totals = reservation_totals(
        tx,
        &principal.tenant_id,
        &principal.execution_reservation_id,
    )
    .await?;
    let projected = totals
        .0
        .checked_add(totals.1)
        .and_then(|value| value.checked_add(additional));
    if projected.is_none_or(|value| value > ceiling) {
        return Err(ModelAccessError {
            kind: ModelAccessErrorKind::QuotaExceeded,
            error: HarnessError::policy("workload model-token ceiling would be exceeded"),
        });
    }
    row.try_get("period_start")
        .map_err(database_error)
        .map_err(Into::into)
}

async fn reservation_totals(
    tx: &mut Transaction,
    tenant: &TenantId,
    reservation: &str,
) -> Result<(u64, u64), HarnessError> {
    let row = sqlx::query("SELECT CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS known_tokens,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN a.reserved_tokens ELSE 0 END),0) AS BIGINT) AS unknown_tokens FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=$1 AND r.execution_reservation_id=$2")
        .bind(tenant.as_str()).bind(reservation).fetch_one(&mut **tx).await.map_err(database_error)?;
    Ok((
        read_number(&row, "known_tokens")?,
        read_number(&row, "unknown_tokens")?,
    ))
}

pub(crate) async fn refresh_reservation_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    reservation: &str,
) -> Result<Option<WorkloadReservationSummary>, HarnessError> {
    scope(tx).await?;
    lock_budget(tx, tenant).await?;
    let row = sqlx::query("SELECT state,period_start,reserved_model_tokens FROM control_quota_reservations WHERE tenant_id=$1 AND reservation_id=$2")
        .bind(tenant.as_str()).bind(reservation).fetch_optional(&mut **tx).await.map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let (known, unknown) = reservation_totals(tx, tenant, reservation).await?;
    let mut state: String = row.try_get("state").map_err(database_error)?;
    if matches!(state.as_str(), "committed" | "released") {
        if known > 0 || unknown > 0 {
            "committed"
        } else {
            "released"
        }
        .clone_into(&mut state);
    }
    let period: String = row.try_get("period_start").map_err(database_error)?;
    sqlx::query("UPDATE control_quota_reservations SET state=$3,committed_model_tokens=$4,unknown_model_tokens=$5 WHERE tenant_id=$1 AND reservation_id=$2")
        .bind(tenant.as_str()).bind(reservation).bind(&state).bind((known>0).then(|| number(known)).transpose()?).bind(number(unknown)?).execute(&mut **tx).await.map_err(database_error)?;
    refresh_period_in(tx, tenant, &period).await?;
    Ok(Some(WorkloadReservationSummary {
        state,
        period_start: period,
        reserved_model_tokens: read_number(&row, "reserved_model_tokens")?,
        used_model_tokens: known,
        unknown_model_tokens: unknown,
    }))
}

pub(crate) async fn refresh_period_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    period: &str,
) -> Result<(), HarnessError> {
    scope(tx).await?;
    set_tenant_scope(tx, tenant).await?;
    let row = sqlx::query("SELECT CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS known_tokens,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN a.reserved_tokens ELSE 0 END),0) AS BIGINT) AS unknown_tokens FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=$1 AND r.budget_period_start=$2")
        .bind(tenant.as_str()).bind(period).fetch_one(&mut **tx).await.map_err(database_error)?;
    sqlx::query("INSERT INTO control_quota_usage(tenant_id,period_start,used_model_tokens,unknown_model_tokens) VALUES($1,$2,$3,$4) ON CONFLICT(tenant_id,period_start) DO UPDATE SET used_model_tokens=EXCLUDED.used_model_tokens,unknown_model_tokens=EXCLUDED.unknown_model_tokens")
        .bind(tenant.as_str()).bind(period).bind(number(read_number(&row,"known_tokens")?)?).bind(number(read_number(&row,"unknown_tokens")?)?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

impl ControlStore {
    /// Read accepted attempt usage after the caller authorizes the canonical workload.
    pub async fn workload_model_budget_in(
        tx: &mut Transaction,
        tenant: &TenantId,
        reservation: &str,
    ) -> Result<WorkloadReservationSummary, HarnessError> {
        refresh_reservation_in(tx, tenant, reservation)
            .await?
            .ok_or_else(|| HarnessError::policy("workload budget reservation does not exist"))
    }

    /// Finalize execution admission while preserving every accepted model attempt's facts.
    pub async fn finalize_workload_reservation_in(
        tx: &mut Transaction,
        tenant: &TenantId,
        reservation: &str,
        _now: u64,
    ) -> Result<WorkloadReservationSummary, HarnessError> {
        scope(tx).await?;
        lock_budget(tx, tenant).await?;
        let current = refresh_reservation_in(tx, tenant, reservation)
            .await?
            .ok_or_else(|| HarnessError::policy("workload budget reservation does not exist"))?;
        if current.state == "active" {
            let state = if current.used_model_tokens > 0 || current.unknown_model_tokens > 0 {
                "committed"
            } else {
                "released"
            };
            sqlx::query("UPDATE control_quota_reservations SET state=$3 WHERE tenant_id=$1 AND reservation_id=$2")
                .bind(tenant.as_str()).bind(reservation).bind(state).execute(&mut **tx).await.map_err(database_error)?;
            return Ok(WorkloadReservationSummary {
                state: state.to_owned(),
                ..current
            });
        }
        Ok(current)
    }
}
