use std::time::Duration;
use ternilo_control::{
    ComputerUpdate, ControlStore, ControlUser, NativeRegistration, SecretCipher,
};
use ternilo_protocol::{ErrorCode, TenantId, WorkspaceId};
use ternilo_transport::ExecutorId;

struct Fixture {
    store: ControlStore,
    owner: ControlUser,
    tenant: TenantId,
    project: String,
    executor: ExecutorId,
    credential: ternilo_control::NodeCredentialGrant,
}

impl Fixture {
    async fn new() -> Self {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([69; 32]), 1)
                .await
                .unwrap();
        Self::with_store(store).await
    }

    async fn with_store(store: ControlStore) -> Self {
        let account = store
            .initialize_owner(
                &NativeRegistration {
                    username: "computer-owner".into(),
                    email: "computers@example.test".into(),
                    password: "computer-password-123".into(),
                },
                1000,
            )
            .await
            .unwrap();
        let owner = account.session.user;
        let tenant = account.session.personal_tenant_id;
        let project = account.session.personal_project_id;
        let executor = ExecutorId::new("home");
        let enrollment = store
            .create_owned_enrollment(
                &owner,
                &tenant,
                Some(&project),
                executor.clone(),
                Duration::from_secs(600),
                1001,
            )
            .await
            .unwrap();
        let credential = store
            .consume_enrollment(&enrollment.token, 1002)
            .await
            .unwrap();
        Self {
            store,
            owner,
            tenant,
            project,
            executor,
            credential,
        }
    }
}

#[tokio::test]
async fn computer_metadata_and_reversible_suspension_keep_original_identity() {
    let f = Fixture::new().await;
    let principal = f
        .store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    let update = ComputerUpdate {
        display_name: Some("  工作电脑  ".into()),
        notes: "桌面上的开发环境\n保留文件".into(),
        expected_revision: 0,
    };
    let management = f
        .store
        .update_computer(&f.owner, &f.tenant, &f.executor, true, &update, 1011)
        .await
        .unwrap();
    assert_eq!(management.display_name.as_deref(), Some("工作电脑"));
    assert_eq!(management.revision, 1);
    assert_eq!(
        f.store
            .update_computer(&f.owner, &f.tenant, &f.executor, true, &update, 1012)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let details = f
        .store
        .computer_details(&f.owner, &f.tenant, &f.executor, true)
        .await
        .unwrap();
    assert_eq!(details.owner, f.owner);
    assert_eq!(details.executor.executor_id, f.executor);
    assert_eq!(details.executor.enrolled_at_ms, 1002);
    assert_eq!(details.management.notes, update.notes);
    let paused = f
        .store
        .set_computer_suspended(&f.owner, &f.tenant, &f.executor, true, true, 1, 1013)
        .await
        .unwrap();
    assert_eq!(paused.suspended_at_ms, Some(1013));
    assert!(
        f.store
            .authenticate_node(&f.credential.token, 1014)
            .await
            .is_err()
    );
    assert!(
        f.store
            .edge_store()
            .require_node_credential(&principal)
            .await
            .is_err()
    );
    let snapshot = f
        .store
        .node_cleanup_snapshot(&f.credential.token)
        .await
        .unwrap();
    assert!(!snapshot.connection_allowed);
    assert!(
        snapshot
            .authorizations
            .iter()
            .any(|account| account.user_id == f.owner.user_id && account.active)
    );
    let resumed = f
        .store
        .set_computer_suspended(&f.owner, &f.tenant, &f.executor, true, false, 2, 1015)
        .await
        .unwrap();
    assert_eq!(resumed.suspended_at_ms, None);
    let next = f
        .store
        .authenticate_node(&f.credential.token, 1016)
        .await
        .unwrap();
    assert_eq!(next.credential_id, f.credential.credential_id);
    f.store
        .edge_store()
        .require_node_credential(&next)
        .await
        .unwrap();
    let list = f
        .store
        .list_owned_executors(&f.owner, &f.tenant)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].management.display_name.as_deref(), Some("工作电脑"));
}

#[tokio::test]
async fn removing_registration_retains_workspaces_and_allows_explicit_reenrollment() {
    let f = Fixture::new().await;
    f.store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    let workspace = f
        .store
        .create_local_workspace(
            &f.owner,
            &f.tenant,
            &f.project,
            "Project",
            (&f.executor, &WorkspaceId::new("local-workspace")),
            1011,
        )
        .await
        .unwrap();
    f.store
        .remove_computer_registration(&f.owner, &f.tenant, &f.executor, true, 0, 1012)
        .await
        .unwrap();
    assert!(
        f.store
            .list_owned_executors(&f.owner, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.store
            .list_executors(&f.owner, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        f.store
            .authenticate_node(&f.credential.token, 1013)
            .await
            .is_err()
    );
    let retained = f
        .store
        .get_workspace(&f.owner, &f.tenant, &workspace.workspace_id)
        .await
        .unwrap();
    assert_eq!(retained.executor_id, Some(f.executor.clone()));
    assert_eq!(retained.owner_user_id, f.owner.user_id);
    let enrollment = f
        .store
        .create_owned_enrollment(
            &f.owner,
            &f.tenant,
            Some(&f.project),
            f.executor.clone(),
            Duration::from_secs(600),
            1014,
        )
        .await
        .unwrap();
    let replacement = f
        .store
        .consume_enrollment(&enrollment.token, 1015)
        .await
        .unwrap();
    assert_ne!(replacement.token, f.credential.token);
    f.store
        .authenticate_node(&replacement.token, 1016)
        .await
        .unwrap();
    let list = f
        .store
        .list_owned_executors(&f.owner, &f.tenant)
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].management.removed_at_ms.is_none());
    assert_eq!(
        f.store
            .get_workspace(&f.owner, &f.tenant, &workspace.workspace_id)
            .await
            .unwrap(),
        retained
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify ownership and role reduction in one lifecycle."
)]
async fn computer_operations_enforce_ownership_and_current_tenant_role() {
    use ternilo_control::{InstanceMode, OidcPrincipal, TenantQuota, TenantRole};
    let f = Fixture::new().await;
    f.store
        .set_instance_mode(&f.owner, InstanceMode::MultiUser, 1, 2000)
        .await
        .unwrap();
    let member = f
        .store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://computers.example.test".into(),
                subject: "member".into(),
                email: None,
                display_name: None,
            },
            "member",
            2001,
        )
        .await
        .unwrap();
    let tenant = f
        .store
        .create_tenant(
            &f.owner,
            "computer-team",
            "Team",
            TenantQuota::default(),
            2002,
        )
        .await
        .unwrap()
        .tenant_id;
    f.store
        .set_membership(&f.owner, &tenant, &member.user_id, TenantRole::Member, 2003)
        .await
        .unwrap();
    let enrollment = f
        .store
        .create_owned_enrollment(
            &member,
            &tenant,
            None,
            ExecutorId::new("member-computer"),
            Duration::from_secs(600),
            2004,
        )
        .await
        .unwrap();
    f.store
        .consume_enrollment(&enrollment.token, 2005)
        .await
        .unwrap();
    let executor = ExecutorId::new("member-computer");
    assert_eq!(
        f.store
            .list_executors(&member, &tenant)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        f.store
            .computer_details(&member, &tenant, &executor, false)
            .await
            .err()
            .unwrap()
            .code,
        ErrorCode::PolicyDenied
    );
    let update = ComputerUpdate {
        display_name: Some("Private computer".into()),
        notes: String::new(),
        expected_revision: 0,
    };
    assert!(
        f.store
            .update_computer(&f.owner, &tenant, &executor, true, &update, 2006)
            .await
            .is_err()
    );
    f.store
        .update_computer(&member, &tenant, &executor, true, &update, 2007)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .set_computer_suspended(&member, &tenant, &executor, false, true, 1, 2008)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    f.store
        .set_membership(&f.owner, &tenant, &member.user_id, TenantRole::Viewer, 2009)
        .await
        .unwrap();
    assert!(
        f.store
            .computer_details(&member, &tenant, &executor, true)
            .await
            .is_ok()
    );
    assert_eq!(
        f.store
            .set_computer_suspended(&member, &tenant, &executor, true, true, 1, 2010)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        f.store
            .remove_computer_registration(&member, &tenant, &executor, true, 1, 2011)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        f.store
            .authenticate_node(&f.credential.token, 2012)
            .await
            .is_ok()
    );
}

#[path = "support/postgres.rs"]
mod postgres_runtime;

#[tokio::test]
#[ignore = "requires a disposable ternilo_control_test_computers PostgreSQL database"]
#[expect(
    clippy::too_many_lines,
    reason = "Verify the restricted runtime, RLS and retained bindings together."
)]
async fn postgres_computer_management_enforces_runtime_scope_and_retains_bindings() {
    use sqlx::Executor;
    let admin_url =
        std::env::var("TERNILO_TEST_DATABASE_URL").expect("disposable PostgreSQL URL required");
    let mut runtime_url = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    assert_eq!(runtime_url.path(), "/ternilo_control_test_computers");
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_computers_test", "computer-test-password")
        .await;
    runtime_url.set_username("ternilo_computers_test").unwrap();
    runtime_url
        .set_password(Some("computer-test-password"))
        .unwrap();
    let store = ControlStore::connect(
        runtime_url.as_str(),
        Some(&admin_url),
        SecretCipher::from_key([69; 32]),
        2,
    )
    .await
    .unwrap();
    let f = Fixture::with_store(store).await;
    let principal = f
        .store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    let update = ComputerUpdate {
        display_name: Some("PostgreSQL computer".into()),
        notes: "runtime scoped".into(),
        expected_revision: 0,
    };
    f.store
        .update_computer(&f.owner, &f.tenant, &f.executor, true, &update, 1011)
        .await
        .unwrap();
    let mut other_scope = f
        .store
        .database()
        .tenant_transaction(&TenantId::new("another-tenant"))
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_computer_management")
        .fetch_one(&mut *other_scope)
        .await
        .unwrap();
    assert_eq!(count, 0);
    other_scope.rollback().await.unwrap();
    let mut forbidden_ddl = f.store.database().begin().await.unwrap();
    assert!(
        sqlx::query("CREATE TABLE forbidden_computer_ddl(id INTEGER)")
            .execute(&mut *forbidden_ddl)
            .await
            .is_err()
    );
    forbidden_ddl.rollback().await.unwrap();
    f.store
        .set_computer_suspended(&f.owner, &f.tenant, &f.executor, true, true, 1, 1012)
        .await
        .unwrap();
    assert!(
        f.store
            .edge_store()
            .require_node_credential(&principal)
            .await
            .is_err()
    );
    assert!(
        !f.store
            .node_cleanup_snapshot(&f.credential.token)
            .await
            .unwrap()
            .connection_allowed
    );
    f.store
        .set_computer_suspended(&f.owner, &f.tenant, &f.executor, true, false, 2, 1013)
        .await
        .unwrap();
    f.store
        .authenticate_node(&f.credential.token, 1014)
        .await
        .unwrap();
    let workspace = f
        .store
        .create_local_workspace(
            &f.owner,
            &f.tenant,
            &f.project,
            "Project",
            (&f.executor, &WorkspaceId::new("local-workspace")),
            1015,
        )
        .await
        .unwrap();
    f.store
        .remove_computer_registration(&f.owner, &f.tenant, &f.executor, true, 3, 1016)
        .await
        .unwrap();
    assert!(
        f.store
            .list_owned_executors(&f.owner, &f.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.store
            .get_workspace(&f.owner, &f.tenant, &workspace.workspace_id)
            .await
            .unwrap(),
        workspace
    );
    f.store.database().close().await;
    admin.close().await;
}

#[tokio::test]
async fn computer_details_and_lists_use_the_latest_server_observed_heartbeat() {
    use ternilo_transport::{EXECUTOR_PROTOCOL_VERSION, ExecutorHello, ExecutorKind};
    let f = Fixture::new().await;
    f.store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    let hello = ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: f.executor.clone(),
        executor_kind: ExecutorKind::EdgeNode,
        instance_nonce: "instance".into(),
        catalog_revision: "catalog".into(),
        capabilities: std::collections::BTreeSet::new(),
    };
    f.store
        .edge_store()
        .register_executor(&f.tenant, &hello, 2010)
        .await
        .unwrap();
    let details = f
        .store
        .computer_details(&f.owner, &f.tenant, &f.executor, true)
        .await
        .unwrap();
    assert_eq!(details.hello, Some(hello));
    assert_eq!(details.executor.last_seen_at_ms, Some(2010));
    assert_eq!(
        f.store
            .list_owned_executors(&f.owner, &f.tenant)
            .await
            .unwrap()[0]
            .last_seen_at_ms,
        Some(2010)
    );
}

#[tokio::test]
async fn revoked_computer_details_keep_recorded_credential_dates() {
    let f = Fixture::new().await;
    f.store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    f.store
        .revoke_owned_executor(&f.owner, &f.tenant, &f.executor, 1011)
        .await
        .unwrap();
    let details = f
        .store
        .computer_details(&f.owner, &f.tenant, &f.executor, true)
        .await
        .unwrap();
    assert_eq!(details.executor.state, "revoked");
    assert_eq!(details.credential_issued_at_ms, Some(1002));
    assert_eq!(details.credential_last_used_at_ms, Some(1010));
}
