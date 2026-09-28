use super::*;

pub(super) async fn connected(
    store: &ControlStore,
    actor: &ControlUser,
    scope: ModelDeviceScope,
    now: u64,
) -> (String, String) {
    let auth = store
        .begin_model_device_authorization("Account device", now)
        .await
        .unwrap();
    store
        .decide_model_device_authorization(
            actor,
            &auth.user_code,
            Some(&scope),
            &ModelDeviceLimits::default(),
            now + 1,
        )
        .await
        .unwrap();
    let ModelDevicePoll::Authorized { token, session } = store
        .poll_model_device_authorization(&auth.device_code, now + 5000)
        .await
        .unwrap()
    else {
        panic!("device did not connect")
    };
    (token, session.identity.device_id)
}

#[expect(
    clippy::too_many_lines,
    reason = "Sequential cross-grant authorization contract"
)]
pub(super) async fn multi_grant_contract(store: &ControlStore, actor: &ControlUser, first: &str) {
    let now = NOW + 70_000;
    let (token, id) = connected(
        store,
        actor,
        ModelDeviceScope::Account {
            include_account_providers: false,
        },
        now,
    )
    .await;
    let second = store
        .save_model_grant(
            actor,
            None,
            &ModelGrantInput {
                name: "Second budget".to_owned(),
                subject: ModelGrantSubject::User {
                    id: actor.user_id.to_string(),
                },
                model_ids: vec!["model".to_owned()],
                monthly_tokens: 4000,
                max_concurrent_requests: 1,
                expires_at_ms: None,
                allow_resource_sharing: false,
            },
            now + 6000,
        )
        .await
        .unwrap();
    let session = store
        .model_device_session(&token, None, now + 6001)
        .await
        .unwrap();
    assert_eq!(session.identity.device_id, id);
    assert_eq!(
        session.grants.len(),
        2,
        "new grants are discovered without a second login"
    );
    assert!(
        store
            .authenticate_model_key(&token, now + 6002)
            .await
            .is_err(),
        "device tokens cannot choose an implicit grant through the API key endpoint"
    );
    let (limited, _) = connected(
        store,
        actor,
        ModelDeviceScope::Selected {
            providers: Vec::new(),
            grants: vec![ModelDeviceGrantScope {
                grant_id: first.to_owned(),
                model_ids: vec!["model".to_owned()],
            }],
        },
        now + 7000,
    )
    .await;
    assert_eq!(
        store
            .model_device_session(&limited, None, now + 12001)
            .await
            .unwrap()
            .grants
            .len(),
        1
    );
    assert!(
        store
            .list_device_models(&limited, &second.grant_id, now + 12002)
            .await
            .is_err()
    );
    let input = ModelRequestInput {
        request_key: "same-key".to_owned(),
        payload_hash: "b".repeat(64),
        model_id: "model".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        reserved_tokens: 100,
    };
    let a = store
        .reserve_device_model_request(&token, first, &input, now + 13000)
        .await
        .unwrap();
    let b = store
        .reserve_device_model_request(&token, &second.grant_id, &input, now + 13000)
        .await
        .unwrap();
    assert_ne!(a.request.request_id, b.request.request_id);
    assert_eq!(a.request.grant_id.as_deref(), Some(first));
    assert_eq!(
        b.request.grant_id.as_deref(),
        Some(second.grant_id.as_str())
    );
    store
        .revoke_model_grant(actor, first, now + 14000)
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(&a.request.request_id, now + 14001)
            .await
            .is_err()
    );
    store
        .check_model_request_authorized(&b.request.request_id, now + 14001)
        .await
        .unwrap();
    assert_eq!(
        store
            .model_device_session(&token, None, now + 14001)
            .await
            .unwrap()
            .grants
            .len(),
        1
    );
    assert!(
        store
            .model_device_session(&limited, None, now + 14001)
            .await
            .unwrap()
            .grants
            .is_empty()
    );
    store
        .revoke_model_device(actor, &id, now + 14002)
        .await
        .unwrap();
    assert!(
        store
            .check_model_request_authorized(&b.request.request_id, now + 14003)
            .await
            .is_err()
    );
    assert!(
        store
            .model_device_session(&token, None, now + 14003)
            .await
            .is_err()
    );
}
