use std::time::Duration;

use serde_json::json;
use ternilo_storage::{Database, Json, lock};

const SCHEMA: &str = "
CREATE TABLE storage_contract_control (id TEXT PRIMARY KEY, value BIGINT NOT NULL, settings TEXT NOT NULL);
CREATE TABLE storage_contract_execution (id TEXT PRIMARY KEY REFERENCES storage_contract_control(id), value BIGINT NOT NULL);
";

#[tokio::test]
async fn sqlite_supports_the_shared_transaction_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("server.sqlite3").display()
    );
    let database = Database::connect(&url, 4).await.unwrap();
    shared_contract(database.clone()).await;
    database.close().await;
    let reopened = Database::connect(&url, 2).await.unwrap();
    reopened
        .initialize("storage-contract", 1, SCHEMA, "")
        .await
        .unwrap();
    assert_eq!(value(&reopened).await, 2);
    reopened.close().await;
}

#[tokio::test]
async fn schema_mismatch_preserves_existing_data_without_applying_replacement_sql() {
    let database = Database::connect("sqlite::memory:", 1).await.unwrap();
    database
        .initialize(
            "versioned",
            2,
            "CREATE TABLE retained (value TEXT); INSERT INTO retained VALUES ('original');",
            "",
        )
        .await
        .unwrap();
    for version in [1, 3] {
        let error = database
            .initialize("versioned", version, "DROP TABLE retained;", "")
            .await
            .unwrap_err();
        assert!(error.message.contains("versioned"));
        assert!(error.message.contains("is version 2"));
        assert!(
            error
                .message
                .contains(&format!("requires version {version}"))
        );
    }
    database
        .initialize("versioned", 2, "DROP TABLE retained;", "")
        .await
        .unwrap();
    let value: String = sqlx::query_scalar("SELECT value FROM retained")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(value, "original");
    database.close().await;
}

#[tokio::test]
#[ignore = "requires TERNILO_STORAGE_TEST_DATABASE_URL for a disposable PostgreSQL database"]
async fn postgres_supports_the_shared_transaction_contract() {
    let url = std::env::var("TERNILO_STORAGE_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_storage_test"));
    let database = Database::connect(&url, 4).await.unwrap();
    shared_contract(database.clone()).await;
    database.close().await;
}

#[tokio::test]
#[ignore = "requires TERNILO_STORAGE_TEST_DATABASE_URL for a disposable PostgreSQL database"]
async fn postgres_initialized_components_need_no_schema_write_privilege() {
    let url = std::env::var("TERNILO_STORAGE_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_storage_test"));
    let database = Database::connect(&url, 2).await.unwrap();
    let schema = "CREATE TABLE storage_runtime_marker (id BIGINT PRIMARY KEY)";
    database
        .initialize("runtime-contract", 1, schema, "")
        .await
        .unwrap();
    sqlx::raw_sql("CREATE ROLE ternilo_storage_runtime_test LOGIN PASSWORD 'fixture-runtime'; GRANT USAGE ON SCHEMA public TO ternilo_storage_runtime_test; GRANT SELECT ON ternilo_schema TO ternilo_storage_runtime_test;")
        .execute(database.pool()).await.unwrap();
    let mut runtime_url = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime_url
        .set_username("ternilo_storage_runtime_test")
        .unwrap();
    runtime_url.set_password(Some("fixture-runtime")).unwrap();
    let runtime = Database::connect(runtime_url.as_str(), 1).await.unwrap();
    runtime
        .initialize("runtime-contract", 1, schema, "")
        .await
        .unwrap();
    assert!(
        runtime
            .initialize(
                "forbidden-runtime-component",
                1,
                "CREATE TABLE storage_forbidden (id BIGINT)",
                ""
            )
            .await
            .is_err()
    );
    runtime.close().await;
    sqlx::raw_sql(
        "DROP OWNED BY ternilo_storage_runtime_test; DROP ROLE ternilo_storage_runtime_test;",
    )
    .execute(database.pool())
    .await
    .unwrap();
    database.close().await;
}

async fn shared_contract(database: Database) {
    database
        .initialize("storage-contract", 1, SCHEMA, "")
        .await
        .unwrap();
    database
        .initialize("storage-contract", 1, SCHEMA, "")
        .await
        .unwrap();
    let mut transaction = database.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO storage_contract_control (id, value, settings) VALUES ('task', 0, $1)",
    )
    .bind(Json(json!({"nested": [true, 42, "中文"]})))
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query("INSERT INTO storage_contract_execution (id, value) VALUES ('task', 0)")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let settings: Json<serde_json::Value> =
        sqlx::query_scalar("SELECT settings FROM storage_contract_control WHERE id = 'task'")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(settings.0, json!({"nested": [true, 42, "中文"]}));

    let mut transaction = database.begin().await.unwrap();
    sqlx::query("UPDATE storage_contract_control SET value = 99 WHERE id = 'task'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("UPDATE storage_contract_execution SET value = 99 WHERE id = 'task'")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(value(&database).await, 0);
    let execution: i64 =
        sqlx::query_scalar("SELECT value FROM storage_contract_execution WHERE id = 'task'")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(execution, 0);

    let mut first = database.begin().await.unwrap();
    lock(&mut first, "storage-contract:task").await.unwrap();
    let second_database = database.clone();
    let second = tokio::spawn(async move {
        let mut transaction = second_database.begin().await.unwrap();
        lock(&mut transaction, "storage-contract:task")
            .await
            .unwrap();
        sqlx::query("UPDATE storage_contract_control SET value = value + 1 WHERE id = 'task'")
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!second.is_finished());
    sqlx::query("UPDATE storage_contract_control SET value = value + 1 WHERE id = 'task'")
        .execute(&mut *first)
        .await
        .unwrap();
    first.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value(&database).await, 2);
}

async fn value(database: &Database) -> i64 {
    sqlx::query_scalar("SELECT value FROM storage_contract_control WHERE id = 'task'")
        .fetch_one(database.pool())
        .await
        .unwrap()
}
