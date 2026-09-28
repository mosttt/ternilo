use sqlx::{Executor as _, Row as _};
use ternilo_cloud::{CloudSessionDraft, CloudStore};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{
    AgentId, AgentTeamMemberRole, AgentTeamMessageSend, AgentTeamTaskCreate, AgentTeamTaskReplace,
    AgentTeamTaskStatus, ErrorCode, PermissionPreset, SessionId, SessionMode, SubagentId,
    SubagentSessionMetadata, SubagentTranscriptKind, TenantId, UserId, WorkspaceId,
};
use ternilo_storage::{Backend, Database};

mod support;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_agent_team_is_owner_scoped_durable_and_matches_local_semantics() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test",
    );
    reset_database(&admin_url).await;
    let runtime_url = support::database_url_for_role(
        &admin_url,
        "ternilo_agent_team_runtime_test",
        "agent-team-runtime-password",
    );
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([41; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&admin_url), 4)
        .await
        .unwrap();
    let audit = Database::connect(&admin_url, 2).await.unwrap();
    agent_team_contract(control, cloud, &audit, &runtime_url).await;
    audit.close().await;
}

#[tokio::test]
async fn sqlite_agent_team_is_owner_scoped_durable_and_matches_local_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("agent-team.sqlite3").display()
    );
    let database = Database::connect(&url, 4).await.unwrap();
    let control = ControlStore::from_database(database.clone(), SecretCipher::from_key([41; 32]))
        .await
        .unwrap();
    let cloud = CloudStore::from_database(database.clone()).await.unwrap();
    agent_team_contract(control, cloud, &database, &url).await;
    database.close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "The same complete task, message, isolation and reopen contract runs on both databases."
)]
async fn agent_team_contract(
    control: ControlStore,
    cloud: CloudStore,
    audit: &Database,
    runtime_url: &str,
) {
    let now = 2_300_000_000_000_u64;
    let (user, tenant, project_id, workspace_id) =
        create_owner_workspace(&control, "primary", now).await;
    let (other_user, other_tenant, other_project, other_workspace) =
        create_owner_workspace(&control, "other", now + 100).await;

    create_session(
        &cloud,
        &tenant,
        &user,
        &project_id,
        &workspace_id,
        "root-internal-session",
        now + 10,
    )
    .await;
    create_session(
        &cloud,
        &tenant,
        &user,
        &project_id,
        &workspace_id,
        "child-internal-session",
        now + 11,
    )
    .await;
    create_session(
        &cloud,
        &tenant,
        &user,
        &project_id,
        &workspace_id,
        "grandchild-internal-session",
        now + 12,
    )
    .await;
    create_session(
        &cloud,
        &tenant,
        &user,
        &project_id,
        &workspace_id,
        "fork-internal-session",
        now + 13,
    )
    .await;
    create_session(
        &cloud,
        &other_tenant,
        &other_user,
        &other_project,
        &other_workspace,
        "other-root-session",
        now + 110,
    )
    .await;

    let admin = audit.pool();
    assert_invalid_metadata_rejected(admin, &tenant).await;
    set_subagent(
        admin,
        &tenant,
        "child-internal-session",
        "root-internal-session",
        "researcher",
    )
    .await;
    set_subagent(
        admin,
        &tenant,
        "grandchild-internal-session",
        "child-internal-session",
        "reviewer",
    )
    .await;
    sqlx::query(
        "UPDATE cloud_sessions
         SET parent_session_id = $3
         WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant.as_str())
    .bind("fork-internal-session")
    .bind("root-internal-session")
    .execute(admin)
    .await
    .unwrap();

    let root_id = SessionId::new("root-internal-session");
    let child_id = SessionId::new("child-internal-session");
    let grandchild_id = SessionId::new("grandchild-internal-session");
    let fork_id = SessionId::new("fork-internal-session");
    let mapped_child = cloud.get_session(&tenant, &child_id).await.unwrap();
    assert_eq!(
        mapped_child
            .subagent
            .as_ref()
            .map(|metadata| metadata.subagent_id.as_str()),
        Some("researcher")
    );
    let owned_child = cloud
        .find_owned_session(&tenant, &user, &child_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned_child.subagent, mapped_child.subagent);
    let listed_child = cloud
        .list_sessions(&tenant, 20)
        .await
        .unwrap()
        .into_iter()
        .find(|session| session.session_id == child_id)
        .unwrap();
    assert_eq!(listed_child.subagent, mapped_child.subagent);
    assert!(
        cloud
            .get_session(&tenant, &fork_id)
            .await
            .unwrap()
            .subagent
            .is_none()
    );

    let root_snapshot = cloud
        .agent_team_snapshot(&tenant, &user, &root_id)
        .await
        .unwrap();
    let child_snapshot = cloud
        .agent_team_snapshot(&tenant, &user, &child_id)
        .await
        .unwrap();
    let grandchild_snapshot = cloud
        .agent_team_snapshot(&tenant, &user, &grandchild_id)
        .await
        .unwrap();
    assert_eq!(root_snapshot.team_id, child_snapshot.team_id);
    assert_eq!(root_snapshot.team_id, grandchild_snapshot.team_id);
    assert_eq!(root_snapshot.members.len(), 3);
    assert!(
        root_snapshot
            .members
            .iter()
            .all(|member| !member.id.as_str().contains("internal-session"))
    );
    assert!(
        !root_snapshot
            .team_id
            .as_str()
            .contains("root-internal-session")
    );
    let lead_member = root_snapshot
        .members
        .iter()
        .find(|member| member.role == AgentTeamMemberRole::Lead)
        .unwrap()
        .id
        .clone();
    let child_member = root_snapshot
        .members
        .iter()
        .find(|member| {
            member
                .subagent_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "researcher")
        })
        .unwrap()
        .id
        .clone();
    let grandchild_member = root_snapshot
        .members
        .iter()
        .find(|member| {
            member
                .subagent_id
                .as_ref()
                .is_some_and(|id| id.as_str() == "reviewer")
        })
        .unwrap()
        .id
        .clone();
    assert_eq!(root_snapshot.current_member_id, lead_member);
    assert_eq!(child_snapshot.current_member_id, child_member);

    let fork_snapshot = cloud
        .agent_team_snapshot(&tenant, &user, &fork_id)
        .await
        .unwrap();
    assert_ne!(fork_snapshot.team_id, root_snapshot.team_id);
    assert_eq!(fork_snapshot.members.len(), 1);
    assert!(fork_snapshot.tasks.is_empty());

    let base = cloud
        .create_agent_team_task(
            &tenant,
            &user,
            &root_id,
            AgentTeamTaskCreate {
                subject: "Base".to_owned(),
                description: "Shared prerequisite".to_owned(),
                status: AgentTeamTaskStatus::Completed,
                dependencies: Vec::new(),
                owner: Some(lead_member.clone()),
            },
            now + 20,
        )
        .await
        .unwrap();
    let dependent = cloud
        .create_agent_team_task(
            &tenant,
            &user,
            &child_id,
            AgentTeamTaskCreate {
                subject: "Dependent".to_owned(),
                description: String::new(),
                status: AgentTeamTaskStatus::InProgress,
                dependencies: vec![base.id.clone()],
                owner: Some(child_member.clone()),
            },
            now + 21,
        )
        .await
        .unwrap();
    let delete_error = cloud
        .delete_agent_team_task(&tenant, &user, &root_id, &base.id, base.revision)
        .await
        .unwrap_err();
    assert_eq!(delete_error.code, ErrorCode::Conflict);
    let stale_error = cloud
        .replace_agent_team_task(
            &tenant,
            &user,
            &root_id,
            &dependent.id,
            AgentTeamTaskReplace {
                expected_revision: 99,
                subject: dependent.subject.clone(),
                description: dependent.description.clone(),
                status: dependent.status,
                dependencies: dependent.dependencies.clone(),
                owner: dependent.owner.clone(),
            },
            now + 22,
        )
        .await
        .unwrap_err();
    assert_eq!(stale_error.code, ErrorCode::Conflict);
    let cycle_error = cloud
        .replace_agent_team_task(
            &tenant,
            &user,
            &root_id,
            &base.id,
            AgentTeamTaskReplace {
                expected_revision: base.revision,
                subject: base.subject.clone(),
                description: base.description.clone(),
                status: base.status,
                dependencies: vec![dependent.id.clone()],
                owner: base.owner.clone(),
            },
            now + 23,
        )
        .await
        .unwrap_err();
    assert_eq!(cycle_error.code, ErrorCode::InvalidInput);
    let dependent = cloud
        .replace_agent_team_task(
            &tenant,
            &user,
            &child_id,
            &dependent.id,
            AgentTeamTaskReplace {
                expected_revision: dependent.revision,
                subject: dependent.subject,
                description: dependent.description,
                status: AgentTeamTaskStatus::Completed,
                dependencies: Vec::new(),
                owner: dependent.owner,
            },
            now + 24,
        )
        .await
        .unwrap();
    assert_eq!(dependent.revision, 2);
    cloud
        .delete_agent_team_task(&tenant, &user, &root_id, &base.id, base.revision)
        .await
        .unwrap();
    let unknown_owner = cloud
        .create_agent_team_task(
            &tenant,
            &user,
            &root_id,
            AgentTeamTaskCreate {
                subject: "Wrong owner".to_owned(),
                description: String::new(),
                status: AgentTeamTaskStatus::Pending,
                dependencies: Vec::new(),
                owner: Some(fork_snapshot.current_member_id.clone()),
            },
            now + 25,
        )
        .await
        .unwrap_err();
    assert_eq!(unknown_owner.code, ErrorCode::InvalidInput);

    let first_message = cloud
        .send_agent_team_message(
            &tenant,
            &user,
            &root_id,
            AgentTeamMessageSend {
                to: child_member.clone(),
                content: "Review this".to_owned(),
            },
            now + 30,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .mark_agent_team_message_read(
                &tenant,
                &user,
                &grandchild_id,
                &first_message.id,
                now + 31,
            )
            .await
            .is_err()
    );
    assert!(
        cloud
            .mark_agent_team_message_read(&tenant, &user, &root_id, &first_message.id, now + 31,)
            .await
            .is_err()
    );
    let read_message = cloud
        .mark_agent_team_message_read(&tenant, &user, &child_id, &first_message.id, now + 32)
        .await
        .unwrap();
    assert_eq!(read_message.read_at_ms, Some(now + 32));
    cloud
        .send_agent_team_message(
            &tenant,
            &user,
            &child_id,
            AgentTeamMessageSend {
                to: grandchild_member,
                content: "Please verify".to_owned(),
            },
            now + 33,
        )
        .await
        .unwrap();
    let root_mailbox = cloud
        .agent_team_snapshot(&tenant, &user, &root_id)
        .await
        .unwrap();
    let child_mailbox = cloud
        .agent_team_snapshot(&tenant, &user, &child_id)
        .await
        .unwrap();
    let grandchild_mailbox = cloud
        .agent_team_snapshot(&tenant, &user, &grandchild_id)
        .await
        .unwrap();
    assert_eq!(root_mailbox.messages.len(), 1);
    assert_eq!(root_mailbox.messages[0].read_at_ms, Some(now + 32));
    assert_eq!(child_mailbox.messages.len(), 2);
    assert_eq!(grandchild_mailbox.messages.len(), 1);
    let isolated_fork = cloud
        .agent_team_snapshot(&tenant, &user, &fork_id)
        .await
        .unwrap();
    assert!(isolated_fork.tasks.is_empty());
    assert!(isolated_fork.messages.is_empty());
    let wrong_recipient = cloud
        .send_agent_team_message(
            &tenant,
            &user,
            &root_id,
            AgentTeamMessageSend {
                to: fork_snapshot.current_member_id,
                content: "Must not cross teams".to_owned(),
            },
            now + 34,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_recipient.code, ErrorCode::InvalidInput);

    let other_id = SessionId::new("other-root-session");
    let other_snapshot = cloud
        .agent_team_snapshot(&other_tenant, &other_user, &other_id)
        .await
        .unwrap();
    assert!(other_snapshot.tasks.is_empty());
    assert!(other_snapshot.messages.is_empty());
    let cross_owner = cloud
        .agent_team_snapshot(&tenant, &other_user, &root_id)
        .await
        .unwrap_err();
    assert_eq!(cross_owner.code, ErrorCode::PolicyDenied);
    let cross_team_dependency = cloud
        .create_agent_team_task(
            &other_tenant,
            &other_user,
            &other_id,
            AgentTeamTaskCreate {
                subject: "Cross tenant".to_owned(),
                description: String::new(),
                status: AgentTeamTaskStatus::Pending,
                dependencies: vec![dependent.id.clone()],
                owner: Some(other_snapshot.current_member_id),
            },
            now + 120,
        )
        .await
        .unwrap_err();
    assert_eq!(cross_team_dependency.code, ErrorCode::InvalidInput);

    if audit.backend() == Backend::Postgres {
        let runtime = Database::connect(runtime_url, 2).await.unwrap();
        let mut transaction = runtime
            .owner_transaction(&tenant, &other_user)
            .await
            .unwrap();
        let hidden_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_agent_team_tasks")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(
            hidden_count, 0,
            "PostgreSQL RLS also excludes the wrong owner in raw SQL"
        );
        transaction.rollback().await.unwrap();
        runtime.close().await;
    }

    drop(cloud);
    let reopened = CloudStore::connect_without_migrations(runtime_url, 2)
        .await
        .unwrap();
    let durable = reopened
        .agent_team_snapshot(&tenant, &user, &root_id)
        .await
        .unwrap();
    assert_eq!(durable.team_id, root_snapshot.team_id);
    assert_eq!(durable.tasks.len(), 1);
    assert_eq!(durable.tasks[0].id, dependent.id);
    assert_eq!(durable.messages.len(), 1);
}

async fn create_owner_workspace(
    control: &ControlStore,
    suffix: &str,
    now: u64,
) -> (UserId, TenantId, String, WorkspaceId) {
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://issuer.example".to_owned(),
                subject: format!("agent-team-{suffix}"),
                email: None,
                display_name: Some(format!("Agent Team {suffix}")),
            },
            &format!("test-agent-team-{suffix}"),
            now,
        )
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &user,
            &format!("agent-team-{suffix}"),
            &format!("Agent Team {suffix}"),
            TenantQuota {
                max_nodes: 1,
                max_concurrent_runs: 2,
                monthly_model_tokens: 10_000,
                max_secrets: 2,
            },
            now + 1,
        )
        .await
        .unwrap();
    let project = control
        .create_project(&user, &tenant.tenant_id, "Agent Team", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &user,
            &tenant.tenant_id,
            &project.project_id,
            "Agent Team workspace",
            now + 3,
        )
        .await
        .unwrap();
    (
        user.user_id,
        tenant.tenant_id,
        project.project_id,
        workspace.workspace_id,
    )
}

async fn create_session(
    cloud: &CloudStore,
    tenant_id: &TenantId,
    user_id: &UserId,
    project_id: &str,
    workspace_id: &WorkspaceId,
    session_id: &str,
    now: u64,
) {
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.to_owned(),
                workspace_id: workspace_id.clone(),
                session_id: Some(SessionId::new(session_id)),
                agent_id: AgentId::new("agent"),
                title: session_id.replace("-internal-session", ""),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            tenant_id,
            user_id,
            now,
        )
        .await
        .unwrap();
}

async fn assert_invalid_metadata_rejected(admin: &sqlx::AnyPool, tenant: &TenantId) {
    for metadata in [
        "null",
        "[]",
        "{}",
        r#"{"subagent_id":"child","provider":"in-process"}"#,
        r#"{"subagent_id":42,"provider":"in-process","transcript_kind":"conversation"}"#,
        r#"{"subagent_id":"bad id","provider":"in-process","transcript_kind":"conversation"}"#,
        r#"{"subagent_id":"child","provider":" ","transcript_kind":"conversation"}"#,
        r#"{"subagent_id":"child","provider":"in-process","transcript_kind":"unknown"}"#,
        r#"{"subagent_id":"child","provider":"in-process","transcript_kind":"conversation","extra":true}"#,
    ] {
        assert!(sqlx::query(
            "UPDATE cloud_sessions SET parent_session_id = 'root-internal-session', subagent_metadata = $2
             WHERE tenant_id = $1 AND session_id = 'child-internal-session'",
        ).bind(tenant.as_str()).bind(metadata).execute(admin).await.is_err(),
            "database accepted invalid subagent metadata: {metadata}");
    }
    assert!(
        sqlx::query(
            "UPDATE cloud_sessions SET subagent_metadata = $2
         WHERE tenant_id = $1 AND session_id = 'root-internal-session'",
        )
        .bind(tenant.as_str())
        .bind(r#"{"subagent_id":"child","provider":"in-process","transcript_kind":"conversation"}"#)
        .execute(admin)
        .await
        .is_err(),
        "subagent metadata requires a parent session"
    );
}

async fn set_subagent(
    admin: &sqlx::AnyPool,
    tenant_id: &TenantId,
    session_id: &str,
    parent_session_id: &str,
    subagent_id: &str,
) {
    let metadata = SubagentSessionMetadata {
        subagent_id: SubagentId::new(subagent_id),
        provider: "in-process".to_owned(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    sqlx::query(
        "UPDATE cloud_sessions
         SET parent_session_id = $3, subagent_metadata = $4
         WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(session_id)
    .bind(parent_session_id)
    .bind(serde_json::to_string(&metadata).unwrap())
    .execute(admin)
    .await
    .unwrap();
}

async fn reset_database(admin_url: &str) {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_agent_team_runtime_test")
        .await
        .unwrap();
    drop(admin);

    let control = ControlStore::connect(admin_url, None, SecretCipher::from_key([41; 32]), 2)
        .await
        .unwrap();
    drop(control);
    let cloud = CloudStore::connect(admin_url, None, 2).await.unwrap();
    drop(cloud);

    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute(
            "CREATE ROLE ternilo_agent_team_runtime_test
             LOGIN PASSWORD 'agent-team-runtime-password'",
        )
        .await
        .unwrap();
    admin
        .execute("GRANT USAGE ON SCHEMA public TO ternilo_agent_team_runtime_test")
        .await
        .unwrap();
    admin
        .execute(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public
             TO ternilo_agent_team_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute(
            "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public
             TO ternilo_agent_team_runtime_test",
        )
        .await
        .unwrap();

    let policies = sqlx::query(
        "SELECT tablename, policyname
         FROM pg_policies
         WHERE tablename IN (
            'cloud_agent_team_tasks',
            'cloud_agent_team_task_dependencies',
            'cloud_agent_team_messages'
         )
         ORDER BY tablename",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(policies.len(), 3);
    assert!(policies.iter().all(|row| {
        row.try_get::<String, _>("policyname")
            .unwrap()
            .ends_with("owner_scope")
    }));
}
