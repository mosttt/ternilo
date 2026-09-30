use super::super::super::{
    ModelGrantInput, ModelGrantSubject, ModelProviderInput, ModelPublicationInput,
    ModelRequestInput, ModelRequestOrigin, ModelRequestSettlement, ModelRequestState,
    ServiceModelUsage,
};
use super::*;
use crate::{NativeRegistration, PageQuery, SecretCipher};
use sqlx::Executor;
use ternilo_protocol::{
    ModelDeviceGrantScope, ModelDevicePoll, ModelDeviceScope, ProviderModel, ProviderModelDefaults,
    ProviderModelSettings, ProviderProfile, ProviderProtocol,
};

const NOW: u64 = 1_800_000_000_000;

async fn setup(store: &ControlStore) -> (ControlUser, String) {
    let actor = store
        .initialize_owner(
            &NativeRegistration {
                email: "device-owner@example.test".to_owned(),
                username: "device-owner".to_owned(),
                password: "test-password-device".to_owned(),
            },
            NOW,
        )
        .await
        .unwrap()
        .session
        .user;
    store
        .save_model_provider(
            &actor,
            &ModelProviderInput {
                profile: ProviderProfile {
                    id: "device-upstream".to_owned(),
                    display_name: "Device upstream".to_owned(),
                    base_url: "https://model.example/v1".to_owned(),
                    protocol: ProviderProtocol::OpenAiResponses,
                    api_key_ref: None,
                    defaults: ProviderModelDefaults {
                        context_window: 4096,
                        max_output_tokens: 1024,
                        reasoning: None,
                    },
                    models: vec![ProviderModel {
                        id: "internal".to_owned(),
                        display_name: None,
                        settings: ProviderModelSettings::Inherit,
                    }],
                    timeout_ms: 30_000,
                    max_attempts: 1,
                    retry_base_delay_ms: 250,
                },
                enabled: true,
                api_key: Some("upstream-secret".to_owned()),
                clear_api_key: false,
            },
            NOW,
        )
        .await
        .unwrap();
    store
        .save_model_publication(
            &actor,
            &ModelPublicationInput {
                model_id: "model".to_owned(),
                display_name: "Model".to_owned(),
                provider_id: "device-upstream".to_owned(),
                upstream_model: "internal".to_owned(),
                enabled: true,
            },
            NOW,
        )
        .await
        .unwrap();
    let grant = store
        .save_model_grant(
            &actor,
            None,
            &ModelGrantInput {
                name: "Personal allowance".to_owned(),
                subject: ModelGrantSubject::User {
                    id: actor.user_id.as_str().to_owned(),
                },
                model_ids: vec!["model".to_owned()],
                monthly_tokens: 10_000,
                max_concurrent_requests: 2,
                expires_at_ms: None,
                allow_resource_sharing: false,
            },
            NOW,
        )
        .await
        .unwrap();
    (actor, grant.grant_id)
}

#[expect(
    clippy::too_many_lines,
    reason = "Sequential authorization and ledger lifecycle contract"
)]
async fn contract(store: &ControlStore) {
    let (actor, grant) = setup(store).await;
    let authorization = store
        .begin_model_device_authorization("My laptop", NOW)
        .await
        .unwrap();
    assert_eq!(authorization.interval, 5);
    assert!(matches!(
        store
            .poll_model_device_authorization(&authorization.device_code, NOW + 1)
            .await
            .unwrap(),
        ModelDevicePoll::SlowDown { interval: 10 }
    ));
    assert!(matches!(
        store
            .poll_model_device_authorization(&authorization.device_code, NOW + 10_001)
            .await
            .unwrap(),
        ModelDevicePoll::Pending { interval: 10 }
    ));
    let review = store
        .review_model_device_authorization(&actor, &authorization.user_code, NOW + 10_002)
        .await
        .unwrap();
    assert_eq!(review.device_name, "My laptop");
    store
        .decide_model_device_authorization(
            &actor,
            &authorization.user_code,
            Some(&ModelDeviceScope::Account {
                include_account_providers: false,
            }),
            &ModelDeviceLimits::default(),
            NOW + 10_003,
        )
        .await
        .unwrap();
    assert!(
        store
            .decide_model_device_authorization(
                &actor,
                &authorization.user_code,
                None,
                &ModelDeviceLimits::default(),
                NOW + 10_004
            )
            .await
            .is_err()
    );
    let (first, second) = tokio::join!(
        store.poll_model_device_authorization(&authorization.device_code, NOW + 20_001),
        store.poll_model_device_authorization(&authorization.device_code, NOW + 20_001)
    );
    let results = [first.unwrap(), second.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, ModelDevicePoll::Authorized { .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, ModelDevicePoll::Expired))
            .count(),
        1
    );
    let (token, session) = results
        .into_iter()
        .find_map(|result| {
            if let ModelDevicePoll::Authorized { token, session } = result {
                Some((token, session))
            } else {
                None
            }
        })
        .unwrap();
    assert!(token.starts_with("ter_d_"));
    assert_eq!(session.identity.user_id, actor.user_id);
    assert_eq!(session.grants[0].grant_id, grant);
    assert_eq!(session.grants[0].models[0].model_id, "model");
    assert_eq!(
        store
            .list_model_keys(&actor, &PageQuery::default())
            .await
            .unwrap()
            .keys
            .len(),
        0
    );
    let devices = store
        .list_model_devices(&actor, &PageQuery::default())
        .await
        .unwrap();
    assert_eq!(devices.devices.len(), 1);
    assert_eq!(devices.devices[0].device_name, "My laptop");
    assert!(!serde_json::to_string(&devices).unwrap().contains(&token));
    let mut tx = store.model_transaction().await.unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT token_hash FROM control_model_devices WHERE device_id=$1")
            .bind(&session.identity.device_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(stored, hex(&token_hash(&token)));
    tx.commit().await.unwrap();
    let input = ModelRequestInput {
        request_key: "device-request".to_owned(),
        payload_hash: "a".repeat(64),
        model_id: "model".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: 200,
    };
    let permit = store
        .reserve_device_model_request(&token, &grant, &input, NOW + 20_010)
        .await
        .unwrap();
    assert_eq!(permit.request.origin, ModelRequestOrigin::ClientDevice);
    assert_eq!(permit.request.actor_user_id, actor.user_id);
    assert_eq!(
        permit.request.key_id.as_deref(),
        Some(session.identity.device_id.as_str())
    );
    store
        .mark_model_request_attempted(&permit.request.request_id, NOW + 20_011)
        .await
        .unwrap();
    store
        .disconnect_model_device(&token, NOW + 20_012)
        .await
        .unwrap();
    assert!(store.list_key_models(&token, NOW + 20_013).await.is_err());
    assert!(
        store
            .check_model_request_authorized(&permit.request.request_id, NOW + 20_013)
            .await
            .is_err()
    );
    let settled = store
        .settle_model_request(
            &permit.request.request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Cancelled,
                usage: Some(ServiceModelUsage {
                    input_tokens: Some(15),
                    output_tokens: Some(5),
                    ..Default::default()
                }),
                upstream_request_id: None,
                error_code: Some("revoked".to_owned()),
            },
            NOW + 20_014,
        )
        .await
        .unwrap();
    assert_eq!(settled.accounted_tokens, Some(20));
    let denied = store
        .begin_model_device_authorization("Denied", NOW + 30_000)
        .await
        .unwrap();
    store
        .decide_model_device_authorization(
            &actor,
            &denied.user_code,
            None,
            &ModelDeviceLimits::default(),
            NOW + 30_001,
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .poll_model_device_authorization(&denied.device_code, NOW + 35_000)
            .await
            .unwrap(),
        ModelDevicePoll::Denied
    ));
    let expired = store
        .begin_model_device_authorization("Expired", NOW + 40_000)
        .await
        .unwrap();
    assert!(matches!(
        store
            .poll_model_device_authorization(&expired.device_code, NOW + 40_000 + LIFETIME_MS)
            .await
            .unwrap(),
        ModelDevicePoll::Expired
    ));
    assert!(
        store
            .review_model_device_authorization(
                &actor,
                &expired.user_code,
                NOW + 40_000 + LIFETIME_MS
            )
            .await
            .is_err()
    );
    multi_grant_contract(store, &actor, &grant).await;
    Box::pin(budget::budget_contract(store, &actor)).await;
    account_revocation_contract(store, &actor).await;
    account::contract(store, &actor).await;
}

#[tokio::test]
async fn sqlite_device_authorization_and_revocation_use_the_existing_model_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("device.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([47; 32]), 8)
        .await
        .unwrap();
    Box::pin(contract(&store)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_device_authorization_is_atomic_under_runtime_rls() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    crate::postgres_test::prepare_role(&admin, "ternilo_device_test", "device-test-password").await;
    let owner = ControlStore::connect(&url, None, SecretCipher::from_key([47; 32]), 1)
        .await
        .unwrap();
    owner.database().close().await;
    let mut runtime = url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime.set_username("ternilo_device_test").unwrap();
    runtime.set_password(Some("device-test-password")).unwrap();
    let store = ControlStore::connect(
        runtime.as_str(),
        Some(&url),
        SecretCipher::from_key([47; 32]),
        8,
    )
    .await
    .unwrap();
    Box::pin(contract(&store)).await;
    let visible: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_model_device_authorizations")
            .fetch_one(store.database.pool())
            .await
            .unwrap();
    assert_eq!(visible, 0);
    store.database().close().await;
    admin.close().await;
}

mod account;
mod budget;
mod grants;
mod lifecycle;
mod limits;
use grants::multi_grant_contract;
use lifecycle::account_revocation_contract;
