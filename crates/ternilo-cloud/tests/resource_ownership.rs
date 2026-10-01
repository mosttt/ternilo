use std::time::Duration;

use ternilo_cloud::{CloudLiveNotification, CloudSessionDraft, CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, InstanceMode, NativeRegistration, OidcPrincipal, PageQuery, ResourceAction,
    ResourceKind, ResourceOwnershipTransfer, ResourcePermissions, SecretCipher, ShareSubject,
    TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, PermissionPreset, RunId, SessionEvent, SessionEventKind, SessionId, SessionMode,
};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

#[expect(
    clippy::too_many_lines,
    reason = "Verify handoff, inherited and independent sessions, immutable storage, revocation and rollback through real stores."
)]
async fn contract(url: &str, owner_url: Option<&str>) {
    let control = ControlStore::connect(url, owner_url, SecretCipher::from_key([63; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(url, owner_url, 4).await.unwrap();
    let owner = control
        .initialize_owner(
            &NativeRegistration {
                username: "ownership-owner".into(),
                email: "ownership-owner@example.test".into(),
                password: "ownership-owner-password".into(),
            },
            1000,
        )
        .await
        .unwrap()
        .session
        .user;
    control
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1001)
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &owner,
            "ownership-team",
            "Ownership team",
            TenantQuota::default(),
            1002,
        )
        .await
        .unwrap()
        .tenant_id;
    let project = control
        .list_projects(&owner, &tenant)
        .await
        .unwrap()
        .remove(0)
        .project_id;
    let mut members = Vec::new();
    for (name, role) in [
        ("recipient", TenantRole::Member),
        ("other", TenantRole::Member),
        ("viewer", TenantRole::Viewer),
    ] {
        let user = control
            .upsert_user(
                &OidcPrincipal {
                    issuer: "ownership".into(),
                    subject: name.into(),
                    email: None,
                    display_name: None,
                },
                name,
                1003,
            )
            .await
            .unwrap();
        control
            .set_membership(&owner, &tenant, &user.user_id, role, 1004)
            .await
            .unwrap();
        members.push(user);
    }
    let recipient = &members[0];
    let other = &members[1];
    let viewer = &members[2];
    let workspace = control
        .create_cloud_workspace(&owner, &tenant, &project, "Workspace", 1005)
        .await
        .unwrap();
    let duplicate = control
        .create_cloud_workspace(recipient, &tenant, &project, "Workspace", 1006)
        .await
        .unwrap();
    let mut sessions = Vec::new();
    for name in ["inherited", "independent"] {
        sessions.push(
            cloud
                .create_session(
                    CloudSessionDraft {
                        project_id: project.clone(),
                        workspace_id: workspace.workspace_id.clone(),
                        session_id: Some(SessionId::new(name)),
                        agent_id: AgentId::new("agent"),
                        title: name.into(),
                        permissions: PermissionPreset::WorkspaceWrite,
                        model: None,
                        reserved_model_tokens: 100,
                        agent_preset: "standard".into(),
                        profile_plugins: vec![],
                        mode: SessionMode::Execute,
                    },
                    &tenant,
                    &owner.user_id,
                    1007,
                )
                .await
                .unwrap(),
        );
    }
    let kind = ResourceKind::Workspace;
    let id = workspace.workspace_id.as_str();
    let input = ResourceOwnershipTransfer {
        owner_user_id: recipient.user_id.clone(),
        expected_owner_user_id: owner.user_id.clone(),
        expected_revision: 0,
        retain_previous_owner: false,
    };
    let candidates = control
        .resource_transfer_candidates(&owner, &tenant, kind, id, &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(candidates.candidates.len(), 2);
    assert!(candidates.candidates.iter().all(|subject|matches!(subject,ShareSubject::User{user} if user.user_id!=viewer.user_id && user.user_id!=owner.user_id)));
    assert!(
        control
            .transfer_resource_ownership(&owner, &tenant, kind, id, &input, 1008)
            .await
            .is_err(),
        "duplicate recipient name must reject atomically"
    );
    assert_eq!(
        control
            .resource_ownership(&owner, &tenant, kind, id)
            .await
            .unwrap()
            .revision,
        0
    );
    control
        .rename_owned_workspace(
            recipient,
            &tenant,
            &duplicate.workspace_id,
            "Recipient workspace",
            1009,
        )
        .await
        .unwrap();
    let invalid = ResourceOwnershipTransfer {
        owner_user_id: viewer.user_id.clone(),
        ..ResourceOwnershipTransfer {
            owner_user_id: input.owner_user_id.clone(),
            expected_owner_user_id: input.expected_owner_user_id.clone(),
            expected_revision: 0,
            retain_previous_owner: false,
        }
    };
    assert!(
        control
            .transfer_resource_ownership(&owner, &tenant, kind, id, &invalid, 1010)
            .await
            .is_err()
    );
    assert!(
        control
            .transfer_resource_ownership(recipient, &tenant, kind, id, &input, 1010)
            .await
            .is_err()
    );
    let read = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    control
        .set_resource_share(&owner, &tenant, kind, id, &other.user_id, Some(read), 1011)
        .await
        .unwrap();
    control
        .transfer_resource_ownership(
            &owner,
            &tenant,
            ResourceKind::Session,
            sessions[1].session_id.as_str(),
            &ResourceOwnershipTransfer {
                owner_user_id: other.user_id.clone(),
                expected_owner_user_id: owner.user_id.clone(),
                expected_revision: 0,
                retain_previous_owner: false,
            },
            1012,
        )
        .await
        .unwrap();
    let feed = CloudSessionEventFeed::connect(url).await.unwrap();
    let mut rx = feed.subscribe();
    let transferred = control
        .transfer_resource_ownership(&owner, &tenant, kind, id, &input, 1013)
        .await
        .unwrap();
    assert_eq!(transferred.owner.user_id, recipient.user_id);
    assert_eq!(transferred.revision, 1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap(),
        CloudLiveNotification::ResourcesChanged {
            tenant_id: tenant.clone()
        }
    );
    let access = control
        .resource_access(recipient, &tenant, kind, id)
        .await
        .unwrap();
    assert!(access.is_owner && access.can_manage_sharing && !access.is_execution_owner);
    assert_eq!(access.owner_user_id, recipient.user_id);
    assert_eq!(access.storage_user_id, owner.user_id);
    access.require(ResourceAction::Delete).unwrap();
    assert!(
        access.require(ResourceAction::ManageExecution).is_err(),
        "management must not grant execution configuration"
    );
    let former = control
        .resource_access(&owner, &tenant, kind, id)
        .await
        .unwrap();
    assert!(former.is_execution_owner && !former.is_owner && !former.permissions.view);
    assert!(
        control
            .rename_owned_workspace(
                &owner,
                &tenant,
                &workspace.workspace_id,
                "No longer mine",
                1014
            )
            .await
            .is_err()
    );
    assert!(
        control
            .transfer_resource_ownership(&owner, &tenant, kind, id, &input, 1014)
            .await
            .is_err()
    );
    let listed = control
        .list_accessible_workspaces(recipient, &tenant)
        .await
        .unwrap();
    assert!(
        listed
            .iter()
            .any(|item| item.workspace_id == workspace.workspace_id
                && item.owner_user_id == owner.user_id)
    );
    assert!(
        control
            .list_accessible_workspaces(&owner, &tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        control
            .resource_access(other, &tenant, kind, id)
            .await
            .unwrap()
            .permissions,
        read
    );
    for (session, manager) in [(&sessions[0], recipient), (&sessions[1], other)] {
        let access = control
            .resource_access(
                manager,
                &tenant,
                ResourceKind::Session,
                session.session_id.as_str(),
            )
            .await
            .unwrap();
        assert!(access.is_owner && !access.is_execution_owner);
        assert_eq!(access.storage_user_id, owner.user_id);
        assert_eq!(
            cloud
                .find_accessible_session(&tenant, &manager.user_id, &session.session_id)
                .await
                .unwrap()
                .unwrap()
                .user_id,
            owner.user_id
        );
        assert!(
            cloud
                .find_accessible_session(&tenant, &owner.user_id, &session.session_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    let mut tx = cloud
        .database()
        .owner_transaction(&tenant, &owner.user_id)
        .await
        .unwrap();
    for (seq, kind) in [
        (0, SessionEventKind::TurnStarted),
        (1, SessionEventKind::TurnCancelled),
    ] {
        let event = SessionEvent {
            seq,
            occurred_at_ms: 1015,
            run_id: RunId::new("ownership-run"),
            kind,
        };
        sqlx::query("INSERT INTO cloud_session_events (tenant_id,session_id,seq,run_id,event,writer_fencing_token,created_at_ms) VALUES ($1,$2,$3,$4,$5,1,1015)")
            .bind(tenant.as_str()).bind(sessions[1].session_id.as_str()).bind(i64::try_from(seq).unwrap()).bind(event.run_id.as_str().to_owned()).bind(ternilo_storage::Json(event)).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let fork = cloud
        .fork_session(&tenant, &other.user_id, &sessions[1].session_id, None, 1016)
        .await
        .unwrap();
    let fork_access = control
        .resource_access(
            other,
            &tenant,
            ResourceKind::Session,
            fork.session_id.as_str(),
        )
        .await
        .unwrap();
    assert!(fork_access.is_owner && !fork_access.is_execution_owner);
    assert_eq!(fork.user_id, owner.user_id);
    assert_eq!(fork.parent_session_id, Some(sessions[1].session_id.clone()));
    assert_eq!(
        cloud
            .list_accessible_sessions(&tenant, &other.user_id, 100)
            .await
            .unwrap()
            .iter()
            .filter(|item| item.session_id == fork.session_id)
            .count(),
        1
    );
    control
        .rename_owned_workspace(
            recipient,
            &tenant,
            &workspace.workspace_id,
            "Transferred workspace",
            1017,
        )
        .await
        .unwrap();
    assert!(
        control
            .create_cloud_workspace(recipient, &tenant, &project, "Transferred workspace", 1018)
            .await
            .is_err(),
        "logical management names must remain unique"
    );
    assert!(
        control
            .set_resource_share(&owner, &tenant, kind, id, &viewer.user_id, Some(read), 1019)
            .await
            .is_err()
    );
    control
        .set_resource_share(
            recipient,
            &tenant,
            kind,
            id,
            &viewer.user_id,
            Some(read),
            1020,
        )
        .await
        .unwrap();
    control
        .transfer_resource_ownership(
            recipient,
            &tenant,
            kind,
            id,
            &ResourceOwnershipTransfer {
                owner_user_id: owner.user_id.clone(),
                expected_owner_user_id: recipient.user_id.clone(),
                expected_revision: 1,
                retain_previous_owner: true,
            },
            1021,
        )
        .await
        .unwrap();
    let retained = control
        .resource_access(recipient, &tenant, kind, id)
        .await
        .unwrap();
    assert!(retained.permissions.submit && retained.permissions.configure);
    assert!(!retained.is_owner && !retained.can_manage_sharing && !retained.is_execution_owner);
    assert!(retained.require(ResourceAction::Delete).is_err());
    assert!(retained.require(ResourceAction::ManageExecution).is_err());
    assert!(
        control
            .transfer_resource_ownership(&owner, &tenant, kind, id, &input, 1022)
            .await
            .is_err(),
        "a round trip must not revive stale revision zero"
    );
    assert_eq!(
        control
            .resource_access(&owner, &tenant, kind, id)
            .await
            .unwrap()
            .ownership_revision,
        2
    );
    assert!(
        control
            .resource_access(
                other,
                &tenant,
                ResourceKind::Session,
                sessions[1].session_id.as_str()
            )
            .await
            .unwrap()
            .is_owner,
        "independent transfer must survive workspace handoff"
    );
    let inherited = sessions[0].session_id.as_str();
    let stale = ResourceOwnershipTransfer {
        owner_user_id: recipient.user_id.clone(),
        expected_owner_user_id: owner.user_id.clone(),
        expected_revision: 0,
        retain_previous_owner: false,
    };
    assert!(
        control
            .transfer_resource_ownership(
                &owner,
                &tenant,
                ResourceKind::Session,
                inherited,
                &stale,
                1022,
            )
            .await
            .is_err(),
        "inherited management must not revive a snapshot from before workspace handoff"
    );
    let independent = control
        .transfer_resource_ownership(
            &owner,
            &tenant,
            ResourceKind::Session,
            inherited,
            &ResourceOwnershipTransfer {
                expected_revision: 2,
                ..stale
            },
            1022,
        )
        .await
        .unwrap();
    assert_eq!(independent.revision, 3);
    assert_eq!(independent.owner.user_id, recipient.user_id);
    cloud
        .delete_session(&tenant, &fork.session_id, &other.user_id, 1023)
        .await
        .unwrap();
    let mut tx = control
        .database()
        .tenant_transaction(&tenant)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_resource_ownership WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2")
        .bind(tenant.as_str()).bind(fork.session_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        count, 0,
        "deletion must not leave ownership attached to a reusable ID"
    );
    tx.commit().await.unwrap();
    control
        .unregister_owned_workspace(&owner, &tenant, &workspace.workspace_id, 1024)
        .await
        .unwrap();
    assert!(
        control
            .transfer_resource_ownership(
                &owner,
                &tenant,
                kind,
                id,
                &ResourceOwnershipTransfer {
                    owner_user_id: recipient.user_id.clone(),
                    expected_owner_user_id: owner.user_id.clone(),
                    expected_revision: 2,
                    retain_previous_owner: false
                },
                1025
            )
            .await
            .is_err()
    );
    drop(feed);
    cloud.database().close().await;
    control.database().close().await;
}

#[tokio::test]
async fn sqlite_resource_handoff_preserves_storage_and_independent_session_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("ownership.sqlite").display()
    );
    contract(&url, None).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn restricted_postgres_resource_handoff_preserves_storage_and_independent_session_ownership()
{
    let owner = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(owner.contains("ternilo_cloud_test"));
    let runtime =
        server_runtime::initialize(&owner, "ternilo_resource_ownership_test", [63; 32]).await;
    server_runtime::assert_scoped_without_schema_access(&runtime).await;
    contract(&runtime, Some(&owner)).await;
}
