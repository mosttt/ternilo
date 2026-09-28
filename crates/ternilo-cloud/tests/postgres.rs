use std::{collections::BTreeSet, fmt::Write as _, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use sqlx::{Executor, Row};
use ternilo_cloud::{
    CloudRunDraft, CloudRunState, CloudSessionDraft, CloudSessionUpdate, CloudStore, TerminalState,
    WorkerPolicy,
};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_extension::{
    EXTENSION_PACKAGE_KIND, EXTENSION_PACKAGE_SCHEMA_VERSION, ExtensionContributions,
    ExtensionInstallRequest, ExtensionManifest, ExtensionPayload,
    ExtensionPromptSectionContribution, ExtensionRuntime, ExtensionSkillContribution,
    ExtensionToolContribution, ExtensionToolEffect, PublisherTrust, RhaiExecutionLimits,
};
use ternilo_protocol::{
    ATTACHMENT_REFERENCE_PREFIX, AgentId, AgentPresetCopyRequest, AgentPresetUpdateRequest,
    Attachment, HarnessError, ModelUsage, PermissionPreset, PluginEntry, Profile, ReasoningEffort,
    RunId, RunLimits, RunOutcome, SessionEvent, SessionEventKind, SessionId, SessionMode,
    SkillInvocationPolicy, ToolSpec, UserQuestion, UserQuestionOption,
};
use ternilo_transport::{
    CommandReply, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorCommandBody,
    ExecutorHello, ExecutorId, ExecutorKind,
};

#[path = "support/model_ledger.rs"]
mod model_ledger;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

const FIXTURE_RHAI: &str = r#"
fn cloud_fixture(context, arguments, settings) {
    #{ content: "cloud fixture", is_error: false }
}
"#;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
#[allow(clippy::too_many_lines)]
async fn postgres_queue_enforces_rls_writer_fencing_cancellation_and_recovery() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test"
    );
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_cloud_runtime_test")
        .await
        .unwrap();
    drop(admin);

    let control_admin = ControlStore::connect(&admin_url, None, SecretCipher::from_key([4; 32]), 2)
        .await
        .unwrap();
    control_admin.health().await.unwrap();
    drop(control_admin);
    let cloud_admin = CloudStore::connect(&admin_url, None, 2).await.unwrap();
    cloud_admin.health().await.unwrap();
    drop(cloud_admin);

    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("CREATE ROLE ternilo_cloud_runtime_test LOGIN PASSWORD 'cloud-runtime-password'")
        .await
        .unwrap();
    admin
        .execute("GRANT USAGE ON SCHEMA public TO ternilo_cloud_runtime_test")
        .await
        .unwrap();
    admin
        .execute(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public
             TO ternilo_cloud_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute(
            "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public
             TO ternilo_cloud_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO ternilo_cloud_runtime_test")
        .await
        .unwrap();
    admin
        .execute("REVOKE ALL ON cloud_runtime_control FROM ternilo_cloud_runtime_test")
        .await
        .unwrap();
    admin.execute("REVOKE INSERT,UPDATE,DELETE ON ternilo_schema,cloud_live_changes FROM ternilo_cloud_runtime_test").await.unwrap();

    let may_create: bool = sqlx::query_scalar(
        "SELECT has_schema_privilege('ternilo_cloud_runtime_test','public','CREATE')",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert!(!may_create, "Server runtime must not create schema objects");
    let public_security_definers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_proc AS p
         JOIN pg_namespace AS namespace ON namespace.oid = p.pronamespace
         CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) AS acl
         WHERE namespace.nspname = 'public' AND p.prosecdef
           AND acl.grantee = 0 AND acl.privilege_type = 'EXECUTE'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(public_security_definers, 0);
    drop(admin);

    let runtime_url = support::database_url_for_role(
        &admin_url,
        "ternilo_cloud_runtime_test",
        "cloud-runtime-password",
    );
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([4; 32]),
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
    let audit_database = ternilo_storage::Database::connect(&admin_url, 2)
        .await
        .unwrap();
    let audit = audit_database.pool().clone();

    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_runs")
        .fetch_one(cloud.database().pool())
        .await
        .unwrap();
    assert_eq!(visible, 0, "Server runtime needs an explicit tenant scope");
    Box::pin(queue_contract(control, cloud, worker, audit)).await;
}

#[tokio::test]
async fn sqlite_queue_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("cloud.sqlite").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([4; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let worker = cloud.clone();
    let audit = cloud.database().pool().clone();
    Box::pin(queue_contract(control, cloud, worker, audit)).await;
}

#[allow(clippy::too_many_lines)]
async fn queue_contract(
    control: ControlStore,
    cloud: CloudStore,
    worker: CloudStore,
    audit: sqlx::AnyPool,
) {
    worker_storage::bind_workers(&cloud, &["worker-a", "worker-b"], "shared-contract-storage")
        .await;
    let now = 1_900_000_000_000_u64;
    let alice = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: "cloud-alice".to_owned(),
                email: None,
                display_name: Some("Alice".to_owned()),
            },
            "test-cloud-alice",
            now,
        )
        .await
        .unwrap();
    let bob = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: "cloud-bob".to_owned(),
                email: None,
                display_name: Some("Bob".to_owned()),
            },
            "test-cloud-bob",
            now,
        )
        .await
        .unwrap();
    let quota = TenantQuota {
        max_nodes: 2,
        max_concurrent_runs: 8,
        monthly_model_tokens: 10_000,
        max_secrets: 4,
    };
    let tenant = control
        .create_tenant(&alice, "cloud-a", "Cloud A", quota.clone(), now + 1)
        .await
        .unwrap();
    let other_tenant = control
        .create_tenant(&bob, "cloud-b", "Cloud B", quota, now + 2)
        .await
        .unwrap();
    let project = control
        .create_project(&alice, &tenant.tenant_id, "Cloud project", now + 3)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &alice,
            &tenant.tenant_id,
            &project.project_id,
            "Main workspace",
            now + 4,
        )
        .await
        .unwrap();
    let reasoning_session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(SessionId::new("reasoning-session")),
                agent_id: AgentId::new("agent"),
                title: "Reasoning session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: Some(model_ledger::snapshot(
                    &alice.user_id,
                    &tenant.tenant_id,
                    "reasoning-model",
                    Some(ReasoningEffort::High),
                )),
                reserved_model_tokens: 1_000,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &alice.user_id,
            now + 5,
        )
        .await
        .unwrap();
    assert_eq!(
        reasoning_session.model.as_ref().unwrap().reasoning_effort,
        Some(ReasoningEffort::High)
    );
    let reasoning_session = cloud
        .get_session(&tenant.tenant_id, &reasoning_session.session_id)
        .await
        .unwrap();
    assert_eq!(
        reasoning_session.model.as_ref().unwrap().reasoning_effort,
        Some(ReasoningEffort::High)
    );
    let reasoning_session = cloud
        .update_session(
            &tenant.tenant_id,
            &reasoning_session.session_id,
            &alice.user_id,
            CloudSessionUpdate {
                model: Some(Some(model_ledger::snapshot(
                    &alice.user_id,
                    &tenant.tenant_id,
                    "reasoning-model",
                    None,
                ))),
                ..CloudSessionUpdate::default()
            },
            now + 6,
        )
        .await
        .unwrap();
    assert_eq!(
        reasoning_session.model.as_ref().unwrap().reasoning_effort,
        None
    );
    assert_eq!(
        cloud
            .get_session(&tenant.tenant_id, &reasoning_session.session_id)
            .await
            .unwrap()
            .model
            .unwrap()
            .reasoning_effort,
        None
    );
    let model = model_ledger::configure(
        &control,
        &alice,
        &tenant.tenant_id,
        "cloud-test-model",
        None,
        now + 3,
    )
    .await;
    let policy = WorkerPolicy {
        catalog_revision: "cloud-test-catalog".to_owned(),
        policy_revision: "cloud-test-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 8,
        },
        max_run_attempts: 2,
        max_tenant_workspace_bytes: 1024 * 1024 * 1024,
        max_tenant_workspace_entries: 100_000,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::from([ternilo_cloud::BROKERED_MODEL_KIND.to_owned()]),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::default(),
    };
    let catalog = model_ledger::catalog("cloud-test-catalog");

    let draft = |run_id: &str| CloudRunDraft {
        project_id: project.project_id.clone(),
        workspace_id: workspace.workspace_id.clone(),
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("shared-session"),
        run_id: Some(RunId::new(run_id)),
        limits: RunLimits {
            max_steps: 2,
            max_tool_calls: 4,
        },
        permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
        mode: ternilo_protocol::SessionMode::Execute,
        profile: model_ledger::profile(&model),
        input: "hello cloud".to_owned(),
        references: Vec::new(),
        reference_contexts: Vec::new(),
        attachments: Vec::new(),
        reserved_model_tokens: 100,
    };

    let first = policy
        .compile_run(
            draft("run-1"),
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-1"),
            100,
            Duration::from_hours(1),
            now + 3,
        )
        .await
        .unwrap();
    let submitted = cloud
        .submit_run(&first, &reservation.reservation_id, now + 4)
        .await
        .unwrap();
    assert_eq!(submitted.state, CloudRunState::Queued);
    assert!(
        cloud
            .get_run(&other_tenant.tenant_id, &RunId::new("run-1"))
            .await
            .is_err()
    );

    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 1 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();
    assert!(
        worker
            .claim_run("worker-a", Duration::from_secs(10), now + 5)
            .await
            .unwrap()
            .is_none(),
        "the operator's maintenance barrier must prevent dispatch"
    );
    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 0 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();
    let claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, RunId::new("run-1"));
    let started = worker
        .start_run(claim, "worker-a", Duration::from_secs(10), now + 6)
        .await
        .unwrap()
        .unwrap();
    assert!(started.prior_events.is_empty());
    let attachment_content = b"cloud attachment survives refresh";
    let mut attachment_digest = String::new();
    for byte in Sha256::digest(attachment_content) {
        write!(attachment_digest, "{byte:02x}").unwrap();
    }
    let attachment = Attachment {
        name: "result.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content: format!("{ATTACHMENT_REFERENCE_PREFIX}{attachment_digest}"),
    };
    worker
        .store_attachment_object(
            &started,
            "worker-a",
            &attachment,
            attachment_content,
            now + 7,
        )
        .await
        .unwrap();
    worker
        .store_attachment_object(
            &started,
            "worker-a",
            &attachment,
            attachment_content,
            now + 7,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .resolve_attachment(
                &tenant.tenant_id,
                &workspace.workspace_id,
                attachment.clone(),
            )
            .await
            .unwrap()
            .content,
        "cloud attachment survives refresh"
    );
    let mut stale_attachment_run = started.clone();
    stale_attachment_run.fencing_token += 1;
    assert!(
        worker
            .store_attachment_object(
                &stale_attachment_run,
                "worker-a",
                &attachment,
                attachment_content,
                now + 7,
            )
            .await
            .is_err()
    );
    let started_event = SessionEvent {
        seq: 0,
        occurred_at_ms: now + 7,
        run_id: RunId::new("run-1"),
        kind: SessionEventKind::TurnStarted,
    };
    worker
        .append_event(&started, "worker-a", &started_event, now + 7)
        .await
        .unwrap();
    worker
        .append_event(&started, "worker-a", &started_event, now + 7)
        .await
        .unwrap();
    let first_question = UserQuestion {
        id: "question-order-0".to_owned(),
        question: "First canonical question".to_owned(),
        detail: None,
        header: None,
        options: vec![UserQuestionOption {
            label: "First answer".to_owned(),
            description: None,
        }],
        multi_select: false,
        presentation: None,
        tool_approval: None,
    };
    let second_question = UserQuestion {
        id: "question-order-1".to_owned(),
        question: "Second canonical question".to_owned(),
        detail: None,
        header: None,
        options: vec![UserQuestionOption {
            label: "Second answer".to_owned(),
            description: None,
        }],
        multi_select: true,
        presentation: None,
        tool_approval: None,
    };
    for (seq, question) in [(1, &first_question), (2, &second_question)] {
        worker
            .append_event(
                &started,
                "worker-a",
                &SessionEvent {
                    seq,
                    occurred_at_ms: now + 7 + seq,
                    run_id: RunId::new("run-1"),
                    kind: SessionEventKind::UserQuestionAsked {
                        question: question.clone(),
                    },
                },
                now + 7 + seq,
            )
            .await
            .unwrap();
    }
    worker
        .record_question(&started, "worker-a", &second_question, now + 8)
        .await
        .unwrap();
    worker
        .record_question(&started, "worker-a", &first_question, now + 9)
        .await
        .unwrap();
    assert_eq!(
        cloud
            .pending_questions(
                &tenant.tenant_id,
                &alice.user_id,
                &SessionId::new("shared-session"),
            )
            .await
            .unwrap()
            .into_iter()
            .map(|pending| pending.question.id)
            .collect::<Vec<_>>(),
        ["question-order-0", "question-order-1"],
    );
    let mut stale = started.clone();
    stale.fencing_token += 1;
    let stale_event = SessionEvent {
        seq: 3,
        occurred_at_ms: now + 8,
        run_id: RunId::new("run-1"),
        kind: SessionEventKind::TurnCancelled,
    };
    assert!(
        worker
            .append_event(&stale, "worker-a", &stale_event, now + 8)
            .await
            .is_err()
    );
    worker
        .renew_run(&started, "worker-a", Duration::from_secs(10), now + 8)
        .await
        .unwrap();
    let cancelled_request = model_ledger::accept(
        &control,
        &cloud,
        &started,
        "worker-a",
        "cancelled-call",
        100,
        now + 8,
    )
    .await
    .unwrap();
    assert_eq!(
        cloud
            .cancel_run(&tenant.tenant_id, &RunId::new("run-1"), now + 9)
            .await
            .unwrap(),
        CloudRunState::CancelRequested
    );
    let cancel_hello = ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new("worker-a"),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: "storage-contract-worker-a".to_owned(),
        catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
        capabilities: BTreeSet::from([
            ExecutorCapability::CloudRun,
            ExecutorCapability::AddressedSessionCommands,
            ExecutorCapability::RunCancellation,
        ]),
    };
    let cancel_worker = worker
        .register_cloud_worker(&cancel_hello, Duration::from_secs(60), now + 9)
        .await
        .unwrap();
    let first_cancel_claim = worker
        .claim_session_commands(
            &cancel_worker,
            &cancel_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 9,
        )
        .await
        .unwrap()
        .pop()
        .expect("running cancel writes a target-run wake command");
    assert!(matches!(
        &first_cancel_claim.command.body,
        ExecutorCommandBody::CancelRun { run_id, .. } if run_id == &RunId::new("run-1")
    ));
    worker
        .defer_session_command(&cancel_worker, &first_cancel_claim, now + 9)
        .await
        .unwrap();
    let second_cancel_claim = worker
        .claim_session_commands(
            &cancel_worker,
            &cancel_hello.capabilities,
            Duration::from_secs(5),
            1,
            now + 10,
        )
        .await
        .unwrap()
        .pop()
        .expect("deferred cancel wake command is claimable again");
    assert_eq!(second_cancel_claim.attempt_count, 2);
    let cancel_reply = CommandReply::success(
        second_cancel_claim.command.command_id.clone(),
        now + 10,
        json!({ "cancelled": true }),
    );
    worker
        .complete_session_command(
            &cancel_worker,
            &second_cancel_claim,
            &cancel_reply,
            now + 10,
        )
        .await
        .unwrap();
    assert!(worker.cancel_requested(&started, "worker-a").await.unwrap());
    worker
        .append_event(&started, "worker-a", &stale_event, now + 10)
        .await
        .unwrap();
    let cancelled_usage = ModelUsage {
        input_tokens: 5,
        output_tokens: 2,
        cached_input_tokens: 1,
        cache_write_tokens: None,
        reasoning_tokens: 0,
    };
    assert_eq!(
        worker.model_budget(&started, "worker-a").await.unwrap(),
        (100, 0)
    );
    model_ledger::settle(
        &control,
        &cancelled_request.request.request_id,
        cancelled_usage,
        Some("provider-cancelled-1"),
        now + 10,
    )
    .await
    .unwrap();
    assert_eq!(
        worker.model_budget(&started, "worker-a").await.unwrap(),
        (100, 7)
    );
    assert!(
        model_ledger::settle(
            &control,
            &cancelled_request.request.request_id,
            ModelUsage {
                input_tokens: 6,
                output_tokens: 2,
                cached_input_tokens: 1,
                cache_write_tokens: None,
                reasoning_tokens: 0,
            },
            Some("provider-cancelled-1"),
            now + 10,
        )
        .await
        .is_err()
    );
    model_ledger::settle(
        &control,
        &cancelled_request.request.request_id,
        cancelled_usage,
        Some("provider-cancelled-1"),
        now + 10,
    )
    .await
    .unwrap();
    worker
        .finish_run(
            &started,
            "worker-a",
            TerminalState::Cancelled,
            None,
            None,
            now + 11,
        )
        .await
        .unwrap();
    let worker_identity = worker
        .cloud_worker(&ternilo_transport::ExecutorId::new("worker-a"))
        .await
        .unwrap()
        .unwrap()
        .identity;
    worker
        .release_resident(
            &(&started).into(),
            &worker_identity,
            worker_identity.generation,
            now + 11,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .get_run(&tenant.tenant_id, &RunId::new("run-1"))
            .await
            .unwrap()
            .state,
        CloudRunState::Cancelled
    );
    assert_eq!(
        cloud
            .session_events(
                &tenant.tenant_id,
                &SessionId::new("shared-session"),
                None,
                100,
            )
            .await
            .unwrap()
            .len(),
        4
    );
    let accounting = sqlx::query(
        "SELECT reservation.state, reservation.committed_model_tokens,
                usage.used_model_tokens
         FROM control_quota_reservations AS reservation
         JOIN control_quota_usage AS usage USING (tenant_id)
         WHERE reservation.tenant_id = $1 AND reservation.reservation_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(&reservation.reservation_id)
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(accounting.get::<String, _>("state"), "committed");
    assert_eq!(accounting.get::<i64, _>("committed_model_tokens"), 7);
    assert_eq!(accounting.get::<i64, _>("used_model_tokens"), 7);

    let second = policy
        .compile_run(
            draft("run-2"),
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let second_reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-2"),
            100,
            Duration::from_hours(1),
            now + 12,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&second, &second_reservation.reservation_id, now + 13)
        .await
        .unwrap();
    let third = policy
        .compile_run(
            draft("run-3"),
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let third_reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-3"),
            100,
            Duration::from_hours(1),
            now + 14,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&third, &third_reservation.reservation_id, now + 15)
        .await
        .unwrap();

    let second_claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 16)
        .await
        .unwrap()
        .unwrap();
    let second_started = worker
        .start_run(second_claim, "worker-a", Duration::from_secs(10), now + 17)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_started.prior_events.len(), 4);
    assert!(
        worker
            .claim_run("worker-b", Duration::from_secs(10), now + 18)
            .await
            .unwrap()
            .is_none(),
        "a later occurrence from the same session is not claimable before its FIFO head finishes",
    );

    let run_two_started = SessionEvent {
        seq: 4,
        occurred_at_ms: now + 21,
        run_id: RunId::new("run-2"),
        kind: SessionEventKind::TurnStarted,
    };
    let run_two_finished = SessionEvent {
        seq: 5,
        occurred_at_ms: now + 22,
        run_id: RunId::new("run-2"),
        kind: SessionEventKind::TurnFinished {
            answer: "done".to_owned(),
            finish_reason: ternilo_protocol::TurnFinishReason::Completed,
        },
    };
    worker
        .append_event(&second_started, "worker-a", &run_two_started, now + 21)
        .await
        .unwrap();
    worker
        .append_event(&second_started, "worker-a", &run_two_finished, now + 22)
        .await
        .unwrap();
    let outcome = RunOutcome {
        answer: "done".to_owned(),
        steps: 1,
        tool_calls: 0,
        events: vec![run_two_started, run_two_finished],
        generated_title: None,
    };
    let second_request = model_ledger::accept(
        &control,
        &cloud,
        &second_started,
        "worker-a",
        "success-call",
        100,
        now + 22,
    )
    .await
    .unwrap();
    model_ledger::settle(
        &control,
        &second_request.request.request_id,
        ModelUsage {
            input_tokens: 8,
            output_tokens: 2,
            cached_input_tokens: 0,
            cache_write_tokens: None,
            reasoning_tokens: 0,
        },
        Some("provider-success-1"),
        now + 22,
    )
    .await
    .unwrap();
    worker
        .finish_run(
            &second_started,
            "worker-a",
            TerminalState::Succeeded,
            Some(&outcome),
            None,
            now + 23,
        )
        .await
        .unwrap();
    let worker_identity = worker
        .cloud_worker(&ternilo_transport::ExecutorId::new("worker-a"))
        .await
        .unwrap()
        .unwrap()
        .identity;
    worker
        .release_resident(
            &(&second_started).into(),
            &worker_identity,
            worker_identity.generation,
            now + 23,
        )
        .await
        .unwrap();
    let completed = cloud
        .get_run(&tenant.tenant_id, &RunId::new("run-2"))
        .await
        .unwrap();
    assert_eq!(completed.state, CloudRunState::Succeeded);
    assert_eq!(completed.outcome.unwrap().answer, "done");
    assert_eq!(
        cloud
            .cancel_run(&tenant.tenant_id, &RunId::new("run-3"), now + 24)
            .await
            .unwrap(),
        CloudRunState::Cancelled
    );

    let one_attempt_policy = WorkerPolicy {
        max_run_attempts: 1,
        ..policy.clone()
    };
    let fourth = one_attempt_policy
        .compile_run(
            draft("run-4"),
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let fourth_reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-4"),
            100,
            Duration::from_hours(1),
            now + 25,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&fourth, &fourth_reservation.reservation_id, now + 26)
        .await
        .unwrap();
    worker
        .claim_run("worker-a", Duration::from_secs(5), now + 27)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(worker.reap_expired(now + 32_001).await.unwrap(), 1);
    assert_eq!(
        cloud
            .get_run(&tenant.tenant_id, &RunId::new("run-4"))
            .await
            .unwrap()
            .state,
        CloudRunState::Failed
    );

    let fifth = policy
        .compile_run(
            draft("run-5"),
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let fifth_reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-5"),
            100,
            Duration::from_hours(1),
            now + 33_000,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&fifth, &fifth_reservation.reservation_id, now + 33_001)
        .await
        .unwrap();
    let fifth_claim = worker
        .claim_run("worker-a", Duration::from_secs(5), now + 33_002)
        .await
        .unwrap()
        .unwrap();
    let fifth_started = worker
        .start_run(
            fifth_claim,
            "worker-a",
            Duration::from_secs(5),
            now + 33_003,
        )
        .await
        .unwrap()
        .unwrap();
    let fifth_request = model_ledger::accept(
        &control,
        &cloud,
        &fifth_started,
        "worker-a",
        "crashed-call",
        100,
        now + 33_004,
    )
    .await
    .unwrap();
    assert_eq!(worker.reap_expired(now + 38_004).await.unwrap(), 1);
    assert_eq!(
        cloud
            .get_run(&tenant.tenant_id, &RunId::new("run-5"))
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate
    );
    // The upstream usage arrives after the expired Run has lost its writer lease.
    model_ledger::settle(
        &control,
        &fifth_request.request.request_id,
        ModelUsage {
            input_tokens: 3,
            output_tokens: 1,
            cached_input_tokens: 0,
            cache_write_tokens: None,
            reasoning_tokens: 0,
        },
        Some("provider-crashed-1"),
        now + 38_005,
    )
    .await
    .unwrap();
    let reaped_accounting = sqlx::query(
        "SELECT reservation.state, reservation.committed_model_tokens,
                usage.used_model_tokens
         FROM control_quota_reservations AS reservation
         JOIN control_quota_usage AS usage USING (tenant_id)
         WHERE reservation.tenant_id = $1 AND reservation.reservation_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(&fifth_reservation.reservation_id)
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(reaped_accounting.get::<String, _>("state"), "committed");
    assert_eq!(reaped_accounting.get::<i64, _>("committed_model_tokens"), 4);
    assert_eq!(reaped_accounting.get::<i64, _>("used_model_tokens"), 21);
    let late_event = SessionEvent {
        seq: 6,
        occurred_at_ms: now + 38_005,
        run_id: RunId::new("run-5"),
        kind: SessionEventKind::TurnStarted,
    };
    assert!(
        worker
            .append_event(&fifth_started, "worker-a", &late_event, now + 38_005)
            .await
            .is_err()
    );
    worker
        .release_resident(
            &(&fifth_started).into(),
            &worker_identity,
            worker_identity.generation,
            now + 38_006,
        )
        .await
        .unwrap();

    let signing_key = SigningKey::from_bytes(&[19; 32]);
    let publisher = PublisherTrust {
        key_id: "cloud-fixture-key".to_owned(),
        public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
        allowed_sources: ["https://plugins.ternilo.dev".to_owned()]
            .into_iter()
            .collect(),
    };
    control
        .trust_extension_publisher(&alice, &tenant.tenant_id, publisher, now + 40_000)
        .await
        .unwrap();
    let manifest = ExtensionManifest {
        schema_version: EXTENSION_PACKAGE_SCHEMA_VERSION,
        package_id: "dev.ternilo.cloud-fixture".to_owned(),
        version: "1.0.0".to_owned(),
        description: Some("Cloud Rhai distribution fixture".to_owned()),
        source: "https://plugins.ternilo.dev".to_owned(),
        publisher_key_id: "cloud-fixture-key".to_owned(),
        payload_sha256: "0".repeat(64),
        runtime: ExtensionRuntime::Rhai {
            limits: RhaiExecutionLimits::default(),
        },
        config_schema: json!({
            "type": "object",
            "properties": {
                "prefix": { "type": "string", "minLength": 1 },
                "offset": { "type": "number", "const": -0.0 }
            },
            "required": ["prefix", "offset"],
            "additionalProperties": false
        }),
        contributions: ExtensionContributions {
            tools: vec![ExtensionToolContribution {
                handler: "cloud_fixture".to_owned(),
                spec: ToolSpec {
                    name: "cloud_fixture".to_owned(),
                    description: "cloud distribution fixture".to_owned(),
                    input_schema: json!({ "type": "object", "additionalProperties": true }),
                },
                output_schema: json!({ "type": "object", "additionalProperties": true }),
                effect: ExtensionToolEffect::ReadOnly,
                presentation: None,
            }],
            prompt_sections: vec![ExtensionPromptSectionContribution {
                id: "cloud-extension-guidance".to_owned(),
                order: 450,
                content: "The cloud fixture contributes cloud_fixture.".to_owned(),
            }],
            skills: vec![ExtensionSkillContribution {
                name: "cloud-extension-tools".to_owned(),
                description: "Use the signed Cloud Extension fixture.".to_owned(),
                when_to_use: Some("When verifying Cloud Extension distribution.".to_owned()),
                invocation: SkillInvocationPolicy::default(),
                content: "# Cloud Extension fixture\n\nUse `cloud_fixture` for the distribution acceptance flow."
                    .to_owned(),
            }],
            hooks: Vec::new(),
            commands: Vec::new(),
            providers: Vec::new(),
        },
        requested_capabilities: BTreeSet::new(),
    };
    let install = ExtensionInstallRequest {
        bundle: ternilo_extension::sign_bundle(
            manifest,
            ExtensionPayload::Utf8(FIXTURE_RHAI.to_owned()),
            &signing_key,
        )
        .unwrap(),
        granted_capabilities: BTreeSet::new(),
    };
    let (first_install, concurrent_install) = tokio::join!(
        control.install_extension(
            &alice,
            &tenant.tenant_id,
            install.clone(),
            &policy.extension_host_policy,
            now + 40_001,
        ),
        control.install_extension(
            &alice,
            &tenant.tenant_id,
            install.clone(),
            &policy.extension_host_policy,
            now + 40_001,
        )
    );
    assert_eq!(first_install.unwrap(), concurrent_install.unwrap());
    let inventory = control
        .extension_inventory(&alice, &tenant.tenant_id)
        .await
        .unwrap();
    assert_eq!(inventory.publishers.len(), 1);
    assert_eq!(inventory.extensions.len(), 1);
    assert_eq!(inventory.extensions[0].manifest, install.bundle.manifest);
    let persisted_install =
        sqlx::query_scalar::<_, ternilo_storage::Json<ternilo_extension::ExtensionInstallRequest>>(
            "SELECT install_request FROM control_extension_packages
         WHERE tenant_id = $1 AND package_id = $2 AND version = $3",
        )
        .bind(tenant.tenant_id.as_str())
        .bind("dev.ternilo.cloud-fixture")
        .bind("1.0.0")
        .fetch_one(&audit)
        .await
        .unwrap()
        .0;
    assert_eq!(persisted_install, install);
    let persisted_manifest_zero =
        &persisted_install.bundle.manifest.config_schema["properties"]["offset"]["const"];
    assert_eq!(
        persisted_manifest_zero.to_string(),
        "-0.0",
        "JSON text preserves the source numeric representation"
    );
    let successful_install_audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_audit_log
         WHERE tenant_id = $1 AND action = 'extension.package.install'
           AND resource_id = $2 AND outcome = 'success'",
    )
    .bind(tenant.tenant_id.as_str())
    .bind("dev.ternilo.cloud-fixture@1.0.0")
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(successful_install_audits, 1);

    let extension_profile = Profile {
        plugins: vec![PluginEntry {
            id: "cloud-extension-fixture".to_owned(),
            kind: EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.ternilo.cloud-fixture",
                "version": "1.0.0",
                "settings": {
                    "prefix": "cloud",
                    "offset": -0.0
                }
            }),
        }],
    };
    let mut extension_policy = policy.clone();
    extension_policy.max_extension_packages_per_run = 1;
    extension_policy
        .allowed_plugin_kinds
        .insert(EXTENSION_PACKAGE_KIND.to_owned());
    let mounted_session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(SessionId::new("extension-mounted-session")),
                agent_id: AgentId::new("agent"),
                title: "Rhai extension mounted session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "extension-lifecycle".to_owned(),
                profile_plugins: extension_profile.plugins.clone(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &alice.user_id,
            now + 40_001,
        )
        .await
        .unwrap();
    let persisted_plugins: ternilo_storage::Json<serde_json::Value> = sqlx::query_scalar(
        "SELECT profile_plugins FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(mounted_session.session_id.as_str())
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(
        persisted_plugins.0[0]["config"]["settings"]["offset"].to_string(),
        "-0.0",
        "session configuration preserves source JSON numbers"
    );
    control
        .copy_user_agent_preset(
            &alice,
            &tenant.tenant_id,
            AgentPresetCopyRequest {
                from: "standard".to_owned(),
                id: "extension-lifecycle".to_owned(),
                display_name: Some("Rhai extension lifecycle".to_owned()),
            },
            now + 40_001,
        )
        .await
        .unwrap();
    control
        .update_user_agent_preset(
            &alice,
            &tenant.tenant_id,
            "extension-lifecycle",
            AgentPresetUpdateRequest {
                display_name: "Rhai extension lifecycle".to_owned(),
                description: String::new(),
                profile: extension_profile.clone(),
            },
            now + 40_001,
        )
        .await
        .unwrap();
    assert_eq!(
        control
            .resolve_extensions(
                &alice,
                &tenant.tenant_id,
                &extension_profile,
                &extension_policy.extension_host_policy,
            )
            .await
            .unwrap()
            .len(),
        1
    );
    let invalid_settings_profile = Profile {
        plugins: vec![PluginEntry {
            id: "cloud-extension-invalid-settings".to_owned(),
            kind: EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.ternilo.cloud-fixture",
                "version": "1.0.0",
                "settings": {
                    "prefix": 7,
                    "offset": -0.0
                }
            }),
        }],
    };
    assert!(
        control
            .resolve_extensions(
                &alice,
                &tenant.tenant_id,
                &invalid_settings_profile,
                &extension_policy.extension_host_policy,
            )
            .await
            .is_err()
    );
    let extension_run = extension_policy
        .compile_run(
            CloudRunDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new("extension-session"),
                run_id: Some(RunId::new("run-extension")),
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 4,
                },
                permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
                mode: ternilo_protocol::SessionMode::Execute,
                profile: extension_profile,
                input: "/cloud_fixture {}".to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 100,
            },
            tenant.tenant_id.clone(),
            alice.user_id.clone(),
            alice.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let extension_reservation = control
        .reserve_quota(
            &alice,
            &tenant.tenant_id,
            Some("run-extension"),
            100,
            Duration::from_hours(1),
            now + 40_002,
        )
        .await
        .unwrap();
    cloud
        .submit_run(
            &extension_run,
            &extension_reservation.reservation_id,
            now + 40_003,
        )
        .await
        .unwrap();
    let extension_claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 40_004)
        .await
        .unwrap()
        .unwrap();
    let extension_started = worker
        .start_run(
            extension_claim,
            "worker-a",
            Duration::from_secs(10),
            now + 40_005,
        )
        .await
        .unwrap()
        .unwrap();
    let distribution = worker
        .extensions_for_run(
            &extension_started,
            "worker-a",
            &extension_policy.extension_host_policy,
            now + 40_006,
        )
        .await
        .unwrap();
    assert_eq!(distribution.len(), 1);
    assert_eq!(distribution[0].install, install);
    assert_eq!(
        distribution[0]
            .install
            .bundle
            .manifest
            .contributions
            .prompt_sections[0]
            .id,
        "cloud-extension-guidance"
    );
    assert_eq!(
        distribution[0].install.bundle.manifest.contributions.skills[0].name,
        "cloud-extension-tools"
    );
    assert!(
        worker
            .extensions_active(&extension_started, "worker-a", now + 40_006)
            .await
            .unwrap()
    );
    control
        .set_extension_enabled(
            &alice,
            &tenant.tenant_id,
            "dev.ternilo.cloud-fixture",
            "1.0.0",
            false,
            now + 40_007,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .get_session(&tenant.tenant_id, &mounted_session.session_id)
            .await
            .unwrap()
            .profile_plugins
            .is_empty()
    );
    assert!(
        control
            .user_agent_preset(&alice, &tenant.tenant_id, "extension-lifecycle")
            .await
            .unwrap()
            .profile
            .plugins
            .is_empty()
    );
    assert!(
        !worker
            .extensions_active(&extension_started, "worker-a", now + 40_008)
            .await
            .unwrap()
    );
    let revoked_event = SessionEvent {
        seq: 0,
        occurred_at_ms: now + 40_009,
        run_id: RunId::new("run-extension"),
        kind: SessionEventKind::TurnFailed {
            message: "plugin disabled".to_owned(),
        },
    };
    worker
        .append_event(&extension_started, "worker-a", &revoked_event, now + 40_009)
        .await
        .unwrap();
    worker
        .finish_run(
            &extension_started,
            "worker-a",
            TerminalState::Failed,
            None,
            Some(&HarnessError::policy("plugin disabled")),
            now + 40_010,
        )
        .await
        .unwrap();
    let released = sqlx::query(
        "SELECT state, committed_model_tokens
         FROM control_quota_reservations
         WHERE tenant_id = $1 AND reservation_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(&extension_reservation.reservation_id)
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(released.get::<String, _>("state"), "released");
    assert!(
        released
            .get::<Option<i64>, _>("committed_model_tokens")
            .is_none()
    );
    control
        .uninstall_extension(
            &alice,
            &tenant.tenant_id,
            "dev.ternilo.cloud-fixture",
            "1.0.0",
            now + 40_011,
        )
        .await
        .unwrap();
    assert!(
        control
            .extension_inventory(&alice, &tenant.tenant_id)
            .await
            .unwrap()
            .extensions
            .is_empty()
    );
    let mut replacement_manifest = install.bundle.manifest.clone();
    replacement_manifest.description = Some("Different code under a reused version".to_owned());
    let replacement = ExtensionInstallRequest {
        bundle: ternilo_extension::sign_bundle(
            replacement_manifest,
            ExtensionPayload::Utf8(FIXTURE_RHAI.to_owned()),
            &signing_key,
        )
        .unwrap(),
        granted_capabilities: install.granted_capabilities.clone(),
    };
    assert!(
        control
            .install_extension(
                &alice,
                &tenant.tenant_id,
                replacement,
                &policy.extension_host_policy,
                now + 40_012,
            )
            .await
            .is_err()
    );
    control
        .install_extension(
            &alice,
            &tenant.tenant_id,
            install,
            &policy.extension_host_policy,
            now + 40_012,
        )
        .await
        .unwrap();
    control
        .revoke_extension(
            &alice,
            &tenant.tenant_id,
            "dev.ternilo.cloud-fixture",
            "1.0.0",
            now + 40_013,
        )
        .await
        .unwrap();
    assert!(
        control
            .set_extension_enabled(
                &alice,
                &tenant.tenant_id,
                "dev.ternilo.cloud-fixture",
                "1.0.0",
                true,
                now + 40_014,
            )
            .await
            .is_err()
    );
    assert!(
        control
            .uninstall_extension(
                &alice,
                &tenant.tenant_id,
                "dev.ternilo.cloud-fixture",
                "1.0.0",
                now + 40_015,
            )
            .await
            .is_err()
    );
    control
        .revoke_extension_publisher(&alice, &tenant.tenant_id, "cloud-fixture-key", now + 40_016)
        .await
        .unwrap();
    let revoked_inventory = control
        .extension_inventory(&alice, &tenant.tenant_id)
        .await
        .unwrap();
    assert!(revoked_inventory.publishers[0].revoked);
    assert!(revoked_inventory.extensions[0].revoked);

    let all_runs = cloud.list_runs(&tenant.tenant_id, 100).await.unwrap();
    assert_eq!(all_runs.len(), 6);
    assert!(
        cloud
            .list_runs(&other_tenant.tenant_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
}
