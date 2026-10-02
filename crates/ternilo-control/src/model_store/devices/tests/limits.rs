use super::*;
use crate::{ModelAccessError, ModelAccessErrorKind, ModelRequestPermit};
use ternilo_protocol::{ErrorCode, ModelDeviceIdentity};

mod accounting;
mod authorization;
mod competition;
mod fixture;
mod management;
mod postgres;
mod rate;
use fixture::{Fixture, Source, input};

async fn sqlite() -> (tempfile::TempDir, Fixture) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("limits.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([71; 32]), 8)
        .await
        .unwrap();
    (directory, Fixture::new(store).await)
}

#[tokio::test]
async fn sqlite_device_limits_authorization_and_expiry() {
    let (_directory, fixture) = sqlite().await;
    authorization::contract(&fixture).await;
}

#[tokio::test]
async fn sqlite_device_rate_limits_roll_across_sources_and_calendar_boundaries() {
    let (_directory, fixture) = sqlite().await;
    Box::pin(rate::contract(&fixture)).await;
}

#[tokio::test]
async fn sqlite_device_limits_owner_only_updates_and_history() {
    let (_directory, fixture) = sqlite().await;
    management::contract(&fixture).await;
}

#[tokio::test]
async fn sqlite_device_limits_shared_accounting_across_sources() {
    let (_directory, fixture) = sqlite().await;
    accounting::shared_budget(&fixture).await;
}

#[tokio::test]
async fn sqlite_device_limits_expiry_revalidation_and_late_usage() {
    let (_directory, fixture) = sqlite().await;
    accounting::expiry(&fixture).await;
}

#[tokio::test]
async fn sqlite_device_limits_utc_month_and_unknown_reservations() {
    let (_directory, fixture) = sqlite().await;
    accounting::month_boundary(&fixture).await;
}

#[tokio::test]
async fn sqlite_device_limits_concurrent_admission_and_retries() {
    let (_directory, fixture) = sqlite().await;
    competition::contract(&fixture).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_device_limits_contract_under_runtime_rls() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(
        &admin,
        "ternilo_device_limits_test",
        "limits-test-password",
    )
    .await;
    let owner = ControlStore::connect(&url, None, SecretCipher::from_key([71; 32]), 1)
        .await
        .unwrap();
    owner.database().close().await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_device_limits_test").unwrap();
    runtime.set_password(Some("limits-test-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([71; 32]),
        8,
    )
    .await
    .unwrap();
    postgres::assert_restricted_runtime(&store).await;
    let fixture = Fixture::new(store).await;
    authorization::contract(&fixture).await;
    management::contract(&fixture).await;
    accounting::shared_budget(&fixture).await;
    accounting::expiry(&fixture).await;
    competition::contract(&fixture).await;
    accounting::month_boundary(&fixture).await;
    Box::pin(rate::contract(&fixture)).await;
    postgres::assert_scopes_cleared(&fixture.store).await;
    postgres::assert_transaction_scope_lifecycle(&fixture.store).await;
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_devices")
        .fetch_one(fixture.store.database.pool())
        .await
        .unwrap();
    assert_eq!(visible, 0);
    fixture.store.database().close().await;
    postgres::assert_schema_rejection(&admin, &url, runtime.as_str()).await;
    admin.close().await;
}
