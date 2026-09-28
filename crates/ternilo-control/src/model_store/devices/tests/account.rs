use super::*;
use ternilo_protocol::{ModelDeviceProviderScope, TenantId, UserId};

pub(super) async fn contract(store: &ControlStore, owner: &ControlUser) {
    let now = NOW + 800_000;
    let tenant = store.account_provider_space(&owner.user_id).await.unwrap();
    let provider = profile();
    store
        .put_user_credential(owner, &tenant, "PRIVATE_KEY", "private-secret", now)
        .await
        .unwrap();
    store
        .upsert_user_provider_profile(owner, &tenant, provider.clone(), now)
        .await
        .unwrap();
    let (legacy, _) = grants::connected(
        store,
        owner,
        serde_json::from_value(serde_json::json!({"kind":"account"})).unwrap(),
        now,
    )
    .await;
    assert!(
        store
            .model_device_session(&legacy, None, now + 5001)
            .await
            .unwrap()
            .providers
            .is_empty()
    );
    assert!(
        store
            .list_device_account_models(&legacy, "device-upstream", now + 5001)
            .await
            .is_err()
    );
    let selected = ModelDeviceScope::Selected {
        grants: Vec::new(),
        providers: vec![ModelDeviceProviderScope {
            provider_id: "device-upstream".to_owned(),
            model_ids: vec!["model".to_owned()],
        }],
    };
    let (limited, limited_id) = grants::connected(store, owner, selected, now + 6000).await;
    let catalog = store
        .model_device_session(&limited, None, now + 11001)
        .await
        .unwrap();
    assert!(catalog.grants.is_empty());
    assert_eq!(catalog.providers[0].models.len(), 1);
    let public = serde_json::to_string(&catalog).unwrap();
    for secret in ["private-secret", "PRIVATE_KEY", "private.example"] {
        assert!(!public.contains(secret));
    }
    let permit =
        assert_selected_request_scope_and_idempotency(store, owner, &limited, &limited_id, now)
            .await;
    assert_credential_revocation_and_usage_settlement(
        store,
        owner,
        &tenant,
        &limited,
        &permit.request.request_id,
        now,
    )
    .await;
    store
        .put_user_credential(owner, &tenant, "PRIVATE_KEY", "private-secret", now + 12000)
        .await
        .unwrap();
    future_and_isolation(store, owner, &limited, now + 12000).await;
    store
        .revoke_model_device(owner, &limited_id, now + 30000)
        .await
        .unwrap();
    assert!(
        store
            .reserve_device_account_request(
                &limited,
                "device-upstream",
                &input("revoked", "model"),
                now + 30001
            )
            .await
            .is_err()
    );
}

async fn assert_selected_request_scope_and_idempotency(
    store: &ControlStore,
    owner: &ControlUser,
    limited: &str,
    limited_id: &str,
    now: u64,
) -> crate::ModelRequestPermit {
    assert!(
        store
            .reserve_device_account_request(
                limited,
                "device-upstream",
                &input("outside", "second"),
                now + 11001
            )
            .await
            .is_err()
    );
    let permit = store
        .reserve_device_account_request(
            limited,
            "device-upstream",
            &input("call", "model"),
            now + 11002,
        )
        .await
        .unwrap();
    assert_eq!(
        permit.route.api_key.as_ref().unwrap().as_str(),
        "private-secret"
    );
    assert_eq!(
        permit.request.source,
        crate::ModelRequestSource::UserProvider
    );
    assert_eq!(permit.request.actor_user_id, owner.user_id);
    assert_eq!(permit.request.key_id.as_deref(), Some(limited_id));
    assert!(permit.request.grant_id.is_none());
    assert!(
        !store
            .reserve_device_account_request(
                limited,
                "device-upstream",
                &input("call", "model"),
                now + 11003
            )
            .await
            .unwrap()
            .newly_accepted
    );
    permit
}

async fn assert_credential_revocation_and_usage_settlement(
    store: &ControlStore,
    owner: &ControlUser,
    tenant: &TenantId,
    limited: &str,
    request_id: &str,
    now: u64,
) {
    store
        .mark_model_request_attempted(request_id, now + 11004)
        .await
        .unwrap();
    store
        .delete_user_credential(owner, tenant, "PRIVATE_KEY")
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(request_id, now + 11005)
            .await
            .is_err()
    );
    assert!(
        store
            .model_device_session(limited, None, now + 11005)
            .await
            .unwrap()
            .providers
            .is_empty()
    );
    store
        .settle_model_request(
            request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Cancelled,
                usage: Some(ServiceModelUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(3),
                    ..Default::default()
                }),
                upstream_request_id: None,
                error_code: Some("credential_removed".to_owned()),
            },
            now + 11006,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .model_service_usage_by_source(
                owner,
                Some(&owner.user_id),
                now + 11007,
                Some(crate::ModelRequestSource::UserProvider)
            )
            .await
            .unwrap()
            .used_tokens,
        13
    );
}

async fn future_and_isolation(store: &ControlStore, owner: &ControlUser, limited: &str, now: u64) {
    let (all, _) = grants::connected(
        store,
        owner,
        ModelDeviceScope::Account {
            include_account_providers: true,
        },
        now,
    )
    .await;
    assert_eq!(
        store
            .model_device_session(&all, None, now + 5001)
            .await
            .unwrap()
            .providers[0]
            .models
            .len(),
        2
    );
    let other = store
        .upsert_user(
            &crate::OidcPrincipal {
                issuer: "https://device.example".to_owned(),
                subject: "other-provider-owner".to_owned(),
                email: None,
                display_name: None,
            },
            "other-provider-owner",
            now,
        )
        .await
        .unwrap();
    let tenant = store.account_provider_space(&other.user_id).await.unwrap();
    store
        .put_user_credential(&other, &tenant, "PRIVATE_KEY", "other-secret", now)
        .await
        .unwrap();
    store
        .upsert_user_provider_profile(&other, &tenant, profile(), now)
        .await
        .unwrap();
    let (other_token, _) = grants::connected(
        store,
        &other,
        ModelDeviceScope::Account {
            include_account_providers: true,
        },
        now,
    )
    .await;
    let permit = store
        .reserve_device_account_request(
            &other_token,
            "device-upstream",
            &input("call", "model"),
            now + 5001,
        )
        .await
        .unwrap();
    assert_eq!(
        permit.route.api_key.as_ref().unwrap().as_str(),
        "other-secret"
    );
    assert_ne!(permit.request.actor_user_id, owner.user_id);
    assert_future_models_follow_device_scope(store, owner, &all, limited, now).await;
    assert!(
        store
            .list_device_account_models("knm_not-a-device", "device-upstream", now + 6001)
            .await
            .is_err()
    );
    let mut transaction = store.model_transaction().await.unwrap();
    let wrong_owner = UserId::new("not-the-owner");
    let mut forged = permit.request.clone();
    forged.model_beneficiary_user_id = wrong_owner;
    assert!(
        store
            .check_device_account_request_in(&mut transaction, &forged, now + 6001)
            .await
            .is_err()
    );
    transaction.rollback().await.unwrap();
}

async fn assert_future_models_follow_device_scope(
    store: &ControlStore,
    owner: &ControlUser,
    all: &str,
    limited: &str,
    now: u64,
) {
    let mut changed = profile();
    changed.models.push(ProviderModel {
        id: "future".to_owned(),
        display_name: None,
        settings: ProviderModelSettings::Inherit,
    });
    let personal = store.account_provider_space(&owner.user_id).await.unwrap();
    store
        .upsert_user_provider_profile(owner, &personal, changed, now + 6000)
        .await
        .unwrap();
    assert_eq!(
        store
            .model_device_session(all, None, now + 6001)
            .await
            .unwrap()
            .providers[0]
            .models
            .len(),
        3
    );
    assert_eq!(
        store
            .model_device_session(limited, None, now + 6001)
            .await
            .unwrap()
            .providers[0]
            .models
            .len(),
        1
    );
}

pub(super) fn profile() -> ProviderProfile {
    ProviderProfile {
        id: "device-upstream".to_owned(),
        display_name: "Private".to_owned(),
        base_url: "https://private.example/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: Some("PRIVATE_KEY".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 4096,
            max_output_tokens: 1024,
            reasoning: None,
        },
        models: ["model", "second"]
            .into_iter()
            .map(|id| ProviderModel {
                id: id.to_owned(),
                display_name: None,
                settings: ProviderModelSettings::Inherit,
            })
            .collect(),
        timeout_ms: 30000,
        max_attempts: 3,
        retry_base_delay_ms: 250,
    }
}

fn input(key: &str, model: &str) -> ModelRequestInput {
    ModelRequestInput {
        request_key: key.to_owned(),
        payload_hash: "a".repeat(64),
        model_id: model.to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: 100,
    }
}
