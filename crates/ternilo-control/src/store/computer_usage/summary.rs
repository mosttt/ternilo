use super::{ControlStore, event_field};
use crate::{ControlUser, PageQuery};
use serde::Serialize;
use sqlx::Row;
use ternilo_protocol::{HarnessError, TenantId};
use ternilo_storage::{Database, database_error};
use ternilo_transport::ExecutorId;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ComputerUsageCount {
    pub tokens: Option<u64>,
    pub reported_attempts: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ComputerUsageTotals {
    pub attempts: u64,
    pub completed: u64,
    pub failed: u64,
    pub input: ComputerUsageCount,
    pub output: ComputerUsageCount,
    pub cached_input: ComputerUsageCount,
    pub cache_write: ComputerUsageCount,
    pub reasoning: ComputerUsageCount,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComputerUsageGroup {
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub totals: ComputerUsageTotals,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComputerUsageSummary {
    pub source: &'static str,
    pub period: String,
    pub observed_at_ms: u64,
    pub totals: ComputerUsageTotals,
    pub groups: Vec<ComputerUsageGroup>,
}

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "computer_usage_indexes",
            1,
            match database.backend() {
                ternilo_storage::Backend::Sqlite => include_str!("sqlite.sql"),
                ternilo_storage::Backend::Postgres => include_str!("postgres.sql"),
            },
            "",
        )
        .await
}

impl ControlStore {
    pub async fn computer_provider_usage_summary(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        period: Option<&str>,
        query: Option<&str>,
        now: u64,
    ) -> Result<ComputerUsageSummary, HarnessError> {
        let period = super::super::usage::period_bounds(period, now)?;
        let (pattern, _) = PageQuery {
            query: query.map(str::to_owned),
            ..PageQuery::default()
        }
        .parameters()?;
        let mut tx = self.database.tenant_transaction(tenant).await?;
        super::require_owner(&mut tx, actor, tenant, executor).await?;
        let backend = ternilo_storage::backend(&tx);
        let field = |alias, name| event_field(backend, alias, name);
        let time = field("e", "occurred_at_ms");
        let provider = field("e", "route.provider");
        let model = field("e", "route.model");
        let protocol = field("e", "route.protocol");
        let start_kind = field("e", "type");
        let source = field("e", "source_session_id");
        let finish_kind = field("f", "type");
        let finish_seq = field("f", "started_seq");
        let run = field("e", "run_id");
        let finish_run = field("f", "run_id");
        let failure = field("f", "error_code");
        let counts = ["input_tokens", "output_tokens", "cached_input_tokens", "cache_write_tokens", "reasoning_tokens"].iter().map(|name| {
            let value = event_field(backend, "f", match *name {
                "input_tokens" => "usage.input_tokens", "output_tokens" => "usage.output_tokens",
                "cached_input_tokens" => "usage.cached_input_tokens", "cache_write_tokens" => "usage.cache_write_tokens", _ => "usage.reasoning_tokens",
            });
            format!("CAST(SUM(CAST({value} AS BIGINT)) AS BIGINT) AS {name},COUNT({value}) AS {name}_reported")
        }).collect::<Vec<_>>().join(",");
        // Only fixed JSON paths and identifiers are interpolated; every request value is bound.
        let statement = format!("SELECT {provider} AS provider,{model} AS model,{protocol} AS protocol,
            COUNT(*) AS attempts,COUNT(f.seq) AS completed,COUNT({failure}) AS failed,{counts}
            FROM control_edge_events e JOIN control_edge_sessions s
              ON s.tenant_id=e.tenant_id AND s.executor_id=e.executor_id AND s.node_session_id=e.session_id
            LEFT JOIN control_edge_events f ON f.tenant_id=e.tenant_id AND f.executor_id=e.executor_id AND f.session_id=e.session_id
              AND f.seq=(SELECT MIN(f.seq) FROM control_edge_events f
                WHERE f.tenant_id=e.tenant_id AND f.executor_id=e.executor_id AND f.session_id=e.session_id AND f.seq>e.seq
                AND {finish_kind}='provider_usage_finished' AND CAST({finish_seq} AS BIGINT)=e.seq AND {finish_run}={run})
            WHERE e.tenant_id=$1 AND e.executor_id=$2 AND s.owner_user_id=$3
              AND {start_kind}='provider_usage_started' AND {source}=s.node_session_id
              AND CAST({time} AS BIGINT)>=$4 AND CAST({time} AS BIGINT)<$5
              AND (CAST($6 AS TEXT) IS NULL OR LOWER({provider}) LIKE $6 ESCAPE '!' OR LOWER({model}) LIKE $6 ESCAPE '!')
            GROUP BY {provider},{model},{protocol} ORDER BY {provider},{model},{protocol} LIMIT 10001");
        let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(tenant.as_str())
            .bind(executor.as_str())
            .bind(actor.user_id.as_str())
            .bind(super::to_i64(period.start_ms, "usage period")?)
            .bind(super::to_i64(period.end_ms, "usage period")?)
            .bind(pattern)
            .fetch_all(&mut *tx)
            .await
            .map_err(database_error)?;
        if rows.len() > 10_000 {
            return Err(HarnessError::invalid(
                "computer usage has more than 10000 model groups; narrow the provider or model query",
            ));
        }
        let (groups, totals) = summary_rows(rows)?;
        tx.commit().await.map_err(database_error)?;
        Ok(ComputerUsageSummary {
            source: "device_reported",
            period: period.label,
            observed_at_ms: now,
            totals,
            groups,
        })
    }
}

fn summary_rows(
    rows: Vec<sqlx::any::AnyRow>,
) -> Result<(Vec<ComputerUsageGroup>, ComputerUsageTotals), HarnessError> {
    let mut groups = Vec::with_capacity(rows.len());
    let mut totals = ComputerUsageTotals::default();
    for row in rows {
        let number = |name: &str| -> Result<u64, HarnessError> {
            u64::try_from(row.try_get::<i64, _>(name).map_err(database_error)?)
                .map_err(|_| HarnessError::execution("invalid device usage count"))
        };
        let count = |field: &str| -> Result<ComputerUsageCount, HarnessError> {
            Ok(ComputerUsageCount {
                tokens: row
                    .try_get::<Option<i64>, _>(field)
                    .map_err(database_error)?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| HarnessError::execution("invalid device usage tokens"))?,
                reported_attempts: number(format!("{field}_reported").as_str())?,
            })
        };
        let group = ComputerUsageTotals {
            attempts: number("attempts")?,
            completed: number("completed")?,
            failed: number("failed")?,
            input: count("input_tokens")?,
            output: count("output_tokens")?,
            cached_input: count("cached_input_tokens")?,
            cache_write: count("cache_write_tokens")?,
            reasoning: count("reasoning_tokens")?,
        };
        totals.add(&group)?;
        groups.push(ComputerUsageGroup {
            provider: row.try_get("provider").map_err(database_error)?,
            model: row.try_get("model").map_err(database_error)?,
            protocol: row.try_get("protocol").map_err(database_error)?,
            totals: group,
        });
    }
    Ok((groups, totals))
}

impl ComputerUsageTotals {
    fn add(&mut self, other: &Self) -> Result<(), HarnessError> {
        fn sum(a: u64, b: u64) -> Result<u64, HarnessError> {
            a.checked_add(b)
                .ok_or_else(|| HarnessError::execution("device usage total overflow"))
        }
        self.attempts = sum(self.attempts, other.attempts)?;
        self.completed = sum(self.completed, other.completed)?;
        self.failed = sum(self.failed, other.failed)?;
        for (target, source) in [
            (&mut self.input, &other.input),
            (&mut self.output, &other.output),
            (&mut self.cached_input, &other.cached_input),
            (&mut self.cache_write, &other.cache_write),
            (&mut self.reasoning, &other.reasoning),
        ] {
            target.reported_attempts = sum(target.reported_attempts, source.reported_attempts)?;
            if let Some(value) = source.tokens {
                target.tokens = Some(sum(target.tokens.unwrap_or(0), value)?);
            }
        }
        Ok(())
    }
}
