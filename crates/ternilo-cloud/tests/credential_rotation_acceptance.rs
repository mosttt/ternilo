use std::{path::PathBuf, time::Duration};

use sqlx::Row as _;
use ternilo_cloud::{CloudRunDraft, CloudRunState, CloudStore, WorkerPolicy};
use ternilo_control::{ControlStore, ControlUser, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{AgentId, PermissionPreset, RunId, RunLimits, SessionId, TenantId};

const FIXTURE_ISSUER: &str = "https://credential-rotation.test.invalid";
const FIXTURE_SUBJECT: &str = "credential-rotation-operator";
const FIXTURE_SLUG: &str = "credential-rotation";
const FIXTURE_CREDENTIAL: &str = "ROTATION_ACCEPTANCE_SECRET";
const FIXTURE_SECRET: &str = "credential-rotation-secret-value";

#[tokio::test]
#[ignore = "requires a disposable production-role Cloud stack"]
async fn seed_rotation_fixture_secret() {
    let store = control_store().await;
    let (actor, tenant_id) = fixture_owner(&store).await;
    store
        .put_user_credential(
            &actor,
            &tenant_id,
            FIXTURE_CREDENTIAL,
            FIXTURE_SECRET,
            unix_time_ms(),
        )
        .await
        .expect("seed encrypted rotation credential");
}

#[tokio::test]
#[ignore = "requires a disposable production-role Cloud stack"]
async fn rotation_fixture_secret_decrypts_with_configured_key() {
    let store = control_store().await;
    let (actor, tenant_id) = fixture_owner(&store).await;
    let value = store
        .resolve_user_credential(&actor, &tenant_id, FIXTURE_CREDENTIAL)
        .await
        .expect("decrypt rotation credential")
        .expect("rotation credential exists");
    assert_eq!(value.as_str(), FIXTURE_SECRET);
}

#[tokio::test]
#[ignore = "requires a disposable production-role Cloud stack"]
async fn rotation_fixture_secret_rejects_configured_key() {
    let store = control_store().await;
    let (actor, tenant_id) = fixture_owner(&store).await;
    assert!(
        store
            .resolve_user_credential(&actor, &tenant_id, FIXTURE_CREDENTIAL)
            .await
            .is_err(),
        "the configured stale master key unexpectedly decrypted rotated ciphertext"
    );
}

#[tokio::test]
#[ignore = "requires a disposable production-role Cloud stack and running Worker"]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the credential rotation canary and its authoritative usage checks together."
)]
async fn rotated_stack_executes_authoritative_model_canary() {
    let runtime_url = required_env("TERNILO_ROTATION_RUNTIME_DATABASE_URL");
    let migration_url = required_env("TERNILO_ROTATION_MIGRATION_DATABASE_URL");
    let policy_path = PathBuf::from(required_env("TERNILO_ROTATION_WORKER_POLICY"));
    let policy: WorkerPolicy =
        serde_json::from_slice(&std::fs::read(&policy_path).expect("read rotation WorkerPolicy"))
            .expect("parse rotation WorkerPolicy");
    let store = control_store().await;
    let (actor, tenant_id) = fixture_owner(&store).await;
    let now = unix_time_ms();
    let project = store
        .create_project(&actor, &tenant_id, &format!("Rotation canary {now}"), now)
        .await
        .expect("create rotation canary project");
    let workspace = store
        .create_cloud_workspace(
            &actor,
            &tenant_id,
            &project.project_id,
            "Rotation canary",
            now + 1,
        )
        .await
        .expect("create rotation canary Workspace");
    let cloud = CloudStore::connect_without_migrations(&runtime_url, 4)
        .await
        .expect("connect rotation Cloud store");
    let catalog = ternilo_cloud::catalog().expect("build Cloud catalog");
    let run_id = RunId::new(format!("rotation-canary-{now}"));
    let binding = ternilo_protocol::RunModelBinding::Platform {
        grant_id: required_env("TERNILO_ROTATION_MODEL_GRANT_ID"),
        model_id: required_env("TERNILO_ROTATION_PUBLIC_MODEL_ID"),
        beneficiary_user_id: actor.user_id.clone(),
    };
    let snapshot = store
        .resolve_workload_model_snapshot(
            &actor.user_id,
            &actor.user_id,
            &tenant_id,
            &binding,
            None,
            now,
        )
        .await
        .expect("resolve the canary's explicit platform model authorization");
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id: project.project_id,
                workspace_id: workspace.workspace_id,
                agent_id: AgentId::new("rotation-canary"),
                session_id: SessionId::new(format!("rotation-session-{now}")),
                run_id: Some(run_id.clone()),
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                },
                permissions: PermissionPreset::ReadOnly,
                mode: ternilo_protocol::SessionMode::Execute,
                profile: ternilo_cloud::cloud_profile(Some(&snapshot)),
                input: "Return the credential rotation canary response.".to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 25_000,
            },
            tenant_id.clone(),
            actor.user_id.clone(),
            actor.user_id.clone(),
            &catalog,
        )
        .expect("compile rotation model run");
    let reservation = store
        .reserve_quota(
            &actor,
            &tenant_id,
            Some(run_id.as_str()),
            compiled.reserved_model_tokens,
            Duration::from_mins(5),
            now + 2,
        )
        .await
        .expect("reserve rotation model quota");
    cloud
        .submit_run(&compiled, &reservation.reservation_id, now + 3)
        .await
        .expect("submit rotation model run");

    let completed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let run = cloud
                .get_run(&tenant_id, &run_id)
                .await
                .expect("read rotation model run");
            if run.state.terminal() {
                break run;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("rotation model canary did not finish");
    assert_eq!(
        completed.state,
        CloudRunState::Succeeded,
        "rotation model canary failed: {:?}",
        completed.error
    );

    let audit = ternilo_storage::Database::connect(&migration_url, 1)
        .await
        .expect("connect migration role for model usage audit");
    let mut audit_transaction = audit
        .owner_transaction(&tenant_id, &actor.user_id)
        .await
        .expect("scope model usage audit to the run owner");
    let usage = sqlx::query(
        "SELECT request.provider_id, request.upstream_model, attempt.input_tokens, attempt.output_tokens, attempt.cached_input_tokens
         FROM control_model_requests AS request JOIN control_model_attempts AS attempt ON attempt.request_id=request.request_id
         WHERE request.tenant_id = $1 AND request.run_id = $2 AND attempt.attempted=1",
    )
    .bind(tenant_id.as_str())
    .bind(run_id.as_str())
    .fetch_one(&mut *audit_transaction)
    .await
    .expect("read authoritative model usage");
    assert_eq!(
        usage.try_get::<String, _>("provider_id").unwrap(),
        "rotation"
    );
    assert_eq!(
        usage.try_get::<String, _>("upstream_model").unwrap(),
        "rotation-model"
    );
    assert_eq!(usage.try_get::<i64, _>("input_tokens").unwrap(), 12);
    assert_eq!(usage.try_get::<i64, _>("output_tokens").unwrap(), 4);
    assert_eq!(usage.try_get::<i64, _>("cached_input_tokens").unwrap(), 2);
}

async fn control_store() -> ControlStore {
    let runtime_url = required_env("TERNILO_ROTATION_RUNTIME_DATABASE_URL");
    let migration_url = required_env("TERNILO_ROTATION_MIGRATION_DATABASE_URL");
    let cipher = SecretCipher::from_base64(&required_env("TERNILO_ROTATION_SECRET_MASTER_KEY"))
        .expect("parse rotation master key");
    ControlStore::connect(&runtime_url, Some(&migration_url), cipher, 4)
        .await
        .expect("connect rotation Control store")
}

async fn fixture_owner(store: &ControlStore) -> (ControlUser, TenantId) {
    let now = unix_time_ms();
    let actor = store
        .upsert_user(
            &OidcPrincipal {
                issuer: FIXTURE_ISSUER.to_owned(),
                subject: FIXTURE_SUBJECT.to_owned(),
                email: None,
                display_name: Some("Credential rotation operator".to_owned()),
            },
            &format!("test-{FIXTURE_SUBJECT}"),
            now,
        )
        .await
        .expect("upsert rotation fixture owner");
    let existing = store
        .list_tenants(&actor)
        .await
        .expect("list rotation fixture tenants")
        .into_iter()
        .find(|tenant| tenant.slug == FIXTURE_SLUG);
    let tenant = match existing {
        Some(tenant) => tenant,
        None => store
            .create_tenant(
                &actor,
                FIXTURE_SLUG,
                "Credential rotation",
                TenantQuota {
                    max_nodes: 1,
                    max_concurrent_runs: 2,
                    monthly_model_tokens: 100_000,
                    max_secrets: 4,
                },
                now + 1,
            )
            .await
            .expect("create rotation fixture tenant"),
    };
    (actor, tenant.tenant_id)
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_millis()
        .try_into()
        .expect("timestamp exceeds u64")
}
