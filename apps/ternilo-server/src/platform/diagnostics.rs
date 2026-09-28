use std::sync::Arc;

use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Response, Router, Text, handler},
};
use salvo_extra::affix_state;
use serde_json::json;
use sqlx::Row;
use ternilo_protocol::HarnessError;
use ternilo_storage::{Backend, Database, database_error};
use ternilo_transport::EXECUTOR_PROTOCOL_VERSION;

use super::edge::EdgeGateway;

const SCHEMA: &str = "
CREATE TABLE server_readiness (
    singleton BIGINT PRIMARY KEY CHECK (singleton = 1),
    probe_count BIGINT NOT NULL
);
INSERT INTO server_readiness (singleton, probe_count) VALUES (1, 0);
";

const COUNTS: &str = "SELECT
    (SELECT COUNT(*) FROM gateway_leases) AS executors,
    (SELECT COUNT(*) FROM gateway_commands) AS commands,
    (SELECT COUNT(*) FROM control_edge_events) AS events";

const POSTGRES_SCHEMA: &str = "
CREATE FUNCTION ternilo_server_storage_counts()
RETURNS TABLE (executors BIGINT, commands BIGINT, events BIGINT)
LANGUAGE SQL STABLE SECURITY DEFINER SET search_path = pg_catalog, public
AS $$ SELECT
    (SELECT COUNT(*) FROM public.gateway_leases),
    (SELECT COUNT(*) FROM public.gateway_commands),
    (SELECT COUNT(*) FROM public.control_edge_events)
$$;
REVOKE ALL ON FUNCTION ternilo_server_storage_counts() FROM PUBLIC;
REVOKE ALL ON server_readiness FROM PUBLIC;
DO $$ BEGIN
IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
    GRANT SELECT, UPDATE ON server_readiness TO ternilo_runtime;
    GRANT EXECUTE ON FUNCTION ternilo_server_storage_counts() TO ternilo_runtime;
END IF;
END $$;
";

pub(super) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize("server_diagnostics", 1, SCHEMA, POSTGRES_SCHEMA)
        .await
}

#[derive(Clone)]
struct Diagnostics {
    database: Database,
    edge: Arc<EdgeGateway>,
}

pub(super) fn router(database: Database, edge: Arc<EdgeGateway>) -> Router {
    Router::new()
        .hoop(affix_state::inject(Diagnostics { database, edge }))
        .push(Router::with_path("livez").get(liveness))
        .push(Router::with_path("readyz").get(readiness))
        .push(Router::with_path("metrics").get(metrics))
}

fn state(depot: &Depot) -> &Diagnostics {
    depot
        .get_typed()
        .expect("diagnostics state middleware must run first")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StorageStatus {
    executors: i64,
    commands: i64,
    events: i64,
}

async fn storage_status(database: &Database) -> Result<StorageStatus, HarnessError> {
    let mut transaction = database.begin().await?;
    let changed = sqlx::query(
        "UPDATE server_readiness SET probe_count = probe_count + 1 WHERE singleton = 1",
    )
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?
    .rows_affected();
    if changed != 1 {
        return Err(HarnessError::execution("server readiness probe is missing"));
    }
    let row = sqlx::query(match database.backend() {
        Backend::Sqlite => COUNTS,
        Backend::Postgres => "SELECT * FROM ternilo_server_storage_counts()",
    })
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    let status = StorageStatus {
        executors: row.try_get("executors").map_err(database_error)?,
        commands: row.try_get("commands").map_err(database_error)?,
        events: row.try_get("events").map_err(database_error)?,
    };
    // A committed main-database write distinguishes readiness from a read-only probe.
    transaction.commit().await.map_err(database_error)?;
    Ok(status)
}

#[handler]
async fn liveness(depot: &mut Depot) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "connected_executors": state(depot).edge.connected_executors().await,
        "protocol_version": EXECUTOR_PROTOCOL_VERSION,
    }))
}

#[handler]
async fn readiness(depot: &mut Depot, response: &mut Response) {
    let state = state(depot);
    let status = storage_status(&state.database)
        .await
        .inspect_err(|error| eprintln!("server readiness probe failed: {error}"));
    if let Ok(status) = status {
        response.render(Json(json!({
            "status": "ready",
            "storage": "ready",
            "connected_executors": state.edge.connected_executors().await,
            "persisted_executors": status.executors,
            "persisted_commands": status.commands,
            "persisted_events": status.events,
            "protocol_version": EXECUTOR_PROTOCOL_VERSION,
        })));
    } else {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
        response.render(Json(
            json!({ "status": "not_ready", "storage": "unavailable" }),
        ));
    }
}

#[handler]
async fn metrics(depot: &mut Depot, response: &mut Response) {
    let state = state(depot);
    let connected = state.edge.connected_executors().await;
    let status = storage_status(&state.database)
        .await
        .inspect_err(|error| eprintln!("server metrics probe failed: {error}"));
    if status.is_err() {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
    }
    response.render(Text::Plain(prometheus(connected, status.ok())));
}

fn prometheus(connected: usize, status: Option<StorageStatus>) -> String {
    let ready = i32::from(status.is_some());
    let status = status.unwrap_or_default();
    format!(
        concat!(
            "# HELP ternilo_server_ready Whether the server database committed a readiness probe.\n",
            "# TYPE ternilo_server_ready gauge\n",
            "ternilo_server_ready {ready}\n",
            "# HELP ternilo_server_connected_executors Currently connected Node executors.\n",
            "# TYPE ternilo_server_connected_executors gauge\n",
            "ternilo_server_connected_executors {connected}\n",
            "# HELP ternilo_server_persisted_executors Executors known to the durable gateway.\n",
            "# TYPE ternilo_server_persisted_executors gauge\n",
            "ternilo_server_persisted_executors {executors}\n",
            "# HELP ternilo_server_persisted_commands Commands in the durable gateway.\n",
            "# TYPE ternilo_server_persisted_commands gauge\n",
            "ternilo_server_persisted_commands {commands}\n",
            "# HELP ternilo_server_persisted_events Canonical Node events cached by the server.\n",
            "# TYPE ternilo_server_persisted_events gauge\n",
            "ternilo_server_persisted_events {events}\n",
        ),
        ready = ready,
        connected = connected,
        executors = status.executors,
        commands = status.commands,
        events = status.events,
    )
}

#[cfg(test)]
mod tests;
