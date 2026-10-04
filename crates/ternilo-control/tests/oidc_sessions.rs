use ternilo_control::{
    AccountStatusAction, ControlStore, ControlUser, InstanceMode, NativeRegistration,
    OidcPrincipal, OidcSessionGrant, OidcSessionIdentity, SecretCipher,
};

async fn initialize_owner(store: &ControlStore) -> ControlUser {
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".into(),
                username: "owner".into(),
                password: "oidc-test-password".into(),
            },
            1000,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1001)
        .await
        .unwrap();
    owner
}

async fn lifecycle(store: &ControlStore) -> (OidcSessionGrant, ControlUser, ControlUser) {
    let owner = initialize_owner(store).await;
    let identity = OidcSessionIdentity {
        principal: OidcPrincipal {
            issuer: "https://identity.example.test/".into(),
            subject: "immutable-subject".into(),
            email: Some("member@example.test".into()),
            display_name: Some("Member".into()),
        },
        nonce: "login-nonce".into(),
        upstream_refresh_token: Some("upstream-private-refresh".into()),
    };
    let member = store
        .upsert_user(&identity.principal, "member", 1002)
        .await
        .unwrap();
    let grant = store
        .create_oidc_session(&identity, "client-binding", 61000, 1003)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_oidc_session(&grant.access_token, &["client-binding"], 2000)
            .await
            .unwrap()
            .0,
        identity.principal
    );
    assert!(
        store
            .authenticate_oidc_session(&grant.access_token, &["other-client"], 2000)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_oidc_session(&grant.access_token, &["client-binding"], 61000)
            .await
            .is_err()
    );
    let previous_refresh = grant.refresh_token.as_deref().unwrap();
    assert!(
        store
            .oidc_refresh_session(previous_refresh, "other-client", 2000)
            .await
            .is_err()
    );
    let refresh = store
        .oidc_refresh_session(previous_refresh, "client-binding", 2000)
        .await
        .unwrap();
    assert_eq!(
        refresh.identity.upstream_refresh_token.as_deref(),
        Some("upstream-private-refresh")
    );
    let (first, second) = tokio::join!(
        store.replace_oidc_session(
            &identity,
            "client-binding",
            62000,
            (previous_refresh, refresh.expires_at_ms),
            2000
        ),
        store.replace_oidc_session(
            &identity,
            "client-binding",
            62000,
            (previous_refresh, refresh.expires_at_ms),
            2000
        ),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let next = first.or(second).unwrap();
    assert!(
        store
            .authenticate_oidc_session(&grant.access_token, &["client-binding"], 2001)
            .await
            .is_err()
    );
    assert!(
        store
            .oidc_refresh_session(previous_refresh, "client-binding", 2001)
            .await
            .is_err()
    );
    let ciphertext: Vec<u8> = sqlx::query_scalar("SELECT ciphertext FROM control_oidc_sessions")
        .fetch_one(store.database().pool())
        .await
        .unwrap();
    assert!(
        !ciphertext
            .windows(b"upstream-private-refresh".len())
            .any(|part| part == b"upstream-private-refresh")
    );
    (next, owner, member)
}

async fn contract(url: &str, migration: Option<&str>) {
    let store = ControlStore::connect(url, migration, SecretCipher::from_key([91; 32]), 2)
        .await
        .unwrap();
    let (grant, owner, member) = lifecycle(&store).await;
    assert_eq!(
        ControlStore::rotate_secret_master_key(
            migration.unwrap_or(url),
            &SecretCipher::from_key([91; 32]),
            &SecretCipher::from_key([92; 32])
        )
        .await
        .unwrap(),
        1
    );
    let reopened = ControlStore::connect(url, migration, SecretCipher::from_key([92; 32]), 2)
        .await
        .unwrap();
    assert!(
        reopened
            .authenticate_oidc_session(&grant.access_token, &["client-binding"], 3000)
            .await
            .is_ok()
    );
    let refresh = reopened
        .oidc_refresh_session(
            grant.refresh_token.as_deref().unwrap(),
            "client-binding",
            3000,
        )
        .await
        .unwrap();
    reopened
        .set_account_status(&owner, &member.user_id, AccountStatusAction::Ban, 1, 3001)
        .await
        .unwrap();
    assert!(
        reopened
            .authenticate_oidc_session(&grant.access_token, &["client-binding"], 3002)
            .await
            .is_err()
    );
    assert!(
        reopened
            .create_oidc_session(&refresh.identity, "client-binding", 61000, 3002)
            .await
            .is_err()
    );
    reopened
        .set_account_status(&owner, &member.user_id, AccountStatusAction::Unban, 2, 3003)
        .await
        .unwrap();
    assert!(
        reopened
            .oidc_refresh_session(
                grant.refresh_token.as_deref().unwrap(),
                "client-binding",
                3004
            )
            .await
            .is_err()
    );
    let next = reopened
        .create_oidc_session(&refresh.identity, "client-binding", 62000, 3004)
        .await
        .unwrap();
    reopened
        .revoke_oidc_session(&next.access_token)
        .await
        .unwrap();
    assert!(
        reopened
            .authenticate_oidc_session(&next.access_token, &["client-binding"], 3005)
            .await
            .is_err()
    );
    assert!(
        reopened
            .replace_oidc_session(
                &refresh.identity,
                "client-binding",
                64000,
                (
                    next.refresh_token.as_deref().unwrap(),
                    refresh.expires_at_ms
                ),
                3005
            )
            .await
            .is_err()
    );
    reopened.database().close().await;
    store.database().close().await;
}

#[tokio::test]
async fn sqlite_oidc_sessions_rotate_refreshes_and_preserve_revocation_and_secret_rotation() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("oidc.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_oidc_sessions_work_with_restricted_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_oidc_test", "oidc-session-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_oidc_test").unwrap();
    runtime.set_password(Some("oidc-session-password")).unwrap();
    contract(runtime.as_str(), Some(&admin_url)).await;
    admin.close().await;
}
