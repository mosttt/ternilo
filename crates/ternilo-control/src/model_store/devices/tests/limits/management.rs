use super::*;

pub(super) async fn contract(fixture: &Fixture) {
    let initial = ModelDeviceLimits {
        monthly_tokens: Some(600),
        max_concurrent_requests: Some(2),
        requests_per_minute: Some(30),
        expires_at_ms: Some(NOW + 20_000),
    };
    let (token, mut identity) = fixture.connect(&initial, NOW).await;
    let store = &fixture.store;
    let id = identity.device_id.clone();
    rejected_updates(fixture, &id).await;
    let before = stored_identity(store, &id).await;
    let replacement = ModelDeviceLimits {
        max_concurrent_requests: Some(10_000),
        ..Default::default()
    };
    let updated = store
        .update_model_device_limits(&fixture.owner, &id, &replacement, NOW + 5002)
        .await
        .unwrap();
    assert_eq!(updated.limits, replacement);
    identity.limits = replacement.clone();
    assert_eq!(
        serde_json::to_value(updated).unwrap(),
        serde_json::to_value(&identity).unwrap()
    );
    assert_eq!(stored_identity(store, &id).await, before);
    assert_eq!(
        store
            .model_device_session(&token, None, NOW + 25_000)
            .await
            .unwrap()
            .identity
            .limits,
        replacement,
        "null expiry cleared the previous limit before it expired"
    );
    store
        .update_model_device_limits(&fixture.owner, &id, &replacement, NOW + 25_001)
        .await
        .unwrap();
    let mut transaction = store.model_transaction().await.unwrap();
    let audits: Vec<String> = sqlx::query_scalar("SELECT metadata FROM control_platform_audit WHERE resource_id=$1 AND action='model.device.limits.update'")
        .bind(&id).fetch_all(&mut *transaction).await.unwrap();
    assert_eq!(audits.len(), 1);
    let audit: serde_json::Value = serde_json::from_str(&audits[0]).unwrap();
    assert_eq!(audit["previous"], serde_json::to_value(initial).unwrap());
    assert_eq!(audit["limits"], serde_json::to_value(replacement).unwrap());
    transaction.commit().await.unwrap();
    store
        .revoke_model_device(&fixture.owner, &id, NOW + 25_002)
        .await
        .unwrap();
    assert_eq!(
        store
            .update_model_device_limits(
                &fixture.owner,
                &id,
                &ModelDeviceLimits::default(),
                NOW + 25_003
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(fixture.usage(&id, NOW + 25_003).await, (0, 0, 0));
    assert_eq!(
        store
            .model_device_session(&token, None, NOW + 25_003)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    expired_cannot_be_restored(fixture).await;
}

async fn rejected_updates(fixture: &Fixture, id: &str) {
    let store = &fixture.store;
    for actor in [&fixture.other, &fixture.admin] {
        assert_eq!(
            store
                .model_device_usage(actor, id, NOW + 5001)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
        assert_eq!(
            store
                .update_model_device_limits(actor, id, &ModelDeviceLimits::default(), NOW + 5001)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
    for limits in authorization::invalid_limits() {
        assert_eq!(
            store
                .update_model_device_limits(&fixture.owner, id, &limits, NOW + 5001)
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
}

async fn stored_identity(
    store: &ControlStore,
    id: &str,
) -> (String, String, String, i64, Option<i64>) {
    let mut transaction = store.model_transaction().await.unwrap();
    let row = sqlx::query("SELECT token_hash,user_id,scope_json,created_at_ms,last_used_at_ms FROM control_model_devices WHERE device_id=$1")
        .bind(id).fetch_one(&mut *transaction).await.unwrap();
    let result = (
        row.get("token_hash"),
        row.get("user_id"),
        row.get("scope_json"),
        row.get("created_at_ms"),
        row.get("last_used_at_ms"),
    );
    transaction.commit().await.unwrap();
    result
}

async fn expired_cannot_be_restored(fixture: &Fixture) {
    let limits = ModelDeviceLimits {
        expires_at_ms: Some(NOW + 6000),
        ..Default::default()
    };
    let (token, identity) = fixture.connect(&limits, NOW).await;
    let id = &identity.device_id;
    let error = fixture
        .store
        .update_model_device_limits(
            &fixture.owner,
            id,
            &ModelDeviceLimits::default(),
            NOW + 6000,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(fixture.usage(id, NOW + 6000).await, (0, 0, 0));
    assert_eq!(
        fixture
            .store
            .model_device_session(&token, None, NOW + 6000)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    let (new_token, new_identity) = fixture
        .connect(&ModelDeviceLimits::default(), NOW + 6001)
        .await;
    assert_ne!(new_token, token);
    assert_ne!(new_identity.device_id, identity.device_id);
    fixture
        .store
        .model_device_session(&new_token, None, NOW + 11_001)
        .await
        .unwrap();
}
