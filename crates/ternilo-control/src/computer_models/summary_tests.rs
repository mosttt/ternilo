use crate::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, PageQuery,
    SecretCipher, TenantQuota, TenantRole,
};
use sqlx::Executor;
use ternilo_protocol::TenantId;

const NOW: u64 = 1_790_985_600_000; // 2026-10-03T00:00:00Z.

#[tokio::test]
async fn sqlite_forwarded_summary_preserves_scope_attempts_unknowns_and_months() {
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([42; 32]), 1)
        .await
        .unwrap();
    Box::pin(contract(&store)).await;
}
#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_forwarded_summary_preserves_scope_attempts_unknowns_and_months() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(
        &admin,
        "ternilo_forward_usage_test",
        "forward-usage-password",
    )
    .await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_forward_usage_test").unwrap();
    runtime
        .set_password(Some("forward-usage-password"))
        .unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([42; 32]),
        4,
    )
    .await
    .unwrap();
    Box::pin(contract(&store)).await;
    store.database().close().await;
    admin.close().await;
}
async fn user(store: &ControlStore, name: &str) -> ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://usage.example.test".into(),
                subject: name.into(),
                email: None,
                display_name: None,
            },
            name,
            NOW,
        )
        .await
        .unwrap()
}
async fn seed(
    store: &ControlStore,
    tenant: &TenantId,
    id: &str,
    actor: &ControlUser,
    source: &ControlUser,
    owner: &ControlUser,
    now: u64,
) {
    let snapshot = serde_json::json!({"binding":{"source":"computer_provider","tenant_id":tenant,"owner_user_id":source.user_id,"executor_id":"source","provider_id":"provider","model":"model"},"protocol":"openai-chat-completions","display_name":"Model","source_name":"Source","defaults":{"context_window":32000,"max_output_tokens":2000},"reasoning_effort":"none"});
    let mut tx = store.database().tenant_transaction(tenant).await.unwrap();
    sqlx::query("INSERT INTO control_computer_model_requests(tenant_id,request_id,credential_id,session_id,run_id,request_key,payload_hash,execution_executor_id,source_executor_id,actor_user_id,model_owner_user_id,resource_owner_user_id,snapshot_json,max_attempts,state,created_at_ms,updated_at_ms) VALUES($1,$2,'credential','session','run',$2,'hash','execution','source',$3,$4,$5,$6,3,'pending',$7,$7)")
        .bind(tenant.as_str()).bind(id).bind(actor.user_id.as_str()).bind(source.user_id.as_str()).bind(owner.user_id.as_str()).bind(snapshot.to_string()).bind(i64::try_from(now).unwrap()).execute(&mut *tx).await.unwrap();
    for (attempt, report) in [
        (
            1_i64,
            Some(
                serde_json::json!({"type":"finished","attempt":1,"usage":{"input_tokens":10,"output_tokens":null},"error_code":"execution"}),
            ),
        ),
        (
            2,
            Some(
                serde_json::json!({"type":"finished","attempt":2,"usage":{"input_tokens":0,"output_tokens":5},"error_code":null}),
            ),
        ),
        (3, None),
    ] {
        sqlx::query("INSERT INTO control_computer_model_attempts(tenant_id,request_id,attempt,report_json,started_at_ms,finished_at_ms) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(tenant.as_str()).bind(id).bind(attempt).bind(report.as_ref().map(ToString::to_string)).bind(i64::try_from(now).unwrap()).bind(report.map(|_| i64::try_from(now + 1).unwrap())).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
}
#[expect(
    clippy::too_many_lines,
    reason = "Exercise two-tenant attribution, partial attempts, period boundaries and revocation in one contract."
)]
async fn contract(store: &ControlStore) {
    let registration = store
        .initialize_owner(
            &NativeRegistration {
                username: "forward-owner".into(),
                email: "forward-owner@example.test".into(),
                password: "forward-owner-password".into(),
            },
            NOW,
        )
        .await
        .unwrap();
    let a = registration.session.user;
    store
        .set_instance_mode(&a, InstanceMode::MultiUser, 1, NOW)
        .await
        .unwrap();
    let b = user(store, "source-owner").await;
    let c = user(store, "resource-owner").await;
    let tenant = store
        .create_tenant(
            &a,
            "summary-team",
            "Summary team",
            TenantQuota {
                max_nodes: 4,
                max_concurrent_runs: 4,
                monthly_model_tokens: 1_000_000,
                max_secrets: 10,
            },
            NOW,
        )
        .await
        .unwrap()
        .tenant_id;
    for member in [&b, &c] {
        store
            .set_membership(&a, &tenant, &member.user_id, TenantRole::Member, NOW)
            .await
            .unwrap();
    }
    seed(store, &tenant, "request-a", &a, &b, &c, NOW).await;
    seed(store, &tenant, "request-c", &c, &c, &c, NOW).await;
    seed(
        store,
        &registration.session.personal_tenant_id,
        "foreign-tenant",
        &a,
        &b,
        &c,
        NOW,
    )
    .await;
    seed(
        store,
        &tenant,
        "previous-month",
        &a,
        &b,
        &c,
        NOW - 10 * 86_400_000,
    )
    .await;
    let mut tx = store.database().tenant_transaction(&tenant).await.unwrap();
    sqlx::query("UPDATE control_computer_model_attempts SET finished_at_ms=$2 WHERE tenant_id=$1 AND request_id='previous-month' AND report_json IS NOT NULL").bind(tenant.as_str()).bind(i64::try_from(NOW).unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    for actor in [&a, &b, &c] {
        let summary = store
            .computer_model_usage_summary(actor, &tenant, Some("2026-10"), None, NOW)
            .await
            .unwrap();
        assert_eq!(
            summary.totals.requests, 1,
            "membership or resource ownership cannot reveal another account's calls"
        );
        assert_eq!(summary.totals.active_requests, 1);
        let usage = &summary.totals.usage;
        assert_eq!((usage.attempts, usage.completed, usage.failed), (3, 2, 1));
        assert_eq!(
            (usage.input.tokens, usage.input.reported_attempts),
            (Some(10), 2)
        );
        assert_eq!(
            (usage.output.tokens, usage.output.reported_attempts),
            (Some(5), 1)
        );
        assert_eq!(
            (usage.reasoning.tokens, usage.reasoning.reported_attempts),
            (None, 0)
        );
        assert_eq!(summary.groups.len(), 1);
        assert_eq!(summary.groups[0].provider, "provider");
    }
    let expired = store
        .computer_model_usage_summary(&a, &tenant, Some("2026-10"), None, NOW + 60_001)
        .await
        .unwrap();
    assert_eq!(expired.totals.active_requests, 0);
    let previous = store
        .computer_model_usage_summary(&a, &tenant, Some("2026-09"), None, NOW)
        .await
        .unwrap();
    assert_eq!(previous.totals.requests, 1);
    let empty = store
        .computer_model_usage_summary(&a, &tenant, Some("2026-10"), Some("request-c"), NOW)
        .await
        .unwrap();
    assert_eq!(empty.totals.requests, 0);
    assert_eq!(empty.totals.usage.input.tokens, None);
    assert!(
        store
            .computer_model_usage_summary(&a, &tenant, Some("2026-13"), None, NOW)
            .await
            .is_err()
    );
    let records = store
        .list_computer_model_requests(&a, &tenant, &PageQuery::default(), Some("2026-10"), NOW)
        .await
        .unwrap();
    assert_eq!(records.requests.len(), 1);
    assert_eq!(records.requests[0].request_id, "request-a");
    let report: ternilo_protocol::ComputerModelAttempt = serde_json::from_value(serde_json::json!({"type":"finished","attempt":3,"http_status":200,"error_code":null,"upstream_request_id":null,"usage":{"input_tokens":7,"output_tokens":null}})).unwrap();
    store
        .finish_computer_model_attempt(&tenant, "request-a", &report, NOW + 2)
        .await
        .unwrap();
    assert!(
        store
            .finish_computer_model_attempt(&tenant, "request-a", &report, NOW + 3)
            .await
            .is_err()
    );
    let late = store
        .computer_model_usage_summary(&a, &tenant, Some("2026-10"), None, NOW + 3)
        .await
        .unwrap();
    assert_eq!(
        (
            late.totals.requests,
            late.totals.usage.attempts,
            late.totals.usage.completed
        ),
        (1, 3, 3)
    );
    assert_eq!(
        (
            late.totals.usage.input.tokens,
            late.totals.usage.input.reported_attempts
        ),
        (Some(17), 3)
    );
    let stale = store
        .list_computer_model_requests(
            &a,
            &tenant,
            &PageQuery::default(),
            Some("2026-10"),
            NOW + 60_001,
        )
        .await
        .unwrap();
    assert_eq!(stale.requests[0].state, "failed");
    assert_eq!(
        stale.requests[0].error_code.as_deref(),
        Some("connection_lost")
    );
    store
        .finish_computer_model_request(&tenant, "request-a", "completed", None, NOW + 60_002)
        .await
        .unwrap();
    let completed = store
        .list_computer_model_requests(
            &a,
            &tenant,
            &PageQuery::default(),
            Some("2026-10"),
            NOW + 60_003,
        )
        .await
        .unwrap();
    assert_eq!(
        completed.requests[0].state, "completed",
        "reading an expired lease cannot erase a later actual completion"
    );
    store
        .remove_membership(&a, &tenant, &b.user_id, NOW + 1)
        .await
        .unwrap();
    assert!(
        store
            .computer_model_usage_summary(&b, &tenant, Some("2026-10"), None, NOW + 1)
            .await
            .is_err()
    );
}
