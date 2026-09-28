#[path = "../../../ternilo-control/tests/support/postgres.rs"]
mod postgres_runtime;

use sqlx::Executor;
use ternilo_cloud::CloudStore;
use ternilo_control::{ControlStore, SecretCipher};

pub async fn initialize(admin_url: &str, role: &str, key: [u8; 32]) -> String {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, role, "server-contract-password").await;
    ControlStore::connect(admin_url, None, SecretCipher::from_key(key), 2)
        .await
        .unwrap();
    CloudStore::connect(admin_url, None, 2).await.unwrap();
    admin.close().await;
    super::support::database_url_for_role(admin_url, role, "server-contract-password")
}

pub async fn assert_scoped_without_schema_access(runtime_url: &str) {
    let runtime = sqlx::PgPool::connect(runtime_url).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_sessions")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(visible, 0, "Server runtime reads require a tenant scope");
    assert!(
        runtime
            .execute("CREATE TABLE unauthorized_schema_change (id BIGINT)")
            .await
            .is_err()
    );
    assert!(
        runtime
            .execute("UPDATE cloud_runtime_control SET claims_paused = 1")
            .await
            .is_err()
    );
    runtime.close().await;
}
