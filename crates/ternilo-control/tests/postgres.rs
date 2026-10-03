#[path = "support/postgres.rs"]
mod postgres_runtime;

use std::time::Duration;

use sqlx::Executor;
use ternilo_control::{
    ControlAction, ControlStore, InstanceMode, NativeRegistration, OidcPrincipal, SecretCipher,
    TenantQuota, TenantRole,
};
use ternilo_transport::ExecutorId;

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise production installation, identity and audit privileges, and schema rejection with one restricted role."
)]
async fn postgres_production_installation_bootstraps_identity_with_only_runtime_grants() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_install_test", "installation-password").await;
    let mut runtime = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_install_test").unwrap();
    runtime.set_password(Some("installation-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&admin_url),
        SecretCipher::from_key([41; 32]),
        2,
    )
    .await
    .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "production-owner@example.test".to_owned(),
                username: "production-owner".to_owned(),
                password: "production-owner-password".to_owned(),
            },
            1_000,
        )
        .await
        .unwrap();
    let login = store
        .login_native("production-owner", "production-owner-password", 2_000)
        .await
        .unwrap();
    assert_eq!(
        login.session.personal_tenant_id,
        owner.session.personal_tenant_id
    );
    assert_eq!(
        login.session.personal_project_id,
        owner.session.personal_project_id
    );
    store
        .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, 3_000)
        .await
        .unwrap();

    let runtime = sqlx::PgPool::connect(runtime.as_str()).await.unwrap();
    let elevated: bool = sqlx::query_scalar(
        "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user",
    )
    .fetch_one(&runtime)
    .await
    .unwrap();
    assert!(!elevated);
    let owns_table: bool = sqlx::query_scalar("SELECT tableowner=current_user FROM pg_tables WHERE schemaname='public' AND tablename='control_account_spaces'")
        .fetch_one(&runtime).await.unwrap();
    assert!(!owns_table);
    let mapped: String = sqlx::query_scalar(
        "SELECT personal_tenant_id FROM control_account_spaces WHERE user_id=$1",
    )
    .bind(owner.session.user.user_id.as_str())
    .fetch_one(&runtime)
    .await
    .unwrap();
    assert_eq!(mapped, owner.session.personal_tenant_id.as_str());
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_platform_audit WHERE action='instance.mode'",
    )
    .fetch_one(&runtime)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    assert!(
        runtime
            .execute("UPDATE control_platform_audit SET action='altered'")
            .await
            .is_err()
    );
    assert!(
        runtime
            .execute("DELETE FROM control_platform_audit")
            .await
            .is_err()
    );
    assert!(
        runtime
            .execute("CREATE TABLE unauthorized_schema_change(id BIGINT)")
            .await
            .is_err()
    );
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_tenants")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "production runtime still requires an explicit tenant scope"
    );
    let legacy_route_absent: bool = sqlx::query_scalar("SELECT to_regprocedure('public.ternilo_user_model_route_for_worker(text,text,text)') IS NULL")
        .fetch_one(&runtime).await.unwrap();
    assert!(
        legacy_route_absent,
        "Workers access models only through the authenticated Server gateway"
    );
    runtime.close().await;
    store.database().close().await;
    admin
        .execute("UPDATE ternilo_schema SET version=6 WHERE component='control'")
        .await
        .unwrap();
    let incompatible = ControlStore::connect(&admin_url, None, SecretCipher::from_key([41; 32]), 1)
        .await
        .err()
        .unwrap();
    assert_eq!(
        incompatible.message,
        "database schema for \"control\" is version 6, but this build requires version 14"
    );
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
#[allow(clippy::too_many_lines)]
async fn postgres_control_plane_enforces_identity_rbac_quota_enrollment_secret_and_audit() {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL")
        .expect("TERNILO_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_control_test"),
        "integration test refuses a database URL without ternilo_control_test"
    );
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_runtime_test", "ternilo-runtime-password")
        .await;
    let migration_store =
        ControlStore::connect(&admin_url, None, SecretCipher::from_key([9; 32]), 2)
            .await
            .unwrap();
    migration_store.health().await.unwrap();
    migration_store.database().close().await;
    admin.close().await;

    let mut runtime_url = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime_url.set_username("ternilo_runtime_test").unwrap();
    runtime_url
        .set_password(Some("ternilo-runtime-password"))
        .unwrap();
    let runtime_url = runtime_url.to_string();
    let store = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([9; 32]),
        4,
    )
    .await
    .unwrap();

    control_contract(store, &admin_url, &runtime_url, Some(&admin_url)).await;
}

#[tokio::test]
async fn sqlite_control_plane_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("control.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([9; 32]), 4)
        .await
        .unwrap();
    control_contract(store, &url, &url, None).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "The same complete identity, quota, Node, secret and audit scenario runs on both backends."
)]
async fn control_contract(
    store: ControlStore,
    admin_url: &str,
    runtime_url: &str,
    migration_url: Option<&str>,
) {
    let now = 1_800_000_000_000;
    let alice = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: "alice".to_owned(),
                email: Some("alice@example.com".to_owned()),
                display_name: Some("Alice".to_owned()),
            },
            "test-alice",
            now,
        )
        .await
        .unwrap();
    let bob = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: "bob".to_owned(),
                email: Some("bob@example.com".to_owned()),
                display_name: Some("Bob".to_owned()),
            },
            "test-bob",
            now,
        )
        .await
        .unwrap();
    let quota = TenantQuota {
        max_nodes: 2,
        max_concurrent_runs: 1,
        monthly_model_tokens: 1_000,
        max_secrets: 4,
    };
    let tenant_a = store
        .create_tenant(&alice, "tenant-a", "Tenant A", quota.clone(), now + 1)
        .await
        .unwrap();
    let tenant_b = store
        .create_tenant(&bob, "tenant-b", "Tenant B", quota, now + 2)
        .await
        .unwrap();
    assert_eq!(
        store
            .list_tenants(&alice)
            .await
            .unwrap()
            .into_iter()
            .filter(|tenant| tenant.kind == ternilo_control::SpaceKind::Team)
            .collect::<Vec<_>>(),
        vec![tenant_a.clone()]
    );
    assert_eq!(
        store
            .list_tenants(&bob)
            .await
            .unwrap()
            .into_iter()
            .filter(|tenant| tenant.kind == ternilo_control::SpaceKind::Team)
            .collect::<Vec<_>>(),
        vec![tenant_b.clone()]
    );
    assert!(
        store
            .authorize(&bob, &tenant_a.tenant_id, ControlAction::TenantRead)
            .await
            .is_err()
    );

    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 3,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .authorize(&bob, &tenant_a.tenant_id, ControlAction::RunReserve)
            .await
            .unwrap(),
        TenantRole::Member
    );
    assert!(
        store
            .authorize(&bob, &tenant_a.tenant_id, ControlAction::SecretManage)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .list_memberships(
                &alice,
                &tenant_a.tenant_id,
                &ternilo_control::PageQuery::default()
            )
            .await
            .unwrap()
            .memberships
            .len(),
        2
    );
    let group = store
        .create_permission_group(
            &alice,
            &tenant_a.tenant_id,
            &ternilo_control::GroupInput {
                name: "Reviewers".to_owned(),
                description: None,
            },
            now + 4,
        )
        .await
        .unwrap();
    store
        .set_permission_group_member(
            &alice,
            &tenant_a.tenant_id,
            &group.group_id,
            &bob.user_id,
            true,
            now + 4,
        )
        .await
        .unwrap();
    assert!(
        store
            .permission_group(&bob, &tenant_a.tenant_id, &group.group_id)
            .await
            .is_err()
    );
    assert!(
        store
            .permission_group(&bob, &tenant_b.tenant_id, &group.group_id)
            .await
            .is_err()
    );
    let groups = store
        .list_permission_groups(
            &alice,
            &tenant_a.tenant_id,
            &ternilo_control::PageQuery {
                query: Some("review".to_owned()),
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(groups.groups[0].member_count, 1);
    let members = store
        .list_permission_group_members(
            &alice,
            &tenant_a.tenant_id,
            &group.group_id,
            &ternilo_control::PageQuery {
                query: Some(bob.username.clone()),
                limit: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(members.memberships[0].user_id, bob.user_id);
    let updated = store
        .update_permission_group(
            &alice,
            &tenant_a.tenant_id,
            &group.group_id,
            &ternilo_control::GroupInput {
                name: "Approvers".to_owned(),
                description: Some("Release approval".to_owned()),
            },
            now + 4,
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "Approvers");
    store
        .delete_permission_group(&alice, &tenant_a.tenant_id, &group.group_id, now + 4)
        .await
        .unwrap();
    assert!(
        store
            .list_permission_groups(
                &alice,
                &tenant_a.tenant_id,
                &ternilo_control::PageQuery::default()
            )
            .await
            .unwrap()
            .groups
            .is_empty()
    );
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Admin,
            now + 4,
        )
        .await
        .unwrap();
    assert!(
        store
            .set_membership(
                &bob,
                &tenant_a.tenant_id,
                &bob.user_id,
                TenantRole::Owner,
                now + 5,
            )
            .await
            .is_err()
    );
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 6,
        )
        .await
        .unwrap();
    assert!(
        store
            .remove_membership(&alice, &tenant_a.tenant_id, &alice.user_id, now + 7)
            .await
            .is_err()
    );

    let project_a = store
        .create_project(&alice, &tenant_a.tenant_id, "Project A", now + 8)
        .await
        .unwrap();
    let project_b = store
        .create_project(&alice, &tenant_a.tenant_id, "Project B", now + 9)
        .await
        .unwrap();

    let alice_workspace = store
        .create_cloud_workspace(
            &alice,
            &tenant_a.tenant_id,
            &project_a.project_id,
            "Alice work",
            now + 9,
        )
        .await
        .unwrap();
    let bob_workspace = store
        .create_cloud_workspace(
            &bob,
            &tenant_a.tenant_id,
            &project_a.project_id,
            "Bob work",
            now + 9,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_workspaces(&alice, &tenant_a.tenant_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .list_workspaces(&bob, &tenant_a.tenant_id)
            .await
            .unwrap(),
        vec![bob_workspace]
    );
    assert!(
        store
            .get_workspace(&bob, &tenant_a.tenant_id, &alice_workspace.workspace_id)
            .await
            .is_err()
    );
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Admin,
            now + 9,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_workspaces(&bob, &tenant_a.tenant_id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .get_workspace(&bob, &tenant_a.tenant_id, &alice_workspace.workspace_id)
            .await
            .unwrap(),
        alice_workspace
    );
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 9,
        )
        .await
        .unwrap();

    let alice_pending = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            ExecutorId::new("alice-pending-node"),
            Duration::from_millis(2),
            now + 9,
        )
        .await
        .unwrap();
    let pending_conflict = store
        .create_owned_enrollment(
            &bob,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            alice_pending.executor_id.clone(),
            Duration::from_mins(5),
            now + 10,
        )
        .await
        .unwrap_err();
    assert!(
        pending_conflict
            .to_string()
            .contains("executor id belongs to another user"),
        "member self-enrollment must not claim another user's active enrollment: {pending_conflict}"
    );

    let alice_owned_enrollment = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            ExecutorId::new("alice-owned-node"),
            Duration::from_mins(5),
            now + 12,
        )
        .await
        .unwrap();
    store
        .consume_enrollment(&alice_owned_enrollment.token, now + 13)
        .await
        .unwrap();
    let active_executor_conflict = store
        .create_owned_enrollment(
            &bob,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            alice_owned_enrollment.executor_id.clone(),
            Duration::from_mins(5),
            now + 14,
        )
        .await
        .unwrap_err();
    assert!(
        active_executor_conflict.code == ternilo_protocol::ErrorCode::PolicyDenied,
        "member self-enrollment must not claim another user's executor: {active_executor_conflict}"
    );
    store
        .revoke_executor(
            &alice,
            &tenant_a.tenant_id,
            &alice_owned_enrollment.executor_id,
            now + 15,
        )
        .await
        .unwrap();
    let revoked_executor_conflict = store
        .create_owned_enrollment(
            &bob,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            alice_owned_enrollment.executor_id.clone(),
            Duration::from_mins(5),
            now + 16,
        )
        .await
        .unwrap_err();
    assert!(
        revoked_executor_conflict.code == ternilo_protocol::ErrorCode::PolicyDenied,
        "revocation must not let a member claim another user's executor id: {revoked_executor_conflict}"
    );

    let bob_owned_enrollment = store
        .create_owned_enrollment(
            &bob,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            ExecutorId::new("bob-owned-node"),
            Duration::from_mins(5),
            now + 17,
        )
        .await
        .unwrap();
    store
        .consume_enrollment(&bob_owned_enrollment.token, now + 18)
        .await
        .unwrap();
    let bob_computers = store
        .list_owned_executors(&bob, &tenant_a.tenant_id)
        .await
        .unwrap();
    assert!(
        bob_computers
            .iter()
            .any(|executor| executor.executor_id == bob_owned_enrollment.executor_id)
    );
    assert!(
        store
            .list_owned_executors(&alice, &tenant_a.tenant_id)
            .await
            .unwrap()
            .iter()
            .all(|executor| executor.executor_id != bob_owned_enrollment.executor_id)
    );
    assert!(
        store
            .revoke_owned_executor(
                &alice,
                &tenant_a.tenant_id,
                &bob_owned_enrollment.executor_id,
                now + 19,
            )
            .await
            .is_err(),
        "the owned-computer route must not revoke another user's executor"
    );
    store
        .revoke_owned_executor(
            &bob,
            &tenant_a.tenant_id,
            &bob_owned_enrollment.executor_id,
            now + 20,
        )
        .await
        .unwrap();

    let global_secret = store
        .put_secret(
            &alice,
            &tenant_a.tenant_id,
            None,
            "provider.api-key",
            b"global-secret-value",
            now + 10,
        )
        .await
        .unwrap();
    assert_eq!(global_secret.version, 1);
    let project_secret = store
        .put_secret(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            "provider.api-key",
            b"project-a-secret-value",
            now + 11,
        )
        .await
        .unwrap();
    assert_eq!(
        project_secret.project_id,
        Some(project_a.project_id.clone())
    );
    store
        .put_secret(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_b.project_id),
            "provider.api-key",
            b"project-b-secret-value",
            now + 12,
        )
        .await
        .unwrap();
    store
        .put_secret(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_b.project_id),
            "project-b-only",
            b"project-b-only-value",
            now + 13,
        )
        .await
        .unwrap();
    let enrollment_a = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            ExecutorId::new("project-a-node"),
            Duration::from_mins(5),
            now + 14,
        )
        .await
        .unwrap();
    let credential_a = store
        .consume_enrollment(&enrollment_a.token, now + 15)
        .await
        .unwrap();
    assert!(
        store
            .consume_enrollment(&enrollment_a.token, now + 16)
            .await
            .is_err()
    );
    let active_reenrollment = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_a.project_id),
            enrollment_a.executor_id.clone(),
            Duration::from_mins(5),
            now + 16,
        )
        .await
        .unwrap_err();
    assert!(
        active_reenrollment
            .to_string()
            .contains("must be revoked before it can be enrolled again"),
        "an active machine credential must not be refreshed or replaced: {active_reenrollment}"
    );
    let enrollment_b = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_b.project_id),
            ExecutorId::new("project-b-node"),
            Duration::from_mins(5),
            now + 17,
        )
        .await
        .unwrap();
    assert!(
        store
            .create_enrollment(
                &alice,
                &tenant_a.tenant_id,
                None,
                ExecutorId::new("over-quota-node"),
                Duration::from_mins(5),
                now + 18,
            )
            .await
            .is_err()
    );
    let credential_b = store
        .consume_enrollment(&enrollment_b.token, now + 19)
        .await
        .unwrap();
    let node_a = store
        .authenticate_node(&credential_a.token, now + 20)
        .await
        .unwrap();
    let node_b = store
        .authenticate_node(&credential_b.token, now + 21)
        .await
        .unwrap();
    assert_eq!(node_a.scope.tenant_id, tenant_a.tenant_id);
    assert_eq!(node_a.scope.user_id, alice.user_id);
    assert_eq!(
        store
            .resolve_secret_for_node(&node_a, "provider.api-key", now + 22)
            .await
            .unwrap()
            .as_slice(),
        b"project-a-secret-value"
    );
    assert_eq!(
        store
            .resolve_secret_for_node(&node_b, "provider.api-key", now + 23)
            .await
            .unwrap()
            .as_slice(),
        b"project-b-secret-value"
    );
    assert!(
        store
            .resolve_secret_for_node(&node_a, "project-b-only", now + 24)
            .await
            .is_err()
    );

    let first = store
        .reserve_quota(
            &bob,
            &tenant_a.tenant_id,
            Some("run-1"),
            600,
            Duration::from_mins(5),
            now + 25,
        )
        .await
        .unwrap();
    let queued = store
        .reserve_quota(
            &bob,
            &tenant_a.tenant_id,
            Some("run-2"),
            1,
            Duration::from_mins(5),
            now + 26,
        )
        .await
        .unwrap();
    assert_ne!(
        queued.reservation_id, first.reservation_id,
        "queued budget reservations do not consume foreground execution slots"
    );
    let mut settlement = store
        .database()
        .tenant_transaction(&tenant_a.tenant_id)
        .await
        .unwrap();
    let summary = ControlStore::finalize_workload_reservation_in(
        &mut settlement,
        &tenant_a.tenant_id,
        &first.reservation_id,
        now + 27,
    )
    .await
    .unwrap();
    assert_eq!(
        summary.used_model_tokens, 0,
        "a reservation without accepted model attempts cannot invent usage"
    );
    settlement.commit().await.unwrap();
    assert!(
        store
            .reserve_quota(
                &bob,
                &tenant_a.tenant_id,
                Some("run-3"),
                1_001,
                Duration::from_mins(5),
                now + 28,
            )
            .await
            .is_err()
    );

    store
        .revoke_executor(
            &alice,
            &tenant_a.tenant_id,
            &ExecutorId::new("project-b-node"),
            now + 29,
        )
        .await
        .unwrap();
    assert!(
        store
            .authenticate_node(&credential_b.token, now + 30)
            .await
            .is_err()
    );
    let replacement_enrollment = store
        .create_enrollment(
            &alice,
            &tenant_a.tenant_id,
            Some(&project_b.project_id),
            ExecutorId::new("project-b-node"),
            Duration::from_mins(5),
            now + 31,
        )
        .await
        .unwrap();
    let replacement_credential = store
        .consume_enrollment(&replacement_enrollment.token, now + 32)
        .await
        .unwrap();
    store
        .authenticate_node(&replacement_credential.token, now + 33)
        .await
        .unwrap();

    let audit = store
        .list_audit(&alice, &tenant_a.tenant_id, 1_000)
        .await
        .unwrap();
    assert!(audit.len() >= 7);
    let audit_json = serde_json::to_string(&audit).unwrap();
    assert!(!audit_json.contains("global-secret-value"));
    assert!(!audit_json.contains("project-a-secret-value"));
    assert!(!audit_json.contains("project-b-secret-value"));
    assert!(!audit_json.contains(&enrollment_a.token));
    assert!(!audit_json.contains(&credential_a.token));
    assert!(!audit_json.contains(&credential_b.token));
    assert!(!audit_json.contains(&replacement_enrollment.token));
    assert!(!audit_json.contains(&replacement_credential.token));

    store
        .remove_membership(&alice, &tenant_a.tenant_id, &bob.user_id, now + 34)
        .await
        .unwrap();
    assert!(
        store
            .authorize(&bob, &tenant_a.tenant_id, ControlAction::TenantRead)
            .await
            .is_err()
    );

    drop(store);
    assert!(
        ControlStore::rotate_secret_master_key(
            admin_url,
            &SecretCipher::from_key([8; 32]),
            &SecretCipher::from_key([10; 32]),
        )
        .await
        .is_err(),
        "an incorrect current key must roll the complete rotation back"
    );
    let unchanged = ControlStore::connect(
        runtime_url,
        migration_url,
        SecretCipher::from_key([9; 32]),
        2,
    )
    .await
    .unwrap();
    let unchanged_node = unchanged
        .authenticate_node(&credential_a.token, now + 35)
        .await
        .unwrap();
    assert_eq!(
        unchanged
            .resolve_secret_for_node(&unchanged_node, "provider.api-key", now + 36)
            .await
            .unwrap()
            .as_slice(),
        b"project-a-secret-value"
    );
    drop(unchanged);

    let rotated = ControlStore::rotate_secret_master_key(
        admin_url,
        &SecretCipher::from_key([9; 32]),
        &SecretCipher::from_key([10; 32]),
    )
    .await
    .unwrap();
    assert_eq!(rotated, 4);

    let stale = ControlStore::connect(
        runtime_url,
        migration_url,
        SecretCipher::from_key([9; 32]),
        2,
    )
    .await
    .unwrap();
    let stale_node = stale
        .authenticate_node(&credential_a.token, now + 37)
        .await
        .unwrap();
    assert!(
        stale
            .resolve_secret_for_node(&stale_node, "provider.api-key", now + 38)
            .await
            .is_err(),
        "the previous key must not decrypt rotated ciphertext"
    );
    drop(stale);

    let rotated_store = ControlStore::connect(
        runtime_url,
        migration_url,
        SecretCipher::from_key([10; 32]),
        2,
    )
    .await
    .unwrap();
    let rotated_node = rotated_store
        .authenticate_node(&credential_a.token, now + 39)
        .await
        .unwrap();
    assert_eq!(
        rotated_store
            .resolve_secret_for_node(&rotated_node, "provider.api-key", now + 40)
            .await
            .unwrap()
            .as_slice(),
        b"project-a-secret-value"
    );
    let ten_year_reconnect = rotated_store
        .authenticate_node(&credential_a.token, now + 10 * 365 * 24 * 60 * 60 * 1_000)
        .await
        .unwrap();
    assert_eq!(ten_year_reconnect.executor_id, credential_a.executor_id);
}
