use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use ternilo_control::{
    AccountStatusAction, BrowserLoginKind, BrowserSessionAuthentication, BrowserSessions,
    ControlStore, ControlUser, InstanceMode, NativeRegistration, NativeSessionGrant, OidcPrincipal,
    SecretCipher,
};
use ternilo_protocol::ErrorCode;

struct Fixture {
    store: ControlStore,
    owner: NativeSessionGrant,
    member: NativeSessionGrant,
    principal: OidcPrincipal,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_database("sqlite::memory:", None).await
    }

    async fn with_database(url: &str, migration: Option<&str>) -> Self {
        let store = ControlStore::connect(url, migration, SecretCipher::from_key([73; 32]), 1)
            .await
            .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "sessions-owner@example.test".to_owned(),
                    username: "sessions-owner".to_owned(),
                    password: "sessions-test-password".to_owned(),
                },
                1_000,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 1_001)
            .await
            .unwrap();
        let principal = OidcPrincipal {
            issuer: "https://sessions.example.test".to_owned(),
            subject: "member".to_owned(),
            email: Some("member@example.test".to_owned()),
            display_name: Some("Member".to_owned()),
        };
        let member = store
            .upsert_user(&principal, "member", 1_002)
            .await
            .unwrap();
        let member = store.create_browser_session(member, 1_003).await.unwrap();
        Self {
            store,
            owner,
            member,
            principal,
        }
    }

    async fn list(&self, grant: &NativeSessionGrant, now: u64) -> BrowserSessions {
        self.store
            .list_browser_sessions(&grant.session.user, &native(grant), now)
            .await
            .unwrap()
    }

    async fn denied(
        &self,
        actor: &ControlUser,
        authentication: &BrowserSessionAuthentication,
        now: u64,
    ) {
        assert_eq!(
            self.store
                .list_browser_sessions(actor, authentication, now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
        assert_eq!(
            self.store
                .revoke_browser_session(actor, authentication, "knbs_unknown", now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
        assert_eq!(
            self.store
                .revoke_other_browser_sessions(actor, authentication, now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
}

fn native(grant: &NativeSessionGrant) -> BrowserSessionAuthentication {
    BrowserSessionAuthentication::NativeToken(grant.access_token.clone())
}

#[tokio::test]
async fn lists_only_owned_active_sessions_with_safe_stable_ids_and_current_marker() {
    let fixture = Fixture::new().await;
    let actor = &fixture.member.session.user;
    let newer = fixture
        .store
        .create_browser_session(actor.clone(), 2_000)
        .await
        .unwrap();
    let revoked = fixture
        .store
        .create_browser_session(actor.clone(), 2_001)
        .await
        .unwrap();
    fixture
        .store
        .logout_native_session(&revoked.access_token, 2_002)
        .await
        .unwrap();
    let expired = fixture
        .store
        .create_browser_session(actor.clone(), 2_003)
        .await
        .unwrap();
    sqlx::query("UPDATE control_browser_sessions SET expires_at_ms = 3000 WHERE user_id = $1 AND created_at_ms = 2003")
        .bind(actor.user_id.as_str()).execute(fixture.store.database().pool()).await.unwrap();
    let listed = fixture.list(&fixture.member, 3_000).await;
    assert_eq!(listed.current_login, BrowserLoginKind::Native);
    assert_eq!(listed.sessions.len(), 2);
    assert!(listed.sessions[0].is_current);
    assert_eq!(listed.sessions[0].created_at_ms, 1_003);
    assert_eq!(
        listed.sessions[0].expires_at_ms,
        fixture.member.session.expires_at_ms.unwrap()
    );
    assert!(!listed.sessions[1].is_current);
    assert_eq!(listed.sessions[1].created_at_ms, 2_000);
    let value = serde_json::to_value(&listed).unwrap();
    assert_eq!(value["sessions"][0].as_object().unwrap().len(), 8);
    let encoded = value.to_string();
    for grant in [&fixture.owner, &fixture.member, &newer, &revoked, &expired] {
        let stored_hash = Sha256::digest(grant.access_token.as_bytes()).iter().fold(
            String::new(),
            |mut encoded, byte| {
                write!(encoded, "{byte:02x}").unwrap();
                encoded
            },
        );
        assert!(!encoded.contains(&grant.access_token));
        assert!(!encoded.contains(&stored_hash));
    }
    for session in &listed.sessions {
        assert!(session.session_id.starts_with("knbs_"));
        assert!(
            fixture
                .store
                .authenticate_native_token(&session.session_id, 3_000)
                .await
                .is_err()
        );
    }
    let relisted = fixture.list(&newer, 3_001).await;
    assert_eq!(
        relisted.sessions[1].session_id,
        listed.sessions[0].session_id
    );
    assert_eq!(
        relisted.sessions[0].session_id,
        listed.sessions[1].session_id
    );
}

#[tokio::test]
async fn revocation_is_scoped_to_the_authenticated_account_even_for_the_owner() {
    let fixture = Fixture::new().await;
    let owner_id = fixture
        .list(&fixture.owner, 3_000)
        .await
        .sessions
        .remove(0)
        .session_id;
    let member_id = fixture
        .list(&fixture.member, 3_000)
        .await
        .sessions
        .remove(0)
        .session_id;
    for (actor, target) in [(&fixture.owner, &member_id), (&fixture.member, &owner_id)] {
        let cross_account = fixture
            .store
            .revoke_browser_session(&actor.session.user, &native(actor), target, 3_000)
            .await
            .unwrap_err();
        let unknown = fixture
            .store
            .revoke_browser_session(&actor.session.user, &native(actor), "knbs_missing", 3_000)
            .await
            .unwrap_err();
        assert_eq!(cross_account.code, ErrorCode::Conflict);
        assert_eq!(cross_account.message, unknown.message);
    }
    fixture
        .denied(&fixture.owner.session.user, &native(&fixture.member), 3_000)
        .await;
    fixture
        .denied(&fixture.member.session.user, &native(&fixture.owner), 3_000)
        .await;
    assert_eq!(fixture.list(&fixture.owner, 3_001).await.sessions.len(), 1);
    assert_eq!(fixture.list(&fixture.member, 3_001).await.sessions.len(), 1);
}

#[tokio::test]
async fn individual_bulk_and_current_revocation_preserve_logout_and_other_accounts() {
    let fixture = Fixture::new().await;
    let actor = &fixture.member.session.user;
    let second = fixture
        .store
        .create_browser_session(actor.clone(), 2_000)
        .await
        .unwrap();
    let third = fixture
        .store
        .create_browser_session(actor.clone(), 2_001)
        .await
        .unwrap();
    let listed = fixture.list(&second, 3_000).await;
    let second_id = &listed.sessions[0].session_id;
    let result = fixture
        .store
        .revoke_browser_session(actor, &native(&fixture.member), second_id, 3_000)
        .await
        .unwrap();
    assert_eq!(result.revoked_count, 1);
    assert!(!result.current_revoked);
    fixture.denied(actor, &native(&second), 3_001).await;
    assert_eq!(
        fixture
            .store
            .revoke_browser_session(actor, &native(&fixture.member), second_id, 3_001)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let result = fixture
        .store
        .revoke_other_browser_sessions(actor, &native(&fixture.member), 3_002)
        .await
        .unwrap();
    assert_eq!(result.revoked_count, 1);
    assert!(!result.current_revoked);
    assert!(
        fixture
            .store
            .authenticate_native_token(&third.access_token, 3_003)
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .revoke_other_browser_sessions(actor, &native(&fixture.member), 3_003)
            .await
            .unwrap()
            .revoked_count,
        0
    );
    let current_id = fixture
        .list(&fixture.member, 3_003)
        .await
        .sessions
        .remove(0)
        .session_id;
    let result = fixture
        .store
        .revoke_browser_session(actor, &native(&fixture.member), &current_id, 3_004)
        .await
        .unwrap();
    assert!(result.current_revoked);
    fixture.denied(actor, &native(&fixture.member), 3_005).await;
    fixture
        .store
        .logout_native_session(&fixture.member.access_token, 3_006)
        .await
        .unwrap();
    assert_eq!(fixture.list(&fixture.owner, 3_006).await.sessions.len(), 1);
}

#[tokio::test]
async fn expired_current_credentials_and_targets_cannot_manage_sessions() {
    let fixture = Fixture::new().await;
    let expired_id = fixture
        .list(&fixture.member, 2_000)
        .await
        .sessions
        .remove(0)
        .session_id;
    let expiry = fixture.member.session.expires_at_ms.unwrap();
    let fresh = fixture
        .store
        .create_browser_session(fixture.member.session.user.clone(), expiry - 1)
        .await
        .unwrap();
    fixture
        .denied(
            &fixture.member.session.user,
            &native(&fixture.member),
            expiry,
        )
        .await;
    let sessions = fixture.list(&fresh, expiry).await;
    assert_eq!(sessions.sessions.len(), 1);
    assert!(sessions.sessions[0].is_current);
    assert_eq!(
        fixture
            .store
            .revoke_browser_session(&fresh.session.user, &native(&fresh), &expired_id, expiry)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn oidc_manages_owned_native_sessions_without_claiming_to_revoke_external_login() {
    let fixture = Fixture::new().await;
    let actor = &fixture.member.session.user;
    let authentication = BrowserSessionAuthentication::VerifiedOidc(fixture.principal.clone());
    let listed = fixture
        .store
        .list_browser_sessions(actor, &authentication, 3_000)
        .await
        .unwrap();
    assert_eq!(listed.current_login, BrowserLoginKind::Oidc);
    assert_eq!(listed.sessions.len(), 1);
    assert!(!listed.sessions[0].is_current);
    let result = fixture
        .store
        .revoke_browser_session(
            actor,
            &authentication,
            &listed.sessions[0].session_id,
            3_001,
        )
        .await
        .unwrap();
    assert!(!result.current_revoked);
    assert_eq!(result.revoked_count, 1);
    fixture
        .store
        .create_browser_session(actor.clone(), 3_002)
        .await
        .unwrap();
    let result = fixture
        .store
        .revoke_other_browser_sessions(actor, &authentication, 3_003)
        .await
        .unwrap();
    assert_eq!(result.revoked_count, 1);
    assert!(!result.current_revoked);
    assert!(
        fixture
            .store
            .list_browser_sessions(actor, &authentication, 3_004)
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
    assert_eq!(fixture.list(&fixture.owner, 3_004).await.sessions.len(), 1);
    fixture
        .denied(&fixture.owner.session.user, &authentication, 3_005)
        .await;
    let mut invalid = fixture.principal.clone();
    invalid.subject = "somebody-else".to_owned();
    fixture
        .denied(
            actor,
            &BrowserSessionAuthentication::VerifiedOidc(invalid),
            3_005,
        )
        .await;
}

#[tokio::test]
async fn account_status_and_single_user_policy_are_rechecked_for_both_login_kinds() {
    let fixture = Fixture::new().await;
    let actor = &fixture.member.session.user;
    let oidc = BrowserSessionAuthentication::VerifiedOidc(fixture.principal.clone());
    for status in ["pending", "rejected", "banned", "removed"] {
        sqlx::query("UPDATE control_users SET status = $2 WHERE user_id = $1")
            .bind(actor.user_id.as_str())
            .bind(status)
            .execute(fixture.store.database().pool())
            .await
            .unwrap();
        fixture.denied(actor, &native(&fixture.member), 3_000).await;
        fixture.denied(actor, &oidc, 3_000).await;
    }
    sqlx::query("UPDATE control_users SET status = 'active' WHERE user_id = $1")
        .bind(actor.user_id.as_str())
        .execute(fixture.store.database().pool())
        .await
        .unwrap();
    fixture
        .store
        .set_instance_mode(
            &fixture.owner.session.user,
            InstanceMode::SingleUser,
            2,
            3_001,
        )
        .await
        .unwrap();
    fixture.denied(actor, &native(&fixture.member), 3_002).await;
    fixture.denied(actor, &oidc, 3_002).await;
    assert_eq!(fixture.list(&fixture.owner, 3_002).await.sessions.len(), 1);
}

#[tokio::test]
async fn ban_still_revokes_native_sessions_and_unban_does_not_restore_them() {
    let fixture = Fixture::new().await;
    let actor = &fixture.member.session.user;
    let banned = fixture
        .store
        .set_account_status(
            &fixture.owner.session.user,
            &actor.user_id,
            AccountStatusAction::Ban,
            1,
            3_000,
        )
        .await
        .unwrap();
    fixture.denied(actor, &native(&fixture.member), 3_001).await;
    fixture
        .store
        .logout_native_session(&fixture.member.access_token, 3_002)
        .await
        .unwrap();
    fixture
        .store
        .set_account_status(
            &fixture.owner.session.user,
            &actor.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            3_003,
        )
        .await
        .unwrap();
    fixture.denied(actor, &native(&fixture.member), 3_004).await;
    assert!(
        fixture
            .store
            .list_browser_sessions(
                actor,
                &BrowserSessionAuthentication::VerifiedOidc(fixture.principal.clone()),
                3_004
            )
            .await
            .unwrap()
            .sessions
            .is_empty()
    );
}

#[tokio::test]
async fn session_activity_preserves_first_source_throttles_updates_and_rejects_other_accounts() {
    activity_contract(Fixture::new().await).await;
}

async fn activity_contract(fixture: Fixture) {
    let actor = &fixture.member.session.user;
    let token = &fixture.member.access_token;
    let first = Some("192.0.2.10".parse().unwrap());
    fixture
        .store
        .record_browser_session_activity(actor, token, Some("test browser"), first, 4_000)
        .await
        .unwrap();
    fixture
        .store
        .record_browser_session_activity(actor, token, Some("changed browser"), first, 5_000)
        .await
        .unwrap();
    let listed = fixture.list(&fixture.member, 5_000).await;
    let details = &listed.sessions[0];
    assert_eq!(details.first_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(details.user_agent.as_deref(), Some("test browser"));
    assert_eq!(details.last_active_at_ms, Some(4_000));
    let changed = Some("2001:db8::1".parse().unwrap());
    fixture
        .store
        .record_browser_session_activity(&fixture.owner.session.user, token, None, changed, 6_000)
        .await
        .unwrap();
    assert_eq!(
        fixture.list(&fixture.member, 6_000).await.sessions[0]
            .last_ip
            .as_deref(),
        Some("192.0.2.10")
    );
    fixture
        .store
        .record_browser_session_activity(actor, token, None, changed, 7_000)
        .await
        .unwrap();
    let details = &fixture.list(&fixture.member, 7_000).await.sessions[0];
    assert_eq!(details.first_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(details.last_ip.as_deref(), Some("2001:db8::1"));
    assert_eq!(details.last_active_at_ms, Some(7_000));
    fixture
        .store
        .record_browser_session_activity(actor, token, None, changed, 68_000)
        .await
        .unwrap();
    assert_eq!(
        fixture.list(&fixture.member, 68_000).await.sessions[0].last_active_at_ms,
        Some(68_000)
    );
    fixture
        .store
        .logout_native_session(token, 69_000)
        .await
        .unwrap();
    fixture
        .store
        .record_browser_session_activity(actor, token, None, first, 70_000)
        .await
        .unwrap();
    let last: i64 =
        sqlx::query_scalar("SELECT MAX(last_active_at_ms) FROM control_browser_session_details")
            .fetch_one(fixture.store.database().pool())
            .await
            .unwrap();
    assert_eq!(last, 68_000);
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_browser_activity_uses_production_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(
        &admin,
        "ternilo_session_details_test",
        "session-details-password",
    )
    .await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime
        .set_username("ternilo_session_details_test")
        .unwrap();
    runtime
        .set_password(Some("session-details-password"))
        .unwrap();
    activity_contract(Fixture::with_database(runtime.as_str(), Some(&admin_url)).await).await;
    admin.close().await;
}
