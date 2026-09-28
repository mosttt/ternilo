use std::collections::BTreeSet;

use super::*;
use crate::{
    InstanceMode, NativeRegistration, OidcPrincipal, SecretCipher, TenantRole,
    UserInvitationRequest,
};

fn registration(username: &str) -> NativeRegistration {
    NativeRegistration {
        email: format!("{}@example.test", username.trim().to_ascii_lowercase()),
        username: username.to_owned(),
        password: "test-password-123".to_owned(),
    }
}

async fn store() -> ControlStore {
    ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([11; 32]), 1)
        .await
        .unwrap()
}

async fn oidc_user(store: &ControlStore, subject: &str, now_ms: u64) -> ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://identity.example".to_owned(),
                subject: subject.to_owned(),
                email: Some(format!("{subject}@example.test")),
                display_name: Some(subject.to_owned()),
            },
            &format!("test-{subject}"),
            now_ms,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn accounts_keep_personal_spaces_when_joining_teams() {
    personal_spaces_and_invitations_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify both invitation flows preserve the same personal resources and team boundaries."
)]
pub(crate) async fn personal_spaces_and_invitations_contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    let actor = &owner.session.user;
    store
        .set_instance_mode(actor, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let alice = oidc_user(store, "alice", 1_002).await;
    let alice_home = store.identity_session(alice.clone()).await.unwrap();
    assert_eq!(
        oidc_user(store, "alice", 1_003).await.user_id,
        alice.user_id
    );
    let simultaneous = OidcPrincipal {
        issuer: "https://identity.example".to_owned(),
        subject: "simultaneous-first-login".to_owned(),
        email: None,
        display_name: None,
    };
    let username = format!("test-{}", simultaneous.subject);
    let (first, second) = tokio::join!(
        store.upsert_user(&simultaneous, &username, 1_003),
        store.upsert_user(&simultaneous, &username, 1_003),
    );
    let first = first.unwrap();
    assert_eq!(first.user_id, second.unwrap().user_id);
    let homes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_account_spaces WHERE user_id = $1")
            .bind(first.user_id.as_str())
            .fetch_one(store.database.pool())
            .await
            .unwrap();
    assert_eq!(
        homes, 1,
        "concurrent first logins must create exactly one personal space"
    );
    let space_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_account_spaces WHERE user_id = $1")
            .bind(alice.user_id.as_str())
            .fetch_one(store.database.pool())
            .await
            .unwrap();
    assert_eq!(space_count, 1);
    assert_ne!(
        alice_home.personal_tenant_id,
        owner.session.personal_tenant_id
    );
    assert!(
        store
            .list_projects(&alice, &owner.session.personal_tenant_id)
            .await
            .is_err()
    );
    assert!(
        store
            .list_projects(actor, &alice_home.personal_tenant_id)
            .await
            .is_err()
    );
    let alice_spaces = store.list_tenants(&alice).await.unwrap();
    assert_eq!(alice_spaces.len(), 1);
    assert_eq!(alice_spaces[0].kind, SpaceKind::Personal);
    assert_eq!(alice_spaces[0].role, TenantRole::Owner);
    for role in [TenantRole::Owner, TenantRole::Admin, TenantRole::Member] {
        assert!(
            store
                .set_membership(
                    actor,
                    &owner.session.personal_tenant_id,
                    &alice.user_id,
                    role,
                    1_004
                )
                .await
                .is_err()
        );
    }
    assert!(
        store
            .remove_membership(
                actor,
                &owner.session.personal_tenant_id,
                &actor.user_id,
                1_004
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_user_invitation(
                actor,
                &UserInvitationRequest {
                    tenant_id: Some(owner.session.personal_tenant_id.clone()),
                    role: TenantRole::Member,
                    expires_in_seconds: 300
                },
                1_004
            )
            .await
            .is_err()
    );
    let workspace = store
        .create_cloud_workspace(
            &alice,
            &alice_home.personal_tenant_id,
            &alice_home.personal_project_id,
            "Private",
            1_004,
        )
        .await
        .unwrap();
    let team = store
        .create_tenant(
            actor,
            "test-team",
            "Test team",
            TenantQuota::default(),
            1_005,
        )
        .await
        .unwrap();
    assert_eq!(team.kind, SpaceKind::Team);
    let invite = store
        .create_user_invitation(
            actor,
            &UserInvitationRequest {
                tenant_id: Some(team.tenant_id.clone()),
                role: TenantRole::Member,
                expires_in_seconds: 300,
            },
            1_006,
        )
        .await
        .unwrap();
    let joined = store
        .join_invitation(&alice, &invite.token, 1_007)
        .await
        .unwrap();
    assert_eq!(joined.role, TenantRole::Member);
    assert_eq!(joined.tenant_id, team.tenant_id);
    assert!(
        store
            .join_invitation(&alice, &invite.token, 1_008)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .resolve_owned_workspace(
                &alice,
                &alice_home.personal_tenant_id,
                &workspace.workspace_id
            )
            .await
            .unwrap(),
        workspace
    );
    assert_eq!(
        store
            .identity_session(alice.clone())
            .await
            .unwrap()
            .personal_tenant_id,
        alice_home.personal_tenant_id
    );
    let native_invite = store
        .create_user_invitation(
            actor,
            &UserInvitationRequest {
                tenant_id: Some(team.tenant_id.clone()),
                role: TenantRole::Viewer,
                expires_in_seconds: 300,
            },
            1_008,
        )
        .await
        .unwrap();
    assert!(
        store
            .accept_user_invitation(&native_invite.token, &registration("team-member"), 1_009)
            .await
            .is_err()
    );
    assert_invitation_unused(store, &native_invite.invitation_id).await;
    let account_invite = store
        .create_user_invitation(
            actor,
            &UserInvitationRequest {
                tenant_id: None,
                role: TenantRole::Member,
                expires_in_seconds: 300,
            },
            1_009,
        )
        .await
        .unwrap();
    let native = store
        .accept_user_invitation(&account_invite.token, &registration("team-member"), 1_009)
        .await
        .unwrap();
    store
        .join_invitation(&native.session.user, &native_invite.token, 1_009)
        .await
        .unwrap();
    let spaces = store.list_tenants(&native.session.user).await.unwrap();
    assert_eq!(spaces.len(), 2);
    assert!(
        spaces
            .iter()
            .any(|space| space.tenant_id == native.session.personal_tenant_id
                && space.kind == SpaceKind::Personal
                && space.role == TenantRole::Owner)
    );
    assert!(
        spaces
            .iter()
            .any(|space| space.tenant_id == team.tenant_id && space.role == TenantRole::Viewer)
    );
    store
        .set_membership(
            actor,
            &team.tenant_id,
            &alice.user_id,
            TenantRole::Admin,
            1_010,
        )
        .await
        .unwrap();
    let lower_invite = store
        .create_user_invitation(
            actor,
            &UserInvitationRequest {
                tenant_id: Some(team.tenant_id.clone()),
                role: TenantRole::Viewer,
                expires_in_seconds: 300,
            },
            1_011,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .join_invitation(&alice, &lower_invite.token, 1_012)
            .await
            .unwrap()
            .role,
        TenantRole::Admin
    );
    let platform_invite = store
        .create_user_invitation(
            actor,
            &UserInvitationRequest {
                tenant_id: None,
                role: TenantRole::Member,
                expires_in_seconds: 300,
            },
            1_013,
        )
        .await
        .unwrap();
    assert!(platform_invite.tenant_id.is_none());
    assert!(
        store
            .join_invitation(&alice, &platform_invite.token, 1_014)
            .await
            .is_err()
    );
    let newcomer = store
        .accept_user_invitation(&platform_invite.token, &registration("newcomer"), 1_015)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_tenants(&newcomer.session.user)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_ne!(
        newcomer.session.personal_tenant_id,
        owner.session.personal_tenant_id
    );
    assert_eq!(newcomer.session.platform_role, PlatformRole::User);
    assert!(
        store
            .list_accounts(&alice, &AccountListQuery::default())
            .await
            .is_err(),
        "team administration grants no platform directory access"
    );
    store
        .remove_membership(actor, &team.tenant_id, &alice.user_id, 1_016)
        .await
        .unwrap();
    assert_eq!(store.list_tenants(&alice).await.unwrap().len(), 1);
    assert_eq!(
        store
            .resolve_owned_workspace(
                &alice,
                &alice_home.personal_tenant_id,
                &workspace.workspace_id
            )
            .await
            .unwrap(),
        workspace
    );
}

#[tokio::test]
async fn account_directory_roles_and_pagination_are_enforced() {
    directory_and_roles_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep authorization, stable paging and role-change audit assertions in the same database contract."
)]
pub(crate) async fn directory_and_roles_contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    let actor = &owner.session.user;
    store
        .set_instance_mode(actor, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let admin = oidc_user(store, "admin", 1_002).await;
    let operator = oidc_user(store, "operator", 1_002).await;
    let auditor = oidc_user(store, "auditor", 1_002).await;
    let member = oidc_user(store, "member", 1_002).await;
    for (user, role) in [
        (&admin, PlatformRole::Admin),
        (&operator, PlatformRole::Operator),
        (&auditor, PlatformRole::Auditor),
    ] {
        let updated = store
            .set_account_role(actor, &user.user_id, role, 1, 1_003)
            .await
            .unwrap();
        assert_eq!(updated.platform_role, role);
        assert_eq!(updated.role_revision, 2);
        assert_eq!(
            store
                .identity_session(user.clone())
                .await
                .unwrap()
                .platform_role,
            role
        );
    }
    for user in [&member, &operator] {
        assert!(
            store
                .list_accounts(user, &AccountListQuery::default())
                .await
                .is_err()
        );
    }
    for user in [actor, &admin, &auditor] {
        assert_eq!(
            store
                .list_accounts(user, &AccountListQuery::default())
                .await
                .unwrap()
                .accounts
                .len(),
            5
        );
    }
    assert!(
        store
            .set_account_role(&admin, &member.user_id, PlatformRole::Admin, 1, 1_004)
            .await
            .is_err()
    );
    assert!(
        store
            .set_account_role(actor, &actor.user_id, PlatformRole::User, 1, 1_004)
            .await
            .is_err()
    );
    assert!(
        store
            .set_account_role(actor, &member.user_id, PlatformRole::Owner, 1, 1_004)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .set_account_role(actor, &admin.user_id, PlatformRole::Auditor, 1, 1_004)
            .await
            .unwrap_err()
            .code,
        ternilo_protocol::ErrorCode::Conflict
    );
    assert!(
        store
            .require_platform_action(&operator, PlatformAction::WorkersManage)
            .await
            .is_ok()
    );
    assert!(
        store
            .require_platform_action(&auditor, PlatformAction::WorkersRead)
            .await
            .is_ok()
    );
    assert!(
        store
            .require_platform_action(&auditor, PlatformAction::WorkersManage)
            .await
            .is_err()
    );
    assert!(
        store
            .require_platform_action(&auditor, PlatformAction::AccountsInvite)
            .await
            .is_err()
    );
    assert!(
        store
            .require_platform_action(&admin, PlatformAction::AccountsInvite)
            .await
            .is_ok()
    );
    assert!(
        store
            .list_projects(&admin, &owner.session.personal_tenant_id)
            .await
            .is_err(),
        "platform role grants no private space membership"
    );
    let mut seen = BTreeSet::new();
    let mut query = AccountListQuery {
        limit: 2,
        ..AccountListQuery::default()
    };
    loop {
        let page = store.list_accounts(&admin, &query).await.unwrap();
        assert!(!page.accounts.is_empty());
        assert!(page.accounts.len() <= 2);
        for account in page.accounts {
            assert!(
                seen.insert(account.user_id.to_string()),
                "page returned an account twice"
            );
        }
        query.cursor = page.next_cursor;
        if query.cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 5);
    let filtered = store
        .list_accounts(
            &auditor,
            &AccountListQuery {
                query: Some("AUDITOR".to_owned()),
                role: Some(PlatformRole::Auditor),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(filtered.accounts.len(), 1);
    assert_eq!(filtered.accounts[0].user_id, auditor.user_id);
    let owners = store
        .list_accounts(
            actor,
            &AccountListQuery {
                role: Some(PlatformRole::Owner),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(owners.accounts.len(), 1);
    assert_eq!(owners.accounts[0].username, "owner");
    let literal = store
        .list_accounts(
            actor,
            &AccountListQuery {
                query: Some("%".to_owned()),
                ..AccountListQuery::default()
            },
        )
        .await
        .unwrap();
    assert!(literal.accounts.is_empty());
    for limit in [0, 101] {
        assert!(
            store
                .list_accounts(
                    actor,
                    &AccountListQuery {
                        limit,
                        ..AccountListQuery::default()
                    }
                )
                .await
                .is_err()
        );
    }
    assert!(
        store
            .list_accounts(
                actor,
                &AccountListQuery {
                    cursor: Some("invalid-cursor".to_owned()),
                    ..AccountListQuery::default()
                }
            )
            .await
            .is_err()
    );
    let role_audit: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_platform_audit WHERE action = 'account.role'",
    )
    .fetch_one(store.database.pool())
    .await
    .unwrap();
    assert_eq!(role_audit, 3);
    assert!(
        sqlx::query("UPDATE control_platform_audit SET resource_id = 'modified'")
            .execute(store.database.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM control_platform_audit")
            .execute(store.database.pool())
            .await
            .is_err()
    );
    store
        .set_account_role(actor, &admin.user_id, PlatformRole::User, 2, 1_005)
        .await
        .unwrap();
    assert!(
        store
            .list_accounts(&admin, &AccountListQuery::default())
            .await
            .is_err(),
        "role changes apply to the next request without renewing credentials"
    );
    store
        .set_instance_mode(actor, InstanceMode::SingleUser, 2, 1_006)
        .await
        .unwrap();
    assert!(
        store
            .require_platform_action(&operator, PlatformAction::WorkersManage)
            .await
            .is_err()
    );
    assert!(
        store
            .list_accounts(&auditor, &AccountListQuery::default())
            .await
            .is_err()
    );
    assert!(
        store
            .list_accounts(actor, &AccountListQuery::default())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn invitations_require_current_grant_authority() {
    invitations_require_current_grant_authority_contract(&store().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify removed and downgraded inviters cannot restore access or create accounts through old links on either database backend."
)]
pub(crate) async fn invitations_require_current_grant_authority_contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(&registration("owner"), 1_000)
        .await
        .unwrap();
    let actor = &owner.session.user;
    store
        .set_instance_mode(actor, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let inviter = oidc_user(store, "inviter", 1_002).await;
    let recipient = oidc_user(store, "recipient", 1_002).await;
    let team = store
        .create_tenant(
            actor,
            "invitation-authority",
            "Invitation authority",
            TenantQuota::default(),
            1_003,
        )
        .await
        .unwrap();
    store
        .set_membership(
            actor,
            &team.tenant_id,
            &inviter.user_id,
            TenantRole::Admin,
            1_004,
        )
        .await
        .unwrap();
    let request = UserInvitationRequest {
        tenant_id: Some(team.tenant_id.clone()),
        role: TenantRole::Admin,
        expires_in_seconds: 300,
    };
    let removed_invite = store
        .create_user_invitation(&inviter, &request, 1_005)
        .await
        .unwrap();
    store
        .remove_membership(actor, &team.tenant_id, &inviter.user_id, 1_006)
        .await
        .unwrap();
    assert_eq!(
        store
            .join_invitation(&inviter, &removed_invite.token, 1_007)
            .await
            .err()
            .unwrap()
            .code,
        ternilo_protocol::ErrorCode::PolicyDenied
    );
    assert!(
        store
            .authorize(&inviter, &team.tenant_id, crate::ControlAction::TenantRead)
            .await
            .is_err(),
        "a removed administrator must not restore their own membership with an old invitation"
    );
    assert_invitation_unused(store, &removed_invite.invitation_id).await;
    assert_registration_rejected_without_resources(
        store,
        &removed_invite,
        "removed-inviter-signup",
        1_008,
    )
    .await;
    store
        .set_membership(
            actor,
            &team.tenant_id,
            &inviter.user_id,
            TenantRole::Admin,
            1_009,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .join_invitation(&recipient, &removed_invite.token, 1_010)
            .await
            .unwrap()
            .role,
        TenantRole::Admin,
        "a failed authorization must not consume the invitation"
    );

    let downgraded_invite = store
        .create_user_invitation(&inviter, &request, 1_011)
        .await
        .unwrap();
    store
        .set_membership(
            actor,
            &team.tenant_id,
            &inviter.user_id,
            TenantRole::Member,
            1_012,
        )
        .await
        .unwrap();
    // Platform authority does not restore the ability to grant this team's membership.
    store
        .set_account_role(actor, &inviter.user_id, PlatformRole::Admin, 1, 1_013)
        .await
        .unwrap();
    assert!(
        store
            .join_invitation(&inviter, &downgraded_invite.token, 1_014)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .authorize(&inviter, &team.tenant_id, crate::ControlAction::TenantRead)
            .await
            .unwrap(),
        TenantRole::Member
    );
    assert_registration_rejected_without_resources(
        store,
        &downgraded_invite,
        "downgraded-team-signup",
        1_015,
    )
    .await;

    let platform_invite = store
        .create_user_invitation(
            &inviter,
            &UserInvitationRequest {
                tenant_id: None,
                role: TenantRole::Member,
                expires_in_seconds: 300,
            },
            1_016,
        )
        .await
        .unwrap();
    // Team administration does not restore the ability to invite platform accounts.
    store
        .set_membership(
            actor,
            &team.tenant_id,
            &inviter.user_id,
            TenantRole::Admin,
            1_017,
        )
        .await
        .unwrap();
    store
        .set_account_role(actor, &inviter.user_id, PlatformRole::User, 2, 1_018)
        .await
        .unwrap();
    assert_registration_rejected_without_resources(
        store,
        &platform_invite,
        "downgraded-platform-signup",
        1_019,
    )
    .await;
    store
        .set_account_role(actor, &inviter.user_id, PlatformRole::Admin, 3, 1_020)
        .await
        .unwrap();
    let accepted = store
        .accept_user_invitation(
            &platform_invite.token,
            &registration("restored-signup"),
            1_021,
        )
        .await
        .unwrap();
    assert_eq!(accepted.session.platform_role, PlatformRole::User);
    let spaces = store.list_tenants(&accepted.session.user).await.unwrap();
    assert_eq!(spaces.len(), 1);
    assert_eq!(spaces[0].kind, SpaceKind::Personal);
}

async fn assert_invitation_unused(store: &ControlStore, invitation_id: &str) {
    let row = sqlx::query(
        "SELECT consumed_at_ms, consumed_by FROM control_user_invitations WHERE invitation_id = $1",
    )
    .bind(invitation_id)
    .fetch_one(store.database.pool())
    .await
    .unwrap();
    assert!(
        row.try_get::<Option<i64>, _>("consumed_at_ms")
            .unwrap()
            .is_none()
    );
    assert!(
        row.try_get::<Option<String>, _>("consumed_by")
            .unwrap()
            .is_none()
    );
}

async fn assert_registration_rejected_without_resources(
    store: &ControlStore,
    invitation: &crate::UserInvitationGrant,
    username: &str,
    now_ms: u64,
) {
    let before = account_resource_counts(store).await;
    assert_eq!(
        store
            .accept_user_invitation(&invitation.token, &registration(username), now_ms)
            .await
            .err()
            .unwrap()
            .code,
        ternilo_protocol::ErrorCode::PolicyDenied
    );
    assert_eq!(
        account_resource_counts(store).await,
        before,
        "rejected registration must not leave an account or personal space"
    );
    assert_invitation_unused(store, &invitation.invitation_id).await;
}

async fn account_resource_counts(store: &ControlStore) -> (i64, i64) {
    let row = sqlx::query("SELECT (SELECT COUNT(*) FROM control_users) AS accounts, (SELECT COUNT(*) FROM control_account_spaces) AS personal_spaces")
        .fetch_one(store.database.pool()).await.unwrap();
    (
        row.try_get("accounts").unwrap(),
        row.try_get("personal_spaces").unwrap(),
    )
}
