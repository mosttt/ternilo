use sqlx::{AnyPool, Row};
use ternilo_cloud::CloudStore;
use ternilo_control::{
    ControlStore, ModelUsageAnomalyKind, OidcPrincipal, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::ErrorCode;

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

const AUGUST_10_2026: i64 = 1_786_320_000_000;
const AUGUST_20_2026: u64 = 1_787_184_000_000;

async fn user(store: &ControlStore, subject: &str) -> ternilo_control::ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: subject.to_owned(),
                email: Some(format!("{subject}@example.com")),
                display_name: Some(subject.to_owned()),
            },
            &format!("test-{subject}"),
            AUGUST_20_2026,
        )
        .await
        .unwrap()
}

struct RunFixture<'a> {
    tenant_id: &'a str,
    user_id: &'a str,
    project_id: &'a str,
    workspace_id: &'a str,
    run_id: &'a str,
    reservation_id: &'a str,
    reservation_state: &'a str,
    run_state: &'a str,
    committed_tokens: Option<i64>,
    created_at_ms: i64,
    expires_at_ms: i64,
}

async fn insert_run(pool: &AnyPool, fixture: &RunFixture<'_>) {
    sqlx::query(
        "INSERT INTO control_quota_reservations
            (tenant_id, reservation_id, user_id, run_id, reserved_model_tokens,
             state, created_at_ms, expires_at_ms, committed_model_tokens, period_start)
         VALUES ($1, $2, $3, $4, 1000, $5, $6, $7, $8, '2026-08-01')",
    )
    .bind(fixture.tenant_id)
    .bind(fixture.reservation_id)
    .bind(fixture.user_id)
    .bind(fixture.run_id)
    .bind(fixture.reservation_state)
    .bind(fixture.created_at_ms)
    .bind(fixture.expires_at_ms)
    .bind(fixture.committed_tokens)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO cloud_runs
            (tenant_id, run_id, user_id, project_id, workspace_id, agent_id, session_id,
             spec, spec_digest, quota_reservation_id, state, available_at_ms,
             created_at_ms, updated_at_ms, finished_at_ms, actor_user_id, authorization_session_id)
         VALUES ($1, $2, $3, $4, $5, 'agent', $2, '{}', $6, $7, $8, $9, $9, $9,
                 CASE WHEN $8 IN ('succeeded', 'failed', 'cancelled', 'indeterminate') THEN $9 ELSE NULL END, $3, $2)",
    )
    .bind(fixture.tenant_id)
    .bind(fixture.run_id)
    .bind(fixture.user_id)
    .bind(fixture.project_id)
    .bind(fixture.workspace_id)
    .bind(vec![7_u8; 32])
    .bind(fixture.reservation_id)
    .bind(fixture.run_state)
    .bind(fixture.created_at_ms)
    .execute(pool)
    .await
    .unwrap();
}

#[expect(
    clippy::too_many_arguments,
    reason = "The fixture specifies each authoritative usage counter explicitly."
)]
async fn insert_usage(
    pool: &AnyPool,
    tenant_id: &str,
    run_id: &str,
    lease_token: i64,
    request_id: i64,
    input: i64,
    output: i64,
    cached: i64,
) {
    let run = sqlx::query("SELECT * FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2")
        .bind(tenant_id)
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let owner: String = run.get("user_id");
    let reservation: String = run.get("quota_reservation_id");
    let logical_id = format!("usage-{run_id}-{lease_token}-{request_id}");
    let principal = serde_json::json!({
        "tenant_id": tenant_id, "project_id": run.get::<String, _>("project_id"),
        "workspace_id": run.get::<String, _>("workspace_id"), "session_id": run_id,
        "authorization_session_id": run_id, "run_id": run_id,
        "actor_user_id": owner, "resource_owner_user_id": owner, "execution_owner_user_id": owner,
        "execution_reservation_id": reservation, "worker_id": "usage-worker", "worker_generation": 1,
        "lease_token": lease_token, "writer_fencing_token": lease_token,
        "model": {"source": "user_provider", "tenant_id": tenant_id, "owner_user_id": owner, "provider_id": "provider-a", "model": "model-a"},
        "run_token_limit": 1000,
    });
    serde_json::from_value::<ternilo_control::WorkloadModelPrincipal>(principal.clone()).unwrap();
    sqlx::query("INSERT INTO control_model_requests(
        request_id,origin,source,caller_scope,request_key,payload_hash,actor_user_id,resource_owner_user_id,
        model_beneficiary_user_id,workload_json,tenant_id,project_id,session_id,run_id,execution_reservation_id,
        budget_period_start,model_id,provider_id,upstream_model,protocol,route_snapshot_json,max_attempts,state,month,
        created_at_ms,expires_at_ms,settled_at_ms)
        VALUES($1,'workload','user_provider',$1,$2,$1,$3,$3,$3,$4,$5,$6,$7,$7,$8,
        '2026-08-01','model-a','provider-a','model-a','openai-chat-completions','{}',1,'completed','2026-08',$9,$10,$9)")
        .bind(&logical_id).bind(request_id.to_string()).bind(&owner).bind(principal.to_string())
        .bind(tenant_id).bind(run.get::<String, _>("project_id")).bind(run_id).bind(&reservation)
        .bind(AUGUST_10_2026 + lease_token * 10 + request_id).bind(AUGUST_10_2026 + 60_000)
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO control_model_attempts(request_id,attempt,state,attempted,reserved_tokens,
        accounted_tokens,input_tokens,output_tokens,cached_input_tokens,upstream_request_id,created_at_ms,settled_at_ms)
        VALUES($1,1,'completed',1,1000,$2,$3,$4,$5,$6,$7,$7)")
        .bind(&logical_id).bind(input+output).bind(input).bind(output).bind(cached)
        .bind(format!("provider-request-{lease_token}-{request_id}"))
        .bind(AUGUST_10_2026 + lease_token * 10 + request_id).execute(pool).await.unwrap();
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_USAGE_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_usage_report_is_authoritative_tenant_scoped_and_admin_only() {
    let admin_url = std::env::var("TERNILO_CLOUD_USAGE_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_USAGE_TEST_DATABASE_URL must be set");
    assert!(admin_url.contains("ternilo_cloud_usage_test"));
    let runtime_url =
        server_runtime::initialize(&admin_url, "ternilo_usage_runtime_test", [19; 32]).await;
    let store = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([19; 32]),
        4,
    )
    .await
    .unwrap();
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
    let database = ternilo_storage::Database::connect(&admin_url, 2)
        .await
        .unwrap();
    usage_contract(store, database.pool().clone()).await;
}

#[tokio::test]
async fn sqlite_usage_report_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("usage.sqlite").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([19; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(store.database().clone())
        .await
        .unwrap();
    usage_contract(store, cloud.database().pool().clone()).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the cross-tenant role and authoritative usage assertions in one contract."
)]
async fn usage_contract(store: ControlStore, admin: AnyPool) {
    let owner = user(&store, "usage-owner").await;
    let administrator = user(&store, "usage-admin").await;
    let member = user(&store, "usage-member").await;
    let viewer = user(&store, "usage-viewer").await;
    let other_owner = user(&store, "usage-other-owner").await;
    let quota = TenantQuota {
        max_nodes: 2,
        max_concurrent_runs: 8,
        monthly_model_tokens: 10_000,
        max_secrets: 4,
    };
    let tenant = store
        .create_tenant(
            &owner,
            "usage-a",
            "Usage A",
            quota.clone(),
            AUGUST_20_2026 + 1,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant.tenant_id,
            &administrator.user_id,
            TenantRole::Admin,
            AUGUST_20_2026 + 2,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant.tenant_id,
            &member.user_id,
            TenantRole::Member,
            AUGUST_20_2026 + 3,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant.tenant_id,
            &viewer.user_id,
            TenantRole::Viewer,
            AUGUST_20_2026 + 4,
        )
        .await
        .unwrap();
    let project = store
        .create_project(
            &owner,
            &tenant.tenant_id,
            "Usage project",
            AUGUST_20_2026 + 5,
        )
        .await
        .unwrap();
    let workspace = store
        .create_cloud_workspace(
            &owner,
            &tenant.tenant_id,
            &project.project_id,
            "Usage workspace",
            AUGUST_20_2026 + 6,
        )
        .await
        .unwrap();

    insert_run(
        &admin,
        &RunFixture {
            tenant_id: tenant.tenant_id.as_str(),
            user_id: owner.user_id.as_str(),
            project_id: &project.project_id,
            workspace_id: workspace.workspace_id.as_str(),
            run_id: "run-committed",
            reservation_id: "reservation-committed",
            reservation_state: "committed",
            run_state: "succeeded",
            committed_tokens: Some(180),
            created_at_ms: AUGUST_10_2026,
            expires_at_ms: AUGUST_10_2026 + 60_000,
        },
    )
    .await;
    insert_usage(
        &admin,
        tenant.tenant_id.as_str(),
        "run-committed",
        1,
        1,
        100,
        20,
        40,
    )
    .await;
    insert_usage(
        &admin,
        tenant.tenant_id.as_str(),
        "run-committed",
        2,
        1,
        50,
        10,
        10,
    )
    .await;
    insert_run(
        &admin,
        &RunFixture {
            tenant_id: tenant.tenant_id.as_str(),
            user_id: administrator.user_id.as_str(),
            project_id: &project.project_id,
            workspace_id: workspace.workspace_id.as_str(),
            run_id: "run-stale",
            reservation_id: "reservation-stale",
            reservation_state: "active",
            run_state: "failed",
            committed_tokens: None,
            created_at_ms: AUGUST_10_2026 + 10,
            expires_at_ms: AUGUST_10_2026 + 20,
        },
    )
    .await;
    sqlx::query(
        "INSERT INTO control_quota_usage (tenant_id, period_start, used_model_tokens)
         VALUES ($1, '2026-08-01', 180)
         ON CONFLICT (tenant_id, period_start) DO UPDATE SET used_model_tokens = 180",
    )
    .bind(tenant.tenant_id.as_str())
    .execute(&admin)
    .await
    .unwrap();

    let other_tenant = store
        .create_tenant(
            &other_owner,
            "usage-b",
            "Usage B",
            quota,
            AUGUST_20_2026 + 7,
        )
        .await
        .unwrap();
    let other_project = store
        .create_project(
            &other_owner,
            &other_tenant.tenant_id,
            "Other project",
            AUGUST_20_2026 + 8,
        )
        .await
        .unwrap();
    let other_workspace = store
        .create_cloud_workspace(
            &other_owner,
            &other_tenant.tenant_id,
            &other_project.project_id,
            "Other workspace",
            AUGUST_20_2026 + 9,
        )
        .await
        .unwrap();
    insert_run(
        &admin,
        &RunFixture {
            tenant_id: other_tenant.tenant_id.as_str(),
            user_id: other_owner.user_id.as_str(),
            project_id: &other_project.project_id,
            workspace_id: other_workspace.workspace_id.as_str(),
            run_id: "other-run",
            reservation_id: "other-reservation",
            reservation_state: "committed",
            run_state: "succeeded",
            committed_tokens: Some(999),
            created_at_ms: AUGUST_10_2026,
            expires_at_ms: AUGUST_10_2026 + 60_000,
        },
    )
    .await;
    insert_usage(
        &admin,
        other_tenant.tenant_id.as_str(),
        "other-run",
        1,
        1,
        900,
        99,
        0,
    )
    .await;

    let owner_report = store
        .model_usage_report(
            &owner,
            &tenant.tenant_id,
            Some("2026-08"),
            200,
            AUGUST_20_2026,
        )
        .await
        .unwrap();
    assert_eq!(owner_report.totals.requests, 2);
    assert_eq!(owner_report.totals.total_tokens, 180);
    assert_eq!(owner_report.totals.cached_input_tokens, 50);
    assert_eq!(owner_report.quota.settled_tokens, 180);
    assert_eq!(owner_report.groups[0].provider, "provider-a");
    assert_eq!(owner_report.ledger[0].actor_user_id, owner.user_id);
    assert_eq!(owner_report.ledger[0].resource_owner_user_id, owner.user_id);
    assert_eq!(owner_report.totals.attempts, 2);
    assert_eq!(
        owner_report
            .ledger
            .iter()
            .map(|entry| (entry.lease_token, entry.request_id.as_str(), entry.attempt))
            .collect::<Vec<_>>(),
        vec![
            (2, "usage-run-committed-2-1", 1),
            (1, "usage-run-committed-1-1", 1)
        ],
    );
    assert!(
        owner_report
            .ledger
            .iter()
            .all(|entry| !entry.run_id.contains("other"))
    );
    let stale = owner_report
        .anomalies
        .iter()
        .find(|reservation| reservation.reservation_id == "reservation-stale")
        .unwrap();
    assert_eq!(stale.user_id, administrator.user_id);
    assert_eq!(
        stale.issues,
        vec![
            ModelUsageAnomalyKind::ExpiredActive,
            ModelUsageAnomalyKind::TerminalRunActive,
        ]
    );

    let admin_report = store
        .model_usage_report(
            &administrator,
            &tenant.tenant_id,
            Some("2026-08"),
            200,
            AUGUST_20_2026,
        )
        .await
        .unwrap();
    assert_eq!(admin_report, owner_report);
    let denied = store
        .model_usage_report(
            &member,
            &tenant.tenant_id,
            Some("2026-08"),
            200,
            AUGUST_20_2026,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PolicyDenied);
    let denied = store
        .model_usage_report(
            &viewer,
            &tenant.tenant_id,
            Some("2026-08"),
            200,
            AUGUST_20_2026,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PolicyDenied);
}
