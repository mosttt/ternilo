use ternilo_control::{ControlStore, ControlUser, NativeRegistration, SecretCipher};

async fn contract(url: &str, migration: Option<&str>) {
    let store = ControlStore::connect(url, migration, SecretCipher::from_key([81; 32]), 2)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".into(),
                username: "owner".into(),
                password: "owner-settings-password".into(),
            },
            1000,
        )
        .await
        .unwrap()
        .session
        .user;
    let outsider = ControlUser {
        user_id: ternilo_protocol::UserId::new("outsider"),
        username: "outsider".into(),
    };
    let secret = b"{\"turnstile\":{\"secret_key\":\"sensitive-authentication-key\"}}";
    assert_eq!(store.authentication_settings_revision().await.unwrap(), 0);
    assert!(
        store
            .set_authentication_settings(&outsider, 0, secret, 1001)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .set_authentication_settings(&owner, 0, secret, 1001)
            .await
            .unwrap(),
        1
    );
    assert!(
        store
            .set_authentication_settings(&owner, 0, b"stale", 1002)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .authentication_settings()
            .await
            .unwrap()
            .unwrap()
            .1
            .as_slice(),
        secret
    );
    let ciphertext: Vec<u8> =
        sqlx::query_scalar("SELECT ciphertext FROM control_authentication_settings")
            .fetch_one(store.database().pool())
            .await
            .unwrap();
    assert!(
        !ciphertext
            .windows(28)
            .any(|chunk| chunk == b"sensitive-authentication-key")
    );
    let audit: String = sqlx::query_scalar(
        "SELECT metadata FROM control_platform_audit WHERE action = 'instance.authentication'",
    )
    .fetch_one(store.database().pool())
    .await
    .unwrap();
    assert!(!audit.contains("secret"));
    rotation_contract(&store, &owner, url, migration, secret).await;
    store.database().close().await;
}

async fn rotation_contract(
    store: &ControlStore,
    owner: &ControlUser,
    url: &str,
    migration: Option<&str>,
    secret: &[u8],
) {
    assert!(
        ControlStore::rotate_secret_master_key(
            migration.unwrap_or(url),
            &SecretCipher::from_key([82; 32]),
            &SecretCipher::from_key([83; 32])
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .authentication_settings()
            .await
            .unwrap()
            .unwrap()
            .1
            .as_slice(),
        secret
    );
    assert_eq!(
        ControlStore::rotate_secret_master_key(
            migration.unwrap_or(url),
            &SecretCipher::from_key([81; 32]),
            &SecretCipher::from_key([83; 32])
        )
        .await
        .unwrap(),
        1
    );
    let reopened = ControlStore::connect(url, migration, SecretCipher::from_key([83; 32]), 2)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .authentication_settings()
            .await
            .unwrap()
            .unwrap()
            .1
            .as_slice(),
        secret
    );
    assert_eq!(
        reopened
            .set_authentication_settings(owner, 1, b"{}", 1003)
            .await
            .unwrap(),
        2
    );
    reopened.database().close().await;
}

#[tokio::test]
async fn sqlite_authentication_settings_are_owner_only_encrypted_revisioned_and_rotatable() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("settings.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_authentication_settings_use_production_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_auth_test", "authentication-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_auth_test").unwrap();
    runtime
        .set_password(Some("authentication-password"))
        .unwrap();
    contract(runtime.as_str(), Some(&admin_url)).await;
    admin.close().await;
}
