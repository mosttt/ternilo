use super::*;
use crate::PlatformRole;
use ternilo_protocol::ErrorCode;

#[expect(
    clippy::too_many_lines,
    reason = "Reconciliation, races, immutable audit and quota accounting share the same accepted request on both databases."
)]
pub(super) async fn verify(
    store: &ControlStore,
    owner: &crate::ControlUser,
    alice: &crate::ControlUser,
    bob: &crate::ControlUser,
    unknown_id: &str,
    known_id: &str,
    grant_id: &str,
) {
    let input = ModelUsageReconciliationInput {
        expected_settled_at_ms: NOW + 5,
        usage: known(70, 30).usage.unwrap(),
        reference: "provider-report/request-17".to_owned(),
        note: "Verified the completed upstream request.".to_owned(),
    };
    let later_month = NOW + 35 * 24 * 60 * 60 * 1000;
    let before = store
        .model_service_usage(alice, Some(&alice.user_id), NOW)
        .await
        .unwrap();
    assert!(
        store
            .model_usage_reconciliations(owner, unknown_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .model_usage_reconciliations(alice, unknown_id)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .reconcile_model_usage(alice, unknown_id, 1, &input, later_month)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_account_role(owner, &bob.user_id, PlatformRole::Operator, 1, NOW + 30)
        .await
        .unwrap();
    assert_eq!(
        store
            .reconcile_model_usage(bob, unknown_id, 1, &input, later_month)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_account_role(owner, &bob.user_id, PlatformRole::Auditor, 2, NOW + 31)
        .await
        .unwrap();
    assert!(
        store
            .model_usage_reconciliations(bob, unknown_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .reconcile_model_usage(bob, unknown_id, 1, &input, later_month)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_account_role(owner, &bob.user_id, PlatformRole::Admin, 3, NOW + 32)
        .await
        .unwrap();
    verify_rejected_inputs(store, owner, unknown_id, known_id, &input).await;
    let (first, duplicate) = tokio::join!(
        store.reconcile_model_usage(owner, unknown_id, 1, &input, later_month),
        store.reconcile_model_usage(bob, unknown_id, 1, &input, later_month + 1),
    );
    let first = first.unwrap();
    let duplicate = duplicate.unwrap();
    assert_eq!(first.attempt.accounted_tokens, Some(100));
    assert_eq!(first.attempt.state, ModelRequestState::Cancelled);
    assert_eq!(first.attempt.error_code.as_deref(), Some("access_revoked"));
    assert_eq!(
        first.reconciliation.reconciled_at_ms,
        duplicate.reconciliation.reconciled_at_ms
    );
    assert_eq!(
        first.reconciliation.actor_user_id,
        duplicate.reconciliation.actor_user_id
    );
    let records = store
        .model_usage_reconciliations(owner, unknown_id)
        .await
        .unwrap();
    assert_eq!(
        records.len(),
        1,
        "concurrent retries append only one immutable audit record"
    );
    assert_eq!(records[0].input, input);
    assert_eq!(records[0].previous_usage, None);
    let changed = ModelUsageReconciliationInput {
        note: "different evidence".to_owned(),
        ..input.clone()
    };
    assert_eq!(
        store
            .reconcile_model_usage(owner, unknown_id, 1, &changed, later_month + 2)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        store
            .settle_model_request(unknown_id, &known(80, 30), later_month + 3)
            .await
            .is_err(),
        "late upstream usage cannot overwrite a completed reconciliation"
    );
    let after = store
        .model_service_usage(alice, Some(&alice.user_id), NOW)
        .await
        .unwrap();
    assert_eq!(after.used_tokens, before.used_tokens + 100);
    assert_eq!(after.reserved_tokens, before.reserved_tokens - 200);
    assert_eq!(after.unknown_requests, before.unknown_requests - 1);
    assert_eq!(after.request_count, before.request_count);
    let quota = store
        .get_model_grant(owner, grant_id, NOW)
        .await
        .unwrap()
        .quota;
    assert_eq!(
        (quota.used_tokens, quota.reserved_tokens),
        (after.used_tokens, after.reserved_tokens)
    );
    assert_eq!(
        store
            .model_service_usage(alice, Some(&alice.user_id), later_month)
            .await
            .unwrap()
            .used_tokens,
        0,
        "reconciliation keeps the original accounting month"
    );
    let request = store
        .list_model_service_requests(alice, Some(&alice.user_id), &PageQuery::default())
        .await
        .unwrap()
        .requests
        .into_iter()
        .find(|value| value.request_id == unknown_id)
        .unwrap();
    assert_eq!(request.actor_user_id, alice.user_id);
    assert_eq!(request.model_beneficiary_user_id, alice.user_id);
    assert_eq!(request.state, ModelRequestState::Cancelled);
    assert_eq!(request.settled_at_ms, Some(NOW + 5));
    assert_eq!(request.accounted_tokens, Some(100));
    assert_eq!(
        store
            .model_usage_reconciliations(owner, unknown_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

async fn verify_rejected_inputs(
    store: &ControlStore,
    owner: &crate::ControlUser,
    unknown_id: &str,
    known_id: &str,
    input: &ModelUsageReconciliationInput,
) {
    let stale = ModelUsageReconciliationInput {
        expected_settled_at_ms: 0,
        ..input.clone()
    };
    assert_eq!(
        store
            .reconcile_model_usage(owner, unknown_id, 1, &stale, NOW + 40)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        store
            .reconcile_model_usage(owner, known_id, 1, input, NOW + 40)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut invalid = input.clone();
    invalid.usage.output_tokens = None;
    assert_eq!(
        store
            .reconcile_model_usage(owner, unknown_id, 1, &invalid, NOW + 40)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    invalid = input.clone();
    invalid.usage.raw_usage = Some(serde_json::json!({"forged": "upstream proof"}));
    assert_eq!(
        store
            .reconcile_model_usage(owner, unknown_id, 1, &invalid, NOW + 40)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    invalid = input.clone();
    invalid.reference = " ".to_owned();
    assert_eq!(
        store
            .reconcile_model_usage(owner, unknown_id, 1, &invalid, NOW + 40)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert!(
        store
            .model_usage_reconciliations(owner, unknown_id)
            .await
            .unwrap()
            .is_empty()
    );
}

pub(super) async fn reject_pending(store: &ControlStore, owner: &crate::ControlUser, id: &str) {
    let input = ModelUsageReconciliationInput {
        expected_settled_at_ms: NOW,
        usage: known(70, 30).usage.unwrap(),
        reference: "provider-report".to_owned(),
        note: "Call is still active.".to_owned(),
    };
    assert_eq!(
        store
            .reconcile_model_usage(owner, id, 1, &input, NOW)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert!(
        store
            .model_usage_reconciliations(owner, id)
            .await
            .unwrap()
            .is_empty()
    );
}
