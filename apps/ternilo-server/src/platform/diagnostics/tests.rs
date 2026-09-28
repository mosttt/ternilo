use salvo_core::test::{ResponseExt, TestClient};
use ternilo_control::{ControlStore, SecretCipher};
use ternilo_protocol::{SessionId, SubmissionId, TenantId, UserId};
use ternilo_transport::{
    ApplicationOperation, CommandId, ExecutorCommand, ExecutorCommandBody, ExecutorId,
    ExecutorScope,
};

use crate::gateway_journal::{GatewayJournal, RouteKey};

use super::*;

async fn install(database: &Database) {
    ControlStore::from_database(database.clone(), SecretCipher::from_key([17; 32]))
        .await
        .unwrap();
    GatewayJournal::open(database.clone()).await.unwrap();
    initialize(database).await.unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise one shared HTTP and persistence contract on both database backends."
)]
async fn diagnostics_contract(database: Database) {
    let store = ControlStore::from_database(database.clone(), SecretCipher::from_key([17; 32]))
        .await
        .unwrap();
    let journal = GatewayJournal::open(database.clone()).await.unwrap();
    initialize(&database).await.unwrap();
    for tenant in ["diagnostics-a", "diagnostics-b"] {
        let route = RouteKey::new(TenantId::new(tenant), ExecutorId::new("same-node"));
        journal
            .acquire(&route, "server", 1, 100)
            .await
            .unwrap()
            .unwrap();
        journal
            .enqueue(
                &route,
                &ExecutorCommand {
                    input_provenance: None,
                    command_id: CommandId::new("same-command"),
                    scope: ExecutorScope {
                        tenant_id: route.tenant_id.clone(),
                        user_id: UserId::new("owner"),
                    },
                    issued_at_ms: 1,
                    expires_at_ms: 101,
                    body: ExecutorCommandBody::Application {
                        request: ApplicationOperation::SessionQueueRemove {
                            session_id: SessionId::new("session"),
                            submission_id: SubmissionId::new("submission"),
                        },
                    },
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(
        storage_status(&database).await.unwrap(),
        StorageStatus {
            executors: 2,
            commands: 2,
            events: 0,
        }
    );
    let edge = Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap());
    let service = salvo_core::Service::new(router(database.clone(), edge));
    let mut ready = TestClient::get("http://localhost/readyz")
        .send(&service)
        .await;
    assert_eq!(ready.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = ready.take_json().await.unwrap();
    assert_eq!(body["status"], "ready");
    assert_eq!(body["persisted_commands"], 2);
    assert_eq!(body["connected_executors"], 0);
    let mut metrics_response = TestClient::get("http://localhost/metrics")
        .send(&service)
        .await;
    assert_eq!(metrics_response.status_code, Some(StatusCode::OK));
    let text = metrics_response.take_string().await.unwrap();
    assert!(text.contains("ternilo_server_ready 1\n"));
    assert!(text.contains("ternilo_server_persisted_commands 2\n"));
    assert!(!text.contains("diagnostics-a"));
    assert!(!text.contains("same-command"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT probe_count FROM server_readiness")
            .fetch_one(database.pool())
            .await
            .unwrap(),
        3,
        "each readiness probe must commit its write"
    );
    if database.backend() == Backend::Postgres {
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM gateway_commands")
                .fetch_one(database.pool())
                .await
                .unwrap(),
            0,
            "aggregate diagnostics must not grant unscoped access to tenant records"
        );
        assert!(
            sqlx::query("CREATE TABLE forbidden_runtime_ddl (value BIGINT)")
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    database.close().await;
    let alive = TestClient::get("http://localhost/livez")
        .send(&service)
        .await;
    assert_eq!(alive.status_code, Some(StatusCode::OK));
    let failed = TestClient::get("http://localhost/readyz")
        .send(&service)
        .await;
    assert_eq!(failed.status_code, Some(StatusCode::SERVICE_UNAVAILABLE));
    let mut failed = TestClient::get("http://localhost/metrics")
        .send(&service)
        .await;
    assert_eq!(failed.status_code, Some(StatusCode::SERVICE_UNAVAILABLE));
    assert!(
        failed
            .take_string()
            .await
            .unwrap()
            .contains("ternilo_server_ready 0\n")
    );
}

#[tokio::test]
async fn sqlite_diagnostics_commit_writes_and_distinguish_liveness_from_readiness() {
    let database = Database::connect("sqlite::memory:", 1).await.unwrap();
    install(&database).await;
    diagnostics_contract(database).await;
}

#[tokio::test]
#[ignore = "requires a disposable PostgreSQL database named ternilo_server_diagnostics_test"]
async fn postgres_diagnostics_preserve_rls_and_work_with_a_non_superuser_schema_owner() {
    let url = std::env::var("TERNILO_SERVER_DIAGNOSTICS_TEST_DATABASE_URL").unwrap();
    let mut parsed = reqwest::Url::parse(&url).unwrap();
    assert_eq!(parsed.path(), "/ternilo_server_diagnostics_test");
    let administrator = Database::connect(&url, 1).await.unwrap();
    let suffix = format!("{:016x}", rand::random::<u64>());
    let owner = format!("diagnostics_owner_{suffix}");
    let runtime = format!("diagnostics_runtime_{suffix}");
    // Role identifiers are generated exclusively from a fixed prefix and hex digits.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA IF EXISTS public CASCADE;
         CREATE ROLE {owner} LOGIN PASSWORD 'fixture-only';
         CREATE ROLE {runtime} LOGIN PASSWORD 'fixture-only';
         CREATE SCHEMA public AUTHORIZATION {owner};
         GRANT USAGE ON SCHEMA public TO {runtime};"
    )))
    .execute(administrator.pool())
    .await
    .unwrap();
    parsed.set_username(&owner).unwrap();
    parsed.set_password(Some("fixture-only")).unwrap();
    let migration = Database::connect(parsed.as_str(), 1).await.unwrap();
    install(&migration).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT ON ternilo_schema TO {runtime};
         GRANT SELECT, UPDATE ON server_readiness TO {runtime};
         GRANT SELECT, INSERT, UPDATE, DELETE ON gateway_leases, gateway_commands TO {runtime};
         GRANT EXECUTE ON FUNCTION ternilo_server_storage_counts() TO {runtime};"
    )))
    .execute(migration.pool())
    .await
    .unwrap();
    migration.close().await;
    parsed.set_username(&runtime).unwrap();
    let database = Database::connect(parsed.as_str(), 2).await.unwrap();
    diagnostics_contract(database).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP OWNED BY {runtime}; DROP ROLE {runtime};
         DROP OWNED BY {owner} CASCADE; DROP ROLE {owner};"
    )))
    .execute(administrator.pool())
    .await
    .unwrap();
    administrator.close().await;
}
