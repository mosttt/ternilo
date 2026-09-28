use std::time::Duration;

use ternilo_control::{
    AccountStatus, AccountStatusAction, ControlStore, InstanceMode, NativeRegistration,
    NativeSessionGrant, OidcPrincipal, OidcSessionIdentity, RegistrationMode, SecretCipher,
    TenantRole, UserInvitationRequest,
};
use ternilo_transport::ExecutorId;

const OLD_PASSWORD: &str = "original-native-password";
const NEW_PASSWORD: &str = " replacement-native-password ";

fn registration(username: &str) -> NativeRegistration {
    NativeRegistration {
        email: format!("{username}@example.test"),
        username: username.to_owned(),
        password: OLD_PASSWORD.to_owned(),
    }
}

async fn accounts(store: &ControlStore) -> (NativeSessionGrant, NativeSessionGrant) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    store
        .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let invitation = store
        .create_user_invitation(
            &owner.session.user,
            &UserInvitationRequest {
                tenant_id: None,
                role: TenantRole::Member,
                expires_in_seconds: 60,
            },
            1_002,
        )
        .await
        .unwrap();
    let member = store
        .accept_user_invitation(&invitation.token, &registration("member"), 1_003)
        .await
        .unwrap();
    (owner, member)
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify session revocation and resource preservation in one reset scenario."
)]
async fn recovery_contract(
    store: &ControlStore,
    owner: &NativeSessionGrant,
    member: &NativeSessionGrant,
) {
    let user = &owner.session.user;
    let old_identity = store.identity_session(user.clone()).await.unwrap();
    let enrollment = store
        .create_enrollment(
            user,
            &old_identity.personal_tenant_id,
            None,
            ExecutorId::new("recovery-node"),
            Duration::from_secs(60),
            1_004,
        )
        .await
        .unwrap();
    let node = store
        .consume_enrollment(&enrollment.token, 1_005)
        .await
        .unwrap();
    let principal = OidcPrincipal {
        issuer: "https://recovery.example.test".into(),
        subject: "linked-owner".into(),
        email: Some("owner@example.test".into()),
        display_name: Some("Owner".into()),
    };
    store
        .link_native_oidc(user, &principal, 1_006)
        .await
        .unwrap();
    let oidc = store
        .create_oidc_session(
            &OidcSessionIdentity {
                principal,
                nonce: "recovery-nonce".into(),
                upstream_refresh_token: Some("upstream-refresh".into()),
            },
            "recovery-binding",
            60_000,
            1_007,
        )
        .await
        .unwrap();
    let stale = store
        .verify_native_credentials("owner", OLD_PASSWORD)
        .await
        .unwrap();
    let reset = store
        .reset_native_password("OWNER", NEW_PASSWORD, 1_010)
        .await
        .unwrap();
    assert_eq!(&reset.user, user);
    assert_eq!(reset.native_sessions_revoked, 1);
    assert_eq!(reset.oidc_sessions_revoked, 1);
    assert!(
        store
            .authenticate_native_session(&owner.access_token, 1_011)
            .await
            .is_err()
    );
    assert!(
        store
            .login_native("owner", OLD_PASSWORD, 1_011)
            .await
            .is_err()
    );
    assert!(
        store
            .create_native_browser_session(stale, 1_011)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_oidc_session(&oidc.access_token, "recovery-binding", 1_011)
            .await
            .is_err()
    );
    assert!(
        store
            .oidc_refresh_session(
                oidc.refresh_token.as_ref().unwrap(),
                "recovery-binding",
                1_011
            )
            .await
            .is_err()
    );
    let recovered = store
        .login_native("owner", NEW_PASSWORD, 1_012)
        .await
        .unwrap();
    assert_eq!(recovered.session.user, *user);
    assert_eq!(
        recovered.session.personal_tenant_id,
        old_identity.personal_tenant_id
    );
    assert_eq!(
        recovered.session.personal_project_id,
        old_identity.personal_project_id
    );
    assert_eq!(recovered.session.platform_role, old_identity.platform_role);
    assert!(recovered.session.is_instance_owner);
    assert!(
        store
            .account_login_methods(user)
            .await
            .unwrap()
            .oidc
            .is_some()
    );
    assert_eq!(
        store
            .authenticate_node(&node.token, 1_012)
            .await
            .unwrap()
            .scope
            .user_id,
        user.user_id
    );
    assert!(
        store
            .authenticate_native_session(&member.access_token, 1_012)
            .await
            .is_ok()
    );
    let audit: String = sqlx::query_scalar(
        "SELECT metadata FROM control_platform_audit WHERE action='account.password.reset'",
    )
    .fetch_one(store.database().pool())
    .await
    .unwrap();
    assert!(audit.contains("operator_cli"));
    assert!(!audit.contains(OLD_PASSWORD));
    assert!(!audit.contains(NEW_PASSWORD));
    assert!(!audit.contains("argon2"));
}

#[expect(
    clippy::too_many_lines,
    reason = "Check failed recovery and account-state restrictions against the same identities."
)]
async fn rejected_recovery_contract(
    store: &ControlStore,
    owner: &NativeSessionGrant,
    member: &NativeSessionGrant,
) {
    let verified = store
        .verify_native_credentials("owner", NEW_PASSWORD)
        .await
        .unwrap();
    assert!(
        store
            .reset_native_password("owner", "short", 1_020)
            .await
            .is_err()
    );
    assert!(
        store
            .reset_native_password("missing", NEW_PASSWORD, 1_020)
            .await
            .is_err()
    );
    let session = store
        .create_native_browser_session(verified, 1_021)
        .await
        .unwrap();
    assert!(
        store
            .authenticate_native_session(&session.access_token, 1_022)
            .await
            .is_ok()
    );
    let banned = store
        .set_account_status(
            &owner.session.user,
            &member.session.user.user_id,
            AccountStatusAction::Ban,
            1,
            1_023,
        )
        .await
        .unwrap();
    store
        .reset_native_password("member", NEW_PASSWORD, 1_024)
        .await
        .unwrap();
    assert!(
        store
            .verify_native_credentials("member", NEW_PASSWORD)
            .await
            .is_ok()
    );
    assert!(
        store
            .login_native("member", NEW_PASSWORD, 1_025)
            .await
            .is_err()
    );
    let removed = store
        .set_account_status(
            &owner.session.user,
            &member.session.user.user_id,
            AccountStatusAction::Remove,
            banned.status_revision,
            1_026,
        )
        .await
        .unwrap();
    assert_eq!(removed.status, AccountStatus::Removed);
    assert!(
        store
            .reset_native_password("member", "cannot-revive-removed", 1_027)
            .await
            .is_err()
    );
    assert!(
        store
            .verify_native_credentials("member", "cannot-revive-removed")
            .await
            .is_err()
    );
    let settings = store.registration_settings().await.unwrap();
    store
        .set_registration_settings(
            &owner.session.user,
            RegistrationMode::Open,
            false,
            settings.revision,
            1_028,
        )
        .await
        .unwrap();
    let principal = OidcPrincipal {
        issuer: "https://recovery.example.test".into(),
        subject: "oidc-only".into(),
        email: Some("oidc-only@example.test".into()),
        display_name: None,
    };
    store
        .register_oidc(&principal, "oidc-only", "oidc-only@example.test", 1_029)
        .await
        .unwrap();
    assert!(
        store
            .reset_native_password("oidc-only", NEW_PASSWORD, 1_030)
            .await
            .is_err()
    );
}

async fn contract(url: &str, migration: Option<&str>) {
    let store = ControlStore::connect(url, migration, SecretCipher::from_key([73; 32]), 4)
        .await
        .unwrap();
    let (owner, member) = accounts(&store).await;
    recovery_contract(&store, &owner, &member).await;
    rejected_recovery_contract(&store, &owner, &member).await;
    store.database().close().await;
}

#[tokio::test]
async fn sqlite_password_recovery_preserves_identity_and_rejects_old_authentication() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("recovery.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_password_recovery_uses_restricted_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_recovery_test", "recovery-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_recovery_test").unwrap();
    runtime.set_password(Some("recovery-password")).unwrap();
    contract(runtime.as_str(), Some(&admin_url)).await;
    admin.close().await;
}
