use std::{collections::BTreeSet, time::Duration};

use ternilo_cloud::{CloudRunDraft, CloudSessionDraft, CloudStore, TerminalState, WorkerPolicy};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    AgentId, HarnessError, PermissionPreset, Profile, RunId, RunLimits, RunOutcome, SessionEvent,
    SessionEventKind, SessionId, SessionMode, TurnFinishReason,
};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_approved_plan_review_is_the_only_terminal_path_that_enters_execute_mode() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test",
    );
    let runtime_url =
        server_runtime::initialize(&admin_url, "ternilo_plan_runtime_test", [29; 32]).await;
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([29; 32]),
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

    plan_mode_contract(control, cloud, worker).await;
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
}

#[tokio::test]
async fn sqlite_plan_review_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("plans.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([29; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    let worker = CloudStore::connect_without_migrations(&url, 2)
        .await
        .unwrap();
    plan_mode_contract(control, cloud, worker).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the original approved, rejected, absent, failed and cancelled review cases identical across backends."
)]
async fn plan_mode_contract(control: ControlStore, cloud: CloudStore, worker: CloudStore) {
    worker_storage::bind_workers(&cloud, &["plan-worker"], "shared-contract-storage").await;
    let now = 2_200_000_000_000_u64;
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://issuer.example".to_owned(),
                subject: "plan-mode-user".to_owned(),
                email: None,
                display_name: Some("Plan mode user".to_owned()),
            },
            "test-plan-mode-user",
            now,
        )
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &user,
            "plan-mode",
            "Plan mode",
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
        .create_project(&user, &tenant.tenant_id, "Plan mode", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &user,
            &tenant.tenant_id,
            &project.project_id,
            "Plan mode workspace",
            now + 3,
        )
        .await
        .unwrap();
    let policy = policy();
    let catalog = Catalog::new("plan-mode-catalog");
    let cases = [
        (
            "approved-success",
            TerminalState::Succeeded,
            Some((true, None)),
            SessionMode::Execute,
        ),
        (
            "feedback-success",
            TerminalState::Succeeded,
            Some((false, Some("revise the plan"))),
            SessionMode::Plan,
        ),
        (
            "no-review-success",
            TerminalState::Succeeded,
            None,
            SessionMode::Plan,
        ),
        (
            "approved-failed",
            TerminalState::Failed,
            Some((true, None)),
            SessionMode::Plan,
        ),
        (
            "approved-cancelled",
            TerminalState::Cancelled,
            Some((true, None)),
            SessionMode::Plan,
        ),
    ];

    for (index, (name, terminal, review, expected_mode)) in cases.into_iter().enumerate() {
        let case_now = now + 100 + u64::try_from(index).unwrap() * 100;
        let session_id = SessionId::new(format!("plan-{name}"));
        let run_id = RunId::new(format!("plan-{name}-run"));
        cloud
            .create_session(
                CloudSessionDraft {
                    project_id: project.project_id.clone(),
                    workspace_id: workspace.workspace_id.clone(),
                    session_id: Some(session_id.clone()),
                    agent_id: AgentId::new("agent"),
                    title: "Plan review".to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 100,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: Vec::new(),
                    mode: SessionMode::Plan,
                },
                &tenant.tenant_id,
                &user.user_id,
                case_now,
            )
            .await
            .unwrap();
        let compiled = policy
            .compile_run(
                CloudRunDraft {
                    project_id: project.project_id.clone(),
                    workspace_id: workspace.workspace_id.clone(),
                    agent_id: AgentId::new("agent"),
                    session_id: session_id.clone(),
                    run_id: Some(run_id.clone()),
                    limits: RunLimits {
                        max_steps: 2,
                        max_tool_calls: 2,
                    },
                    permissions: PermissionPreset::WorkspaceWrite,
                    mode: SessionMode::Plan,
                    profile: Profile::default(),
                    input: "review this plan".to_owned(),
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                    reserved_model_tokens: 100,
                },
                tenant.tenant_id.clone(),
                user.user_id.clone(),
                user.user_id.clone(),
                &catalog,
            )
            .unwrap();
        let reservation = control
            .reserve_quota(
                &user,
                &tenant.tenant_id,
                Some(run_id.as_str()),
                100,
                Duration::from_secs(60),
                case_now + 1,
            )
            .await
            .unwrap();
        cloud
            .submit_run(&compiled, &reservation.reservation_id, case_now + 2)
            .await
            .unwrap();
        let claim = worker
            .claim_run("plan-worker", Duration::from_secs(30), case_now + 3)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.run_id, run_id);
        let started = worker
            .start_run(claim, "plan-worker", Duration::from_secs(30), case_now + 4)
            .await
            .unwrap()
            .unwrap();
        let events = review_events(&run_id, terminal, review, case_now + 5);
        for event in &events {
            worker
                .append_event(&started, "plan-worker", event, event.occurred_at_ms)
                .await
                .unwrap();
        }
        let outcome = RunOutcome {
            answer: "review complete".to_owned(),
            steps: 1,
            tool_calls: 1,
            events,
            generated_title: None,
        };
        let terminal_error = (terminal == TerminalState::Failed)
            .then(|| HarnessError::execution("plan review run failed"));
        worker
            .finish_run(
                &started,
                "plan-worker",
                terminal,
                Some(&outcome),
                terminal_error.as_ref(),
                case_now + 20,
            )
            .await
            .unwrap();
        let identity = worker
            .cloud_worker(&ternilo_transport::ExecutorId::new("plan-worker"))
            .await
            .unwrap()
            .unwrap()
            .identity;
        worker
            .release_resident(
                &(&started).into(),
                &identity,
                identity.generation,
                case_now + 21,
            )
            .await
            .unwrap();

        let session = cloud
            .get_session(&tenant.tenant_id, &session_id)
            .await
            .unwrap();
        assert_eq!(session.mode, expected_mode, "case {name}");
    }
}

fn review_events(
    run_id: &RunId,
    terminal: TerminalState,
    review: Option<(bool, Option<&str>)>,
    now: u64,
) -> Vec<SessionEvent> {
    let mut events = vec![SessionEvent {
        seq: 0,
        occurred_at_ms: now,
        run_id: run_id.clone(),
        kind: SessionEventKind::TurnStarted,
    }];
    if let Some((approved, feedback)) = review {
        events.push(SessionEvent {
            seq: 1,
            occurred_at_ms: now + 1,
            run_id: run_id.clone(),
            kind: SessionEventKind::PlanReviewCompleted {
                plan: "typed plan".to_owned(),
                approved,
                feedback: feedback.map(str::to_owned),
            },
        });
    }
    let kind = match terminal {
        TerminalState::Succeeded => SessionEventKind::TurnFinished {
            answer: "review complete".to_owned(),
            finish_reason: TurnFinishReason::Completed,
        },
        TerminalState::Failed => SessionEventKind::TurnFailed {
            message: "plan review run failed".to_owned(),
        },
        TerminalState::Cancelled => SessionEventKind::TurnCancelled,
        TerminalState::Indeterminate => unreachable!("indeterminate is not a test case"),
    };
    events.push(SessionEvent {
        seq: u64::try_from(events.len()).unwrap(),
        occurred_at_ms: now + u64::try_from(events.len()).unwrap(),
        run_id: run_id.clone(),
        kind,
    });
    events
}

fn policy() -> WorkerPolicy {
    WorkerPolicy {
        catalog_revision: "plan-mode-catalog".to_owned(),
        policy_revision: "plan-mode-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 4,
        },
        max_run_attempts: 1,
        max_tenant_workspace_bytes: 1024 * 1024,
        max_tenant_workspace_entries: 100,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::new(),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::new(),
    }
}
