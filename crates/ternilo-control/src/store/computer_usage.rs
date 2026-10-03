mod summary;
pub(crate) use summary::initialize;
pub use summary::{
    ComputerUsageCount, ComputerUsageGroup, ComputerUsageSummary, ComputerUsageTotals,
};

use super::{ControlStore, require_action, set_tenant, to_i64, usage::period_bounds};
use crate::{ControlAction, ControlUser, PageQuery};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::BTreeMap;
use ternilo_protocol::{
    ErrorCode, HarnessError, InputAuthor, ProviderUsageRoute, ReportedModelUsage, RunId,
    SessionEvent, SessionEventKind, SessionId, TenantId,
};
use ternilo_storage::{Backend, Json, Transaction, database_error};
use ternilo_transport::ExecutorId;

#[derive(Clone, Debug, Serialize)]
pub struct ComputerProviderUsage {
    pub session_id: SessionId,
    pub session_title: String,
    pub run_id: RunId,
    pub started_seq: u64,
    pub started_at_ms: u64,
    pub step: u32,
    pub attempt: u32,
    pub route: ProviderUsageRoute,
    pub input_author: Option<InputAuthor>,
    pub finished_at_ms: Option<u64>,
    pub usage: Option<ReportedModelUsage>,
    pub error_code: Option<ErrorCode>,
    pub upstream_request_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ComputerProviderUsagePage {
    pub source: &'static str,
    pub period: String,
    pub observations: Vec<ComputerProviderUsage>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageCursor {
    time: u64,
    session: SessionId,
    seq: u64,
}

impl ControlStore {
    pub async fn computer_provider_usage(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        executor: &ExecutorId,
        period: Option<&str>,
        query: &PageQuery,
        now: u64,
    ) -> Result<ComputerProviderUsagePage, HarnessError> {
        let period = period_bounds(period, now)?;
        let mut tx = self.database.begin().await?;
        set_tenant(&mut tx, tenant).await?;
        require_owner(&mut tx, actor, tenant, executor).await?;
        let rows = start_rows(&mut tx, actor, tenant, executor, &period, query).await?;
        let mut observations = Vec::with_capacity(rows.len());
        let mut authors = BTreeMap::new();
        for row in rows {
            let metadata: Json<crate::EdgeSessionMetadata> =
                row.try_get("metadata_json").map_err(database_error)?;
            let public_session: String = row.try_get("public_session").map_err(database_error)?;
            let node_session = SessionId::new(
                row.try_get::<String, _>("node_session")
                    .map_err(database_error)?,
            );
            let start: Json<SessionEvent> = row.try_get("event_json").map_err(database_error)?;
            let key = (
                node_session.as_str().to_owned(),
                start.0.run_id.as_str().to_owned(),
            );
            if let std::collections::btree_map::Entry::Vacant(entry) = authors.entry(key.clone()) {
                entry.insert(
                    input_author(&mut tx, tenant, executor, &node_session, &start.0.run_id).await?,
                );
            }
            let author = authors.get(&key).cloned().flatten();
            let record = observation(
                &mut tx,
                tenant,
                executor,
                UsageSession {
                    node_id: node_session,
                    public_id: SessionId::new(public_session),
                    title: metadata.0.title,
                },
                start.0,
                author,
            )
            .await?;
            observations.push(record);
        }
        let next_cursor = query.finish(&mut observations, |value| {
            serde_json::to_string(&UsageCursor {
                time: value.started_at_ms,
                session: value.session_id.clone(),
                seq: value.started_seq,
            })
            .expect("usage cursor consists of scalar fields")
        });
        tx.commit().await.map_err(database_error)?;
        Ok(ComputerProviderUsagePage {
            source: "device_reported",
            period: period.label,
            observations,
            next_cursor,
        })
    }
}

struct UsageSession {
    public_id: SessionId,
    node_id: SessionId,
    title: String,
}

async fn start_rows(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant: &TenantId,
    executor: &ExecutorId,
    period: &super::usage::UsagePeriod,
    query: &PageQuery,
) -> Result<Vec<sqlx::any::AnyRow>, HarnessError> {
    let (pattern, cursor) = query.parameters()?;
    let cursor: Option<UsageCursor> = cursor
        .map(|value| {
            serde_json::from_str(&value)
                .map_err(|_| HarnessError::invalid("invalid device usage cursor"))
        })
        .transpose()?;
    if let Some(cursor) = &cursor {
        cursor.session.validate()?;
    }
    let backend = ternilo_storage::backend(tx);
    let time = format!(
        "CAST({} AS BIGINT)",
        event_field(backend, "e", "occurred_at_ms")
    );
    let kind = event_field(backend, "e", "type");
    let source = event_field(backend, "e", "source_session_id");
    let provider = event_field(backend, "e", "route.provider");
    let model = event_field(backend, "e", "route.model");
    // All SQL fragments are fixed identifiers and backend JSON expressions. Values are bound.
    let sql = format!("SELECT s.session_id AS public_session, e.session_id AS node_session, s.metadata_json, e.event_json
            FROM control_edge_events e JOIN control_edge_sessions s
              ON s.tenant_id=e.tenant_id AND s.executor_id=e.executor_id AND s.node_session_id=e.session_id
            WHERE e.tenant_id=$1 AND e.executor_id=$2 AND s.owner_user_id=$3
              AND {kind}='provider_usage_started' AND {source}=s.node_session_id
              AND {time}>=$4 AND {time}<$5
              AND (CAST($6 AS BIGINT) IS NULL OR {time}<$6 OR ({time}=$6 AND s.session_id<$7) OR ({time}=$6 AND s.session_id=$7 AND e.seq<$8))
              AND (CAST($9 AS TEXT) IS NULL OR LOWER({provider}) LIKE $9 ESCAPE '!' OR LOWER({model}) LIKE $9 ESCAPE '!')
            ORDER BY {time} DESC,s.session_id DESC,e.seq DESC LIMIT $10");
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .bind(actor.user_id.as_str())
        .bind(to_i64(period.start_ms, "usage period start")?)
        .bind(to_i64(period.end_ms, "usage period end")?)
        .bind(
            cursor
                .as_ref()
                .map(|value| to_i64(value.time, "usage cursor time"))
                .transpose()?,
        )
        .bind(cursor.as_ref().map(|value| value.session.as_str()))
        .bind(
            cursor
                .as_ref()
                .map(|value| to_i64(value.seq, "usage cursor sequence"))
                .transpose()?,
        )
        .bind(pattern)
        .bind(i64::from(query.limit) + 1)
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)
}

async fn observation(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    session: UsageSession,
    start: SessionEvent,
    input_author: Option<InputAuthor>,
) -> Result<ComputerProviderUsage, HarnessError> {
    let SessionEventKind::ProviderUsageStarted {
        step,
        attempt,
        route,
        ..
    } = start.kind
    else {
        return Err(HarnessError::execution(
            "device usage query returned a different event kind",
        ));
    };
    let backend = ternilo_storage::backend(tx);
    let kind = event_field(backend, "f", "type");
    let started = event_field(backend, "f", "started_seq");
    let run = event_field(backend, "f", "run_id");
    let sql = format!(
        "SELECT f.event_json FROM control_edge_events f
        WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3 AND seq>$4
          AND {kind}='provider_usage_finished' AND CAST({started} AS BIGINT)=$4 AND {run}=$5
        ORDER BY seq LIMIT 1"
    );
    let finish: Option<Json<SessionEvent>> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .bind(session.node_id.as_str())
        .bind(to_i64(start.seq, "usage sequence")?)
        .bind(start.run_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
    let mut record = ComputerProviderUsage {
        session_id: session.public_id,
        session_title: session.title,
        run_id: start.run_id,
        started_seq: start.seq,
        started_at_ms: start.occurred_at_ms,
        step,
        attempt,
        route,
        input_author,
        finished_at_ms: None,
        usage: None,
        error_code: None,
        upstream_request_id: None,
    };
    if let Some(Json(event)) = finish
        && let SessionEventKind::ProviderUsageFinished {
            usage,
            error_code,
            upstream_request_id,
            ..
        } = event.kind
    {
        record.finished_at_ms = Some(event.occurred_at_ms);
        record.usage = usage;
        record.error_code = error_code;
        record.upstream_request_id = upstream_request_id;
    }
    Ok(record)
}

async fn input_author(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    session: &SessionId,
    run_id: &RunId,
) -> Result<Option<InputAuthor>, HarnessError> {
    let backend = ternilo_storage::backend(tx);
    let kind = event_field(backend, "e", "type");
    let run = event_field(backend, "e", "run_id");
    let sql = format!(
        "SELECT e.event_json FROM control_edge_events e WHERE tenant_id=$1 AND executor_id=$2 AND session_id=$3 AND {kind}='user_message' AND {run}=$4 ORDER BY seq LIMIT 1"
    );
    let event: Option<Json<SessionEvent>> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .bind(session.as_str())
        .bind(run_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?;
    let Some(Json(mut event)) = event else {
        return Ok(None);
    };
    crate::edge_store::EdgeStore::project_event_provenance_in_transaction(
        tx,
        tenant,
        executor,
        session,
        std::slice::from_mut(&mut event),
    )
    .await?;
    if let SessionEventKind::UserMessage { provenance, .. } = event.kind {
        Ok(provenance.map(|value| value.author))
    } else {
        Ok(None)
    }
}

fn event_field(backend: Backend, alias: &'static str, field: &'static str) -> String {
    match backend {
        Backend::Postgres => format!(
            "({alias}.event_json::jsonb #>> '{{{}}}')",
            field.replace('.', ",")
        ),
        Backend::Sqlite => format!("json_extract({alias}.event_json, '$.{field}')"),
    }
}

async fn require_owner(
    tx: &mut Transaction,
    actor: &ControlUser,
    tenant: &TenantId,
    executor: &ExecutorId,
) -> Result<(), HarnessError> {
    executor.validate()?;
    require_action(tx, tenant, &actor.user_id, ControlAction::ExecutorRead).await?;
    let owned: Option<i64> = sqlx::query_scalar("SELECT 1 FROM control_executors WHERE tenant_id=$1 AND executor_id=$2 AND owner_user_id=$3")
        .bind(tenant.as_str()).bind(executor.as_str()).bind(actor.user_id.as_str()).fetch_optional(&mut **tx).await.map_err(database_error)?;
    if owned.is_none() {
        return Err(HarnessError::policy(
            "computer usage is visible only to its owner",
        ));
    }
    Ok(())
}
