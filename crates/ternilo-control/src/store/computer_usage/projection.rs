use ternilo_protocol::{HarnessError, SessionId, TenantId};
use ternilo_storage::{Backend, Database, Transaction, database_error};
use ternilo_transport::ExecutorId;

pub(crate) async fn initialize(database: &Database) -> Result<(), HarnessError> {
    database
        .initialize(
            "computer_usage_projection",
            1,
            match database.backend() {
                Backend::Sqlite => {
                    concat!(include_str!("schema.sql"), include_str!("sqlite.sql"), ";")
                }
                Backend::Postgres => concat!(
                    include_str!("schema.sql"),
                    include_str!("postgres.sql"),
                    ";"
                ),
            },
            include_str!("postgres_access.sql"),
        )
        .await
}

/// Materialize only newly appended usage facts in the original journal transaction.
pub(crate) async fn project_events(
    tx: &mut Transaction,
    tenant: &TenantId,
    executor: &ExecutorId,
    session: &SessionId,
    first: i64,
    last: i64,
) -> Result<(), HarnessError> {
    let source = match ternilo_storage::backend(tx) {
        Backend::Sqlite => include_str!("sqlite.sql"),
        Backend::Postgres => include_str!("postgres.sql"),
    };
    let statement = format!(
        "{source} AND e.tenant_id=$1 AND e.executor_id=$2 AND e.session_id=$3 AND e.seq BETWEEN $4 AND $5 ON CONFLICT DO NOTHING"
    );
    sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(tenant.as_str())
        .bind(executor.as_str())
        .bind(session.as_str())
        .bind(first)
        .bind(last)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::initialize;
    use serde_json::json;
    use ternilo_storage::{Database, Json};

    #[tokio::test]
    async fn existing_usage_is_indexed_without_copying_reasoning_and_deletion_cascades() {
        let database = Database::connect("sqlite::memory:", 1).await.unwrap();
        database.initialize("source", 1, "CREATE TABLE control_edge_events(tenant_id TEXT,executor_id TEXT,session_id TEXT,seq BIGINT,event_json TEXT,PRIMARY KEY(tenant_id,executor_id,session_id,seq));", "").await.unwrap();
        for (seq, event) in [
            json!({"type":"provider_usage_started","run_id":"run","occurred_at_ms":1,"source_session_id":"session","route":{"provider":"test","model":"model","protocol":"openai-responses"}}),
            json!({"type":"assistant_reasoning_delta","run_id":"run","occurred_at_ms":2,"step":1,"delta":"private reasoning must stay in the canonical journal"}),
            json!({"type":"provider_usage_finished","run_id":"run","occurred_at_ms":3,"started_seq":0,"usage":{"input_tokens":0}}),
        ].into_iter().enumerate() {
            sqlx::query("INSERT INTO control_edge_events VALUES('tenant','computer','session',$1,$2)").bind(i64::try_from(seq).unwrap()).bind(Json(event)).execute(database.pool()).await.unwrap();
        }
        initialize(&database).await.unwrap();
        initialize(&database).await.unwrap();
        let projected: Vec<String> =
            sqlx::query_scalar("SELECT event_json FROM control_edge_usage_events ORDER BY seq")
                .fetch_all(database.pool())
                .await
                .unwrap();
        assert_eq!(projected.len(), 2);
        assert!(!projected.join("").contains("private reasoning"));
        sqlx::query("DELETE FROM control_edge_events WHERE seq=0")
            .execute(database.pool())
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM control_edge_usage_events")
                .fetch_one(database.pool())
                .await
                .unwrap(),
            1
        );
    }
}
