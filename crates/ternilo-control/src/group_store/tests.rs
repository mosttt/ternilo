use std::collections::BTreeSet;

use super::*;
use crate::{
    InstanceMode, NativeRegistration, OidcPrincipal, ResourceAction, ResourceKind,
    ResourcePermissions, SecretCipher, ShareSubject, TenantQuota,
};

struct Fixture {
    store: ControlStore,
    owner: ControlUser,
    member: ControlUser,
    admin: ControlUser,
    outsider: ControlUser,
    tenant: TenantId,
    personal: TenantId,
}

async fn fixture() -> Fixture {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([38; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "group-owner@example.test".to_owned(),
                username: "group-owner".to_owned(),
                password: "test-password-123".to_owned(),
            },
            1_000,
        )
        .await
        .unwrap();
    let personal = owner.session.personal_tenant_id;
    let owner = owner.session.user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let mut users = Vec::new();
    for name in ["member", "admin", "outsider"] {
        users.push(
            store
                .upsert_user(
                    &OidcPrincipal {
                        issuer: "https://identity.example".to_owned(),
                        subject: name.to_owned(),
                        email: Some(format!("{name}@example.test")),
                        display_name: Some(name.to_owned()),
                    },
                    &format!("test-{name}"),
                    1_002,
                )
                .await
                .unwrap(),
        );
    }
    let member = users.remove(0);
    let admin = users.remove(0);
    let outsider = users.remove(0);
    let tenant = store
        .create_tenant(
            &owner,
            "group-team",
            "Group team",
            TenantQuota::default(),
            1_003,
        )
        .await
        .unwrap()
        .tenant_id;
    store
        .set_membership(&owner, &tenant, &member.user_id, TenantRole::Member, 1_004)
        .await
        .unwrap();
    store
        .set_membership(&owner, &tenant, &admin.user_id, TenantRole::Admin, 1_004)
        .await
        .unwrap();
    Fixture {
        store,
        owner,
        member,
        admin,
        outsider,
        tenant,
        personal,
    }
}

fn input(name: &str) -> GroupInput {
    GroupInput {
        name: name.to_owned(),
        description: None,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise the complete permissions and pagination transitions on the same team fixture."
)]
async fn groups_are_team_scoped_managed_by_team_roles_and_cascade_membership_removal() {
    let f = fixture().await;
    for actor in [&f.member, &f.outsider] {
        assert!(
            f.store
                .create_permission_group(actor, &f.tenant, &input("Denied"), 1_005)
                .await
                .is_err()
        );
        assert!(
            f.store
                .list_permission_groups(actor, &f.tenant, &PageQuery::default())
                .await
                .is_err()
        );
    }
    assert!(
        f.store
            .create_permission_group(&f.owner, &f.personal, &input("Personal"), 1_005)
            .await
            .is_err()
    );
    let group = f
        .store
        .create_permission_group(&f.admin, &f.tenant, &input("Developers"), 1_005)
        .await
        .unwrap();
    assert!(
        f.store
            .set_permission_group_member(
                &f.owner,
                &f.tenant,
                &group.group_id,
                &f.outsider.user_id,
                true,
                1_006
            )
            .await
            .is_err()
    );
    f.store
        .set_permission_group_member(
            &f.admin,
            &f.tenant,
            &group.group_id,
            &f.member.user_id,
            true,
            1_006,
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .permission_group(&f.admin, &f.tenant, &group.group_id)
            .await
            .unwrap()
            .member_count,
        1
    );
    f.store
        .update_permission_group(
            &f.admin,
            &f.tenant,
            &group.group_id,
            &GroupInput {
                name: "Release team".to_owned(),
                description: Some("Release access".to_owned()),
            },
            1_007,
        )
        .await
        .unwrap();
    let page = f
        .store
        .list_permission_groups(
            &f.owner,
            &f.tenant,
            &PageQuery {
                query: Some("release".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.groups[0].name, "Release team");
    f.store
        .remove_membership(&f.owner, &f.tenant, &f.member.user_id, 1_008)
        .await
        .unwrap();
    assert!(
        f.store
            .list_permission_group_members(
                &f.owner,
                &f.tenant,
                &group.group_id,
                &PageQuery::default()
            )
            .await
            .unwrap()
            .memberships
            .is_empty()
    );
    f.store
        .set_membership(
            &f.owner,
            &f.tenant,
            &f.member.user_id,
            TenantRole::Member,
            1_009,
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .permission_group(&f.owner, &f.tenant, &group.group_id)
            .await
            .unwrap()
            .member_count,
        0
    );
    f.store
        .delete_permission_group(&f.admin, &f.tenant, &group.group_id, 1_010)
        .await
        .unwrap();
    assert!(
        f.store
            .permission_group(&f.owner, &f.tenant, &group.group_id)
            .await
            .is_err()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise the complete permissions and pagination transitions on the same team fixture."
)]
async fn group_grants_preserve_resource_privacy_combine_sources_and_revoke_dynamically() {
    let f = fixture().await;
    let project = f
        .store
        .list_projects(&f.owner, &f.tenant)
        .await
        .unwrap()
        .remove(0);
    let workspace = f
        .store
        .create_cloud_workspace(
            &f.owner,
            &f.tenant,
            &project.project_id,
            "Private workspace",
            1_005,
        )
        .await
        .unwrap();
    let id = workspace.workspace_id.as_str();
    let kind = ResourceKind::Workspace;
    let group = f
        .store
        .create_permission_group(&f.admin, &f.tenant, &input("Build"), 1_006)
        .await
        .unwrap();
    f.store
        .set_permission_group_member(
            &f.admin,
            &f.tenant,
            &group.group_id,
            &f.member.user_id,
            true,
            1_007,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .set_resource_group_share(
                &f.admin,
                &f.tenant,
                kind,
                id,
                &group.group_id,
                Some(ResourcePermissions::OWNER),
                1_008
            )
            .await
            .is_err()
    );
    let read = ResourcePermissions {
        view: true,
        ..Default::default()
    };
    f.store
        .set_resource_share(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &f.member.user_id,
            Some(read),
            1_008,
        )
        .await
        .unwrap();
    f.store
        .set_resource_group_share(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &group.group_id,
            Some(ResourcePermissions::OWNER),
            1_009,
        )
        .await
        .unwrap();
    let access = f
        .store
        .resource_access(&f.member, &f.tenant, kind, id)
        .await
        .unwrap();
    access.require(ResourceAction::Submit).unwrap();
    assert!(!access.is_owner);
    assert!(access.require(ResourceAction::ManageSharing).is_err());
    assert_eq!(access.sources.len(), 2);
    assert!(!access.role_limited);
    assert_eq!(
        f.store
            .list_accessible_workspaces(&f.member, &f.tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        f.store
            .list_accessible_workspaces(&f.admin, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    let shares = f
        .store
        .list_resource_shares(&f.owner, &f.tenant, kind, id, &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(shares.shares.len(), 2);
    assert!(shares.shares.iter().any(|grant| matches!(&grant.subject,ShareSubject::Group{group: candidate} if candidate.group_id==group.group_id)));
    let first = f
        .store
        .list_resource_shares(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &PageQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let second = f
        .store
        .list_resource_shares(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &PageQuery {
                limit: 1,
                cursor: first.next_cursor,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.shares.len(), 1);
    assert_eq!(second.shares.len(), 1);
    assert!(matches!(
        first.shares[0].subject,
        ShareSubject::Group { .. }
    ));
    assert!(matches!(
        second.shares[0].subject,
        ShareSubject::User { .. }
    ));
    assert!(second.next_cursor.is_none());
    f.store
        .set_membership(
            &f.owner,
            &f.tenant,
            &f.member.user_id,
            TenantRole::Viewer,
            1_010,
        )
        .await
        .unwrap();
    let access = f
        .store
        .resource_access(&f.member, &f.tenant, kind, id)
        .await
        .unwrap();
    assert!(access.role_limited);
    assert_eq!(access.permissions, read);
    f.store
        .set_membership(
            &f.owner,
            &f.tenant,
            &f.member.user_id,
            TenantRole::Member,
            1_011,
        )
        .await
        .unwrap();
    f.store
        .set_permission_group_member(
            &f.admin,
            &f.tenant,
            &group.group_id,
            &f.member.user_id,
            false,
            1_012,
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .resource_access(&f.member, &f.tenant, kind, id)
            .await
            .unwrap()
            .permissions,
        read
    );
    f.store
        .set_permission_group_member(
            &f.admin,
            &f.tenant,
            &group.group_id,
            &f.member.user_id,
            true,
            1_013,
        )
        .await
        .unwrap();
    f.store
        .set_resource_group_share(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &group.group_id,
            Some(read),
            1_014,
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .resource_access(&f.member, &f.tenant, kind, id)
            .await
            .unwrap()
            .permissions,
        read
    );
    f.store
        .set_resource_share(
            &f.owner,
            &f.tenant,
            kind,
            id,
            &f.member.user_id,
            None,
            1_015,
        )
        .await
        .unwrap();
    f.store
        .delete_permission_group(&f.admin, &f.tenant, &group.group_id, 1_016)
        .await
        .unwrap();
    assert!(
        f.store
            .list_accessible_workspaces(&f.member, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !f.store
            .resource_access(&f.member, &f.tenant, kind, id)
            .await
            .unwrap()
            .permissions
            .view
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise the complete permissions and pagination transitions on the same team fixture."
)]
async fn group_member_and_share_candidates_use_bounded_searchable_pages() {
    let f = fixture().await;
    let mut expected = BTreeSet::new();
    for name in ["Alpha", "Beta", "Gamma"] {
        expected.insert(
            f.store
                .create_permission_group(&f.owner, &f.tenant, &input(name), 1_005)
                .await
                .unwrap()
                .group_id,
        );
    }
    let query = PageQuery {
        limit: 1,
        ..Default::default()
    };
    let mut cursor = None;
    let mut actual = BTreeSet::new();
    loop {
        let page = f
            .store
            .list_permission_groups(
                &f.owner,
                &f.tenant,
                &PageQuery {
                    cursor,
                    ..query.clone()
                },
            )
            .await
            .unwrap();
        assert!(page.groups.len() <= 1);
        for group in page.groups {
            assert!(actual.insert(group.group_id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let group_id = expected.first().unwrap();
    for user in [&f.owner, &f.member, &f.admin] {
        f.store
            .set_permission_group_member(&f.owner, &f.tenant, group_id, &user.user_id, true, 1_006)
            .await
            .unwrap();
    }
    let mut seen_members = BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = f
            .store
            .list_permission_group_members(
                &f.owner,
                &f.tenant,
                group_id,
                &PageQuery {
                    cursor,
                    limit: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(page.memberships.len() <= 1);
        for member in page.memberships {
            assert!(seen_members.insert(member.user_id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen_members.len(), 3);
    for query in [
        PageQuery {
            limit: 0,
            ..Default::default()
        },
        PageQuery {
            limit: 101,
            ..Default::default()
        },
        PageQuery {
            cursor: Some("%%%".to_owned()),
            ..Default::default()
        },
    ] {
        assert!(
            f.store
                .list_permission_groups(&f.owner, &f.tenant, &query)
                .await
                .is_err()
        );
    }
    let members = f
        .store
        .list_memberships(
            &f.owner,
            &f.tenant,
            &PageQuery {
                query: Some("test-member".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(members.memberships.len(), 1);
    assert_eq!(members.memberships[0].user_id, f.member.user_id);
    let project = f
        .store
        .list_projects(&f.member, &f.tenant)
        .await
        .unwrap()
        .remove(0);
    let workspace = f
        .store
        .create_cloud_workspace(
            &f.member,
            &f.tenant,
            &project.project_id,
            "Member workspace",
            1_006,
        )
        .await
        .unwrap();
    let candidates = f
        .store
        .resource_share_candidates(
            &f.member,
            &f.tenant,
            ResourceKind::Workspace,
            workspace.workspace_id.as_str(),
            "group",
            &PageQuery {
                query: Some("beta".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(candidates.candidates.len(), 1);
    assert!(matches!(&candidates.candidates[0],ShareSubject::Group{group} if group.name=="Beta"));
    let candidates = f
        .store
        .resource_share_candidates(
            &f.member,
            &f.tenant,
            ResourceKind::Workspace,
            workspace.workspace_id.as_str(),
            "user",
            &PageQuery {
                query: Some("member".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        candidates.candidates.is_empty(),
        "the owner is excluded from sharing candidates"
    );
    assert!(
        f.store
            .resource_share_candidates(
                &f.admin,
                &f.tenant,
                ResourceKind::Workspace,
                workspace.workspace_id.as_str(),
                "group",
                &PageQuery::default()
            )
            .await
            .is_err()
    );
}
