use crate::{ComputerUsageTotals, ControlAction, ControlStore, ControlUser, PageQuery};
use serde::Serialize;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId, UserId};
use ternilo_storage::{Backend, database_error};

#[derive(Default, Serialize)]
pub struct ComputerModelUsageTotals {
    pub requests: u64,
    pub active_requests: u64,
    pub usage: ComputerUsageTotals,
}
#[derive(Serialize)]
pub struct ComputerModelUsageGroup {
    pub execution_executor_id: String,
    pub source_executor_id: String,
    pub execution_computer_name: String,
    pub source_computer_name: String,
    pub actor_user_id: UserId,
    pub model_owner_user_id: UserId,
    pub resource_owner_user_id: UserId,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub totals: ComputerModelUsageTotals,
}
#[derive(Serialize)]
pub struct ComputerModelUsageSummary {
    pub source: &'static str,
    pub period: String,
    pub observed_at_ms: u64,
    pub totals: ComputerModelUsageTotals,
    pub groups: Vec<ComputerModelUsageGroup>,
}

fn field(backend: Backend, column: &'static str, path: &'static str) -> String {
    match backend {
        Backend::Sqlite => format!("json_extract({column},'$.{path}')"),
        Backend::Postgres => format!("CAST({column} AS JSONB)#>>'{{{}}}'", path.replace('.', ",")),
    }
}
fn number(row: &sqlx::any::AnyRow, key: &str) -> Result<u64, HarnessError> {
    u64::try_from(row.try_get::<i64, _>(key).map_err(database_error)?)
        .map_err(|_| HarnessError::execution("invalid forwarded model usage count"))
}

impl ControlStore {
    pub async fn computer_model_usage_summary(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        period: Option<&str>,
        query: Option<&str>,
        now: u64,
    ) -> Result<ComputerModelUsageSummary, HarnessError> {
        let period = crate::store::period_bounds(period, now)?;
        let (pattern, _) = PageQuery {
            query: query.map(str::to_owned),
            ..PageQuery::default()
        }
        .parameters()?;
        let mut tx = self.database.tenant_read_transaction(tenant).await?;
        crate::store::require_action(&mut tx, tenant, &actor.user_id, ControlAction::TenantRead)
            .await?;
        let backend = ternilo_storage::backend(&tx);
        let provider = field(backend, "r.snapshot_json", "binding.provider_id");
        let model = field(backend, "r.snapshot_json", "binding.model");
        let protocol = field(backend, "r.snapshot_json", "protocol");
        let failure = field(backend, "a.report_json", "error_code");
        let counters = ["input_tokens", "output_tokens", "cached_input_tokens", "cache_write_tokens", "reasoning_tokens"].iter().map(|key| {
            let path = match *key {
                "input_tokens" => "usage.input_tokens", "output_tokens" => "usage.output_tokens",
                "cached_input_tokens" => "usage.cached_input_tokens", "cache_write_tokens" => "usage.cache_write_tokens", _ => "usage.reasoning_tokens",
            };
            let value = field(backend, "a.report_json", path);
            format!("CAST(SUM(CAST({value} AS BIGINT)) AS BIGINT) AS {key},COUNT({value}) AS {key}_reported")
        }).collect::<Vec<_>>().join(",");
        // Requests and attempt usage have separate counters: retries never duplicate requests.
        let statement = format!("SELECT r.execution_executor_id,r.source_executor_id,r.actor_user_id,r.model_owner_user_id,r.resource_owner_user_id,
            COALESCE(e.display_name,r.execution_executor_id) AS execution_computer_name,
            COALESCE(s.display_name,r.source_executor_id) AS source_computer_name,
            {provider} AS provider,{model} AS model,{protocol} AS protocol,
            COUNT(DISTINCT r.request_id) AS requests,
            COUNT(DISTINCT CASE WHEN r.state='pending' AND r.updated_at_ms >= $6 THEN r.request_id END) AS active_requests,
            COUNT(a.attempt) AS attempts,COUNT(a.finished_at_ms) AS completed,COUNT({failure}) AS failed,{counters}
            FROM control_computer_model_requests r
            LEFT JOIN control_computer_model_attempts a ON a.tenant_id=r.tenant_id AND a.request_id=r.request_id
            LEFT JOIN control_computer_management e ON e.tenant_id=r.tenant_id AND e.executor_id=r.execution_executor_id
            LEFT JOIN control_computer_management s ON s.tenant_id=r.tenant_id AND s.executor_id=r.source_executor_id
            WHERE r.tenant_id=$1 AND (r.actor_user_id=$2 OR r.model_owner_user_id=$2)
              AND r.created_at_ms >= $3 AND r.created_at_ms < $4
              AND (CAST($5 AS TEXT) IS NULL OR LOWER(r.request_id) LIKE $5 ESCAPE '!' OR LOWER(r.snapshot_json) LIKE $5 ESCAPE '!')
            GROUP BY r.execution_executor_id,r.source_executor_id,r.actor_user_id,r.model_owner_user_id,r.resource_owner_user_id,e.display_name,s.display_name,{provider},{model},{protocol}
            ORDER BY r.execution_executor_id,r.source_executor_id,r.actor_user_id,r.model_owner_user_id,r.resource_owner_user_id,{provider},{model},{protocol} LIMIT 10001");
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(tenant.as_str())
            .bind(actor.user_id.as_str())
            .bind(super::to_i64(period.start_ms, "usage period")?)
            .bind(super::to_i64(period.end_ms, "usage period")?)
            .bind(pattern)
            .bind(super::to_i64(
                now.saturating_sub(60_000),
                "active request lease",
            )?)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
        if rows.len() > 10_000 {
            return Err(HarnessError::invalid(
                "computer model usage has more than 10000 groups; narrow the model query",
            ));
        }
        let (totals, groups) = summarize(rows)?;
        tx.commit().await.map_err(database_error)?;
        Ok(ComputerModelUsageSummary {
            source: "computer_forwarded",
            period: period.label,
            observed_at_ms: now,
            totals,
            groups,
        })
    }
}

fn summarize(
    rows: Vec<sqlx::any::AnyRow>,
) -> Result<(ComputerModelUsageTotals, Vec<ComputerModelUsageGroup>), HarnessError> {
    let mut totals = ComputerModelUsageTotals::default();
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows {
        let usage = ComputerModelUsageTotals {
            requests: number(&row, "requests")?,
            active_requests: number(&row, "active_requests")?,
            usage: ComputerUsageTotals::from_row(&row)?,
        };
        totals.requests = totals
            .requests
            .checked_add(usage.requests)
            .ok_or_else(|| HarnessError::execution("forwarded request total overflow"))?;
        totals.active_requests = totals
            .active_requests
            .checked_add(usage.active_requests)
            .ok_or_else(|| HarnessError::execution("forwarded active total overflow"))?;
        totals.usage.add(&usage.usage)?;
        groups.push(ComputerModelUsageGroup {
            execution_executor_id: row
                .try_get("execution_executor_id")
                .map_err(database_error)?,
            source_executor_id: row.try_get("source_executor_id").map_err(database_error)?,
            execution_computer_name: row
                .try_get("execution_computer_name")
                .map_err(database_error)?,
            source_computer_name: row
                .try_get("source_computer_name")
                .map_err(database_error)?,
            actor_user_id: UserId::new(
                row.try_get::<String, _>("actor_user_id")
                    .map_err(database_error)?,
            ),
            model_owner_user_id: UserId::new(
                row.try_get::<String, _>("model_owner_user_id")
                    .map_err(database_error)?,
            ),
            resource_owner_user_id: UserId::new(
                row.try_get::<String, _>("resource_owner_user_id")
                    .map_err(database_error)?,
            ),
            provider: row.try_get("provider").map_err(database_error)?,
            model: row.try_get("model").map_err(database_error)?,
            protocol: row.try_get("protocol").map_err(database_error)?,
            totals: usage,
        });
    }
    Ok((totals, groups))
}
