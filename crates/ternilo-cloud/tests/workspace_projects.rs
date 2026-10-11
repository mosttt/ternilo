use std::time::Duration;

use ternilo_cloud::{CloudLiveNotification, CloudSessionDraft, CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourceOwnershipTransfer, ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
    WorkspaceProject, WorkspaceProjectMove,
};
use ternilo_protocol::{AgentId, ErrorCode, PermissionPreset, SessionMode, TenantId, WorkspaceId};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

async fn member(
    control: &ControlStore,
    admin: &ControlUser,
    tenant: &TenantId,
    name: &str,
) -> ControlUser {
    let actor = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "workspace-project-test".into(),
                subject: name.into(),
                email: None,
                display_name: None,
            },
            name,
            1_004,
        )
        .await
        .unwrap();
    control
        .set_membership(admin, tenant, &actor.user_id, TenantRole::Member, 1_005)
        .await
        .unwrap();
    actor
}

fn input(previous: &WorkspaceProject, project: &str, inherit: bool) -> WorkspaceProjectMove {
    WorkspaceProjectMove {
        project_id: project.into(),
        expected_project_id: previous.project_id.clone(),
        expected_revision: previous.revision,
        inherit_project_sharing: inherit,
    }
}

async fn can_view(
    control: &ControlStore,
    actor: &ControlUser,
    tenant: &TenantId,
    workspace: &WorkspaceId,
) -> bool {
    control
        .resource_access(actor, tenant, ResourceKind::Workspace, workspace.as_str())
        .await
        .unwrap()
        .permissions
        .view
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise project moves, history, grants, ownership, version conflicts and rollback on the same resources."
)]
async fn contract(url: &str, migration: Option<&str>) {
    let control = ControlStore::connect(url, migration, SecretCipher::from_key([64; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(url, migration, 4).await.unwrap();
    let identity = control
        .initialize_owner(
            &NativeRegistration {
                username: "project-move-admin".into(),
                email: "project-move@example.test".into(),
                password: "project-move-password".into(),
            },
            1_000,
        )
        .await
        .unwrap()
        .session;
    let personal_tenant = identity.personal_tenant_id;
    let personal_project = identity.personal_project_id;
    let admin = identity.user;
    control
        .set_instance_mode(&admin, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &admin,
            "project-moves",
            "Project moves",
            TenantQuota::default(),
            1_002,
        )
        .await
        .unwrap()
        .tenant_id;
    let source = control
        .list_projects(&admin, &tenant)
        .await
        .unwrap()
        .remove(0)
        .project_id;
    let destination = control
        .create_project(&admin, &tenant, "Destination", 1_003)
        .await
        .unwrap()
        .project_id;
    let manager = member(&control, &admin, &tenant, "workspace-manager").await;
    let previous_reader = member(&control, &admin, &tenant, "previous-reader").await;
    let next_reader = member(&control, &admin, &tenant, "next-reader").await;
    let direct_reader = member(&control, &admin, &tenant, "direct-reader").await;
    let workspace = control
        .create_cloud_workspace(&manager, &tenant, &source, "Moving workspace", 1_006)
        .await
        .unwrap();
    let id = &workspace.workspace_id;
    let draft = CloudSessionDraft {
        project_id: source.clone(),
        workspace_id: id.clone(),
        session_id: None,
        agent_id: AgentId::new("agent"),
        title: "History stays here".into(),
        permissions: PermissionPreset::WorkspaceWrite,
        model: None,
        reserved_model_tokens: 100,
        agent_preset: "standard".into(),
        profile_plugins: vec![],
        mode: SessionMode::Execute,
    };
    let session = cloud
        .create_session(draft.clone(), &tenant, &manager.user_id, 1_007)
        .await
        .unwrap();
    let independent = cloud
        .create_session(draft.clone(), &tenant, &manager.user_id, 1_007)
        .await
        .unwrap();
    control
        .transfer_resource_ownership(
            &manager,
            &tenant,
            ResourceKind::Session,
            independent.session_id.as_str(),
            &ResourceOwnershipTransfer {
                owner_user_id: next_reader.user_id.clone(),
                expected_owner_user_id: manager.user_id.clone(),
                expected_revision: 0,
                retain_previous_owner: false,
            },
            1_008,
        )
        .await
        .unwrap();
    let event = ternilo_protocol::SessionEvent {
        seq: 0,
        run_id: ternilo_protocol::RunId::new("before-move"),
        occurred_at_ms: 1_008,
        kind: ternilo_protocol::SessionEventKind::UserMessage {
            content: "Original history".into(),
            provenance: None,
            display_content: None,
            source: None,
            references: vec![],
            attachments: vec![],
        },
    };
    let mut tx = control
        .database()
        .tenant_transaction(&tenant)
        .await
        .unwrap();
    sqlx::query("INSERT INTO cloud_session_events VALUES($1,$2,0,'before-move',$3,1,1008)")
        .bind(tenant.as_str())
        .bind(session.session_id.as_str())
        .bind(ternilo_storage::Json(&event))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let read = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    for (project, reader) in [(&source, &previous_reader), (&destination, &next_reader)] {
        control
            .set_resource_share(
                &admin,
                &tenant,
                ResourceKind::Project,
                project,
                &reader.user_id,
                Some(read),
                1_009,
            )
            .await
            .unwrap();
    }
    control
        .set_resource_share(
            &manager,
            &tenant,
            ResourceKind::Workspace,
            id.as_str(),
            &direct_reader.user_id,
            Some(read),
            1_010,
        )
        .await
        .unwrap();
    control
        .set_workspace_project_sharing(&manager, &tenant, id, true, 1_011)
        .await
        .unwrap();
    let initial = control
        .workspace_project(&manager, &tenant, id)
        .await
        .unwrap();
    assert_eq!(initial.revision, 0);
    assert_eq!(initial.project_id, source);
    assert!(initial.inherit_project_sharing);
    let stale = input(&initial, &destination, false);
    let audit_before = control.list_audit(&admin, &tenant, 100).await.unwrap();
    for actor in [&admin, &previous_reader, &next_reader, &direct_reader] {
        assert_eq!(
            control
                .move_workspace_project(actor, &tenant, id, &stale, 1_012)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
    assert_eq!(
        control.list_audit(&admin, &tenant, 100).await.unwrap(),
        audit_before
    );
    let duplicate = control
        .create_cloud_workspace(&manager, &tenant, &destination, "Moving workspace", 1_013)
        .await
        .unwrap();
    let before = control.list_audit(&admin, &tenant, 100).await.unwrap();
    assert_eq!(
        control
            .move_workspace_project(&manager, &tenant, id, &stale, 1_014)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        control
            .workspace_project(&manager, &tenant, id)
            .await
            .unwrap()
            .revision,
        0
    );
    assert_eq!(
        control.list_audit(&admin, &tenant, 100).await.unwrap(),
        before
    );
    control
        .rename_owned_workspace(
            &manager,
            &tenant,
            &duplicate.workspace_id,
            "Other workspace",
            1_015,
        )
        .await
        .unwrap();
    let other_tenant = control
        .create_tenant(
            &admin,
            "other-space",
            "Other space",
            TenantQuota::default(),
            1_016,
        )
        .await
        .unwrap()
        .tenant_id;
    let foreign_project = control
        .list_projects(&admin, &other_tenant)
        .await
        .unwrap()
        .remove(0)
        .project_id;
    assert_eq!(
        control
            .move_workspace_project(
                &manager,
                &tenant,
                id,
                &input(&initial, &foreign_project, false),
                1_017
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    control
        .set_membership(&admin, &tenant, &manager.user_id, TenantRole::Viewer, 1_018)
        .await
        .unwrap();
    assert_eq!(
        control
            .move_workspace_project(&manager, &tenant, id, &stale, 1_019)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    control
        .set_membership(&admin, &tenant, &manager.user_id, TenantRole::Member, 1_020)
        .await
        .unwrap();
    let feed = CloudSessionEventFeed::connect(url).await.unwrap();
    let mut notifications = feed.subscribe();
    let moved = control
        .move_workspace_project(&manager, &tenant, id, &stale, 1_021)
        .await
        .unwrap();
    assert_eq!(moved.project_id, destination);
    assert_eq!(moved.execution_project_id, source);
    assert_eq!(moved.revision, 1);
    assert!(!moved.inherit_project_sharing);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), notifications.recv())
            .await
            .unwrap()
            .unwrap(),
        CloudLiveNotification::ResourcesChanged {
            tenant_id: tenant.clone()
        }
    );
    assert!(!can_view(&control, &previous_reader, &tenant, id).await);
    assert!(!can_view(&control, &next_reader, &tenant, id).await);
    assert!(can_view(&control, &direct_reader, &tenant, id).await);
    assert!(
        cloud
            .find_accessible_session(&tenant, &next_reader.user_id, &independent.session_id)
            .await
            .unwrap()
            .is_some()
    );
    let unchanged = cloud
        .find_accessible_session(&tenant, &manager.user_id, &session.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.project_id, source);
    assert_eq!(unchanged.user_id, manager.user_id);
    assert_eq!(unchanged.workspace_id, *id);
    let mut tx = control
        .database()
        .tenant_transaction(&tenant)
        .await
        .unwrap();
    let preserved: ternilo_storage::Json<ternilo_protocol::SessionEvent> = sqlx::query_scalar(
        "SELECT event FROM cloud_session_events WHERE tenant_id=$1 AND session_id=$2 AND seq=0",
    )
    .bind(tenant.as_str())
    .bind(session.session_id.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(preserved.0, event);
    tx.commit().await.unwrap();
    cloud
        .create_session(draft, &tenant, &manager.user_id, 1_022)
        .await
        .unwrap();
    let replacement = control
        .create_cloud_workspace(&manager, &tenant, &source, "Moving workspace", 1_023)
        .await
        .unwrap();
    assert!(
        control
            .rename_owned_workspace(
                &manager,
                &tenant,
                &duplicate.workspace_id,
                "Moving workspace",
                1_024
            )
            .await
            .is_err()
    );
    assert_eq!(
        control
            .delete_project(&admin, &tenant, &destination, 1_025)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    control
        .unregister_owned_workspace(&manager, &tenant, &replacement.workspace_id, 1_026)
        .await
        .unwrap();
    let returned = control
        .move_workspace_project(&manager, &tenant, id, &input(&moved, &source, true), 1_027)
        .await
        .unwrap();
    assert_eq!(returned.revision, 2);
    assert!(can_view(&control, &previous_reader, &tenant, id).await);
    assert_eq!(
        control
            .move_workspace_project(&manager, &tenant, id, &stale, 1_028)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let moved = control
        .move_workspace_project(
            &manager,
            &tenant,
            id,
            &input(&returned, &destination, true),
            1_029,
        )
        .await
        .unwrap();
    assert!(can_view(&control, &next_reader, &tenant, id).await);
    assert!(!can_view(&control, &previous_reader, &tenant, id).await);
    assert!(
        control
            .list_accessible_workspaces(&next_reader, &tenant)
            .await
            .unwrap()
            .iter()
            .any(|row| row.workspace_id == *id)
    );
    assert!(
        !control
            .list_accessible_workspaces(&previous_reader, &tenant)
            .await
            .unwrap()
            .iter()
            .any(|row| row.workspace_id == *id)
    );
    assert!(
        cloud
            .list_accessible_sessions(&tenant, &next_reader.user_id, 100)
            .await
            .unwrap()
            .iter()
            .any(|row| row.session_id == session.session_id)
    );
    assert!(
        !cloud
            .list_accessible_sessions(&tenant, &previous_reader.user_id, 100)
            .await
            .unwrap()
            .iter()
            .any(|row| row.session_id == session.session_id)
    );
    assert!(
        cloud
            .find_accessible_session(&tenant, &next_reader.user_id, &session.session_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        cloud
            .find_accessible_session(&tenant, &previous_reader.user_id, &session.session_id)
            .await
            .unwrap()
            .is_none()
    );
    let recipient_duplicate = control
        .create_cloud_workspace(
            &direct_reader,
            &tenant,
            &destination,
            "Moving workspace",
            1_030,
        )
        .await
        .unwrap();
    let handoff = ResourceOwnershipTransfer {
        owner_user_id: direct_reader.user_id.clone(),
        expected_owner_user_id: manager.user_id.clone(),
        expected_revision: 0,
        retain_previous_owner: true,
    };
    assert_eq!(
        control
            .transfer_resource_ownership(
                &manager,
                &tenant,
                ResourceKind::Workspace,
                id.as_str(),
                &handoff,
                1_031
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    control
        .rename_owned_workspace(
            &direct_reader,
            &tenant,
            &recipient_duplicate.workspace_id,
            "Recipient other",
            1_032,
        )
        .await
        .unwrap();
    control
        .transfer_resource_ownership(
            &manager,
            &tenant,
            ResourceKind::Workspace,
            id.as_str(),
            &handoff,
            1_033,
        )
        .await
        .unwrap();
    assert_eq!(
        control
            .move_workspace_project(&manager, &tenant, id, &input(&moved, &source, false), 1_034)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    control
        .move_workspace_project(
            &direct_reader,
            &tenant,
            id,
            &input(&moved, &source, false),
            1_035,
        )
        .await
        .unwrap();
    let access = control
        .resource_access(
            &direct_reader,
            &tenant,
            ResourceKind::Workspace,
            id.as_str(),
        )
        .await
        .unwrap();
    assert!(access.is_owner && !access.is_execution_owner);
    assert_eq!(access.storage_user_id, manager.user_id);
    control
        .unregister_owned_workspace(&direct_reader, &tenant, id, 1_036)
        .await
        .unwrap();
    assert_eq!(
        control
            .workspace_project(&direct_reader, &tenant, id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    drop(feed);
    let (first, second) = tokio::join!(
        control.create_cloud_workspace(
            &manager,
            &tenant,
            &source,
            "Concurrent registration",
            2_000
        ),
        control.create_cloud_workspace(
            &manager,
            &tenant,
            &source,
            "Concurrent registration",
            2_000
        ),
    );
    let registrations = [first, second];
    assert_eq!(
        registrations.iter().filter(|result| result.is_ok()).count(),
        1
    );
    for error in registrations.into_iter().filter_map(Result::err) {
        assert_eq!(error.code, ErrorCode::Conflict);
    }
    let personal = control
        .create_cloud_workspace(
            &admin,
            &personal_tenant,
            &personal_project,
            "Personal workspace",
            2_001,
        )
        .await
        .unwrap();
    let personal_destination = control
        .create_project(&admin, &personal_tenant, "Personal destination", 2_002)
        .await
        .unwrap();
    let before = control
        .workspace_project(&admin, &personal_tenant, &personal.workspace_id)
        .await
        .unwrap();
    assert_eq!(
        control
            .move_workspace_project(
                &admin,
                &personal_tenant,
                &personal.workspace_id,
                &input(&before, &personal_destination.project_id, true),
                2_003
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        control
            .workspace_project(&admin, &personal_tenant, &personal.workspace_id)
            .await
            .unwrap()
            .revision,
        0,
        "invalid personal sharing must roll back the project assignment"
    );
    let personal_moved = control
        .move_workspace_project(
            &admin,
            &personal_tenant,
            &personal.workspace_id,
            &input(&before, &personal_destination.project_id, false),
            2_004,
        )
        .await
        .unwrap();
    assert_eq!(personal_moved.execution_project_id, personal_project);
    assert_eq!(personal_moved.project_id, personal_destination.project_id);
    assert!(!personal_moved.inherit_project_sharing);
    if control.database().backend() == ternilo_storage::Backend::Postgres {
        let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_workspace_projects")
            .fetch_one(control.database().pool())
            .await
            .unwrap();
        assert_eq!(visible, 0, "project assignments require a tenant scope");
    }
    cloud.database().close().await;
    control.database().close().await;
}

#[tokio::test]
async fn sqlite_project_moves_preserve_execution_and_update_explicit_sharing() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}",
            directory.path().join("projects.sqlite").display()
        ),
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn restricted_postgres_project_moves_preserve_execution_and_update_explicit_sharing() {
    let owner = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(owner.contains("ternilo_cloud_test"));
    let runtime =
        server_runtime::initialize(&owner, "ternilo_workspace_projects_test", [64; 32]).await;
    server_runtime::assert_scoped_without_schema_access(&runtime).await;
    contract(&runtime, Some(&owner)).await;
}
