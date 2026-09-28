use std::{collections::BTreeSet, time::Duration};

use crate::{CloudSessionDraft, CloudSessionUpdate, CloudStore};
use sqlx::Executor;
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{
    AgentId, PermissionPreset, PluginEntry, RunId, SessionEvent, SessionEventKind, SessionId,
    SessionMode, SessionTelemetrySharingStatus,
};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind,
};

use ternilo_storage::{Database, Json};

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_telemetry_is_scoped_durable_feedback_gated_and_worker_fenced() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test"
    );
    reset_database_and_roles(&admin_url).await;
    let control_admin =
        ControlStore::connect(&admin_url, None, SecretCipher::from_key([51; 32]), 2)
            .await
            .unwrap();
    control_admin.health().await.unwrap();
    drop(control_admin);
    let cloud_admin = CloudStore::connect(&admin_url, None, 2).await.unwrap();
    cloud_admin.health().await.unwrap();
    drop(cloud_admin);
    create_restricted_roles(&admin_url).await;

    let runtime_url = database_url_for_role(
        &admin_url,
        "ternilo_telemetry_runtime_test",
        "telemetry-runtime-password",
    );
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([51; 32]),
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
    let audit = Database::connect(&admin_url, 2).await.unwrap();

    telemetry_contract(control, cloud, worker, &audit, &runtime_url).await;
    audit.close().await;
    let runtime = sqlx::PgPool::connect(&runtime_url).await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_telemetry_outbox")
        .fetch_one(&runtime)
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "unscoped Server runtime cannot read another owner outbox"
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
async fn sqlite_telemetry_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("telemetry.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([51; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    let worker = CloudStore::connect_without_migrations(&url, 2)
        .await
        .unwrap();
    let audit = Database::connect(&url, 2).await.unwrap();
    telemetry_contract(control, cloud, worker, &audit, &url).await;
    audit.close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "One full disclosure, feedback, retry, and worker fencing scenario runs on both backends."
)]
async fn telemetry_contract(
    control: ControlStore,
    cloud: CloudStore,
    worker: CloudStore,
    audit: &Database,
    runtime_url: &str,
) {
    let now = 2_200_000_000_000_u64;
    let owner = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.test".to_owned(),
                subject: "telemetry-owner".to_owned(),
                email: None,
                display_name: Some("Telemetry owner".to_owned()),
            },
            "test-telemetry-owner",
            now,
        )
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &owner,
            "telemetry-tenant",
            "Telemetry tenant",
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
        .create_project(&owner, &tenant.tenant_id, "Telemetry", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &owner,
            &tenant.tenant_id,
            &project.project_id,
            "Telemetry workspace",
            now + 3,
        )
        .await
        .unwrap();

    let disabled = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-disabled",
        "disabled",
        now + 4,
    )
    .await;
    let feedback_a = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-feedback-a",
        "feedback_only",
        now + 5,
    )
    .await;
    let feedback_b = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-feedback-b",
        "feedback_only",
        now + 6,
    )
    .await;
    let full = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-full",
        "full",
        now + 7,
    )
    .await;
    let disable_after_pending = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-disable-after-pending",
        "full",
        now + 8,
    )
    .await;
    let effective_disabled = create_session_with_plugins(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-effective-overlay",
        vec![
            telemetry_plugin("shared-row", "full", true),
            telemetry_plugin("shared-row", "full", false),
        ],
        now + 9,
    )
    .await;
    let clock_race = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-clock-race",
        "full",
        now + 10,
    )
    .await;
    let release_reenable = create_session(
        &cloud,
        &tenant.tenant_id,
        &owner.user_id,
        &project.project_id,
        &workspace.workspace_id,
        "telemetry-release-reenable",
        "full",
        now + 11,
    )
    .await;
    assert_eq!(
        cloud
            .session_telemetry(&tenant.tenant_id, &owner.user_id, &disabled)
            .await
            .unwrap()
            .sharing,
        SessionTelemetrySharingStatus::Disabled
    );
    assert_eq!(
        cloud
            .session_telemetry(&tenant.tenant_id, &owner.user_id, &feedback_a)
            .await
            .unwrap()
            .sharing,
        SessionTelemetrySharingStatus::FeedbackOnly
    );
    assert_eq!(
        cloud
            .session_telemetry(&tenant.tenant_id, &owner.user_id, &full)
            .await
            .unwrap()
            .sharing,
        SessionTelemetrySharingStatus::Full
    );
    assert_eq!(
        cloud
            .session_telemetry(&tenant.tenant_id, &owner.user_id, &effective_disabled,)
            .await
            .unwrap()
            .sharing,
        SessionTelemetrySharingStatus::Disabled
    );

    for (offset, session_id) in [&disabled, &feedback_a, &feedback_b, &full]
        .into_iter()
        .enumerate()
    {
        append_event(
            audit,
            &tenant.tenant_id,
            session_id,
            SessionEvent {
                seq: 0,
                occurred_at_ms: now + 10 + u64::try_from(offset).unwrap(),
                run_id: RunId::new(format!("telemetry-seed-{offset}")),
                kind: SessionEventKind::TurnStarted,
            },
        )
        .await;
    }
    assert_eq!(outbox_count(audit, &tenant.tenant_id, &disabled).await, 0);
    assert_eq!(outbox_count(audit, &tenant.tenant_id, &feedback_a).await, 0);
    assert_eq!(outbox_count(audit, &tenant.tenant_id, &feedback_b).await, 0);
    assert_eq!(outbox_count(audit, &tenant.tenant_id, &full).await, 1);

    append_event(
        audit,
        &tenant.tenant_id,
        &disable_after_pending,
        SessionEvent {
            seq: 0,
            occurred_at_ms: now + 14,
            run_id: RunId::new("telemetry-disable-pending"),
            kind: SessionEventKind::TurnStarted,
        },
    )
    .await;
    assert_eq!(
        outbox_count(audit, &tenant.tenant_id, &disable_after_pending).await,
        1
    );
    cloud
        .update_session(
            &tenant.tenant_id,
            &disable_after_pending,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "disabled", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 15,
        )
        .await
        .unwrap();
    assert_eq!(
        outbox_count(audit, &tenant.tenant_id, &disable_after_pending).await,
        0
    );
    assert_eq!(
        cloud
            .session_telemetry(&tenant.tenant_id, &owner.user_id, &disable_after_pending,)
            .await
            .unwrap()
            .sharing,
        SessionTelemetrySharingStatus::Disabled
    );

    append_event(
        audit,
        &tenant.tenant_id,
        &clock_race,
        SessionEvent {
            seq: 0,
            occurred_at_ms: now + 100,
            run_id: RunId::new("telemetry-clock-race"),
            kind: SessionEventKind::TurnStarted,
        },
    )
    .await;
    append_event(
        audit,
        &tenant.tenant_id,
        &release_reenable,
        SessionEvent {
            seq: 0,
            occurred_at_ms: now + 16,
            run_id: RunId::new("telemetry-release-before-disable"),
            kind: SessionEventKind::TurnStarted,
        },
    )
    .await;

    cloud
        .record_command_feedback(
            &tenant.tenant_id,
            &owner.user_id,
            &feedback_a,
            "first release".to_owned(),
            now + 20,
        )
        .await
        .unwrap();
    cloud
        .record_command_feedback(
            &tenant.tenant_id,
            &owner.user_id,
            &feedback_a,
            "second release".to_owned(),
            now + 21,
        )
        .await
        .unwrap();
    cloud
        .record_command_feedback(
            &tenant.tenant_id,
            &owner.user_id,
            &feedback_b,
            "independent release".to_owned(),
            now + 22,
        )
        .await
        .unwrap();
    let ranges: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT session_id, from_seq, to_seq FROM cloud_telemetry_outbox
         WHERE tenant_id = $1 ORDER BY session_id, from_seq",
    )
    .bind(tenant.tenant_id.as_str())
    .fetch_all(audit.pool())
    .await
    .unwrap();
    assert!(ranges.contains(&(feedback_a.as_str().to_owned(), 0, 2)));
    assert!(ranges.contains(&(feedback_a.as_str().to_owned(), 3, 5)));
    assert!(ranges.contains(&(feedback_b.as_str().to_owned(), 0, 2)));
    assert!(
        !ranges
            .iter()
            .any(|(session, _, _)| session == disabled.as_str())
    );

    let mut incapable_hello = worker_hello("telemetry-incapable-worker", "instance-a");
    incapable_hello.capabilities.clear();
    let incapable = worker
        .register_cloud_worker(&incapable_hello, Duration::from_secs(60), now + 29)
        .await
        .unwrap();
    assert!(
        worker
            .claim_telemetry(&incapable, Duration::from_secs(5), 16, now + 30)
            .await
            .unwrap()
            .is_empty()
    );

    let worker_a = worker
        .register_cloud_worker(
            &worker_hello("telemetry-worker", "instance-a"),
            Duration::from_secs(60),
            now + 30,
        )
        .await
        .unwrap();
    let initial_claims = worker
        .claim_telemetry(&worker_a, Duration::from_secs(5), 2, now + 31)
        .await
        .unwrap();
    assert_eq!(initial_claims.len(), 2);
    let inflight_disabled = initial_claims
        .iter()
        .find(|claim| claim.identity.session_id == full)
        .expect("full occurrence claimed")
        .clone();
    let released_disabled = initial_claims
        .iter()
        .find(|claim| claim.identity.session_id == release_reenable)
        .expect("release occurrence claimed")
        .clone();
    cloud
        .update_session(
            &tenant.tenant_id,
            &full,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "disabled", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 32,
        )
        .await
        .unwrap();
    worker
        .fail_telemetry(
            &worker_a,
            &inflight_disabled,
            "collector unavailable",
            now + 33,
        )
        .await
        .unwrap();
    assert_eq!(outbox_count(audit, &tenant.tenant_id, &full).await, 0);
    cloud
        .update_session(
            &tenant.tenant_id,
            &release_reenable,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "disabled", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 32,
        )
        .await
        .unwrap();
    assert!(
        worker
            .drain_cloud_worker(&worker_a, now + 33)
            .await
            .unwrap()
            >= 1
    );
    assert_eq!(
        outbox_count(audit, &tenant.tenant_id, &release_reenable).await,
        0
    );
    cloud
        .update_session(
            &tenant.tenant_id,
            &release_reenable,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "full", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 34,
        )
        .await
        .unwrap();
    append_event(
        audit,
        &tenant.tenant_id,
        &release_reenable,
        SessionEvent {
            seq: 1,
            occurred_at_ms: now + 35,
            run_id: RunId::new("telemetry-release-after-reenable"),
            kind: SessionEventKind::TurnStarted,
        },
    )
    .await;
    let first_claims = worker
        .claim_telemetry(&worker_a, Duration::from_secs(5), 16, now + 36)
        .await
        .unwrap();
    assert!(first_claims.iter().any(|claim| {
        claim.identity.session_id == release_reenable
            && claim.from_seq == 1
            && claim.to_seq == 1
            && claim.occurrence_id != released_disabled.occurrence_id
    }));
    let (race_created_at, race_updated_at): (i64, i64) = sqlx::query_as(
        "SELECT created_at_ms, updated_at_ms FROM cloud_telemetry_outbox
         WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(clock_race.as_str())
    .fetch_one(audit.pool())
    .await
    .unwrap();
    assert_eq!(race_created_at, i64::try_from(now + 100).unwrap());
    assert!(race_updated_at >= race_created_at);
    let crashed_disabled = first_claims
        .iter()
        .find(|claim| claim.identity.session_id == clock_race)
        .expect("clock-race occurrence claimed")
        .clone();
    cloud
        .update_session(
            &tenant.tenant_id,
            &clock_race,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "disabled", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 37,
        )
        .await
        .unwrap();
    cloud
        .update_session(
            &tenant.tenant_id,
            &clock_race,
            &owner.user_id,
            CloudSessionUpdate {
                profile_plugins: Some(vec![telemetry_plugin("telemetry", "full", true)]),
                ..CloudSessionUpdate::default()
            },
            now + 38,
        )
        .await
        .unwrap();
    append_event(
        audit,
        &tenant.tenant_id,
        &clock_race,
        SessionEvent {
            seq: 1,
            occurred_at_ms: now + 39,
            run_id: RunId::new("telemetry-clock-after-reenable"),
            kind: SessionEventKind::TurnStarted,
        },
    )
    .await;
    let interrupted = first_claims
        .iter()
        .find(|claim| claim.identity.session_id == feedback_a && claim.from_seq == 0)
        .expect("first feedback suffix claimed")
        .clone();
    assert_eq!(interrupted.attempt_count, 1);
    assert!(
        interrupted.records().iter().all(|record| record
            .body
            .to_string()
            .find("secret-value")
            .is_none())
    );

    drop(worker);
    let restarted_worker = CloudStore::connect_without_migrations(runtime_url, 2)
        .await
        .unwrap();
    let worker_b = restarted_worker
        .register_cloud_worker(
            &worker_hello("telemetry-worker", "instance-b"),
            Duration::from_secs(60),
            now + 40,
        )
        .await
        .unwrap();
    assert!(worker_b.generation > worker_a.generation);
    let restart_claims = restarted_worker
        .claim_telemetry(&worker_b, Duration::from_secs(5), 16, now + 36 + 5_001)
        .await
        .unwrap();
    assert!(
        restart_claims
            .iter()
            .all(|claim| claim.occurrence_id != crashed_disabled.occurrence_id)
    );
    assert!(restart_claims.iter().any(|claim| {
        claim.identity.session_id == clock_race && claim.from_seq == 1 && claim.to_seq == 1
    }));
    let retry = restart_claims
        .into_iter()
        .find(|claim| claim.occurrence_id == interrupted.occurrence_id)
        .expect("expired occurrence is reclaimed after Worker restart");
    assert_eq!(retry.attempt_count, 2);
    assert_eq!(retry.from_seq, interrupted.from_seq);
    assert_eq!(retry.to_seq, interrupted.to_seq);
    restarted_worker
        .acknowledge_telemetry(&worker_b, &retry, now + 36 + 5_002)
        .await
        .unwrap();
    let projection = cloud
        .session_telemetry(&tenant.tenant_id, &owner.user_id, &feedback_a)
        .await
        .unwrap();
    assert_eq!(projection.export_seq, Some(2));
    assert_eq!(projection.handoff_seq, Some(5));
}

#[allow(clippy::too_many_arguments)]
async fn create_session(
    cloud: &CloudStore,
    tenant_id: &ternilo_protocol::TenantId,
    user_id: &ternilo_protocol::UserId,
    project_id: &str,
    workspace_id: &ternilo_protocol::WorkspaceId,
    session_id: &str,
    mode: &str,
    now_ms: u64,
) -> SessionId {
    create_session_with_plugins(
        cloud,
        tenant_id,
        user_id,
        project_id,
        workspace_id,
        session_id,
        vec![telemetry_plugin("telemetry", mode, true)],
        now_ms,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_session_with_plugins(
    cloud: &CloudStore,
    tenant_id: &ternilo_protocol::TenantId,
    user_id: &ternilo_protocol::UserId,
    project_id: &str,
    workspace_id: &ternilo_protocol::WorkspaceId,
    session_id: &str,
    profile_plugins: Vec<PluginEntry>,
    now_ms: u64,
) -> SessionId {
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.to_owned(),
                workspace_id: workspace_id.clone(),
                session_id: Some(SessionId::new(session_id)),
                agent_id: AgentId::new("telemetry-agent"),
                title: session_id.to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 1_000,
                agent_preset: "telemetry".to_owned(),
                profile_plugins,
                mode: SessionMode::Execute,
            },
            tenant_id,
            user_id,
            now_ms,
        )
        .await
        .unwrap()
        .session_id
}

fn telemetry_plugin(id: &str, mode: &str, enabled: bool) -> PluginEntry {
    PluginEntry {
        id: id.to_owned(),
        kind: ternilo_builtins::OTLP_TELEMETRY_KIND.to_owned(),
        enabled,
        config: serde_json::json!({
            "mode": mode,
            "endpoint": "https://collector.example.test/v1/logs",
            "credential_headers": {"authorization": "OTLP_TOKEN"}
        }),
    }
}

async fn append_event(
    audit: &Database,
    tenant_id: &ternilo_protocol::TenantId,
    session_id: &SessionId,
    event: SessionEvent,
) {
    let mut transaction = audit.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO cloud_session_events
            (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms)
         VALUES ($1, $2, $3, $4, $5, 1, $6)",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .bind(i64::try_from(event.seq).unwrap())
    .bind(event.run_id.as_str())
    .bind(Json(&event))
    .bind(i64::try_from(event.occurred_at_ms).unwrap())
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE cloud_sessions SET last_seq = $3, updated_at_ms = $4
         WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .bind(i64::try_from(event.seq).unwrap())
    .bind(i64::try_from(event.occurred_at_ms).unwrap())
    .execute(&mut *transaction)
    .await
    .unwrap();
    super::capture_event_in(
        &mut transaction,
        tenant_id,
        session_id,
        &event,
        event.occurred_at_ms,
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
}

async fn outbox_count(
    audit: &Database,
    tenant_id: &ternilo_protocol::TenantId,
    session_id: &SessionId,
) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_telemetry_outbox
         WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant_id.as_str())
    .bind(session_id.as_str())
    .fetch_one(audit.pool())
    .await
    .unwrap()
}

fn worker_hello(worker_id: &str, instance_nonce: &str) -> ExecutorHello {
    ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new(worker_id),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: instance_nonce.to_owned(),
        catalog_revision: "telemetry-test-catalog".to_owned(),
        capabilities: BTreeSet::from([ExecutorCapability::TelemetryDisclosure]),
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
        .execute("DROP ROLE IF EXISTS ternilo_telemetry_runtime_test")
        .await
        .unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_telemetry_worker_test")
        .await
        .unwrap();
}

async fn create_restricted_roles(admin_url: &str) {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin.execute("CREATE ROLE ternilo_telemetry_runtime_test LOGIN PASSWORD 'telemetry-runtime-password'").await.unwrap();
    admin
        .execute("GRANT USAGE ON SCHEMA public TO ternilo_telemetry_runtime_test")
        .await
        .unwrap();
    admin.execute("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO ternilo_telemetry_runtime_test").await.unwrap();
    admin.execute("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO ternilo_telemetry_runtime_test").await.unwrap();
    admin
        .execute(
            "GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO ternilo_telemetry_runtime_test",
        )
        .await
        .unwrap();
}

fn database_url_for_role(url: &str, username: &str, password: &str) -> String {
    let mut url = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    url.set_username(username).unwrap();
    url.set_password(Some(password)).unwrap();
    url.to_string()
}
