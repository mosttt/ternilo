use std::{path::PathBuf, time::Duration};

use sqlx::Row as _;
use ternilo_cloud::{CloudRunDraft, CloudRunState, CloudStore, WorkerPolicy};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{AgentId, PermissionPreset, RunId, RunLimits, SessionId};

#[tokio::test]
#[ignore = "requires a restored production-role stack and a running ternilo-worker"]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the complete restored-stack execution contract in one test."
)]
async fn restored_stack_accepts_and_executes_a_fresh_workspace_run() {
    let runtime_url = std::env::var("TERNILO_RESTORE_RUNTIME_DATABASE_URL")
        .expect("TERNILO_RESTORE_RUNTIME_DATABASE_URL must be set");
    let migration_url = std::env::var("TERNILO_RESTORE_MIGRATION_DATABASE_URL")
        .expect("TERNILO_RESTORE_MIGRATION_DATABASE_URL must be set");
    let policy_path = PathBuf::from(
        std::env::var("TERNILO_RESTORE_WORKER_POLICY")
            .expect("TERNILO_RESTORE_WORKER_POLICY must be set"),
    );
    let policy: WorkerPolicy =
        serde_json::from_slice(&std::fs::read(&policy_path).expect("read restored worker policy"))
            .expect("parse restored worker policy");
    let catalog = ternilo_cloud::catalog().expect("build cloud catalog");
    let control = ControlStore::connect(
        &runtime_url,
        Some(&migration_url),
        SecretCipher::from_key([11; 32]),
        4,
    )
    .await
    .expect("connect restored control store through production roles");
    let cloud = CloudStore::connect_without_migrations(&runtime_url, 4)
        .await
        .expect("connect restored cloud store through runtime role");
    let now = unix_time_ms();
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://restore.test.invalid".to_owned(),
                subject: format!("restore-acceptance-{now}"),
                email: None,
                display_name: Some("Restore acceptance".to_owned()),
            },
            &format!("test-restore-acceptance-{now}"),
            now,
        )
        .await
        .expect("create restore acceptance user");
    let tenant = control
        .create_tenant(
            &user,
            &format!("restore-{now}"),
            "Restore acceptance",
            TenantQuota {
                max_nodes: 1,
                max_concurrent_runs: 2,
                monthly_model_tokens: 10_000,
                max_secrets: 2,
            },
            now + 1,
        )
        .await
        .expect("create restore acceptance tenant");
    let project = control
        .create_project(&user, &tenant.tenant_id, "Restore project", now + 2)
        .await
        .expect("create restore acceptance project");
    let workspace = control
        .create_cloud_workspace(
            &user,
            &tenant.tenant_id,
            &project.project_id,
            "Restore workspace",
            now + 3,
        )
        .await
        .expect("create restore acceptance workspace");

    let mut profile = ternilo_cloud::cloud_profile(Some(&ternilo_protocol::RunModelSnapshot {
        binding: ternilo_protocol::RunModelBinding::Platform {
            grant_id: "test-grant".to_owned(),
            model_id: "test-model".to_owned(),
            beneficiary_user_id: user.user_id.clone(),
        },
        protocol: ternilo_protocol::ProviderProtocol::OpenAiChatCompletions,
        defaults: ternilo_protocol::ProviderModelDefaults {
            context_window: 128_000,
            max_output_tokens: 4_096,
            reasoning: None,
        },
        reasoning_effort: None,
        display_name: "Test model".to_owned(),
        source_name: "Test allowance".to_owned(),
    }));
    let model = profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "model")
        .expect("cloud profile model plugin");
    model.kind = "ternilo.model.rule".to_owned();
    model.config = serde_json::json!({ "prefix": "restore: " });
    let run_id = RunId::new(format!("restore-run-{now}"));
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                agent_id: AgentId::new("restore-agent"),
                session_id: SessionId::new(format!("restore-session-{now}")),
                run_id: Some(run_id.clone()),
                limits: RunLimits {
                    max_steps: 4,
                    max_tool_calls: 4,
                },
                permissions: PermissionPreset::WorkspaceWrite,
                mode: ternilo_protocol::SessionMode::Execute,
                profile,
                input: "/write restore-proof.txt restored-stack-run".to_owned(),
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
        .expect("compile restore acceptance run");
    let reservation = control
        .reserve_quota(
            &user,
            &tenant.tenant_id,
            Some(run_id.as_str()),
            compiled.reserved_model_tokens,
            Duration::from_mins(5),
            now + 4,
        )
        .await
        .expect("reserve restore acceptance quota");
    cloud
        .submit_run(&compiled, &reservation.reservation_id, now + 5)
        .await
        .expect("submit restore acceptance run");

    let completed = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let run = cloud
                .get_run(&tenant.tenant_id, &run_id)
                .await
                .expect("read restore acceptance run");
            if run.state.terminal() {
                break run;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("restored worker did not finish the acceptance run");
    assert_eq!(
        completed.state,
        CloudRunState::Succeeded,
        "restored worker failed the acceptance run: {:?}",
        completed.error
    );
    assert!(completed.outcome.is_some());
    let audit = ternilo_storage::Database::connect(&migration_url, 1)
        .await
        .expect("connect restored migration role for quota audit");
    let mut audit_transaction = audit
        .owner_transaction(&tenant.tenant_id, &user.user_id)
        .await
        .expect("scope restored quota audit to the run owner");
    let quota = sqlx::query(
        "SELECT state, committed_model_tokens
         FROM control_quota_reservations
         WHERE tenant_id = $1 AND reservation_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(&reservation.reservation_id)
    .fetch_one(&mut *audit_transaction)
    .await
    .expect("read restore acceptance quota reservation");
    assert_eq!(quota.try_get::<String, _>("state").unwrap(), "released");
    assert_eq!(
        quota
            .try_get::<Option<i64>, _>("committed_model_tokens")
            .unwrap(),
        None,
        "a run without broker usage must release rather than consume its reservation",
    );
    println!(
        "restore acceptance passed: tenant={} workspace={} run={}",
        tenant.tenant_id.as_str(),
        workspace.workspace_id.as_str(),
        run_id.as_str()
    );
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis()
        .try_into()
        .expect("timestamp exceeds u64")
}
