use super::*;

pub(super) async fn contract(fixture: &Fixture) {
    let limits = ModelDeviceLimits {
        monthly_tokens: Some(450),
        max_concurrent_requests: Some(2),
        requests_per_minute: Some(30),
        expires_at_ms: Some(NOW + 2 * LIFETIME_MS),
    };
    let (token, identity) = fixture.connect(&limits, NOW).await;
    let session = fixture
        .store
        .model_device_session(&token, None, NOW + LIFETIME_MS)
        .await
        .unwrap();
    assert_eq!(
        session.identity.limits, limits,
        "device expiry is not the pending authorization TTL"
    );
    assert_eq!(session.grants.len(), 2);
    assert_eq!(session.providers.len(), 1);
    let listing = fixture
        .store
        .list_model_devices(&fixture.owner, &PageQuery::default())
        .await
        .unwrap();
    let saved = listing
        .devices
        .iter()
        .find(|entry| entry.device_id == identity.device_id)
        .unwrap();
    assert_eq!(saved.limits, limits);
    let maximum = ModelDeviceLimits {
        monthly_tokens: Some(9_007_199_254_740_991),
        max_concurrent_requests: Some(10_000),
        requests_per_minute: Some(10_000),
        expires_at_ms: Some(253_402_300_799_999),
    };
    fixture.connect(&maximum, NOW).await;
    invalid_authorizations(fixture).await;
    expires_before_exchange(fixture).await;
}

pub(super) fn invalid_limits() -> Vec<ModelDeviceLimits> {
    vec![
        ModelDeviceLimits {
            requests_per_minute: Some(0),
            ..Default::default()
        },
        ModelDeviceLimits {
            requests_per_minute: Some(10_001),
            ..Default::default()
        },
        ModelDeviceLimits {
            monthly_tokens: Some(0),
            ..Default::default()
        },
        ModelDeviceLimits {
            monthly_tokens: Some(u64::MAX),
            ..Default::default()
        },
        ModelDeviceLimits {
            monthly_tokens: Some(9_007_199_254_740_992),
            ..Default::default()
        },
        ModelDeviceLimits {
            max_concurrent_requests: Some(0),
            ..Default::default()
        },
        ModelDeviceLimits {
            max_concurrent_requests: Some(10_001),
            ..Default::default()
        },
        ModelDeviceLimits {
            expires_at_ms: Some(NOW - 1),
            ..Default::default()
        },
        ModelDeviceLimits {
            expires_at_ms: Some(NOW),
            ..Default::default()
        },
        ModelDeviceLimits {
            expires_at_ms: Some(u64::MAX),
            ..Default::default()
        },
        ModelDeviceLimits {
            expires_at_ms: Some(253_402_300_800_000),
            ..Default::default()
        },
        ModelDeviceLimits {
            expires_at_ms: Some(1_000_000_000_000_000_000),
            ..Default::default()
        },
    ]
}

async fn invalid_authorizations(fixture: &Fixture) {
    let store = &fixture.store;
    let authorization = store
        .begin_model_device_authorization("Invalid", NOW)
        .await
        .unwrap();
    let scope = ModelDeviceScope::Account {
        include_account_providers: true,
    };
    for limits in invalid_limits() {
        let error = store
            .decide_model_device_authorization(
                &fixture.owner,
                &authorization.user_code,
                Some(&scope),
                &limits,
                NOW,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{limits:?}");
    }
    assert!(matches!(
        store
            .poll_model_device_authorization(&authorization.device_code, NOW + 5000)
            .await
            .unwrap(),
        ModelDevicePoll::Pending { .. }
    ));
    store
        .decide_model_device_authorization(
            &fixture.owner,
            &authorization.user_code,
            None,
            &ModelDeviceLimits {
                monthly_tokens: Some(0),
                ..Default::default()
            },
            NOW + 5001,
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .poll_model_device_authorization(&authorization.device_code, NOW + 10_000)
            .await
            .unwrap(),
        ModelDevicePoll::Denied
    ));
}

async fn expires_before_exchange(fixture: &Fixture) {
    let store = &fixture.store;
    let authorization = store
        .begin_model_device_authorization("Too late", NOW)
        .await
        .unwrap();
    store
        .decide_model_device_authorization(
            &fixture.owner,
            &authorization.user_code,
            Some(&ModelDeviceScope::Account {
                include_account_providers: true,
            }),
            &ModelDeviceLimits {
                expires_at_ms: Some(NOW + 2000),
                ..Default::default()
            },
            NOW + 1,
        )
        .await
        .unwrap();
    for now in [NOW + 2000, NOW + 5000] {
        assert!(matches!(
            store
                .poll_model_device_authorization(&authorization.device_code, now)
                .await
                .unwrap(),
            ModelDevicePoll::Expired
        ));
    }
    let devices = store
        .list_model_devices(&fixture.owner, &PageQuery::default())
        .await
        .unwrap();
    assert!(
        devices
            .devices
            .iter()
            .all(|device| device.device_name != "Too late")
    );
    let default = ModelDeviceLimits::default();
    let (_, identity) = fixture.connect(&default, NOW).await;
    assert_eq!(identity.limits, default);
}
