use super::grants::connected;
use super::*;
use crate::{ModelAccessErrorKind, ModelKeyInput};

fn request(key: &str, tokens: u64) -> ModelRequestInput {
    ModelRequestInput {
        request_key: key.to_owned(),
        payload_hash: "c".repeat(64),
        model_id: "model".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: tokens,
    }
}

fn settlement(tokens: Option<u64>) -> ModelRequestSettlement {
    ModelRequestSettlement {
        state: ModelRequestState::Cancelled,
        usage: tokens.map(|count| ServiceModelUsage {
            input_tokens: Some(count),
            output_tokens: Some(0),
            ..Default::default()
        }),
        upstream_request_id: None,
        error_code: Some("client_cancelled".to_owned()),
    }
}

pub(super) async fn budget_contract(store: &ControlStore, actor: &ControlUser) {
    let now = NOW + 100_000;
    let grant = store
        .save_model_grant(
            actor,
            None,
            &ModelGrantInput {
                name: "Device shared budget".to_owned(),
                subject: ModelGrantSubject::User {
                    id: actor.user_id.to_string(),
                },
                model_ids: vec!["model".to_owned()],
                monthly_tokens: 500,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: false,
            },
            now,
        )
        .await
        .unwrap();
    let (device, _) = connected(
        store,
        actor,
        ModelDeviceScope::Account {
            include_account_providers: false,
        },
        now,
    )
    .await;
    let key = store
        .create_model_key(
            actor,
            &ModelKeyInput {
                name: "Same budget API key".to_owned(),
                grant_id: grant.grant_id.clone(),
                model_ids: vec!["model".to_owned()],
                monthly_tokens: None,
                max_concurrent_requests: None,
                expires_at_ms: None,
            },
            now + 5000,
        )
        .await
        .unwrap();
    let input = request("shared-concurrency", 300);
    let results = tokio::join!(
        store.reserve_device_model_request(&device, &grant.grant_id, &input, now + 6000),
        store.reserve_model_request(&key.token, &input, now + 6000),
    );
    let ((Ok(accepted), Err(rejected)) | (Err(rejected), Ok(accepted))) = results else {
        panic!("device and API key must share exactly one concurrency slot");
    };
    assert_eq!(rejected.kind, ModelAccessErrorKind::QuotaExceeded);
    assert_eq!(accepted.request.actor_user_id, actor.user_id);
    let duplicate = if accepted.request.origin == ModelRequestOrigin::ClientDevice {
        store
            .reserve_device_model_request(&device, &grant.grant_id, &input, now + 6001)
            .await
            .unwrap()
    } else {
        store
            .reserve_model_request(&key.token, &input, now + 6001)
            .await
            .unwrap()
    };
    assert_eq!(duplicate.request.request_id, accepted.request.request_id);
    assert!(!duplicate.newly_accepted);
    store
        .mark_model_request_attempted(&accepted.request.request_id, now + 6002)
        .await
        .unwrap();
    store
        .settle_model_request(
            &accepted.request.request_id,
            &settlement(Some(30)),
            now + 6003,
        )
        .await
        .unwrap();
    retained_usage_contract(store, actor, &grant.grant_id, &device, now + 7000).await;
}

async fn retained_usage_contract(
    store: &ControlStore,
    actor: &ControlUser,
    grant: &str,
    device: &str,
    now: u64,
) {
    let (revoked, id) = connected(
        store,
        actor,
        ModelDeviceScope::Account {
            include_account_providers: false,
        },
        now,
    )
    .await;
    let pending = store
        .reserve_device_model_request(&revoked, grant, &request("unknown", 400), now + 5001)
        .await
        .unwrap();
    store
        .mark_model_request_attempted(&pending.request.request_id, now + 5002)
        .await
        .unwrap();
    store
        .revoke_model_device(actor, &id, now + 5003)
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(&pending.request.request_id, now + 5004)
            .await
            .is_err()
    );
    store
        .settle_model_request(&pending.request.request_id, &settlement(None), now + 5005)
        .await
        .unwrap();
    assert_eq!(quota(store, actor, grant, now + 5006).await, (30, 400, 0));
    let error = store
        .reserve_device_model_request(device, grant, &request("unknown-budget", 80), now + 5007)
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ModelAccessErrorKind::QuotaExceeded);
    let late = settlement(Some(20));
    let request = store
        .settle_model_request(&pending.request.request_id, &late, now + 5008)
        .await
        .unwrap();
    assert_eq!(request.accounted_tokens, Some(20));
    store
        .settle_model_request(&pending.request.request_id, &late, now + 5009)
        .await
        .unwrap();
    assert_eq!(quota(store, actor, grant, now + 5010).await, (50, 0, 0));
    assert!(
        store
            .settle_model_request(
                &pending.request.request_id,
                &settlement(Some(21)),
                now + 5011
            )
            .await
            .is_err()
    );
    assert!(
        store
            .model_device_session(&revoked, None, now + 5012)
            .await
            .is_err()
    );
    let next = store
        .reserve_device_model_request(
            device,
            grant,
            &self::request("remaining-budget", 450),
            now + 5013,
        )
        .await
        .unwrap();
    assert_eq!(next.request.origin, ModelRequestOrigin::ClientDevice);
    store
        .settle_model_request(&next.request.request_id, &settlement(None), now + 5014)
        .await
        .unwrap();
    assert_eq!(quota(store, actor, grant, now + 5015).await, (50, 0, 0));
}

async fn quota(
    store: &ControlStore,
    actor: &ControlUser,
    grant: &str,
    now: u64,
) -> (u64, u64, u64) {
    let value = store
        .get_model_grant(actor, grant, now)
        .await
        .unwrap()
        .quota;
    (
        value.used_tokens,
        value.reserved_tokens,
        value.active_requests,
    )
}
