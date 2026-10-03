use super::*;
use ternilo_protocol::{ComputerModelAttempt, ReportedModelUsage, RunModelSnapshot};

#[expect(
    clippy::too_many_lines,
    reason = "Verify source ownership, retry accounting, tenant isolation and revoked-source settlement in one shared database contract."
)]
pub(super) async fn contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    accepted: &NodeModelRequest,
    original: &RunModelSnapshot,
) {
    let now = fixture.now + 600_000;
    let tenant = &fixture.tenant_a;
    let source_id = ExecutorId::new("forwarded-model-source");
    let enrollment = store
        .create_owned_enrollment(
            &fixture.bob,
            tenant,
            None,
            source_id.clone(),
            Duration::from_secs(60),
            now,
        )
        .await
        .unwrap();
    store
        .consume_enrollment(&enrollment.token, now)
        .await
        .unwrap();
    let names = store
        .computer_display_names(
            &fixture.alice,
            tenant,
            &[
                source_id.clone(),
                fixture.executor_a.clone(),
                fixture.executor_b.clone(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        names.get(&source_id).map(String::as_str),
        Some(source_id.as_str())
    );
    assert!(names.contains_key(&fixture.executor_a));
    assert!(!names.contains_key(&fixture.executor_b));
    let mut snapshot = original.clone();
    snapshot.binding = RunModelBinding::ComputerProvider {
        tenant_id: tenant.clone(),
        owner_user_id: fixture.bob.user_id.clone(),
        executor_id: source_id.to_string(),
        provider_id: "private-computer-provider".to_owned(),
        model: "private-model".to_owned(),
    };
    store
        .set_edge_session_model_snapshot(
            &fixture.bob,
            tenant,
            &mapping.browser_session,
            Some(snapshot.clone()),
        )
        .await
        .unwrap();
    let mut body = accepted.clone();
    body.binding = snapshot.binding.clone();
    let mut tx = store.database().begin().await.unwrap();
    let principal = store
        .authorize_node_model_in(&mut tx, node, &body)
        .await
        .unwrap();
    let id = store
        .accept_computer_model_request_in(
            &mut tx,
            &principal,
            "computer-request",
            &"a".repeat(64),
            2,
            now,
        )
        .await
        .unwrap();
    store
        .begin_computer_model_attempt_in(&mut tx, &principal, &id, 1, now)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = store.database().begin().await.unwrap();
    assert!(
        store
            .accept_computer_model_request_in(
                &mut tx,
                &principal,
                "computer-request",
                &"b".repeat(64),
                2,
                now
            )
            .await
            .is_err(),
        "accepted work must never be replayed, even with a changed prompt"
    );
    tx.rollback().await.unwrap();
    let mut tx = store.database().begin().await.unwrap();
    assert!(
        store
            .begin_computer_model_attempt_in(&mut tx, &principal, &id, 2, now)
            .await
            .is_err(),
        "retry requires a completed failed attempt"
    );
    tx.rollback().await.unwrap();

    store
        .finish_computer_model_attempt(tenant, &id, &report(1, Some(ErrorCode::Execution)), now + 1)
        .await
        .unwrap();
    let mut tx = store.database().begin().await.unwrap();
    store
        .begin_computer_model_attempt_in(&mut tx, &principal, &id, 2, now + 2)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    store
        .finish_computer_model_attempt(tenant, &id, &report(2, None), now + 3)
        .await
        .unwrap();
    store
        .finish_computer_model_request(tenant, &id, "completed", None, now + 4)
        .await
        .unwrap();
    for actor in [&fixture.alice, &fixture.bob] {
        let page = store
            .list_computer_model_requests(
                actor,
                tenant,
                &ternilo_control::PageQuery::default(),
                None,
                now + 5,
            )
            .await
            .unwrap();
        assert_eq!(page.requests.len(), 1);
        let row = &page.requests[0];
        assert_eq!(row.actor_user_id, fixture.alice.user_id);
        assert_eq!(row.model_owner_user_id, fixture.bob.user_id);
        assert_eq!(row.resource_owner_user_id, fixture.alice.user_id);
        assert_eq!(row.execution_executor_id, fixture.executor_a.as_str());
        assert_eq!(row.source_executor_id, source_id.as_str());
        assert_eq!(row.attempts.len(), 2);
        assert_eq!(row.state, "completed");
    }
    assert!(
        store
            .list_computer_model_requests(
                &fixture.bob,
                &fixture.tenant_b,
                &ternilo_control::PageQuery::default(),
                None,
                now + 5
            )
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    let mut tx = store
        .database()
        .tenant_transaction(&fixture.tenant_b)
        .await
        .unwrap();
    if ternilo_storage::backend(&tx) == ternilo_storage::Backend::Postgres {
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_computer_model_requests WHERE request_id=$1",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(
            leaked, 0,
            "runtime RLS must hide another tenant's model calls"
        );
    }
    tx.commit().await.unwrap();

    // Even an execution owner cannot use a saved computer binding from local input.
    let mut own = snapshot.clone();
    own.binding = RunModelBinding::ComputerProvider {
        tenant_id: tenant.clone(),
        owner_user_id: fixture.alice.user_id.clone(),
        executor_id: fixture.executor_a.to_string(),
        provider_id: "private-computer-provider".to_owned(),
        model: "private-model".to_owned(),
    };
    store
        .set_edge_session_model_snapshot(
            &fixture.alice,
            tenant,
            &mapping.browser_session,
            Some(own.clone()),
        )
        .await
        .unwrap();
    let mut direct = body.clone();
    direct.binding = own.binding;
    direct.provenance = None;
    assert_denied(store, node, &direct).await;
    store
        .set_edge_session_model_snapshot(
            &fixture.bob,
            tenant,
            &mapping.browser_session,
            Some(snapshot),
        )
        .await
        .unwrap();

    let mut tx = store.database().begin().await.unwrap();
    let pending = store
        .accept_computer_model_request_in(
            &mut tx,
            &principal,
            "revoked-computer-request",
            &"c".repeat(64),
            2,
            now + 6,
        )
        .await
        .unwrap();
    store
        .begin_computer_model_attempt_in(&mut tx, &principal, &pending, 1, now + 6)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    store
        .revoke_owned_executor(&fixture.bob, tenant, &source_id, now + 7)
        .await
        .unwrap();
    let mut tx = store.database().begin().await.unwrap();
    assert!(
        store
            .begin_computer_model_attempt_in(&mut tx, &principal, &pending, 2, now + 8)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    store
        .finish_computer_model_attempt(
            tenant,
            &pending,
            &report(1, Some(ErrorCode::Cancelled)),
            now + 9,
        )
        .await
        .unwrap();
    store
        .finish_computer_model_request(
            tenant,
            &pending,
            "cancelled",
            Some("source_revoked"),
            now + 9,
        )
        .await
        .unwrap();
    store
        .set_edge_session_model_snapshot(
            &fixture.bob,
            tenant,
            &mapping.browser_session,
            Some(original.clone()),
        )
        .await
        .unwrap();
}

fn report(attempt: u32, error_code: Option<ErrorCode>) -> ComputerModelAttempt {
    ComputerModelAttempt::Finished {
        attempt,
        http_status: Some(if error_code.is_some() { 503 } else { 200 }),
        usage: Some(ReportedModelUsage {
            input_tokens: Some(11),
            output_tokens: Some(7),
            cached_input_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
        }),
        upstream_request_id: Some(format!("source-attempt-{attempt}")),
        error_code,
    }
}
