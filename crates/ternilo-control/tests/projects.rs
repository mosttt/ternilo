use std::time::Duration;

use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ProjectRecord,
    SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{ErrorCode, TenantId};
use ternilo_transport::ExecutorId;

struct Fixture {
    store: ControlStore,
    owner: ControlUser,
    tenant: TenantId,
    project: ProjectRecord,
}

impl Fixture {
    async fn new() -> Self {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([71; 32]), 1)
                .await
                .unwrap();
        Self::with_store(store).await
    }

    async fn with_store(store: ControlStore) -> Self {
        let account = store
            .initialize_owner(
                &NativeRegistration {
                    email: "projects@example.test".to_owned(),
                    username: "projects-owner".to_owned(),
                    password: "projects-password-123".to_owned(),
                },
                1_000,
            )
            .await
            .unwrap();
        let owner = account.session.user;
        store
            .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1_001)
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                &owner,
                "projects-team",
                "Projects",
                TenantQuota::default(),
                1_002,
            )
            .await
            .unwrap()
            .tenant_id;
        let project = store
            .create_project(&owner, &tenant, "Editable", 1_003)
            .await
            .unwrap();
        Self {
            store,
            owner,
            tenant,
            project,
        }
    }

    async fn user(&self, subject: &str) -> ControlUser {
        self.store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://projects.example.test".to_owned(),
                    subject: subject.to_owned(),
                    email: Some(format!("{subject}@example.test")),
                    display_name: Some(subject.to_owned()),
                },
                subject,
                1_004,
            )
            .await
            .unwrap()
    }

    async fn assert_referenced(&self) {
        let before = self
            .store
            .list_audit(&self.owner, &self.tenant, 100)
            .await
            .unwrap();
        let error = self
            .store
            .delete_project(&self.owner, &self.tenant, &self.project.project_id, 2_000)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert_eq!(
            error.message,
            "project has associated resources and cannot be deleted"
        );
        assert!(
            self.store
                .list_projects(&self.owner, &self.tenant)
                .await
                .unwrap()
                .contains(&self.project)
        );
        assert_eq!(
            self.store
                .list_audit(&self.owner, &self.tenant, 100)
                .await
                .unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn project_management_enforces_space_roles() {
    let fixture = Fixture::new().await;
    let actor = fixture.user("project-actor").await;
    for role in [
        None,
        Some(TenantRole::Viewer),
        Some(TenantRole::Member),
        Some(TenantRole::Admin),
    ] {
        if let Some(role) = role {
            fixture
                .store
                .set_membership(&fixture.owner, &fixture.tenant, &actor.user_id, role, 1_010)
                .await
                .unwrap();
        }
        let renamed = fixture
            .store
            .rename_project(
                &actor,
                &fixture.tenant,
                &fixture.project.project_id,
                "Renamed",
                1_011,
            )
            .await;
        if role == Some(TenantRole::Admin) {
            let renamed = renamed.unwrap();
            assert_eq!(renamed.name, "Renamed");
            assert_eq!(renamed.created_at_ms, fixture.project.created_at_ms);
            fixture
                .store
                .delete_project(&actor, &fixture.tenant, &fixture.project.project_id, 1_012)
                .await
                .unwrap();
        } else {
            assert_eq!(renamed.unwrap_err().code, ErrorCode::PolicyDenied);
            assert_eq!(
                fixture
                    .store
                    .delete_project(&actor, &fixture.tenant, &fixture.project.project_id, 1_012)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::PolicyDenied
            );
            assert!(
                fixture
                    .store
                    .list_projects(&fixture.owner, &fixture.tenant)
                    .await
                    .unwrap()
                    .contains(&fixture.project)
            );
        }
    }
    let audit = fixture
        .store
        .list_audit(&fixture.owner, &fixture.tenant, 100)
        .await
        .unwrap();
    let mutations: Vec<_> = audit
        .iter()
        .filter(|entry| matches!(entry.action.as_str(), "project.rename" | "project.delete"))
        .collect();
    assert_eq!(mutations.len(), 2);
    assert!(
        mutations
            .iter()
            .all(|entry| entry.actor_user_id.as_ref() == Some(&actor.user_id))
    );
}

#[tokio::test]
async fn project_management_enforces_tenant_boundaries_for_the_instance_owner() {
    let fixture = Fixture::new().await;
    let actor = fixture.user("private-project-owner").await;
    let personal = fixture.store.identity_session(actor.clone()).await.unwrap();
    assert_eq!(
        fixture
            .store
            .rename_project(
                &fixture.owner,
                &personal.personal_tenant_id,
                &personal.personal_project_id,
                "Wrong",
                1_013
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .store
            .delete_project(
                &fixture.owner,
                &personal.personal_tenant_id,
                &personal.personal_project_id,
                1_013
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .store
            .rename_project(
                &fixture.owner,
                &fixture.tenant,
                &personal.personal_project_id,
                "Wrong",
                1_014
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        fixture
            .store
            .delete_project(
                &fixture.owner,
                &fixture.tenant,
                &personal.personal_project_id,
                1_014
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    let audit = fixture
        .store
        .list_audit(&fixture.owner, &fixture.tenant, 100)
        .await
        .unwrap();
    assert!(
        !audit
            .iter()
            .any(|entry| matches!(entry.action.as_str(), "project.rename" | "project.delete"))
    );
}

#[tokio::test]
async fn project_default_and_last_constraints_survive_renames_and_concurrent_deletes() {
    let fixture = Fixture::new().await;
    let home = fixture
        .store
        .identity_session(fixture.owner.clone())
        .await
        .unwrap();
    fixture
        .store
        .create_project(&fixture.owner, &home.personal_tenant_id, "Another", 1_005)
        .await
        .unwrap();
    fixture
        .store
        .rename_project(
            &fixture.owner,
            &home.personal_tenant_id,
            &home.personal_project_id,
            "  My home  ",
            1_006,
        )
        .await
        .unwrap();
    let error = fixture
        .store
        .delete_project(
            &fixture.owner,
            &home.personal_tenant_id,
            &home.personal_project_id,
            1_007,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(error.message, "personal default project cannot be deleted");
    assert_eq!(
        fixture
            .store
            .identity_session(fixture.owner.clone())
            .await
            .unwrap()
            .personal_project_id,
        home.personal_project_id
    );
    let projects = fixture
        .store
        .list_projects(&fixture.owner, &fixture.tenant)
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        fixture.store.delete_project(
            &fixture.owner,
            &fixture.tenant,
            &projects[0].project_id,
            1_008
        ),
        fixture.store.delete_project(
            &fixture.owner,
            &fixture.tenant,
            &projects[1].project_id,
            1_008
        ),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let rejected = first.err().or_else(|| second.err()).unwrap();
    assert_eq!(rejected.code, ErrorCode::Conflict);
    assert_eq!(
        rejected.message,
        "the last project in a space cannot be deleted"
    );
    assert_eq!(
        fixture
            .store
            .list_projects(&fixture.owner, &fixture.tenant)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn project_names_validate_and_audit_failures_roll_back_both_mutations() {
    let fixture = Fixture::new().await;
    for name in [
        String::new(),
        "   ".to_owned(),
        "bad\nname".to_owned(),
        "界".repeat(86),
    ] {
        assert_eq!(
            fixture
                .store
                .rename_project(
                    &fixture.owner,
                    &fixture.tenant,
                    &fixture.project.project_id,
                    &name,
                    1_004
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
    let audit = fixture
        .store
        .list_audit(&fixture.owner, &fixture.tenant, 100)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_project_audit BEFORE INSERT ON control_audit_log
        WHEN NEW.action IN ('project.rename', 'project.delete') BEGIN SELECT RAISE(ABORT, 'audit failure'); END")
        .execute(fixture.store.database().pool()).await.unwrap();
    assert!(
        fixture
            .store
            .rename_project(
                &fixture.owner,
                &fixture.tenant,
                &fixture.project.project_id,
                "Rolled back",
                1_005
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .delete_project(
                &fixture.owner,
                &fixture.tenant,
                &fixture.project.project_id,
                1_006
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .list_projects(&fixture.owner, &fixture.tenant)
            .await
            .unwrap()
            .contains(&fixture.project)
    );
    assert_eq!(
        fixture
            .store
            .list_audit(&fixture.owner, &fixture.tenant, 100)
            .await
            .unwrap(),
        audit
    );
}

#[tokio::test]
async fn project_deletion_preserves_hidden_workspaces_and_sessions() {
    let fixture = Fixture::new().await;
    let member = fixture.user("workspace-member").await;
    fixture
        .store
        .set_membership(
            &fixture.owner,
            &fixture.tenant,
            &member.user_id,
            TenantRole::Member,
            1_005,
        )
        .await
        .unwrap();
    let workspace = fixture
        .store
        .create_cloud_workspace(
            &member,
            &fixture.tenant,
            &fixture.project.project_id,
            "Private workspace",
            1_006,
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO control_executors (tenant_id, executor_id, owner_user_id, state, enrolled_at_ms)
        VALUES ($1, 'test-node', $2, 'active', 1006)")
        .bind(fixture.tenant.as_str()).bind(member.user_id.as_str()).execute(fixture.store.database().pool()).await.unwrap();
    sqlx::query("INSERT INTO control_edge_sessions (tenant_id, session_id, workspace_id, executor_id, owner_user_id, node_session_id, metadata_json, created_at_ms, updated_at_ms)
        VALUES ($1, 'test-session', $2, 'test-node', $3, 'node-session', '{}', 1006, 1006)")
        .bind(fixture.tenant.as_str()).bind(workspace.workspace_id.as_str()).bind(member.user_id.as_str())
        .execute(fixture.store.database().pool()).await.unwrap();
    assert_ne!(workspace.owner_user_id, fixture.owner.user_id);
    fixture.assert_referenced().await;
    sqlx::query("UPDATE control_workspaces SET unregistered_at_ms = 1007 WHERE workspace_id = $1")
        .bind(workspace.workspace_id.as_str())
        .execute(fixture.store.database().pool())
        .await
        .unwrap();
    assert!(
        fixture
            .store
            .list_workspaces(&fixture.owner, &fixture.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    fixture.assert_referenced().await;
    let sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_edge_sessions WHERE session_id = 'test-session'",
    )
    .fetch_one(fixture.store.database().pool())
    .await
    .unwrap();
    assert_eq!(sessions, 1);
    let renamed = fixture
        .store
        .rename_project(
            &fixture.owner,
            &fixture.tenant,
            &fixture.project.project_id,
            "  Still here  ",
            1_008,
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "Still here");
    let project: String =
        sqlx::query_scalar("SELECT project_id FROM control_workspaces WHERE workspace_id = $1")
            .bind(workspace.workspace_id.as_str())
            .fetch_one(fixture.store.database().pool())
            .await
            .unwrap();
    assert_eq!(project, fixture.project.project_id);
}

#[tokio::test]
async fn project_deletion_rejects_expired_enrollments_and_revoked_computers() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .create_enrollment(
            &fixture.owner,
            &fixture.tenant,
            Some(&fixture.project.project_id),
            ExecutorId::new("expired-node"),
            Duration::from_secs(60),
            1_004,
        )
        .await
        .unwrap();
    let error = fixture
        .store
        .delete_project(
            &fixture.owner,
            &fixture.tenant,
            &fixture.project.project_id,
            100_000,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    fixture.assert_referenced().await;
    sqlx::query("DELETE FROM control_executor_enrollments")
        .execute(fixture.store.database().pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO control_executors (tenant_id, executor_id, project_id, owner_user_id, state, enrolled_at_ms)
        VALUES ($1, 'revoked-node', $2, $3, 'revoked', 1006)")
        .bind(fixture.tenant.as_str()).bind(&fixture.project.project_id).bind(fixture.owner.user_id.as_str())
        .execute(fixture.store.database().pool()).await.unwrap();
    fixture.assert_referenced().await;
}

#[tokio::test]
async fn project_deletion_rejects_deleted_secret_history() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .put_secret(
            &fixture.owner,
            &fixture.tenant,
            Some(&fixture.project.project_id),
            "PROJECT_KEY",
            b"test-value",
            1_004,
        )
        .await
        .unwrap();
    fixture.assert_referenced().await;
    fixture
        .store
        .delete_secret(
            &fixture.owner,
            &fixture.tenant,
            Some(&fixture.project.project_id),
            "PROJECT_KEY",
            1_005,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .store
            .list_secrets(&fixture.owner, &fixture.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    fixture.assert_referenced().await;
}

#[tokio::test]
async fn project_deletion_rejects_usage_history_without_foreign_keys() {
    let fixture = Fixture::new().await;
    sqlx::query("INSERT INTO control_model_requests
        (request_id, origin, source, caller_scope, request_key, payload_hash, actor_user_id,
         model_beneficiary_user_id, workload_json, tenant_id, project_id, model_id, provider_id,
         upstream_model, protocol, route_snapshot_json, max_attempts, state, month, created_at_ms, expires_at_ms)
        VALUES ('history', 'workload', 'user_provider', 'test', 'request', 'hash', $1,
         $1, '{}', $2, $3, 'model', 'provider', 'model', 'openai-responses', '{}', 1, 'completed', '2026-09', 1004, 1005)")
        .bind(fixture.owner.user_id.as_str()).bind(fixture.tenant.as_str()).bind(&fixture.project.project_id)
        .execute(fixture.store.database().pool()).await.unwrap();
    fixture.assert_referenced().await;
}

#[tokio::test]
async fn project_deletion_and_workspace_creation_never_cascade_a_committed_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("projects.sqlite").display()
    );
    let store = ControlStore::connect(&database_url, None, SecretCipher::from_key([71; 32]), 4)
        .await
        .unwrap();
    let fixture = Fixture::with_store(store).await;
    let (deleted, created) = tokio::join!(
        fixture.store.delete_project(
            &fixture.owner,
            &fixture.tenant,
            &fixture.project.project_id,
            1_004
        ),
        fixture.store.create_cloud_workspace(
            &fixture.owner,
            &fixture.tenant,
            &fixture.project.project_id,
            "Concurrent workspace",
            1_004
        ),
    );
    assert_ne!(deleted.is_ok(), created.is_ok());
    let workspaces = fixture
        .store
        .list_workspaces(&fixture.owner, &fixture.tenant)
        .await
        .unwrap();
    if let Ok(workspace) = created {
        assert_eq!(deleted.unwrap_err().code, ErrorCode::Conflict);
        assert_eq!(workspaces, vec![workspace]);
    } else {
        assert!(workspaces.is_empty());
    }
    fixture.store.database().close().await;
}
