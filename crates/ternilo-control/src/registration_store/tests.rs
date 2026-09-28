use std::collections::BTreeSet;

use ternilo_protocol::{ErrorCode, RunModelBinding};

use super::*;
use crate::{
    AccountListQuery, InstanceMode, PageQuery, PlatformRole, SecretCipher, TenantQuota, TenantRole,
    UserInvitationRequest,
};

fn registration(username: &str) -> NativeRegistration {
    NativeRegistration {
        email: format!("{}@example.test", username.trim().to_ascii_lowercase()),
        username: username.to_owned(),
        password: "registration-test-password".to_owned(),
    }
}

fn principal(subject: &str) -> OidcPrincipal {
    OidcPrincipal {
        issuer: "https://registration.example.test".to_owned(),
        subject: subject.to_owned(),
        email: Some(format!("{subject}@example.test")),
        display_name: Some(subject.to_owned()),
    }
}

async fn store() -> ControlStore {
    ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([29; 32]), 1)
        .await
        .unwrap()
}

async fn owner(store: &ControlStore) -> NativeSessionGrant {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    store
        .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    owner
}

fn account_invitation() -> UserInvitationRequest {
    UserInvitationRequest {
        tenant_id: None,
        role: TenantRole::Member,
        expires_in_seconds: 300,
    }
}

async fn resource_counts(store: &ControlStore) -> (i64, i64, i64) {
    let row = sqlx::query("SELECT (SELECT COUNT(*) FROM control_users) AS users, (SELECT COUNT(*) FROM control_account_spaces) AS spaces, (SELECT COUNT(*) FROM control_browser_sessions) AS sessions")
        .fetch_one(store.database.pool()).await.unwrap();
    (
        row.try_get("users").unwrap(),
        row.try_get("spaces").unwrap(),
        row.try_get("sessions").unwrap(),
    )
}

#[tokio::test]
async fn native_registration_requires_review_without_issuing_credentials() {
    native_review_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify pending admission, authorization, review conflicts and login across one complete persisted lifecycle."
)]
pub(crate) async fn native_review_contract(store: &ControlStore) {
    assert_eq!(
        store.registration_settings().await.unwrap(),
        RegistrationSettings::default()
    );
    assert!(
        store
            .register_native(&registration("before-setup"), 900)
            .await
            .is_err()
    );
    let owner = owner(store).await;
    let actor = &owner.session.user;
    let defaults = store.registration_settings().await.unwrap();
    assert!(
        store
            .register_native(&registration("closed"), 1_002)
            .await
            .is_err()
    );
    assert!(
        store
            .set_registration_settings(actor, RegistrationMode::Invite, true, 1, 1_003)
            .await
            .is_err()
    );
    assert_eq!(store.registration_settings().await.unwrap(), defaults);
    let settings = store
        .set_registration_settings(actor, RegistrationMode::Open, true, 1, 1_004)
        .await
        .unwrap();
    let registered = store
        .register_native(&registration("waiting"), 1_005)
        .await
        .unwrap();
    assert_eq!(registered.status, AccountStatus::Pending);
    assert!(registered.session.is_none());
    assert_eq!(resource_counts(store).await, (2, 2, 1));
    let pending = store
        .authenticate_native_credentials("waiting", "registration-test-password")
        .await
        .unwrap();
    assert_eq!(pending.user_id, registered.user_id);
    assert_eq!(
        store
            .identity_session(pending.clone())
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    assert_eq!(
        store
            .login_native("waiting", "registration-test-password", 1_006)
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    assert_eq!(
        resource_counts(store).await.2,
        1,
        "a denied login must roll back the token insert"
    );
    assert_eq!(
        store
            .list_model_keys(&pending, &PageQuery::default())
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    assert_eq!(
        store
            .set_account_role(actor, &pending.user_id, PlatformRole::Admin, 1, 1_006)
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    let binding = RunModelBinding::Platform {
        grant_id: "grant-placeholder".to_owned(),
        model_id: "model-placeholder".to_owned(),
        beneficiary_user_id: actor.user_id.clone(),
    };
    assert_eq!(
        store
            .resolve_workload_model_snapshot(
                &pending.user_id,
                &actor.user_id,
                &owner.session.personal_tenant_id,
                &binding,
                None,
                1_006
            )
            .await
            .err()
            .unwrap()
            .error
            .message,
        "account registration is pending approval"
    );
    assert_eq!(
        store
            .resolve_workload_model_snapshot(
                &actor.user_id,
                &pending.user_id,
                &owner.session.personal_tenant_id,
                &binding,
                None,
                1_006
            )
            .await
            .err()
            .unwrap()
            .error
            .message,
        "account registration is pending approval"
    );
    let auditor = store
        .upsert_user(&principal("auditor"), "auditor", 1_006)
        .await
        .unwrap();
    store
        .set_account_role(actor, &auditor.user_id, PlatformRole::Auditor, 1, 1_007)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_accounts(
                &auditor,
                &AccountListQuery {
                    status: Some(AccountStatus::Pending),
                    ..AccountListQuery::default()
                }
            )
            .await
            .unwrap()
            .accounts
            .len(),
        1
    );
    for denied_actor in [&pending, &auditor] {
        assert!(
            store
                .review_account_registration(
                    denied_actor,
                    &pending.user_id,
                    RegistrationDecision::Approve,
                    1,
                    1_008
                )
                .await
                .is_err()
        );
        assert!(
            store
                .set_registration_settings(
                    denied_actor,
                    RegistrationMode::Open,
                    false,
                    settings.revision,
                    1_008
                )
                .await
                .is_err()
        );
    }
    assert!(
        store
            .review_account_registration(
                actor,
                &actor.user_id,
                RegistrationDecision::Reject,
                1,
                1_008
            )
            .await
            .is_err()
    );
    let approved = store
        .review_account_registration(
            actor,
            &pending.user_id,
            RegistrationDecision::Approve,
            1,
            1_009,
        )
        .await
        .unwrap();
    assert_eq!(
        (approved.status, approved.status_revision),
        (AccountStatus::Active, 2)
    );
    assert_eq!(
        store
            .review_account_registration(
                actor,
                &pending.user_id,
                RegistrationDecision::Reject,
                1,
                1_010
            )
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::Conflict
    );
    let session = store
        .login_native("waiting", "registration-test-password", 1_011)
        .await
        .unwrap();
    assert_eq!(session.session.user.user_id, pending.user_id);
    assert_eq!(session.session.platform_role, PlatformRole::User);
    let rejected = store
        .register_native(&registration("rejected"), 1_012)
        .await
        .unwrap();
    let admin = store
        .upsert_user(&principal("admin"), "admin", 1_012)
        .await
        .unwrap();
    store
        .set_account_role(actor, &admin.user_id, PlatformRole::Admin, 1, 1_013)
        .await
        .unwrap();
    let rejected = store
        .review_account_registration(
            &admin,
            &rejected.user_id,
            RegistrationDecision::Reject,
            1,
            1_014,
        )
        .await
        .unwrap();
    assert_eq!(rejected.status, AccountStatus::Rejected);
    assert_eq!(
        store
            .login_native("rejected", "registration-test-password", 1_015)
            .await
            .err()
            .unwrap()
            .message,
        "account registration was rejected"
    );
    assert_eq!(
        store
            .login_native("rejected", "wrong-password", 1_015)
            .await
            .err()
            .unwrap()
            .message,
        "invalid username or password"
    );
    store
        .set_registration_settings(
            &admin,
            RegistrationMode::Open,
            false,
            settings.revision,
            1_016,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .login_native("rejected", "registration-test-password", 1_017)
            .await
            .err()
            .unwrap()
            .message,
        "account registration was rejected"
    );
    let open = store
        .register_native(&registration("immediate"), 1_018)
        .await
        .unwrap();
    assert_eq!(open.status, AccountStatus::Active);
    let grant = open.session.unwrap();
    assert_eq!(
        store
            .authenticate_native_session(&grant.access_token, 1_019)
            .await
            .unwrap()
            .user
            .user_id,
        open.user_id
    );
    let audit = sqlx::query("SELECT action, metadata FROM control_platform_audit WHERE action LIKE 'account.registration%' ORDER BY sequence")
        .fetch_all(store.database.pool()).await.unwrap();
    assert_eq!(audit.len(), 5);
    for row in audit {
        let metadata: String = row.try_get("metadata").unwrap();
        assert!(!metadata.contains("password"));
        assert!(!metadata.contains(&grant.access_token));
    }
}

#[tokio::test]
async fn registration_modes_exclude_account_invites_and_preserve_team_joins() {
    invitation_gate_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify mode switching cannot admit accounts through stale account links or team invitations."
)]
pub(crate) async fn invitation_gate_contract(store: &ControlStore) {
    let owner = owner(store).await;
    let actor = &owner.session.user;
    let link = store
        .create_user_invitation(actor, &account_invitation(), 1_002)
        .await
        .unwrap();
    let open = store
        .set_registration_settings(actor, RegistrationMode::Open, true, 1, 1_003)
        .await
        .unwrap();
    let before = resource_counts(store).await;
    assert_eq!(
        store
            .create_user_invitation(actor, &account_invitation(), 1_004)
            .await
            .err()
            .unwrap()
            .message,
        "account invitations are disabled while open registration is enabled"
    );
    assert!(
        store
            .accept_user_invitation(&link.token, &registration("stale-link"), 1_004)
            .await
            .is_err()
    );
    assert_eq!(resource_counts(store).await, before);
    let invite = store
        .set_registration_settings(actor, RegistrationMode::Invite, false, open.revision, 1_005)
        .await
        .unwrap();
    let invited = store
        .accept_user_invitation(&link.token, &registration("invited"), 1_006)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_account(actor, &invited.session.user.user_id)
            .await
            .unwrap()
            .status,
        AccountStatus::Active
    );
    assert_eq!(invited.session.platform_role, PlatformRole::User);
    assert!(
        store
            .accept_user_invitation(&link.token, &registration("replay"), 1_006)
            .await
            .is_err()
    );
    let team = store
        .create_tenant(
            actor,
            "registration-team",
            "Registration team",
            TenantQuota::default(),
            1_007,
        )
        .await
        .unwrap();
    let team_request = UserInvitationRequest {
        tenant_id: Some(team.tenant_id.clone()),
        role: TenantRole::Member,
        expires_in_seconds: 300,
    };
    let team_link = store
        .create_user_invitation(actor, &team_request, 1_008)
        .await
        .unwrap();
    let before = resource_counts(store).await;
    assert_eq!(
        store
            .accept_user_invitation(&team_link.token, &registration("bypass"), 1_009)
            .await
            .err()
            .unwrap()
            .message,
        "team invitations require an existing active account; register or sign in first"
    );
    assert_eq!(resource_counts(store).await, before);
    let open = store
        .set_registration_settings(actor, RegistrationMode::Open, true, invite.revision, 1_010)
        .await
        .unwrap();
    let pending = store
        .register_native(&registration("pending-team"), 1_011)
        .await
        .unwrap();
    let pending_user = store
        .authenticate_native_credentials("pending-team", "registration-test-password")
        .await
        .unwrap();
    assert_eq!(
        store
            .join_invitation(&pending_user, &team_link.token, 1_012)
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    store
        .review_account_registration(
            actor,
            &pending.user_id,
            RegistrationDecision::Approve,
            1,
            1_013,
        )
        .await
        .unwrap();
    let joined = store
        .join_invitation(&pending_user, &team_link.token, 1_014)
        .await
        .unwrap();
    assert_eq!(joined.tenant_id, team.tenant_id);
    assert_eq!(joined.role, TenantRole::Member);
    let still_available = store
        .create_user_invitation(actor, &team_request, 1_015)
        .await
        .unwrap();
    assert_eq!(
        store
            .join_invitation(&invited.session.user, &still_available.token, 1_016)
            .await
            .unwrap()
            .tenant_id,
        team.tenant_id
    );
    assert!(
        store
            .set_registration_settings(
                actor,
                RegistrationMode::Invite,
                false,
                invite.revision,
                1_017
            )
            .await
            .is_err()
    );
    assert_eq!(store.registration_settings().await.unwrap(), open);
    store
        .set_instance_mode(actor, InstanceMode::SingleUser, 2, 1_018)
        .await
        .unwrap();
    assert!(
        store
            .register_native(&registration("single-mode"), 1_019)
            .await
            .is_err()
    );
    assert!(
        store
            .authenticate_native_session(&owner.access_token, 1_020)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn first_oidc_identity_obeys_registration_and_does_not_reset_review() {
    oidc_gate_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify first OIDC admission, persisted review state, concurrent identity creation and subsequent linked-account login together."
)]
pub(crate) async fn oidc_gate_contract(store: &ControlStore) {
    let owner = owner(store).await;
    let actor = &owner.session.user;
    let identity = principal("external");
    let before = resource_counts(store).await;
    assert_eq!(
        store
            .authenticate_oidc_user(&identity, 1_002)
            .await
            .err()
            .unwrap()
            .message,
        "registration requires an administrator invitation"
    );
    assert_eq!(resource_counts(store).await, before);
    let settings = store
        .set_registration_settings(actor, RegistrationMode::Open, true, 1, 1_003)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_oidc_user(&identity, 1_004)
            .await
            .err()
            .unwrap()
            .message,
        "choose a platform username to finish registration"
    );
    assert_eq!(
        resource_counts(store).await,
        before,
        "OIDC login cannot silently create an unnamed account"
    );
    let (first, second) = tokio::join!(
        store.register_oidc(&identity, "chosen-external", "external@example.test", 1_004),
        store.register_oidc(&identity, "other-name", "external@example.test", 1_004)
    );
    let first = first.unwrap();
    assert_eq!(first.status, AccountStatus::Pending);
    assert_eq!(second.unwrap().user_id, first.user_id);
    let pending = store
        .authenticate_oidc_user(&identity, 1_004)
        .await
        .unwrap();
    let username = pending.username.clone();
    assert!(matches!(
        username.as_str(),
        "chosen-external" | "other-name"
    ));
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_platform_audit WHERE action='account.registration' AND resource_id=$1")
        .bind(pending.user_id.as_str()).fetch_one(store.database.pool()).await.unwrap();
    assert_eq!(audits, 1);
    assert_eq!(resource_counts(store).await, (2, 2, 1));
    assert_eq!(
        store
            .identity_session(pending.clone())
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    let settings = store
        .set_registration_settings(
            actor,
            RegistrationMode::Open,
            false,
            settings.revision,
            1_005,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_oidc_user(&identity, 1_006)
            .await
            .unwrap()
            .user_id,
        pending.user_id
    );
    assert_eq!(
        store
            .identity_session(pending.clone())
            .await
            .err()
            .unwrap()
            .message,
        "account registration is pending approval"
    );
    store
        .review_account_registration(
            actor,
            &pending.user_id,
            RegistrationDecision::Reject,
            1,
            1_007,
        )
        .await
        .unwrap();
    let mut updated_identity = identity.clone();
    updated_identity.email = Some("renamed@example.test".to_owned());
    updated_identity.display_name = Some("Renamed external display".to_owned());
    let again = store
        .authenticate_oidc_user(&updated_identity, 1_008)
        .await
        .unwrap();
    assert_eq!(again.username, username);
    assert_eq!(
        store.identity_session(again).await.err().unwrap().message,
        "account registration was rejected"
    );
    let registered = store
        .register_oidc(
            &principal("immediate"),
            "immediate",
            "immediate@example.test",
            1_009,
        )
        .await
        .unwrap();
    assert_eq!(registered.status, AccountStatus::Active);
    let immediate = store
        .authenticate_oidc_user(&principal("immediate"), 1_009)
        .await
        .unwrap();
    assert!(store.identity_session(immediate.clone()).await.is_ok());
    store
        .set_registration_settings(
            actor,
            RegistrationMode::Invite,
            false,
            settings.revision,
            1_010,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_oidc_user(&principal("immediate"), 1_011)
            .await
            .unwrap()
            .user_id,
        immediate.user_id
    );
    if store.database.backend() == ternilo_storage::Backend::Postgres {
        let mut policy_transaction = store.database.begin().await.unwrap();
        lock(&mut policy_transaction, "ternilo:instance")
            .await
            .unwrap();
        let known = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            store.authenticate_oidc_user(&principal("immediate"), 1_011),
        )
        .await
        .expect("known OIDC identities must not wait for the global admission policy lock")
        .unwrap();
        assert_eq!(known.user_id, immediate.user_id);
        policy_transaction.rollback().await.unwrap();
    }
    let owner_identity = principal("linked-owner");
    store
        .link_native_oidc(actor, &owner_identity, 1_012)
        .await
        .unwrap();
    assert_eq!(
        store
            .authenticate_oidc_user(&owner_identity, 1_013)
            .await
            .unwrap()
            .user_id,
        actor.user_id
    );
    let home_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_account_spaces WHERE user_id=$1")
            .bind(pending.user_id.as_str())
            .fetch_one(store.database.pool())
            .await
            .unwrap();
    assert_eq!(home_count, 1);
}

#[tokio::test]
async fn account_status_pages_remain_stable_and_concurrent_registration_is_unique() {
    status_pagination_contract(&store().await).await;
}

pub(crate) async fn status_pagination_contract(store: &ControlStore) {
    let owner = owner(store).await;
    let actor = &owner.session.user;
    store
        .set_registration_settings(actor, RegistrationMode::Open, true, 1, 1_002)
        .await
        .unwrap();
    let mut expected = BTreeSet::new();
    for subject in ["waiting-a", "waiting-b", "waiting-c"] {
        expected.insert(
            store
                .register_oidc(
                    &principal(subject),
                    subject,
                    &format!("{subject}@example.test"),
                    1_003,
                )
                .await
                .unwrap()
                .user_id
                .to_string(),
        );
    }
    let registration = registration("duplicate");
    let (first, second) = tokio::join!(
        store.register_native(&registration, 1_003),
        store.register_native(&registration, 1_003)
    );
    let accepted = match (first, second) {
        (Ok(accepted), Err(error)) | (Err(error), Ok(accepted)) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            accepted
        }
        _ => panic!("exactly one concurrent username registration must succeed"),
    };
    expected.insert(accepted.user_id.to_string());
    assert_eq!(resource_counts(store).await, (5, 5, 1));
    let mut query = AccountListQuery {
        status: Some(AccountStatus::Pending),
        role: Some(PlatformRole::User),
        limit: 1,
        ..AccountListQuery::default()
    };
    let mut actual = BTreeSet::new();
    loop {
        let page = store.list_accounts(actor, &query).await.unwrap();
        for account in page.accounts {
            assert_eq!(account.status, AccountStatus::Pending);
            assert!(actual.insert(account.user_id.to_string()));
        }
        query.cursor = page.next_cursor;
        if query.cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let filtered = store
        .list_accounts(
            actor,
            &AccountListQuery {
                query: Some("WAITING".to_owned()),
                status: Some(AccountStatus::Pending),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(filtered.accounts.len(), 3);
    let user_id = accepted.user_id;
    let (approved, rejected) = tokio::join!(
        store.review_account_registration(actor, &user_id, RegistrationDecision::Approve, 1, 1_004),
        store.review_account_registration(actor, &user_id, RegistrationDecision::Reject, 1, 1_004),
    );
    assert_eq!(
        usize::from(approved.is_ok()) + usize::from(rejected.is_ok()),
        1
    );
    let error = approved.err().or_else(|| rejected.err()).unwrap();
    assert_eq!(error.code, ErrorCode::Conflict);
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_platform_audit WHERE action='account.registration.review' AND resource_id=$1")
        .bind(user_id.as_str()).fetch_one(store.database.pool()).await.unwrap();
    assert_eq!(audits, 1);
}

#[tokio::test]
async fn canonical_username_is_unique_across_native_and_external_accounts() {
    canonical_username_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify cross-login naming, immutable ownership and private external claims in one persisted account lifecycle."
)]
pub(crate) async fn canonical_username_contract(store: &ControlStore) {
    let owner = owner(store).await;
    let actor = &owner.session.user;
    assert_eq!(actor.username, "owner");
    store
        .set_registration_settings(actor, RegistrationMode::Open, false, 1, 1_002)
        .await
        .unwrap();
    let external = principal("arbitrary-external-subject");
    assert_eq!(
        store
            .authenticate_oidc_user(&external, 1_003)
            .await
            .err()
            .unwrap()
            .message,
        "choose a platform username to finish registration"
    );
    let registered = store
        .register_oidc(
            &external,
            "  Alice.User  ",
            "alice.user@example.test",
            1_004,
        )
        .await
        .unwrap();
    assert_eq!(registered.status, AccountStatus::Active);
    let alice = store
        .authenticate_oidc_user(&external, 1_005)
        .await
        .unwrap();
    assert_eq!(alice.username, "alice.user");
    assert_eq!(alice.user_id, registered.user_id);
    let identity = store.identity_session(alice.clone()).await.unwrap();
    let counts = resource_counts(store).await;
    for result in [
        store
            .register_native(&registration("ALICE.USER"), 1_006)
            .await
            .map(|_| ()),
        store
            .register_oidc(
                &principal("other-subject"),
                "Alice.User",
                "other-subject@example.test",
                1_006,
            )
            .await
            .map(|_| ()),
        store
            .register_oidc(
                &principal("other-owner"),
                "OWNER",
                "other-owner@example.test",
                1_006,
            )
            .await
            .map(|_| ()),
    ] {
        let error = result.err().unwrap();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(error.message, "username is already registered");
    }
    assert_eq!(
        resource_counts(store).await,
        counts,
        "name conflicts cannot leak accounts, spaces or tokens"
    );
    let mut changed_claims = external.clone();
    changed_claims.email = Some("owner@example.test".to_owned());
    changed_claims.display_name = Some("Owner".to_owned());
    let replay = store
        .register_oidc(
            &changed_claims,
            "attempted-rename",
            "changed@example.test",
            1_007,
        )
        .await
        .unwrap();
    assert_eq!(replay.user_id, alice.user_id);
    let unchanged = store
        .authenticate_oidc_user(&changed_claims, 1_008)
        .await
        .unwrap();
    assert_eq!(unchanged, alice);
    let current = store.identity_session(unchanged).await.unwrap();
    assert_eq!(current.personal_tenant_id, identity.personal_tenant_id);
    assert_eq!(current.platform_role, PlatformRole::User);
    assert!(!current.is_instance_owner);
    assert!(!store.account_login_methods(&alice).await.unwrap().native);
    assert_eq!(resource_counts(store).await, counts);
    let serialized = serde_json::to_value(&alice).unwrap();
    assert_eq!(
        serialized,
        json!({"user_id": alice.user_id, "username":"alice.user"})
    );
    let account = store.get_account(actor, &alice.user_id).await.unwrap();
    assert_eq!(account.username, "alice.user");
    let account = serde_json::to_value(account).unwrap();
    assert_eq!(account["email"], "alice.user@example.test");
    assert!(account.get("display_name").is_none());
    for query in ["alice.user", alice.user_id.as_str()] {
        let page = store
            .list_accounts(
                actor,
                &AccountListQuery {
                    query: Some(query.to_owned()),
                    ..AccountListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(page.accounts.len(), 1);
        assert_eq!(page.accounts[0].user_id, alice.user_id);
    }
    let matching_email = store
        .list_accounts(
            actor,
            &AccountListQuery {
                query: changed_claims.email.clone(),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(matching_email.accounts.len(), 1);
    assert_eq!(
        matching_email.accounts[0].user_id, actor.user_id,
        "OIDC claims cannot replace another account's registered contact email"
    );
    let linked_identity = principal("native-owner-link");
    store
        .link_native_oidc(actor, &linked_identity, 1_009)
        .await
        .unwrap();
    let linked = store
        .authenticate_oidc_user(&linked_identity, 1_010)
        .await
        .unwrap();
    assert_eq!(&linked, actor);
    let native = store
        .login_native("OWNER", "registration-test-password", 1_011)
        .await
        .unwrap();
    assert_eq!(&native.session.user, actor);
    assert_eq!(
        native.session.personal_tenant_id,
        owner.session.personal_tenant_id
    );
    assert_eq!(
        store
            .login_native("owner", "wrong-password", 1_012)
            .await
            .err()
            .unwrap()
            .message,
        "invalid username or password"
    );
    let reopened = store
        .register_oidc(
            &linked_identity,
            "attempted-owner-rename",
            "ignored@example.test",
            1_013,
        )
        .await
        .unwrap();
    assert_eq!(reopened.user_id, actor.user_id);
    assert_eq!(
        store
            .authenticate_oidc_user(&linked_identity, 1_014)
            .await
            .unwrap()
            .username,
        "owner"
    );
}

#[tokio::test]
async fn concurrent_native_and_oidc_registration_share_one_username_namespace() {
    username_race_contract(&store().await).await;
}

pub(crate) async fn username_race_contract(store: &ControlStore) {
    let owner = owner(store).await;
    store
        .set_registration_settings(&owner.session.user, RegistrationMode::Open, false, 1, 1_002)
        .await
        .unwrap();
    let native_request = registration("Race.User");
    let external = principal("race-external");
    let (native, oidc) = tokio::join!(
        store.register_native(&native_request, 1_003),
        store.register_oidc(&external, "RACE.USER", "race.user@example.test", 1_003),
    );
    let native = native.map(|outcome| outcome.user_id);
    let oidc = oidc.map(|outcome| outcome.user_id);
    let (winner, conflict) = match (native, oidc) {
        (Ok(winner), Err(conflict)) | (Err(conflict), Ok(winner)) => (winner, conflict),
        other => panic!("exactly one account must own the username: {other:?}"),
    };
    assert_eq!(conflict.code, ErrorCode::Conflict);
    assert_eq!(conflict.message, "username is already registered");
    let page = store
        .list_accounts(
            &owner.session.user,
            &AccountListQuery {
                query: Some("race.user".to_owned()),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.accounts.len(), 1);
    assert_eq!(page.accounts[0].user_id, winner);
    assert_eq!(page.accounts[0].username, "race.user");
    let counts = resource_counts(store).await;
    assert_eq!((counts.0, counts.1), (2, 2));
    let linked_identity = principal("registration-link-race");
    let (registered, linked) = tokio::join!(
        store.register_oidc(
            &linked_identity,
            "race-link-user",
            "race-link@example.test",
            1_004
        ),
        store.link_native_oidc(&owner.session.user, &linked_identity, 1_004),
    );
    let registered = registered.expect(
        "OIDC registration must resolve a concurrent native binding without a database error",
    );
    let current = store
        .authenticate_oidc_user(&linked_identity, 1_005)
        .await
        .unwrap();
    assert_eq!(registered.user_id, current.user_id);
    let counts = resource_counts(store).await;
    match linked {
        Ok(methods) => {
            assert!(methods.native);
            assert_eq!(current, owner.session.user);
            assert_eq!((counts.0, counts.1), (2, 2));
        }
        Err(error) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(current.username, "race-link-user");
            assert_ne!(current.user_id, owner.session.user.user_id);
            assert!(!store.account_login_methods(&current).await.unwrap().native);
            assert!(
                store
                    .account_login_methods(&owner.session.user)
                    .await
                    .unwrap()
                    .oidc
                    .is_none()
            );
            assert_eq!((counts.0, counts.1), (3, 3));
        }
    }
    let native = store
        .login_native("owner", "registration-test-password", 1_006)
        .await
        .unwrap();
    assert_eq!(native.session.user, owner.session.user);
    assert_eq!(
        native.session.personal_tenant_id,
        owner.session.personal_tenant_id
    );
    assert!(native.session.is_instance_owner);
}
