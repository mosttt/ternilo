use ternilo_control::{
    AccountStatusAction, ControlStore, InstanceMode, NativeRegistration, RegistrationMode,
    SecretCipher,
};

const PASSWORD: &str = "email-test-password";

#[expect(
    clippy::too_many_lines,
    reason = "Exercise verification, recovery, revocation and one-use races as one identity lifecycle."
)]
async fn contract(url: &str, migration: Option<&str>) {
    let store = ControlStore::connect(url, migration, SecretCipher::from_key([39; 32]), 4)
        .await
        .unwrap();
    for _ in 0..5 {
        assert!(
            store
                .admit_email_delivery("192.0.2.1", 10_000)
                .await
                .unwrap()
        );
    }
    assert!(
        !store
            .admit_email_delivery("192.0.2.1", 10_001)
            .await
            .unwrap()
    );
    assert!(
        store
            .admit_email_delivery("192.0.2.2", 10_001)
            .await
            .unwrap()
    );
    assert!(
        store
            .admit_email_delivery("192.0.2.1", 70_000)
            .await
            .unwrap()
    );
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".into(),
                username: "owner".into(),
                password: PASSWORD.into(),
            },
            100_000,
        )
        .await
        .unwrap();
    let user = &owner.session.user;
    assert!(
        store
            .request_password_recovery("owner@example.test", 100_001)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .request_password_recovery("absent@example.test", 100_001)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .request_password_recovery("invalid", 100_001)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .account_email_status(user)
            .await
            .unwrap()
            .verified_at_ms
            .is_none()
    );
    let verification = store
        .request_email_verification(user, 100_002)
        .await
        .unwrap()
        .unwrap();
    assert!(verification.token.starts_with("ter_ev_"));
    assert_eq!(verification.email, "owner@example.test");
    assert!(
        store
            .request_email_verification(user, 100_003)
            .await
            .unwrap()
            .is_none(),
        "delivery has a persistent cooldown"
    );
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM control_email_challenges")
        .fetch_one(store.database().pool())
        .await
        .unwrap();
    assert!(!stored.contains(&verification.token));
    assert!(
        store
            .recover_native_password(&verification.token, "new-password", 100_003)
            .await
            .is_err(),
        "verification cannot reset a password"
    );
    store
        .set_instance_mode(user, InstanceMode::MultiUser, 1, 100_004)
        .await
        .unwrap();
    let settings = store.registration_settings().await.unwrap();
    store
        .set_registration_settings(
            user,
            RegistrationMode::Open,
            false,
            settings.revision,
            100_005,
        )
        .await
        .unwrap();
    let member = store
        .register_native(
            &NativeRegistration {
                email: "member@example.test".into(),
                username: "member".into(),
                password: PASSWORD.into(),
            },
            100_006,
        )
        .await
        .unwrap();
    let member = member.session.unwrap();
    assert!(
        store
            .verify_account_email(&member.session.user, &verification.token, 100_007)
            .await
            .is_err()
    );
    store
        .verify_account_email(user, &verification.token, 100_008)
        .await
        .unwrap();
    assert!(
        store
            .verify_account_email(user, &verification.token, 100_009)
            .await
            .is_err(),
        "verification is one-use"
    );
    assert_eq!(
        store
            .account_email_status(user)
            .await
            .unwrap()
            .verified_at_ms,
        Some(100_008)
    );
    assert!(
        store
            .request_email_verification(user, 100_010)
            .await
            .unwrap()
            .is_none()
    );
    let stale = store
        .request_password_recovery("OWNER@example.test", 100_011)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .request_password_recovery("owner@example.test", 100_012)
            .await
            .unwrap()
            .is_none()
    );
    let latest = store
        .request_password_recovery("owner@example.test", 160_012)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .recover_native_password(&stale.token, "new-password", 160_013)
            .await
            .is_err(),
        "resending invalidates the previous link"
    );
    assert!(
        store
            .verify_account_email(user, &latest.token, 160_014)
            .await
            .is_err()
    );
    let proof = store
        .verify_native_credentials("owner", PASSWORD)
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        store.recover_native_password(&latest.token, "new-password-one", 160_015),
        store.recover_native_password(&latest.token, "new-password-two", 160_015),
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "exactly one concurrent reset succeeds"
    );
    let new_password = if first.is_ok() {
        "new-password-one"
    } else {
        "new-password-two"
    };
    assert!(
        store
            .login_native("owner", PASSWORD, 160_016)
            .await
            .is_err()
    );
    assert!(
        store
            .create_native_browser_session(proof, 160_016)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_native_session(&owner.access_token, 160_016)
            .await
            .is_err()
    );
    let signed_in = store
        .login_native("owner", new_password, 160_017)
        .await
        .unwrap();
    assert_eq!(signed_in.session.user, *user);
    assert_eq!(
        signed_in.session.personal_project_id,
        owner.session.personal_project_id
    );
    assert!(
        store
            .authenticate_native_session(&member.access_token, 160_017)
            .await
            .is_ok()
    );
    let reset = store
        .request_password_recovery("owner@example.test", 160_018)
        .await
        .unwrap()
        .unwrap();
    store
        .change_native_password(user, new_password, "changed-password", 160_019)
        .await
        .unwrap();
    assert!(
        store
            .recover_native_password(&reset.token, PASSWORD, 160_020)
            .await
            .is_err(),
        "changing the password invalidates recovery links"
    );
    let expired = store
        .request_password_recovery("owner@example.test", 160_021)
        .await
        .unwrap()
        .unwrap();
    assert!(
        store
            .recover_native_password(&expired.token, PASSWORD, 160_021 + 15 * 60_000)
            .await
            .is_err()
    );
    sqlx::query("UPDATE control_users SET email='changed@example.test' WHERE user_id=$1")
        .bind(user.user_id.as_str())
        .execute(store.database().pool())
        .await
        .unwrap();
    assert!(
        store
            .account_email_status(user)
            .await
            .unwrap()
            .verified_at_ms
            .is_none()
    );
    assert!(
        store
            .request_password_recovery("owner@example.test", 1_100_000)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .request_password_recovery("changed@example.test", 1_100_000)
            .await
            .unwrap()
            .is_none()
    );
    let changed = store
        .request_email_verification(user, 1_100_001)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.email, "changed@example.test");
    let member_verification = store
        .request_email_verification(&member.session.user, 1_100_002)
        .await
        .unwrap()
        .unwrap();
    store
        .verify_account_email(&member.session.user, &member_verification.token, 1_100_003)
        .await
        .unwrap();
    let member_reset = store
        .request_password_recovery("member@example.test", 1_100_004)
        .await
        .unwrap()
        .unwrap();
    store
        .set_account_status(
            user,
            &member.session.user.user_id,
            AccountStatusAction::Ban,
            1,
            1_100_005,
        )
        .await
        .unwrap();
    assert!(
        store
            .request_password_recovery("member@example.test", 1_100_006)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .recover_native_password(&member_reset.token, "new-password", 1_100_006)
            .await
            .is_err()
    );
    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT metadata FROM control_platform_audit WHERE action LIKE 'account.%'",
    )
    .fetch_all(store.database().pool())
    .await
    .unwrap();
    for value in audit {
        assert!(
            !value.contains("password-one")
                && !value.contains("ter_ev_")
                && !value.contains("ter_pr_")
                && !value.contains("argon2")
        );
    }
    store.database().close().await;
}

#[tokio::test]
async fn sqlite_verified_email_recovery_is_single_use_and_preserves_identity() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("email.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_verified_email_recovery_uses_restricted_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_email_test", "email-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_email_test").unwrap();
    runtime.set_password(Some("email-password")).unwrap();
    contract(runtime.as_str(), Some(&admin_url)).await;
    admin.close().await;
}
