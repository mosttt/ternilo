#[path = "support/postgres.rs"]
mod postgres_runtime;

use sqlx::Executor;
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, SecretCipher, TenantQuota,
};
use ternilo_protocol::{ErrorCode, TenantId};

struct Fixture {
    admin: sqlx::PgPool,
    store: ControlStore,
    owner: ControlUser,
    tenant: TenantId,
    project_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL")
            .expect("TERNILO_TEST_DATABASE_URL must point to the disposable project test database");
        let mut runtime_url = admin_url
            .parse::<sqlx::any::AnyConnectOptions>()
            .unwrap()
            .database_url;
        assert_eq!(runtime_url.path(), "/ternilo_control_test_projects");
        let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&admin)
            .await
            .unwrap();
        assert_eq!(database, "ternilo_control_test_projects");
        admin
            .execute("DROP SCHEMA IF EXISTS public CASCADE")
            .await
            .unwrap();
        admin.execute("CREATE SCHEMA public").await.unwrap();
        postgres_runtime::prepare_role(&admin, "ternilo_projects_test", "projects-test-password")
            .await;
        runtime_url.set_username("ternilo_projects_test").unwrap();
        runtime_url
            .set_password(Some("projects-test-password"))
            .unwrap();
        let store = ControlStore::connect(
            runtime_url.as_str(),
            Some(&admin_url),
            SecretCipher::from_key([71; 32]),
            1,
        )
        .await
        .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "projects-rls@example.test".to_owned(),
                    username: "projects-rls-owner".to_owned(),
                    password: "projects-rls-password".to_owned(),
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
                "projects-rls",
                "Projects RLS",
                TenantQuota::default(),
                1_002,
            )
            .await
            .unwrap()
            .tenant_id;
        let project_id = store
            .create_project(&owner, &tenant, "Usage history only", 1_003)
            .await
            .unwrap()
            .project_id;
        Self {
            admin,
            store,
            owner,
            tenant,
            project_id,
        }
    }

    async fn seed_usage(&self) {
        sqlx::query("INSERT INTO control_model_requests
            (request_id, origin, source, caller_scope, request_key, payload_hash, actor_user_id,
             model_beneficiary_user_id, workload_json, tenant_id, project_id, model_id, provider_id,
             upstream_model, protocol, route_snapshot_json, max_attempts, state, month, created_at_ms, expires_at_ms)
            VALUES ('project-rls-history', 'workload', 'user_provider', 'project-rls', 'request', 'hash', $1,
             $1, '{}', $2, $3, 'model', 'provider', 'model', 'openai-responses', '{}', 1, 'completed', '2026-09', 1004, 1005)")
            .bind(self.owner.user_id.as_str()).bind(self.tenant.as_str()).bind(&self.project_id)
            .execute(&self.admin).await.unwrap();
    }

    async fn assert_restricted_runtime(&self) {
        let privileged: i64 = sqlx::query_scalar(
            "SELECT CAST(rolsuper OR rolbypassrls AS INTEGER) FROM pg_roles WHERE rolname = current_user",
        ).fetch_one(self.store.database().pool()).await.unwrap();
        assert_eq!(privileged, 0);
        let owns_table: i64 = sqlx::query_scalar(
            "SELECT CAST(tableowner = current_user AS INTEGER) FROM pg_tables
             WHERE schemaname = 'public' AND tablename = 'control_model_requests'",
        )
        .fetch_one(self.store.database().pool())
        .await
        .unwrap();
        assert_eq!(owns_table, 0);
        let rls_active: i64 = sqlx::query_scalar(
            "SELECT CAST(row_security_active('control_model_requests') AS INTEGER)",
        )
        .fetch_one(self.store.database().pool())
        .await
        .unwrap();
        assert_eq!(rls_active, 1);
        println!(
            "restricted runtime: privileged={privileged}, owns_table={owns_table}, rls_active={rls_active}"
        );
    }

    async fn assert_usage_hidden_without_model_scope(&self) {
        let mut transaction = self
            .store
            .database()
            .tenant_transaction(&self.tenant)
            .await
            .unwrap();
        let projects: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM control_projects WHERE tenant_id = $1")
                .bind(self.tenant.as_str())
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        assert!(projects >= 2);
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_requests")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(
            requests, 0,
            "tenant scope alone must not expose model usage"
        );
        let model_scope: String = sqlx::query_scalar(
            "SELECT COALESCE(current_setting('ternilo.model_service', true), '')",
        )
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_ne!(
            model_scope, "on",
            "model service scope must not leak through the pool"
        );
        println!(
            "tenant scope: projects={projects}, visible_model_requests={requests}, model_service={model_scope:?}"
        );
        transaction.commit().await.unwrap();
    }

    async fn assert_model_scope_reveals_usage(&self) {
        let mut transaction = self
            .store
            .database()
            .tenant_transaction(&self.tenant)
            .await
            .unwrap();
        sqlx::query("SELECT set_config('ternilo.model_service', 'on', true)")
            .execute(&mut *transaction)
            .await
            .unwrap();
        let requests: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_model_requests WHERE tenant_id = $1 AND project_id = $2",
        )
        .bind(self.tenant.as_str())
        .bind(&self.project_id)
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(
            requests, 1,
            "the same runtime role must see the seeded history with model scope"
        );
        println!("tenant and model service scope: visible_project_model_requests={requests}");
        transaction.rollback().await.unwrap();
    }

    async fn assert_history_rejects_deletion_atomically(&self) {
        let projects = self
            .store
            .list_projects(&self.owner, &self.tenant)
            .await
            .unwrap();
        let audit = self
            .store
            .list_audit(&self.owner, &self.tenant, 100)
            .await
            .unwrap();
        let error = self
            .store
            .delete_project(&self.owner, &self.tenant, &self.project_id, 1_010)
            .await
            .expect_err("runtime RLS must not hide model history from project deletion");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(
            error.message,
            "project has associated resources and cannot be deleted"
        );
        assert_eq!(
            self.store
                .list_projects(&self.owner, &self.tenant)
                .await
                .unwrap(),
            projects
        );
        assert_eq!(
            self.store
                .list_audit(&self.owner, &self.tenant, 100)
                .await
                .unwrap(),
            audit
        );
        let retained: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_model_requests
             WHERE tenant_id = $1 AND project_id = $2 AND request_id = 'project-rls-history'",
        )
        .bind(self.tenant.as_str())
        .bind(&self.project_id)
        .fetch_one(&self.admin)
        .await
        .unwrap();
        assert_eq!(retained, 1);
        println!(
            "project deletion rejected with Conflict; project, model history and audit retained"
        );
    }

    async fn assert_history_filter_keeps_tenant_and_project_boundaries(&self) {
        let empty = self
            .store
            .create_project(&self.owner, &self.tenant, "Empty sibling", 1_011)
            .await
            .unwrap();
        self.store
            .delete_project(&self.owner, &self.tenant, &empty.project_id, 1_012)
            .await
            .unwrap();
        assert!(
            !self
                .store
                .list_projects(&self.owner, &self.tenant)
                .await
                .unwrap()
                .contains(&empty)
        );
        let other = self
            .store
            .create_tenant(
                &self.owner,
                "other-projects-rls",
                "Other projects",
                TenantQuota::default(),
                1_013,
            )
            .await
            .unwrap()
            .tenant_id;
        sqlx::query(
            "INSERT INTO control_projects (tenant_id, project_id, name, created_by, created_at_ms)
            VALUES ($1, $2, 'Same project ID, other tenant', $3, 1014)",
        )
        .bind(other.as_str())
        .bind(&self.project_id)
        .bind(self.owner.user_id.as_str())
        .execute(&self.admin)
        .await
        .unwrap();
        self.store
            .delete_project(&self.owner, &other, &self.project_id, 1_015)
            .await
            .unwrap();
        assert_eq!(
            self.store
                .list_projects(&self.owner, &other)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            self.store
                .list_projects(&self.owner, &self.tenant)
                .await
                .unwrap()
                .iter()
                .any(|project| project.project_id == self.project_id)
        );
    }
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL for disposable ternilo_control_test_projects; resets its public schema"]
async fn postgres_project_deletion_sees_usage_history_with_restricted_runtime_rls() {
    let fixture = Fixture::new().await;
    fixture.assert_restricted_runtime().await;
    fixture.seed_usage().await;
    fixture.assert_usage_hidden_without_model_scope().await;
    fixture.assert_model_scope_reveals_usage().await;
    fixture.assert_usage_hidden_without_model_scope().await;
    fixture.assert_history_rejects_deletion_atomically().await;
    fixture.assert_usage_hidden_without_model_scope().await;
    fixture
        .assert_history_filter_keeps_tenant_and_project_boundaries()
        .await;
    fixture.assert_usage_hidden_without_model_scope().await;
    fixture.store.database().close().await;
    fixture.admin.close().await;
}
