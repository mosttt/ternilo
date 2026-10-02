use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, SecretCipher,
    ServiceAccountCreate, ServiceAccountUpdate, ServiceCredentialCreate, ServiceScope, TenantQuota,
};
use ternilo_protocol::{ErrorCode, TenantId};

#[derive(Clone)]
struct Fixture {
    store: ControlStore,
    owner: ControlUser,
    tenant: TenantId,
    project: String,
    native_token: String,
}

impl Fixture {
    async fn new() -> Self {
        Self::connect("sqlite::memory:", None).await
    }

    async fn connect(url: &str, migration: Option<&str>) -> Self {
        let store = ControlStore::connect(url, migration, SecretCipher::from_key([57; 32]), 2)
            .await
            .unwrap();
        let grant = store
            .initialize_owner(
                &NativeRegistration {
                    username: "service-owner".into(),
                    email: "service-owner@example.test".into(),
                    password: "service-test-password".into(),
                },
                1_000,
            )
            .await
            .unwrap();
        Self {
            store,
            owner: grant.session.user,
            tenant: grant.session.personal_tenant_id,
            project: grant.session.personal_project_id,
            native_token: grant.access_token,
        }
    }
}

#[tokio::test]
async fn service_identity_credentials_and_resources_do_not_impersonate_the_creator() {
    identity_contract(Fixture::new().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "One lifecycle proves independent authorship, resource isolation and revocation using both database backends."
)]
async fn identity_contract(f: Fixture) {
    let account = f
        .store
        .create_service_account(
            &f.owner,
            &f.tenant,
            &ServiceAccountCreate {
                name: "CI 发布".into(),
                notes: String::new(),
            },
            2_000,
        )
        .await
        .unwrap();
    assert!(account.service_account_id.as_str().starts_with("ter_sa_"));
    assert_ne!(account.service_account_id, f.owner.user_id);
    if f.store.database().backend() == ternilo_storage::Backend::Postgres {
        let mut other = f
            .store
            .database()
            .tenant_transaction(&TenantId::new("other-tenant"))
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_service_accounts")
            .fetch_one(&mut *other)
            .await
            .unwrap();
        assert_eq!(count, 0);
        other.rollback().await.unwrap();
    }
    let grant = f
        .store
        .create_service_credential(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &ServiceCredentialCreate {
                name: "发布任务".into(),
                scopes: vec![ServiceScope::ResourceRead, ServiceScope::RunExecute],
                expires_at_ms: 100_000,
            },
            2_001,
        )
        .await
        .unwrap();
    assert!(grant.access_token.starts_with("ter_t_"));
    let principal = f
        .store
        .authenticate_service_credential(&grant.access_token, &f.tenant, 2_002)
        .await
        .unwrap();
    assert_eq!(principal.user.user_id, account.service_account_id);
    assert_eq!(principal.tenant_id, f.tenant);
    principal.require(ServiceScope::RunExecute).unwrap();
    assert!(
        f.store
            .identity_session(principal.user.clone())
            .await
            .is_err(),
        "service credentials do not create a human browser session"
    );
    assert!(
        f.store
            .list_service_accounts(&principal.user, &f.tenant)
            .await
            .is_err(),
        "service accounts cannot manage accounts or issue more credentials"
    );
    let private = f
        .store
        .create_cloud_workspace(
            &f.owner,
            &f.tenant,
            &f.project,
            "Private human workspace",
            2_003,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .list_accessible_workspaces(&principal.user, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.store
            .resolve_accessible_workspace(&principal.user, &f.tenant, &private.workspace_id)
            .await
            .is_err()
    );
    let workspace = f
        .store
        .create_cloud_workspace(&principal.user, &f.tenant, &f.project, "Automation", 2_003)
        .await
        .unwrap();
    assert_eq!(workspace.owner_user_id, account.service_account_id);
    assert_ne!(workspace.owner_user_id, f.owner.user_id);
    let credentials = f
        .store
        .list_service_credentials(&f.owner, &f.tenant, &account.service_account_id)
        .await
        .unwrap();
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].last_used_at_ms, Some(2_002));
    let encoded = serde_json::to_string(&credentials).unwrap();
    assert!(!encoded.contains(&grant.access_token));
    assert!(!encoded.contains("token_hash"));
    f.store
        .revoke_service_credential(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &grant.credential.credential_id,
            2_004,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &f.tenant, 2_005)
            .await
            .is_err()
    );
    assert!(
        f.store
            .authenticate_native_token(&f.native_token, 2_005)
            .await
            .is_ok()
    );
    assert_eq!(
        f.store
            .list_service_accounts(&f.owner, &f.tenant)
            .await
            .unwrap()[0]
            .service_account_id,
        account.service_account_id
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The lifecycle keeps identity and credential state across disable, re-enable, stale edits and expiry."
)]
async fn service_scope_expiry_disable_and_revision_enforce_the_observed_identity() {
    let f = Fixture::new().await;
    let account = f
        .store
        .create_service_account(
            &f.owner,
            &f.tenant,
            &ServiceAccountCreate {
                name: "只读任务".into(),
                notes: "自动读取报告".into(),
            },
            2_000,
        )
        .await
        .unwrap();
    let grant = f
        .store
        .create_service_credential(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &ServiceCredentialCreate {
                name: "统计读取".into(),
                scopes: vec![ServiceScope::ResourceRead, ServiceScope::ResourceRead],
                expires_at_ms: 5_000,
            },
            2_001,
        )
        .await
        .unwrap();
    let principal = f
        .store
        .authenticate_service_credential(&grant.access_token, &f.tenant, 2_002)
        .await
        .unwrap();
    assert_eq!(principal.scopes, vec![ServiceScope::ResourceRead]);
    assert_eq!(
        principal
            .require(ServiceScope::RunExecute)
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let update = ServiceAccountUpdate {
        name: "改名后的任务".into(),
        notes: "保留身份和记录".into(),
        enabled: false,
        expected_revision: account.revision,
    };
    let disabled = f
        .store
        .update_service_account(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &update,
            2_003,
        )
        .await
        .unwrap();
    assert_eq!(disabled.service_account_id, account.service_account_id);
    assert!(!disabled.enabled);
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &f.tenant, 2_004)
            .await
            .is_err()
    );
    assert_eq!(
        f.store
            .update_service_account(
                &f.owner,
                &f.tenant,
                &account.service_account_id,
                &update,
                2_004
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let enabled = f
        .store
        .update_service_account(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &ServiceAccountUpdate {
                enabled: true,
                expected_revision: disabled.revision,
                ..update
            },
            2_005,
        )
        .await
        .unwrap();
    assert!(enabled.enabled);
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &f.tenant, 2_006)
            .await
            .is_ok()
    );
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &f.tenant, 5_000)
            .await
            .is_err()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The contract checks credential and name boundaries between two real spaces."
)]
async fn service_credentials_cannot_cross_spaces_and_creation_requires_management() {
    let f = Fixture::new().await;
    let account = f
        .store
        .create_service_account(
            &f.owner,
            &f.tenant,
            &ServiceAccountCreate {
                name: "报告任务".into(),
                notes: String::new(),
            },
            2_000,
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .create_service_account(
                &f.owner,
                &f.tenant,
                &ServiceAccountCreate {
                    name: " 报告任务 ".into(),
                    notes: String::new()
                },
                2_001
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    f.store
        .set_instance_mode(&f.owner, InstanceMode::MultiUser, 1, 2_002)
        .await
        .unwrap();
    let other = f
        .store
        .create_tenant(
            &f.owner,
            "automation-team",
            "Automation team",
            TenantQuota::default(),
            2_003,
        )
        .await
        .unwrap();
    let grant = f
        .store
        .create_service_credential(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &ServiceCredentialCreate {
                name: "统计".into(),
                scopes: vec![ServiceScope::ResourceRead],
                expires_at_ms: 5_000,
            },
            2_004,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &other.tenant_id, 2_005)
            .await
            .is_err()
    );
    assert!(
        f.store
            .list_service_credentials(&f.owner, &other.tenant_id, &account.service_account_id)
            .await
            .is_err()
    );
    assert!(
        f.store
            .revoke_service_credential(
                &f.owner,
                &other.tenant_id,
                &account.service_account_id,
                &grant.credential.credential_id,
                2_005
            )
            .await
            .is_err()
    );
    assert!(
        f.store
            .authenticate_service_credential(&grant.access_token, &f.tenant, 2_006)
            .await
            .is_ok()
    );
    assert_eq!(
        f.store
            .list_service_accounts(&f.owner, &f.tenant)
            .await
            .unwrap()
            .len(),
        1
    );
    let same_name = f
        .store
        .create_service_account(
            &f.owner,
            &other.tenant_id,
            &ServiceAccountCreate {
                name: account.name,
                notes: String::new(),
            },
            2_007,
        )
        .await
        .unwrap();
    assert_ne!(same_name.service_account_id, account.service_account_id);
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires a disposable ternilo_control_test_services PostgreSQL database"]
async fn postgres_service_identity_and_credentials_use_tenant_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    assert_eq!(runtime.path(), "/ternilo_control_test_services");
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    sqlx::raw_sql("DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        .execute(&admin)
        .await
        .unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_service_test", "service-test-password").await;
    runtime.set_username("ternilo_service_test").unwrap();
    runtime.set_password(Some("service-test-password")).unwrap();
    let fixture = Fixture::connect(runtime.as_str(), Some(&admin_url)).await;
    identity_contract(fixture.clone()).await;
    workspace_access_contract(fixture).await;
    admin.close().await;
}

#[tokio::test]
async fn service_workspaces_require_explicit_current_owner_grants_and_observed_permissions() {
    workspace_access_contract(Fixture::new().await).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "One lifecycle verifies owned discovery, pagination, explicit grants, stale edits, resource isolation and revocation on both databases."
)]
async fn workspace_access_contract(f: Fixture) {
    use ternilo_control::{
        PageQuery, ResourceAction, ResourceKind, ResourcePermissions, ServiceWorkspaceUpdate,
    };
    let account = f
        .store
        .create_service_account(
            &f.owner,
            &f.tenant,
            &ServiceAccountCreate {
                name: "workspace-grants".into(),
                notes: String::new(),
            },
            3000,
        )
        .await
        .unwrap();
    let grant = f
        .store
        .create_service_credential(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &ServiceCredentialCreate {
                name: "read-only".into(),
                scopes: vec![ServiceScope::ResourceRead],
                expires_at_ms: 100_000,
            },
            3001,
        )
        .await
        .unwrap();
    let principal = f
        .store
        .authenticate_service_credential(&grant.access_token, &f.tenant, 3002)
        .await
        .unwrap();
    let first = f
        .store
        .create_cloud_workspace(&f.owner, &f.tenant, &f.project, "Grant alpha", 3003)
        .await
        .unwrap();
    f.store
        .create_cloud_workspace(&f.owner, &f.tenant, &f.project, "Grant beta", 3003)
        .await
        .unwrap();
    let private = f
        .store
        .create_cloud_workspace(
            &principal.user,
            &f.tenant,
            &f.project,
            "Grant private",
            3003,
        )
        .await
        .unwrap();
    let query = PageQuery {
        query: Some("Grant ".into()),
        cursor: None,
        limit: 1,
    };
    let page = f
        .store
        .service_account_workspaces(&f.owner, &f.tenant, &account.service_account_id, &query)
        .await
        .unwrap();
    assert_eq!(page.workspaces.len(), 1);
    assert!(page.workspaces[0].permissions.is_none());
    assert_ne!(page.workspaces[0].workspace_id, private.workspace_id);
    let next = f
        .store
        .service_account_workspaces(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &PageQuery {
                cursor: page.next_cursor.clone(),
                ..query
            },
        )
        .await
        .unwrap();
    assert!(page.next_cursor.is_some());
    assert!(next.next_cursor.is_none());
    assert_ne!(
        page.workspaces[0].workspace_id,
        next.workspaces[0].workspace_id
    );
    assert!(
        f.store
            .service_account_workspaces(
                &principal.user,
                &f.tenant,
                &account.service_account_id,
                &PageQuery::default()
            )
            .await
            .is_err()
    );
    let read = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    let execute = ResourcePermissions {
        view: true,
        submit: true,
        stop: true,
        configure: false,
    };
    let update = ServiceWorkspaceUpdate {
        permissions: Some(read),
        expected_permissions: None,
    };
    assert!(
        f.store
            .set_service_workspace_access(
                &f.owner,
                &f.tenant,
                &account.service_account_id,
                &private.workspace_id,
                &update,
                3004
            )
            .await
            .is_err(),
        "tenant owner cannot grant somebody else's private resources"
    );
    assert!(
        f.store
            .resolve_accessible_workspace(&principal.user, &f.tenant, &first.workspace_id)
            .await
            .is_err()
    );
    f.store
        .set_service_workspace_access(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &first.workspace_id,
            &update,
            3004,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .resolve_accessible_workspace(&principal.user, &f.tenant, &first.workspace_id)
            .await
            .is_ok()
    );
    let access = f
        .store
        .resource_access(
            &principal.user,
            &f.tenant,
            ResourceKind::Workspace,
            first.workspace_id.as_str(),
        )
        .await
        .unwrap();
    access.require(ResourceAction::View).unwrap();
    assert!(access.require(ResourceAction::Submit).is_err());
    assert_eq!(
        f.store
            .set_service_workspace_access(
                &f.owner,
                &f.tenant,
                &account.service_account_id,
                &first.workspace_id,
                &ServiceWorkspaceUpdate {
                    permissions: Some(execute),
                    expected_permissions: None
                },
                3005
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    f.store
        .set_service_workspace_access(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &first.workspace_id,
            &ServiceWorkspaceUpdate {
                permissions: Some(execute),
                expected_permissions: Some(read),
            },
            3006,
        )
        .await
        .unwrap();
    let access = f
        .store
        .resource_access(
            &principal.user,
            &f.tenant,
            ResourceKind::Workspace,
            first.workspace_id.as_str(),
        )
        .await
        .unwrap();
    access.require(ResourceAction::Submit).unwrap();
    access.require(ResourceAction::Stop).unwrap();
    assert!(
        principal.require(ServiceScope::RunExecute).is_err(),
        "resource permission cannot expand a credential's scopes"
    );
    f.store
        .set_service_workspace_access(
            &f.owner,
            &f.tenant,
            &account.service_account_id,
            &first.workspace_id,
            &ServiceWorkspaceUpdate {
                permissions: None,
                expected_permissions: Some(execute),
            },
            3007,
        )
        .await
        .unwrap();
    assert!(
        f.store
            .resolve_accessible_workspace(&principal.user, &f.tenant, &first.workspace_id)
            .await
            .is_err()
    );
    assert_eq!(
        f.store
            .get_workspace(&f.owner, &f.tenant, &first.workspace_id)
            .await
            .unwrap(),
        first
    );
    assert!(
        f.store
            .set_service_workspace_access(
                &f.owner,
                &f.tenant,
                &f.owner.user_id,
                &first.workspace_id,
                &update,
                3008
            )
            .await
            .is_err(),
        "service grant entry cannot authorize an ordinary account in a personal space"
    );
}
