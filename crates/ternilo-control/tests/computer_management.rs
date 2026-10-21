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
        let enrollment = store
            .create_computer_enrollment(
                &owner,
                &tenant,
                Some(&project),
                "home",
                true,
                Duration::from_secs(600),
                1001,
            )
            .await
            .unwrap();
        let executor = enrollment.executor_id.clone();
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
        name: "  工作电脑  ".into(),
        notes: "桌面上的开发环境\n保留文件".into(),
        expected_revision: 0,
    };
    let management = f
        .store
        .update_computer(&f.owner, &f.tenant, &f.executor, true, &update, 1011)
        .await
        .unwrap();
    assert_eq!(management.name, "工作电脑");
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
    assert_eq!(list[0].management.name, "工作电脑");
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
        name: "Private computer".into(),
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
        .revoke_executor(&f.owner, &tenant, &executor, 2008)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .recover_computer_enrollment(
                &f.owner,
                &tenant,
                &executor,
                "Private computer",
                true,
                1,
                Duration::from_secs(600),
                2008,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let recovery = f
        .store
        .recover_computer_enrollment(
            &f.owner,
            &tenant,
            &executor,
            "Private computer",
            false,
            1,
            Duration::from_secs(600),
            2008,
        )
        .await
        .unwrap();
    let recovered = f
        .store
        .consume_enrollment(&recovery.token, 2008)
        .await
        .unwrap();
    assert_eq!(recovered.scope.user_id, member.user_id);
    assert_eq!(recovered.executor_id, executor);
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
#[ignore = "requires a disposable ternilo_control_test PostgreSQL database"]
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
    assert_eq!(runtime_url.path(), "/ternilo_control_test");
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
    f.store
        .synchronize_node_cleanup(&f.credential.token, "postgres-original-directory", 1010)
        .await
        .unwrap();
    let principal = f
        .store
        .authenticate_node(&f.credential.token, 1010)
        .await
        .unwrap();
    let update = ComputerUpdate {
        name: "PostgreSQL computer".into(),
        notes: "runtime scoped".into(),
        expected_revision: 0,
    };
    f.store
        .update_computer(&f.owner, &f.tenant, &f.executor, true, &update, 1011)
        .await
        .unwrap();
    let update_job = ternilo_control::ClientUpdateJob {
        progress: ternilo_protocol::ClientUpdateProgress {
            id: "postgres-update-12345678".into(),
            version: "0.2.21".into(),
            phase: ternilo_protocol::ClientUpdatePhase::Preparing,
            revision: 0,
            error: None,
        },
        target: "x86_64-unknown-linux-gnu".into(),
        manifest: "signed fixture".into(),
        signature: "fixture signature".into(),
        created_at_ms: 1011,
        expires_at_ms: 100_000,
    };
    f.store
        .create_client_update_job(
            &f.owner,
            &f.tenant,
            &f.executor,
            &update_job,
            "postgres-download-grant",
        )
        .await
        .unwrap();
    assert_eq!(
        f.store
            .client_update_job(&f.owner, &f.tenant, &f.executor)
            .await
            .unwrap()
            .unwrap()
            .progress
            .id,
        update_job.progress.id
    );
    f.store
        .client_update_download(
            &f.tenant,
            &f.executor,
            &update_job.progress.id,
            "postgres-download-grant",
            1012,
        )
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
    let hidden_updates: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_client_updates")
        .fetch_one(&mut *other_scope)
        .await
        .unwrap();
    assert_eq!(
        hidden_updates, 0,
        "update grants must be isolated by PostgreSQL RLS"
    );
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
    let removed = f
        .store
        .computer_details(&f.owner, &f.tenant, &f.executor, true)
        .await
        .unwrap();
    let restored = f
        .store
        .recover_computer_enrollment(
            &f.owner,
            &f.tenant,
            &f.executor,
            "恢复后的电脑",
            true,
            removed.management.revision,
            Duration::from_secs(600),
            1017,
        )
        .await
        .unwrap();
    assert_eq!(restored.executor_id, f.executor);
    let credential = f
        .store
        .consume_enrollment(&restored.token, 1018)
        .await
        .unwrap();
    assert_eq!(credential.executor_id, f.executor);
    assert_eq!(credential.scope.user_id, f.owner.user_id);
    assert_eq!(credential.project_id, Some(f.project.clone()));
    assert!(
        f.store
            .synchronize_node_cleanup(&credential.token, "other-directory", 1019)
            .await
            .is_err()
    );
    f.store
        .synchronize_node_cleanup(&credential.token, "postgres-original-directory", 1019)
        .await
        .unwrap();
    let computer = f
        .store
        .owned_executor(&f.owner, &f.tenant, &f.executor)
        .await
        .unwrap();
    assert_eq!(computer.management.name, "恢复后的电脑");
    assert_eq!(computer.management.removed_at_ms, None);
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
        client: None,
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

#[tokio::test]
async fn generated_identity_and_name_reservations_are_distinct_and_expire() {
    let f = Fixture::new().await;
    assert!(f.executor.as_str().starts_with("ter_pc_"));
    assert_ne!(f.executor.as_str(), "home");
    let first = f
        .store
        .create_computer_enrollment(
            &f.owner,
            &f.tenant,
            None,
            " Pending 💻 ",
            true,
            Duration::from_millis(10),
            2000,
        )
        .await
        .unwrap();
    assert_eq!(first.name, "Pending 💻");
    assert_ne!(first.executor_id, f.executor);
    assert_eq!(
        f.store
            .create_computer_enrollment(
                &f.owner,
                &f.tenant,
                None,
                "Pending 💻",
                true,
                Duration::from_millis(10),
                2001
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let second = f
        .store
        .create_computer_enrollment(
            &f.owner,
            &f.tenant,
            None,
            "Pending 💻",
            true,
            Duration::from_secs(600),
            2011,
        )
        .await
        .unwrap();
    assert_ne!(second.executor_id, first.executor_id);
    assert!(
        f.store
            .consume_enrollment(&first.token, 2012)
            .await
            .is_err()
    );
    f.store
        .consume_enrollment(&second.token, 2012)
        .await
        .unwrap();
    let update = ComputerUpdate {
        name: "home".into(),
        notes: String::new(),
        expected_revision: 0,
    };
    assert_eq!(
        f.store
            .update_computer(
                &f.owner,
                &f.tenant,
                &second.executor_id,
                true,
                &update,
                2013
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let invalid = ComputerUpdate {
        name: " ".into(),
        ..update
    };
    assert_eq!(
        f.store
            .update_computer(
                &f.owner,
                &f.tenant,
                &second.executor_id,
                true,
                &invalid,
                2014
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[tokio::test]
async fn recovery_retains_identity_owner_project_and_the_original_storage_binding() {
    let f = Fixture::new().await;
    f.store
        .synchronize_node_cleanup(&f.credential.token, "original-directory", 2000)
        .await
        .unwrap();
    f.store
        .revoke_owned_executor(&f.owner, &f.tenant, &f.executor, 2001)
        .await
        .unwrap();
    let grant = f
        .store
        .recover_computer_enrollment(
            &f.owner,
            &f.tenant,
            &f.executor,
            "改名后的电脑",
            true,
            0,
            Duration::from_secs(600),
            2002,
        )
        .await
        .unwrap();
    assert_eq!(grant.executor_id, f.executor);
    let restored = f
        .store
        .consume_enrollment(&grant.token, 2003)
        .await
        .unwrap();
    assert_eq!(restored.executor_id, f.executor);
    assert_eq!(restored.scope.user_id, f.owner.user_id);
    assert_eq!(restored.project_id, Some(f.project.clone()));
    assert!(
        f.store
            .authenticate_node(&f.credential.token, 2004)
            .await
            .is_err()
    );
    assert_eq!(
        f.store
            .synchronize_node_cleanup(&restored.token, "different-directory", 2004)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    f.store
        .synchronize_node_cleanup(&restored.token, "original-directory", 2004)
        .await
        .unwrap();
    let computer = f
        .store
        .owned_executor(&f.owner, &f.tenant, &f.executor)
        .await
        .unwrap();
    assert_eq!(computer.management.name, "改名后的电脑");
    assert_eq!(computer.management.removed_at_ms, None);
    assert_eq!(
        f.store
            .recover_computer_enrollment(
                &f.owner,
                &f.tenant,
                &f.executor,
                "other",
                true,
                computer.management.revision,
                Duration::from_secs(600),
                2005
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn removed_name_can_be_used_by_a_new_identity_and_old_identity_recovery_requires_a_unique_name()
 {
    let f = Fixture::new().await;
    f.store
        .remove_computer_registration(&f.owner, &f.tenant, &f.executor, true, 0, 2000)
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
            .list_computers(&f.owner, &f.tenant, true, true)
            .await
            .unwrap()
            .len(),
        1
    );
    let new = f
        .store
        .create_computer_enrollment(
            &f.owner,
            &f.tenant,
            None,
            "home",
            true,
            Duration::from_secs(600),
            2001,
        )
        .await
        .unwrap();
    assert_ne!(new.executor_id, f.executor);
    f.store.consume_enrollment(&new.token, 2002).await.unwrap();
    assert_eq!(
        f.store
            .recover_computer_enrollment(
                &f.owner,
                &f.tenant,
                &f.executor,
                "home",
                true,
                1,
                Duration::from_secs(600),
                2003
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let recovered = f
        .store
        .recover_computer_enrollment(
            &f.owner,
            &f.tenant,
            &f.executor,
            "原电脑",
            true,
            1,
            Duration::from_secs(600),
            2004,
        )
        .await
        .unwrap();
    assert_eq!(recovered.executor_id, f.executor);
    f.store
        .consume_enrollment(&recovered.token, 2005)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .list_owned_executors(&f.owner, &f.tenant)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise the complete grant lifecycle against one registered computer."
)]
async fn client_update_grants_are_owner_scoped_idempotent_expiring_and_revocable() {
    use ternilo_control::ClientUpdateJob;
    use ternilo_protocol::{ClientUpdatePhase, ClientUpdateProgress, UserId};
    let f = Fixture::new().await;
    let job = ClientUpdateJob {
        progress: ClientUpdateProgress {
            id: "update-1234567890".into(),
            version: "0.2.21".into(),
            phase: ClientUpdatePhase::Preparing,
            revision: 0,
            error: None,
        },
        target: "x86_64-unknown-linux-gnu".into(),
        manifest: "signed release fixture".into(),
        signature: "fixture".into(),
        created_at_ms: 1100,
        expires_at_ms: 100_000,
    };
    let outsider = ControlUser {
        user_id: UserId::new("another-user"),
        username: "another".into(),
    };
    assert!(
        f.store
            .create_client_update_job(
                &outsider,
                &f.tenant,
                &f.executor,
                &job,
                "secret-download-grant"
            )
            .await
            .is_err()
    );
    assert!(
        f.store
            .create_client_update_job(
                &f.owner,
                &f.tenant,
                &f.executor,
                &job,
                "secret-download-grant"
            )
            .await
            .unwrap()
    );
    assert!(
        !f.store
            .create_client_update_job(&f.owner, &f.tenant, &f.executor, &job, "different-grant")
            .await
            .unwrap()
    );
    assert!(
        f.store
            .client_update_job(&outsider, &f.tenant, &f.executor)
            .await
            .is_err()
    );
    let downloaded = f
        .store
        .client_update_download(
            &f.tenant,
            &f.executor,
            &job.progress.id,
            "secret-download-grant",
            1200,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.progress.id, job.progress.id);
    for (executor, token, now) in [
        (f.executor.clone(), "wrong-token", 1200),
        (
            ExecutorId::new("another-computer"),
            "secret-download-grant",
            1200,
        ),
        (f.executor.clone(), "secret-download-grant", 1_900_000),
    ] {
        assert!(
            f.store
                .client_update_download(&f.tenant, &executor, &job.progress.id, token, now)
                .await
                .is_err()
        );
    }
    f.store
        .set_computer_suspended(&f.owner, &f.tenant, &f.executor, true, true, 0, 1300)
        .await
        .unwrap();
    assert!(
        f.store
            .client_update_download(
                &f.tenant,
                &f.executor,
                &job.progress.id,
                "secret-download-grant",
                1400
            )
            .await
            .is_err()
    );
    let stored: String = sqlx::query_scalar("SELECT job_json FROM control_client_updates")
        .fetch_one(f.store.database().pool())
        .await
        .unwrap();
    assert!(!stored.contains("secret-download-grant"));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Follow one update across connection replacement, completion and stale reports."
)]
async fn update_progress_rejects_old_connections_and_requires_the_reconnected_release() {
    use ternilo_control::ClientUpdateJob;
    use ternilo_protocol::{ClientUpdateInfo, ClientUpdatePhase, ClientUpdateProgress};
    use ternilo_transport::{
        EXECUTOR_PROTOCOL_VERSION, ExecutorCapabilities, ExecutorHello, ExecutorKind,
    };
    let f = Fixture::new().await;
    let info = ClientUpdateInfo {
        protocol_version: 1,
        version: "0.2.20".into(),
        target: Some("x86_64-unknown-linux-gnu".into()),
        enabled: true,
        data_epoch: 1,
        restart_method: Some("direct".into()),
        progress: None,
    };
    let mut hello = ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: f.executor.clone(),
        executor_kind: ExecutorKind::EdgeNode,
        instance_nonce: "first-instance".into(),
        catalog_revision: "test".into(),
        capabilities: ExecutorCapabilities::new(),
        client: Some(info.clone()),
    };
    f.store
        .edge_store()
        .register_executor(&f.tenant, &hello, 1100)
        .await
        .unwrap();
    let job = ClientUpdateJob {
        progress: ClientUpdateProgress {
            id: "update-1234567890".into(),
            version: "0.2.21".into(),
            phase: ClientUpdatePhase::Preparing,
            revision: 0,
            error: None,
        },
        target: info.target.clone().unwrap(),
        manifest: "signed fixture".into(),
        signature: "signature".into(),
        created_at_ms: 1200,
        expires_at_ms: 100_000,
    };
    f.store
        .create_client_update_job(&f.owner, &f.tenant, &f.executor, &job, "grant")
        .await
        .unwrap();
    let mut next = info.clone();
    next.progress = Some(ClientUpdateProgress {
        phase: ClientUpdatePhase::Completed,
        revision: 2,
        ..job.progress.clone()
    });
    assert!(
        f.store
            .edge_store()
            .record_client_update_state(&f.tenant, &f.executor, "first-instance", &next)
            .await
            .is_err()
    );
    hello.instance_nonce = "new-instance".into();
    hello.client.as_mut().unwrap().version = "0.2.21".into();
    f.store
        .edge_store()
        .register_executor(&f.tenant, &hello, 1300)
        .await
        .unwrap();
    next.version = "0.2.21".into();
    assert!(
        !f.store
            .edge_store()
            .record_client_update_state(&f.tenant, &f.executor, "first-instance", &next)
            .await
            .unwrap()
    );
    assert!(
        f.store
            .edge_store()
            .record_client_update_state(&f.tenant, &f.executor, "new-instance", &next)
            .await
            .unwrap()
    );
    assert_eq!(
        f.store
            .client_update_job(&f.owner, &f.tenant, &f.executor)
            .await
            .unwrap()
            .unwrap()
            .progress
            .phase,
        ClientUpdatePhase::Completed
    );
    let mut stale = next.clone();
    stale.progress.as_mut().unwrap().revision = 1;
    stale.progress.as_mut().unwrap().phase = ClientUpdatePhase::Downloading;
    f.store
        .edge_store()
        .record_client_update_state(&f.tenant, &f.executor, "new-instance", &stale)
        .await
        .unwrap();
    assert_eq!(
        f.store
            .client_update_job(&f.owner, &f.tenant, &f.executor)
            .await
            .unwrap()
            .unwrap()
            .progress
            .phase,
        ClientUpdatePhase::Completed
    );
    assert_eq!(
        f.store
            .computer_details(&f.owner, &f.tenant, &f.executor, true)
            .await
            .unwrap()
            .hello
            .unwrap()
            .client
            .unwrap()
            .progress
            .unwrap()
            .phase,
        ClientUpdatePhase::Completed
    );
}
