use std::{collections::BTreeSet, time::Duration};

use serde_json::json;
use sqlx::Executor;
use ternilo_cloud::{
    CloudCommandDelivery, CloudSessionCommandDraft, CloudSessionCommandState, CloudSessionDraft,
    CloudStore,
};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota, TenantRole};
use ternilo_protocol::{AgentId, PermissionPreset, SessionId, SessionMode};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability,
    ExecutorCommand, ExecutorCommandBody, ExecutorHello, ExecutorId, ExecutorKind, ExecutorScope,
};

#[path = "support/command_services.rs"]
mod command_services;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_command_journal_is_scoped_durable_idempotent_and_worker_fenced() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test"
    );
    reset_database_and_roles(&admin_url).await;

    let control_admin =
        ControlStore::connect(&admin_url, None, SecretCipher::from_key([31; 32]), 2)
            .await
            .unwrap();
    control_admin.health().await.unwrap();
    drop(control_admin);
    let cloud_admin = CloudStore::connect(&admin_url, None, 2).await.unwrap();
    cloud_admin.health().await.unwrap();
    drop(cloud_admin);

    create_restricted_roles(&admin_url).await;
    let runtime_url = support::database_url_for_role(
        &admin_url,
        "ternilo_command_runtime_test",
        "command-runtime-password",
    );
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([31; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&admin_url), 4)
        .await
        .unwrap();
    let worker = CloudStore::connect_without_migrations(&runtime_url, 2)
        .await
        .unwrap();

    command_journal_contract(control, cloud, worker, &runtime_url).await;
    let runtime = sqlx::PgPool::connect(&runtime_url).await.unwrap();
    let unscoped_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_session_commands")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(
        unscoped_rows, 0,
        "Server runtime reads still require an owner scope"
    );
    assert!(
        runtime
            .execute("CREATE TABLE unauthorized_schema_change (id BIGINT)")
            .await
            .is_err()
    );
    runtime.close().await;
}

#[tokio::test]
async fn sqlite_command_journal_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("commands.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([31; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    let worker = CloudStore::connect_without_migrations(&url, 2)
        .await
        .unwrap();
    command_journal_contract(control, cloud, worker, &url).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep one complete durable command and worker fencing scenario for both database backends."
)]
async fn command_journal_contract(
    control: ControlStore,
    cloud: CloudStore,
    worker: CloudStore,
    runtime_url: &str,
) {
    worker_storage::bind_workers(&cloud, &["worker-a", "worker-b"], "shared-contract-storage")
        .await;
    let now = 2_100_000_000_000_u64;
    let alice = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.test".to_owned(),
                subject: "command-alice".to_owned(),
                email: None,
                display_name: Some("Alice".to_owned()),
            },
            "test-command-alice",
            now,
        )
        .await
        .unwrap();
    let bob = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.test".to_owned(),
                subject: "command-bob".to_owned(),
                email: None,
                display_name: Some("Bob".to_owned()),
            },
            "test-command-bob",
            now + 1,
        )
        .await
        .unwrap();
    let quota = TenantQuota {
        max_nodes: 2,
        max_concurrent_runs: 4,
        monthly_model_tokens: 100_000,
        max_secrets: 4,
    };
    let tenant = control
        .create_tenant(&alice, "command-a", "Command A", quota.clone(), now + 2)
        .await
        .unwrap();
    control
        .set_membership(
            &alice,
            &tenant.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 3,
        )
        .await
        .unwrap();
    let other_tenant = control
        .create_tenant(&alice, "command-b", "Command B", quota, now + 4)
        .await
        .unwrap();
    let project = control
        .create_project(&alice, &tenant.tenant_id, "Command project", now + 5)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &alice,
            &tenant.tenant_id,
            &project.project_id,
            "Command workspace",
            now + 6,
        )
        .await
        .unwrap();
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id,
                workspace_id: workspace.workspace_id,
                session_id: Some(SessionId::new("command-session")),
                agent_id: AgentId::new("agent"),
                title: "Command session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 1_000,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &alice.user_id,
            now + 7,
        )
        .await
        .unwrap();

    let draft = CloudSessionCommandDraft {
        session_id: session.session_id.clone(),
        command: ExecutorCommand {
            input_provenance: None,
            command_id: CommandId::new("skills-command"),
            scope: ExecutorScope {
                tenant_id: tenant.tenant_id.clone(),
                user_id: alice.user_id.clone(),
            },
            issued_at_ms: now + 8,
            expires_at_ms: now + 120_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionSkills {
                    session_id: session.session_id.clone(),
                },
            },
        },
        required_capability: ExecutorCapability::Skills,
        required_catalog_revision: Some("command-test-catalog".to_owned()),
        delivery: CloudCommandDelivery::ReadOnly,
    };
    let first = cloud
        .enqueue_session_command(&tenant.tenant_id, &alice.user_id, &draft, now + 8)
        .await
        .unwrap();
    let repeated = cloud
        .enqueue_session_command(&tenant.tenant_id, &alice.user_id, &draft, now + 9)
        .await
        .unwrap();
    assert_eq!(first.command_seq, repeated.command_seq);
    assert_eq!(first.state, CloudSessionCommandState::Pending);

    let mut conflicting = draft.clone();
    conflicting.command.expires_at_ms += 1;
    let conflict = cloud
        .enqueue_session_command(&tenant.tenant_id, &alice.user_id, &conflicting, now + 9)
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ternilo_protocol::ErrorCode::Conflict);
    assert!(
        cloud
            .session_command(&tenant.tenant_id, &bob.user_id, &draft.command.command_id,)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cloud
            .session_command(
                &other_tenant.tenant_id,
                &alice.user_id,
                &draft.command.command_id,
            )
            .await
            .unwrap()
            .is_none()
    );

    let primary_hello = worker_hello("worker-a", "instance-a-1");
    let stale_worker_a = worker
        .register_cloud_worker(&primary_hello, Duration::from_secs(60), now + 10)
        .await
        .unwrap();
    let same_worker_a = worker
        .register_cloud_worker(&primary_hello, Duration::from_secs(60), now + 11)
        .await
        .unwrap();
    assert_eq!(stale_worker_a.generation, same_worker_a.generation);
    let first_claim = worker
        .claim_session_commands(
            &stale_worker_a,
            &primary_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 12,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first_claim.attempt_count, 1);
    let inspected = worker
        .inspection_session_for_worker(&stale_worker_a, &first_claim, now + 12)
        .await
        .unwrap();
    assert_eq!(inspected.session_id, session.session_id);
    assert_eq!(inspected.workspace_id, session.workspace_id);

    let replacement_worker_a = worker
        .register_cloud_worker(
            &worker_hello("worker-a", "instance-a-2"),
            Duration::from_secs(60),
            now + 13,
        )
        .await
        .unwrap();
    assert!(replacement_worker_a.generation > stale_worker_a.generation);
    assert_eq!(
        worker
            .drain_cloud_worker(&stale_worker_a, now + 14)
            .await
            .unwrap(),
        0,
        "a fenced Worker generation cannot drain its replacement",
    );
    assert_eq!(
        cloud
            .cloud_worker(&ExecutorId::new("worker-a"))
            .await
            .unwrap()
            .unwrap()
            .identity,
        replacement_worker_a
    );
    let stale_reply = CommandReply::success(
        first_claim.command.command_id.clone(),
        now + 14,
        json!({ "skills": [] }),
    );
    assert!(
        worker
            .complete_session_command(&stale_worker_a, &first_claim, &stale_reply, now + 14,)
            .await
            .is_err()
    );

    assert_eq!(worker.reap_session_commands(now + 17_001).await.unwrap(), 1);
    let replacement_hello = worker_hello("worker-b", "instance-b-1");
    let worker_b = worker
        .register_cloud_worker(&replacement_hello, Duration::from_secs(60), now + 17_002)
        .await
        .unwrap();
    let reclaimed = worker
        .claim_session_commands(
            &worker_b,
            &replacement_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 17_003,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(reclaimed.attempt_count, 2);
    let reply = CommandReply::success(
        reclaimed.command.command_id.clone(),
        now + 17_004,
        json!({ "skills": [{ "name": "review" }] }),
    );
    worker
        .complete_session_command(&worker_b, &reclaimed, &reply, now + 17_004)
        .await
        .unwrap();

    worker
        .heartbeat_cloud_worker(&worker_b, Duration::from_secs(60), now + 18_000)
        .await
        .unwrap();
    let mut clock_race_draft = draft.clone();
    clock_race_draft.command.command_id = CommandId::new("clock-race-skills-command");
    clock_race_draft.command.issued_at_ms = now + 18_000;
    cloud
        .enqueue_session_command(
            &tenant.tenant_id,
            &alice.user_id,
            &clock_race_draft,
            now + 18_002,
        )
        .await
        .unwrap();
    let clock_race_claim = worker
        .claim_session_commands(
            &worker_b,
            &replacement_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 18_001,
        )
        .await
        .unwrap()
        .pop()
        .expect("a command created after the poll timestamp remains claimable");
    worker
        .complete_session_command(
            &worker_b,
            &clock_race_claim,
            &CommandReply::success(
                clock_race_claim.command.command_id.clone(),
                now + 18_003,
                json!({ "skills": [] }),
            ),
            now + 18_003,
        )
        .await
        .unwrap();
    Box::pin(command_services::verify(command_services::Fixture {
        control: &control,
        cloud: &cloud,
        worker: &worker,
        session: &session,
        owner: &alice,
        other_user: &bob,
        other_tenant: &other_tenant.tenant_id,
        worker_a: &replacement_worker_a,
        worker_b: &worker_b,
        now: now + 18_004,
    }))
    .await;
    let mut released_draft = draft.clone();
    released_draft.command.command_id = CommandId::new("released-skills-command");
    released_draft.command.issued_at_ms = now + 18_000;
    cloud
        .enqueue_session_command(
            &tenant.tenant_id,
            &alice.user_id,
            &released_draft,
            now + 18_000,
        )
        .await
        .unwrap();
    let released_claim = worker
        .claim_session_commands(
            &worker_b,
            &replacement_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 18_001,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        released_claim.command.command_id,
        CommandId::new("released-skills-command")
    );
    assert_eq!(
        worker
            .release_session_commands(&worker_b, now + 18_002)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        cloud
            .session_command(
                &tenant.tenant_id,
                &alice.user_id,
                &released_draft.command.command_id,
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        CloudSessionCommandState::Pending
    );

    drop(cloud);
    let restarted_control = CloudStore::connect_without_migrations(runtime_url, 2)
        .await
        .unwrap();
    let persisted = restarted_control
        .session_command(&tenant.tenant_id, &alice.user_id, &draft.command.command_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, CloudSessionCommandState::Completed);
    assert_eq!(persisted.reply, Some(reply.clone()));
    assert_eq!(
        restarted_control
            .wait_for_session_command_reply(
                &tenant.tenant_id,
                &alice.user_id,
                &draft.command.command_id,
                Duration::from_millis(50),
                Duration::from_millis(5),
            )
            .await
            .unwrap(),
        Some(reply)
    );
}

fn worker_hello(worker_id: &str, instance_nonce: &str) -> ExecutorHello {
    ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new(worker_id),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: instance_nonce.to_owned(),
        catalog_revision: "command-test-catalog".to_owned(),
        capabilities: BTreeSet::from([
            ExecutorCapability::CloudRun,
            ExecutorCapability::AddressedSessionCommands,
            ExecutorCapability::Skills,
        ]),
    }
}

async fn reset_database_and_roles(admin_url: &str) {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_command_runtime_test")
        .await
        .unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_command_worker_test")
        .await
        .unwrap();
}

async fn create_restricted_roles(admin_url: &str) {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute(
            "CREATE ROLE ternilo_command_runtime_test LOGIN PASSWORD 'command-runtime-password'",
        )
        .await
        .unwrap();
    admin
        .execute("GRANT USAGE ON SCHEMA public TO ternilo_command_runtime_test")
        .await
        .unwrap();
    admin.execute("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO ternilo_command_runtime_test").await.unwrap();
    admin
        .execute(
            "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO ternilo_command_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO ternilo_command_runtime_test")
        .await
        .unwrap();
}
