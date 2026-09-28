use chrono::{Months, TimeZone, Utc};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, TenantId, UserId};

use crate::{
    ControlAction, ControlUser, ModelUsageAnomalyKind, ModelUsageGroup, ModelUsageLedgerEntry,
    ModelUsageQuotaSnapshot, ModelUsageReport, ModelUsageReservation, ModelUsageTotals,
};

use super::{ControlStore, database_error, from_i64, require_action, set_tenant, to_i64};

const MAX_REPORT_ROWS: u32 = 1_000;

struct UsagePeriod {
    label: String,
    start_ms: u64,
    end_ms: u64,
}

impl ControlStore {
    pub async fn model_usage_report(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        period: Option<&str>,
        limit: u32,
        now_ms: u64,
    ) -> Result<ModelUsageReport, HarnessError> {
        validate_limit(limit)?;
        let mut transaction = self.database.begin().await?;
        set_tenant(&mut transaction, tenant_id).await?;
        require_action(
            &mut transaction,
            tenant_id,
            &actor.user_id,
            ControlAction::UsageRead,
        )
        .await?;

        crate::model_store::scope(&mut transaction).await?;
        let period = period_bounds(period, now_ms)?;
        let quota = quota_snapshot(&mut transaction, tenant_id, &period, now_ms).await?;
        let totals = usage_totals(&mut transaction, tenant_id, &period).await?;
        let groups = usage_groups(&mut transaction, tenant_id, &period).await?;
        let (ledger, ledger_truncated) =
            usage_ledger(&mut transaction, tenant_id, &period, limit).await?;
        let (reservations, reservations_truncated) =
            usage_reservations(&mut transaction, tenant_id, &period, limit, now_ms).await?;
        let (anomalies, anomalies_truncated) =
            usage_anomalies(&mut transaction, tenant_id, limit, now_ms).await?;

        transaction.commit().await.map_err(database_error)?;
        Ok(ModelUsageReport {
            tenant_id: tenant_id.clone(),
            period: period.label,
            period_start_ms: period.start_ms,
            period_end_ms: period.end_ms,
            limit,
            quota,
            totals,
            groups,
            ledger,
            ledger_truncated,
            reservations,
            reservations_truncated,
            anomalies,
            anomalies_truncated,
        })
    }
}

fn validate_limit(limit: u32) -> Result<(), HarnessError> {
    if (1..=MAX_REPORT_ROWS).contains(&limit) {
        Ok(())
    } else {
        Err(HarnessError::invalid(
            "model usage limit must be between 1 and 1000",
        ))
    }
}

fn period_bounds(period: Option<&str>, now_ms: u64) -> Result<UsagePeriod, HarnessError> {
    let label = match period {
        Some(period) => period.to_owned(),
        None => Utc
            .timestamp_millis_opt(to_i64(now_ms, "model usage report timestamp")?)
            .single()
            .ok_or_else(|| HarnessError::invalid("model usage timestamp is out of range"))?
            .format("%Y-%m")
            .to_string(),
    };
    let (year, month) = parse_period(&label)?;
    let start = Utc
        .with_ymd_and_hms(
            year,
            u32::try_from(month).map_err(|_| HarnessError::invalid("invalid usage month"))?,
            1,
            0,
            0,
            0,
        )
        .single()
        .ok_or_else(|| HarnessError::invalid("model usage period is out of range"))?;
    let end = start
        .checked_add_months(Months::new(1))
        .ok_or_else(|| HarnessError::invalid("model usage period is out of range"))?;
    Ok(UsagePeriod {
        label,
        start_ms: from_i64(start.timestamp_millis(), "model usage period start")?,
        end_ms: from_i64(end.timestamp_millis(), "model usage period end")?,
    })
}

pub(super) fn quota_period(now_ms: u64) -> Result<String, HarnessError> {
    Ok(format!("{}-01", period_bounds(None, now_ms)?.label))
}

async fn quota_snapshot(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    period: &UsagePeriod,
    now_ms: u64,
) -> Result<ModelUsageQuotaSnapshot, HarnessError> {
    let row=sqlx::query("SELECT q.monthly_model_tokens,CAST(COALESCE(u.used_model_tokens,0) AS BIGINT) AS settled_tokens,CAST(COALESCE(u.unknown_model_tokens,0) AS BIGINT) AS unknown_reserved_tokens,CAST(COALESCE((SELECT SUM(CASE WHEN r.reserved_model_tokens>COALESCE(r.committed_model_tokens,0)+r.unknown_model_tokens THEN r.reserved_model_tokens-COALESCE(r.committed_model_tokens,0)-r.unknown_model_tokens ELSE 0 END) FROM control_quota_reservations r WHERE r.tenant_id=q.tenant_id AND r.period_start=$1 AND r.state='active' AND r.expires_at_ms>$2),0) AS BIGINT) AS active_reserved_tokens FROM control_quotas q LEFT JOIN control_quota_usage u ON u.tenant_id=q.tenant_id AND u.period_start=$1 WHERE q.tenant_id=$3")
        .bind(format!("{}-01",period.label)).bind(to_i64(now_ms,"usage timestamp")?).bind(tenant_id.as_str()).fetch_one(&mut **transaction).await.map_err(database_error)?;
    Ok(ModelUsageQuotaSnapshot {
        monthly_limit_tokens: row_u64(&row, "monthly_model_tokens", "monthly limit")?,
        settled_tokens: row_u64(&row, "settled_tokens", "settled tokens")?,
        active_reserved_tokens: row_u64(&row, "active_reserved_tokens", "active reservation")?,
        unknown_reserved_tokens: row_u64(&row, "unknown_reserved_tokens", "unknown reservation")?,
    })
}

const TOTAL_COLUMNS: &str = "COUNT(DISTINCT r.request_id) AS requests,COUNT(*) AS attempts,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN 1 ELSE 0 END),0) AS BIGINT) AS unknown_attempts,CAST(COALESCE(SUM(a.input_tokens),0) AS BIGINT) AS input_tokens,CAST(COALESCE(SUM(a.output_tokens),0) AS BIGINT) AS output_tokens,CAST(COALESCE(SUM(a.cached_input_tokens),0) AS BIGINT) AS cached_input_tokens,CAST(COALESCE(SUM(a.cache_write_tokens),0) AS BIGINT) AS cache_write_tokens,CAST(COALESCE(SUM(a.reasoning_tokens),0) AS BIGINT) AS reasoning_tokens,CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS total_tokens";

async fn usage_totals(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    period: &UsagePeriod,
) -> Result<ModelUsageTotals, HarnessError> {
    let query = format!(
        "SELECT {TOTAL_COLUMNS} FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=$1 AND r.budget_period_start=$2"
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(tenant_id.as_str())
        .bind(format!("{}-01", period.label))
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    totals_from_row(&row)
}

async fn usage_groups(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    period: &UsagePeriod,
) -> Result<Vec<ModelUsageGroup>, HarnessError> {
    let query = format!(
        "SELECT r.provider_id AS route_id,r.model_id AS model,{TOTAL_COLUMNS} FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=$1 AND r.budget_period_start=$2 GROUP BY r.provider_id,r.model_id ORDER BY total_tokens DESC,r.provider_id,r.model_id"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(tenant_id.as_str())
        .bind(format!("{}-01", period.label))
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    rows.iter()
        .map(|row| {
            Ok(ModelUsageGroup {
                provider: row.try_get("route_id").map_err(database_error)?,
                model: row.try_get("model").map_err(database_error)?,
                usage: totals_from_row(row)?,
            })
        })
        .collect()
}

async fn usage_ledger(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    period: &UsagePeriod,
    limit: u32,
) -> Result<(Vec<ModelUsageLedgerEntry>, bool), HarnessError> {
    let rows=sqlx::query("SELECT r.request_id,r.run_id,r.actor_user_id,r.resource_owner_user_id,r.model_beneficiary_user_id,r.workload_json,r.provider_id AS route_id,r.model_id AS model,r.execution_reservation_id AS quota_reservation_id,COALESCE(q.state,'deleted') AS reservation_state,a.attempt,a.input_tokens,a.output_tokens,a.cached_input_tokens,a.cache_write_tokens,a.reasoning_tokens,a.accounted_tokens,a.upstream_request_id AS provider_request_id,COALESCE(a.settled_at_ms,a.created_at_ms) AS recorded_at_ms FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id LEFT JOIN control_quota_reservations q ON q.tenant_id=r.tenant_id AND q.reservation_id=r.execution_reservation_id WHERE r.tenant_id=$1 AND r.budget_period_start=$2 ORDER BY recorded_at_ms DESC,r.request_id,a.attempt DESC LIMIT $3")
        .bind(tenant_id.as_str()).bind(format!("{}-01",period.label)).bind(i64::from(limit)+1).fetch_all(&mut **transaction).await.map_err(database_error)?;
    let (rows, truncated) = cap(rows, limit);
    let entries = rows
        .iter()
        .map(ledger_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((entries, truncated))
}

const RESERVATION_COLUMNS: &str = "q.reservation_id,q.user_id,q.run_id,q.reserved_model_tokens,q.committed_model_tokens,q.unknown_model_tokens,q.state,q.created_at_ms,q.expires_at_ms,run.state AS run_state,CAST(COALESCE((SELECT SUM(a.accounted_tokens) FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=q.tenant_id AND r.execution_reservation_id=q.reservation_id),0) AS BIGINT) AS ledger_tokens";

async fn usage_reservations(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    period: &UsagePeriod,
    limit: u32,
    now_ms: u64,
) -> Result<(Vec<ModelUsageReservation>, bool), HarnessError> {
    let query = format!(
        "SELECT {RESERVATION_COLUMNS} FROM control_quota_reservations q LEFT JOIN cloud_runs run ON run.tenant_id=q.tenant_id AND run.run_id=q.run_id WHERE q.tenant_id=$1 AND q.period_start=$2 ORDER BY q.created_at_ms DESC,q.reservation_id LIMIT $3"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(tenant_id.as_str())
        .bind(format!("{}-01", period.label))
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    reservations_from_rows(rows, limit, now_ms)
}

async fn usage_anomalies(
    transaction: &mut ternilo_storage::Transaction,
    tenant_id: &TenantId,
    limit: u32,
    now_ms: u64,
) -> Result<(Vec<ModelUsageReservation>, bool), HarnessError> {
    let query = format!(
        "SELECT {RESERVATION_COLUMNS} FROM control_quota_reservations q LEFT JOIN cloud_runs run ON run.tenant_id=q.tenant_id AND run.run_id=q.run_id WHERE q.tenant_id=$1 AND ((q.state='active' AND (q.expires_at_ms<=$2 OR run.state IN ('succeeded','failed','cancelled','indeterminate'))) OR (q.state<>'active' AND q.unknown_model_tokens>0) OR (q.state='committed' AND COALESCE(q.committed_model_tokens,0) <> COALESCE((SELECT SUM(a.accounted_tokens) FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.tenant_id=q.tenant_id AND r.execution_reservation_id=q.reservation_id),0))) ORDER BY q.created_at_ms DESC,q.reservation_id LIMIT $3"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(query))
        .bind(tenant_id.as_str())
        .bind(to_i64(now_ms, "usage timestamp")?)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    reservations_from_rows(rows, limit, now_ms)
}

fn parse_period(period: &str) -> Result<(i32, i32), HarnessError> {
    if period.len() != 7 || period.as_bytes().get(4) != Some(&b'-') {
        return Err(HarnessError::invalid("model usage period must use YYYY-MM"));
    }
    let year = period[..4]
        .parse::<i32>()
        .map_err(|_| HarnessError::invalid("model usage period must use YYYY-MM"))?;
    let month = period[5..]
        .parse::<i32>()
        .map_err(|_| HarnessError::invalid("model usage period must use YYYY-MM"))?;
    if !(1_970..=9_999).contains(&year) || !(1..=12).contains(&month) {
        return Err(HarnessError::invalid(
            "model usage period must be a valid UTC month from 1970 onward",
        ));
    }
    Ok((year, month))
}

fn row_u64(row: &AnyRow, column: &str, label: &str) -> Result<u64, HarnessError> {
    from_i64(row.try_get(column).map_err(database_error)?, label)
}

fn totals_from_row(row: &AnyRow) -> Result<ModelUsageTotals, HarnessError> {
    Ok(ModelUsageTotals {
        requests: row_u64(row, "requests", "model usage request count")?,
        attempts: row_u64(row, "attempts", "model attempt count")?,
        unknown_attempts: row_u64(row, "unknown_attempts", "unknown attempt count")?,
        input_tokens: row_u64(row, "input_tokens", "model input tokens")?,
        output_tokens: row_u64(row, "output_tokens", "model output tokens")?,
        cached_input_tokens: row_u64(row, "cached_input_tokens", "cached model input tokens")?,
        total_tokens: row_u64(row, "total_tokens", "total model tokens")?,
        cache_write_tokens: row_u64(row, "cache_write_tokens", "cache write tokens")?,
        reasoning_tokens: row_u64(row, "reasoning_tokens", "reasoning tokens")?,
    })
}

fn ledger_from_row(row: &AnyRow) -> Result<ModelUsageLedgerEntry, HarnessError> {
    let principal: crate::WorkloadModelPrincipal = serde_json::from_str(
        &row.try_get::<String, _>("workload_json")
            .map_err(database_error)?,
    )
    .map_err(|error| HarnessError::execution(error.to_string()))?;
    let optional = |name: &str| {
        row.try_get::<Option<i64>, _>(name)
            .map_err(database_error)?
            .map(|value| from_i64(value, name))
            .transpose()
    };
    Ok(ModelUsageLedgerEntry {
        run_id: row.try_get("run_id").map_err(database_error)?,
        actor_user_id: principal.actor_user_id,
        resource_owner_user_id: principal.resource_owner_user_id,
        model_beneficiary_user_id: principal.model.beneficiary_user_id().clone(),
        lease_token: principal.lease_token,
        request_id: row.try_get("request_id").map_err(database_error)?,
        attempt: u32::try_from(row_u64(row, "attempt", "attempt")?)
            .map_err(|_| HarnessError::execution("stored model attempt is out of range"))?,
        provider: row.try_get("route_id").map_err(database_error)?,
        model: row.try_get("model").map_err(database_error)?,
        input_tokens: optional("input_tokens")?,
        output_tokens: optional("output_tokens")?,
        cached_input_tokens: optional("cached_input_tokens")?,
        cache_write_tokens: optional("cache_write_tokens")?,
        reasoning_tokens: optional("reasoning_tokens")?,
        accounted_tokens: optional("accounted_tokens")?,
        provider_request_id: row.try_get("provider_request_id").map_err(database_error)?,
        recorded_at_ms: row_u64(row, "recorded_at_ms", "model usage timestamp")?,
        reservation_id: row
            .try_get("quota_reservation_id")
            .map_err(database_error)?,
        reservation_state: row.try_get("reservation_state").map_err(database_error)?,
    })
}

fn reservations_from_rows(
    rows: Vec<AnyRow>,
    limit: u32,
    now_ms: u64,
) -> Result<(Vec<ModelUsageReservation>, bool), HarnessError> {
    let (rows, truncated) = cap(rows, limit);
    let reservations = rows
        .iter()
        .map(|row| reservation_from_row(row, now_ms))
        .collect::<Result<Vec<_>, HarnessError>>()?;
    Ok((reservations, truncated))
}

fn reservation_from_row(row: &AnyRow, now_ms: u64) -> Result<ModelUsageReservation, HarnessError> {
    let state = row.try_get::<String, _>("state").map_err(database_error)?;
    let expires_at_ms = row_u64(row, "expires_at_ms", "quota reservation expiry")?;
    let run_state = row
        .try_get::<Option<String>, _>("run_state")
        .map_err(database_error)?;
    let committed_tokens = row
        .try_get::<Option<i64>, _>("committed_model_tokens")
        .map_err(database_error)?
        .map(|value| from_i64(value, "committed model tokens"))
        .transpose()?;
    let ledger_tokens = row_u64(row, "ledger_tokens", "reservation ledger tokens")?;
    let mut issues = anomaly_kinds(
        &state,
        expires_at_ms,
        run_state.as_deref(),
        committed_tokens,
        ledger_tokens,
        now_ms,
    );
    let unknown_tokens = row_u64(row, "unknown_model_tokens", "unknown model tokens")?;
    if state != "active" && unknown_tokens > 0 {
        issues.push(ModelUsageAnomalyKind::UnknownModelUsage);
    }
    if unknown_tokens > 0 {
        issues.retain(|issue| *issue != ModelUsageAnomalyKind::MissingCommittedTokens);
    }
    Ok(ModelUsageReservation {
        reservation_id: row.try_get("reservation_id").map_err(database_error)?,
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        run_id: row.try_get("run_id").map_err(database_error)?,
        reserved_tokens: row_u64(row, "reserved_model_tokens", "reserved model tokens")?,
        committed_tokens,
        state,
        created_at_ms: row_u64(row, "created_at_ms", "quota reservation creation timestamp")?,
        expires_at_ms,
        run_state,
        ledger_tokens,
        unknown_tokens,
        issues,
    })
}

fn anomaly_kinds(
    state: &str,
    expires_at_ms: u64,
    run_state: Option<&str>,
    committed_tokens: Option<u64>,
    ledger_tokens: u64,
    now_ms: u64,
) -> Vec<ModelUsageAnomalyKind> {
    let mut issues = Vec::new();
    if state == "active" {
        if expires_at_ms <= now_ms {
            issues.push(ModelUsageAnomalyKind::ExpiredActive);
        }
        if matches!(
            run_state,
            Some("succeeded" | "failed" | "cancelled" | "indeterminate")
        ) {
            issues.push(ModelUsageAnomalyKind::TerminalRunActive);
        }
    }
    if state == "committed" {
        match committed_tokens {
            None => issues.push(ModelUsageAnomalyKind::MissingCommittedTokens),
            Some(committed) if run_state.is_some() && committed != ledger_tokens => {
                issues.push(ModelUsageAnomalyKind::CommittedUsageMismatch);
            }
            Some(_) => {}
        }
    }
    issues
}

fn cap<T>(mut rows: Vec<T>, limit: u32) -> (Vec<T>, bool) {
    let limit = limit as usize;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_and_anomaly_rules_are_explicit() {
        assert_eq!(parse_period("2026-08").unwrap(), (2026, 8));
        assert!(parse_period("2026-8").is_err());
        assert!(parse_period("2026-13").is_err());
        assert_eq!(
            anomaly_kinds("active", 10, Some("failed"), None, 0, 11),
            vec![
                ModelUsageAnomalyKind::ExpiredActive,
                ModelUsageAnomalyKind::TerminalRunActive,
            ],
        );
        assert_eq!(
            anomaly_kinds("committed", 20, Some("succeeded"), None, 10, 11),
            vec![ModelUsageAnomalyKind::MissingCommittedTokens],
        );
        assert_eq!(
            anomaly_kinds("committed", 20, Some("succeeded"), Some(11), 10, 11),
            vec![ModelUsageAnomalyKind::CommittedUsageMismatch],
        );
        assert!(anomaly_kinds("active", 20, Some("running"), None, 0, 11).is_empty());
    }
}
