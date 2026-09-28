use crate::{
    ControlStore, ControlUser, GroupInput, InstanceMode, NativeRegistration, OidcPrincipal,
    PageQuery, ResourceAction, ResourceKind, ResourcePermissions, SecretCipher, ShareSubject,
    TenantQuota, TenantRole,
};
use sqlx::Executor as _;
use ternilo_protocol::{ErrorCode, TenantId, WorkspaceId};

async fn member(
    store: &ControlStore,
    owner: &ControlUser,
    tenant: &TenantId,
    name: &str,
) -> ControlUser {
    let user = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "project-test".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            1_001,
        )
        .await
        .unwrap();
    store
        .set_membership(owner, tenant, &user.user_id, TenantRole::Member, 1_002)
        .await
        .unwrap();
    user
}

async fn access(
    store: &ControlStore,
    actor: &ControlUser,
    tenant: &TenantId,
    workspace: &WorkspaceId,
) -> crate::ResourceAccess {
    store
        .resource_access(actor, tenant, ResourceKind::Workspace, workspace.as_str())
        .await
        .unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise owner opt-in, role limits, direct grants and live group rules on the same independent resource."
)]
async fn contract(store: &ControlStore) {
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "project-owner@example.test".to_owned(),
                username: "project-owner".to_owned(),
                password: "project-owner-password".to_owned(),
            },
            1_000,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1_001)
        .await
        .unwrap();
    let tenant = store
        .create_tenant(
            &owner,
            "projects",
            "Projects",
            TenantQuota::default(),
            1_002,
        )
        .await
        .unwrap()
        .tenant_id;
    let machine_owner = member(store, &owner, &tenant, "machine-owner").await;
    let reader = member(store, &owner, &tenant, "project-reader").await;
    let project = store
        .list_projects(&owner, &tenant)
        .await
        .unwrap()
        .remove(0);
    let workspace = store
        .create_cloud_workspace(
            &machine_owner,
            &tenant,
            &project.project_id,
            "Private work",
            1_003,
        )
        .await
        .unwrap();
    let private = store
        .create_cloud_workspace(
            &machine_owner,
            &tenant,
            &project.project_id,
            "Other private work",
            1_003,
        )
        .await
        .unwrap();
    let id = &workspace.workspace_id;
    let read = ResourcePermissions {
        view: true,
        ..Default::default()
    };
    let work = ResourcePermissions::OWNER;
    let project_access = store
        .resource_access(&owner, &tenant, ResourceKind::Project, &project.project_id)
        .await
        .unwrap();
    assert!(project_access.can_manage_sharing);
    assert!(!access(store, &owner, &tenant, id).await.permissions.view);
    assert!(
        !store
            .workspace_project_sharing(&machine_owner, &tenant, id)
            .await
            .unwrap()
            .enabled
    );
    store
        .set_membership(
            &owner,
            &tenant,
            &machine_owner.user_id,
            TenantRole::Admin,
            1_004,
        )
        .await
        .unwrap();
    let administrator = store
        .resource_access(
            &machine_owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
        )
        .await
        .unwrap();
    assert!(administrator.can_manage_sharing && !administrator.is_owner);
    store
        .set_resource_share(
            &machine_owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &reader.user_id,
            Some(work),
            1_004,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant,
            &machine_owner.user_id,
            TenantRole::Member,
            1_004,
        )
        .await
        .unwrap();
    assert!(
        !store
            .resource_access(
                &machine_owner,
                &tenant,
                ResourceKind::Project,
                &project.project_id
            )
            .await
            .unwrap()
            .can_manage_sharing
    );
    assert!(
        !access(store, &reader, &tenant, id).await.permissions.view,
        "a project rule cannot enroll another user's private workspace"
    );
    assert_eq!(
        store
            .set_workspace_project_sharing(&owner, &tenant, id, true, 1_005)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        store
            .set_resource_share(
                &reader,
                &tenant,
                ResourceKind::Project,
                &project.project_id,
                &owner.user_id,
                Some(work),
                1_005
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_workspace_project_sharing(&machine_owner, &tenant, id, true, 1_006)
        .await
        .unwrap();
    let inherited = access(store, &reader, &tenant, id).await;
    assert_eq!(inherited.permissions, work);
    assert!(!inherited.is_owner && !inherited.can_manage_sharing);
    assert_eq!(inherited.owner_user_id, machine_owner.user_id);
    assert!(inherited.require(ResourceAction::Delete).is_err());
    assert!(inherited.require(ResourceAction::ManageSharing).is_err());
    assert_eq!(inherited.sources[0].resource_kind, ResourceKind::Project);
    assert_eq!(inherited.sources[0].resource_id, project.project_id);
    assert_eq!(
        inherited.sources[0].resource_name.as_deref(),
        Some(project.name.as_str())
    );
    assert!(
        !access(store, &owner, &tenant, id).await.permissions.view,
        "project management is not workspace access"
    );
    assert!(
        !access(store, &reader, &tenant, &private.workspace_id)
            .await
            .permissions
            .view
    );
    assert_eq!(
        store
            .list_accessible_workspaces(&reader, &tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    let candidates = store
        .resource_share_candidates(
            &owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            "user",
            &PageQuery::default(),
        )
        .await
        .unwrap();
    assert!(candidates.candidates.iter().any(
        |subject| matches!(subject, ShareSubject::User { user } if user.user_id == owner.user_id)
    ));
    store
        .set_resource_share(
            &owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &owner.user_id,
            Some(read),
            1_007,
        )
        .await
        .unwrap();
    let admin_read = access(store, &owner, &tenant, id).await;
    assert!(admin_read.permissions.view && !admin_read.can_manage_sharing && !admin_read.is_owner);
    let page = store
        .list_resource_shares(
            &reader,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &PageQuery {
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.shares.len(), 1);
    assert!(
        page.next_cursor.is_some(),
        "members can inspect paginated project rules before opting in"
    );
    let next = store
        .list_resource_shares(
            &reader,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &PageQuery {
                limit: 1,
                cursor: page.next_cursor,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(next.shares.len(), 1);
    assert!(next.next_cursor.is_none());
    store
        .set_membership(&owner, &tenant, &reader.user_id, TenantRole::Viewer, 1_008)
        .await
        .unwrap();
    let limited = access(store, &reader, &tenant, id).await;
    assert!(limited.role_limited);
    assert_eq!(limited.permissions, read);
    store
        .set_membership(&owner, &tenant, &reader.user_id, TenantRole::Member, 1_009)
        .await
        .unwrap();
    store
        .set_resource_share(
            &machine_owner,
            &tenant,
            ResourceKind::Workspace,
            id.as_str(),
            &reader.user_id,
            Some(read),
            1_010,
        )
        .await
        .unwrap();
    store
        .set_resource_share(
            &owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &reader.user_id,
            None,
            1_011,
        )
        .await
        .unwrap();
    assert_eq!(
        access(store, &reader, &tenant, id).await.permissions,
        read,
        "direct grants survive project revocation independently"
    );
    store
        .set_resource_share(
            &machine_owner,
            &tenant,
            ResourceKind::Workspace,
            id.as_str(),
            &reader.user_id,
            None,
            1_012,
        )
        .await
        .unwrap();
    let group = store
        .create_permission_group(
            &owner,
            &tenant,
            &GroupInput {
                name: "Contributors".to_owned(),
                description: None,
            },
            1_013,
        )
        .await
        .unwrap();
    store
        .set_permission_group_member(
            &owner,
            &tenant,
            &group.group_id,
            &reader.user_id,
            true,
            1_014,
        )
        .await
        .unwrap();
    store
        .set_resource_group_share(
            &owner,
            &tenant,
            ResourceKind::Project,
            &project.project_id,
            &group.group_id,
            Some(work),
            1_015,
        )
        .await
        .unwrap();
    assert_eq!(access(store, &reader, &tenant, id).await.permissions, work);
    store
        .set_permission_group_member(
            &owner,
            &tenant,
            &group.group_id,
            &reader.user_id,
            false,
            1_016,
        )
        .await
        .unwrap();
    assert!(!access(store, &reader, &tenant, id).await.permissions.view);
    assert!(
        store
            .list_accessible_workspaces(&reader, &tenant)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .set_permission_group_member(
            &owner,
            &tenant,
            &group.group_id,
            &reader.user_id,
            true,
            1_017,
        )
        .await
        .unwrap();
    store
        .set_workspace_project_sharing(&machine_owner, &tenant, id, false, 1_018)
        .await
        .unwrap();
    assert!(!access(store, &reader, &tenant, id).await.permissions.view);
    store
        .set_workspace_project_sharing(&machine_owner, &tenant, id, true, 1_019)
        .await
        .unwrap();
    assert_eq!(access(store, &reader, &tenant, id).await.permissions, work);
    let other = store
        .create_tenant(
            &owner,
            "other-projects",
            "Other projects",
            TenantQuota::default(),
            1_020,
        )
        .await
        .unwrap()
        .tenant_id;
    assert!(
        store
            .resource_access(&owner, &other, ResourceKind::Project, &project.project_id)
            .await
            .is_err()
    );
    assert!(
        store
            .set_workspace_project_sharing(&machine_owner, &other, id, true, 1_021)
            .await
            .is_err()
    );
    if store.database().backend() == ternilo_storage::Backend::Postgres {
        let mut tx = store.database().tenant_transaction(&tenant).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM control_project_workspace_access")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert!(count > 0);
        tx.commit().await.unwrap();
        for table in [
            "control_project_user_shares",
            "control_project_group_shares",
            "control_workspace_project_sharing",
            "control_project_workspace_access",
        ] {
            let sql = format!("SELECT COUNT(*) FROM {table}");
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.clone()))
                .fetch_one(store.database().pool())
                .await
                .unwrap();
            assert_eq!(count, 0, "{table} must not bypass the runtime tenant scope");
            let mut tx = store.database().tenant_transaction(&other).await.unwrap();
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            assert_eq!(
                count, 0,
                "{table} must not leak through another tenant or a view owner"
            );
            tx.rollback().await.unwrap();
        }
    }
}

#[tokio::test]
async fn sqlite_project_sharing_requires_owner_opt_in_and_current_policy() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([83; 32]), 1)
        .await
        .unwrap();
    contract(&store).await;
}

#[tokio::test]
async fn existing_control_database_installs_project_component_without_changing_identity_or_ownership()
 {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("existing.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([83; 32]), 1)
        .await
        .unwrap();
    contract(&store).await;
    let owner = store
        .authenticate_native_credentials("project-owner", "project-owner-password")
        .await
        .unwrap();
    let existing: Vec<(String, String)> = sqlx::query_as(
        "SELECT workspace_id,owner_user_id FROM control_workspaces ORDER BY workspace_id",
    )
    .fetch_all(store.database().pool())
    .await
    .unwrap();
    // Reproduce Control 14 before the optional sharing component existed.
    sqlx::raw_sql(
        "DROP VIEW control_project_workspace_access;
        DROP TABLE control_workspace_project_sharing;
        DROP TABLE control_project_group_shares;
        DROP TABLE control_project_user_shares;
        DELETE FROM ternilo_schema WHERE component='project_sharing';",
    )
    .execute(store.database().pool())
    .await
    .unwrap();
    store.database().close().await;
    let restored = ControlStore::connect(&url, None, SecretCipher::from_key([83; 32]), 1)
        .await
        .unwrap();
    assert_eq!(
        restored
            .authenticate_native_credentials("project-owner", "project-owner-password")
            .await
            .unwrap(),
        owner
    );
    let retained: Vec<(String, String)> = sqlx::query_as(
        "SELECT workspace_id,owner_user_id FROM control_workspaces ORDER BY workspace_id",
    )
    .fetch_all(restored.database().pool())
    .await
    .unwrap();
    assert_eq!(retained, existing);
    let control: i64 =
        sqlx::query_scalar("SELECT version FROM ternilo_schema WHERE component='control'")
            .fetch_one(restored.database().pool())
            .await
            .unwrap();
    let opted_in: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_workspace_project_sharing")
            .fetch_one(restored.database().pool())
            .await
            .unwrap();
    assert_eq!((control, opted_in), (14, 0));
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_project_sharing_preserves_runtime_rls() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(
        &admin,
        "project_sharing_runtime",
        "project-runtime-password",
    )
    .await;
    let mut runtime = reqwest::Url::parse(&url).unwrap();
    runtime.set_username("project_sharing_runtime").unwrap();
    runtime
        .set_password(Some("project-runtime-password"))
        .unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([83; 32]),
        2,
    )
    .await
    .unwrap();
    contract(&store).await;
    store.database().close().await;
    admin.close().await;
}
