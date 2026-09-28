use super::*;
use accounting::assert_quota;

pub(super) async fn contract(fixture: &Fixture) {
    Box::pin(compete(
        fixture,
        &ModelDeviceLimits {
            max_concurrent_requests: Some(1),
            ..Default::default()
        },
    ))
    .await;
    Box::pin(compete(
        fixture,
        &ModelDeviceLimits {
            monthly_tokens: Some(100),
            ..Default::default()
        },
    ))
    .await;
    retries(fixture).await;
    Box::pin(compete(
        fixture,
        &ModelDeviceLimits {
            requests_per_minute: Some(1),
            ..Default::default()
        },
    ))
    .await;
    lowering(fixture).await;
    grant_still_applies(fixture).await;
}

async fn compete(fixture: &Fixture, limits: &ModelDeviceLimits) {
    let (token, device) = fixture.connect(limits, NOW).await;
    let request = input("racing", 100);
    let results = tokio::join!(
        fixture.reserve(&token, Source::First, &request, NOW + 6000),
        fixture.reserve(&token, Source::Second, &request, NOW + 6000),
        fixture.reserve(&token, Source::Account, &request, NOW + 6000),
    );
    let mut accepted = Vec::new();
    for result in [results.0, results.1, results.2] {
        match result {
            Ok(permit) => accepted.push(permit),
            Err(error) => assert_eq!(error.kind, ModelAccessErrorKind::QuotaExceeded),
        }
    }
    assert_eq!(
        accepted.len(),
        1,
        "one shared device allowance, not one per source"
    );
    assert_eq!(
        fixture.usage(&device.device_id, NOW + 6000).await,
        (0, 100, 1)
    );
    fixture
        .settle(&accepted[0].request.request_id, None, NOW + 6001)
        .await;
}

async fn retries(fixture: &Fixture) {
    for source in [Source::First, Source::Account] {
        let limits = ModelDeviceLimits {
            monthly_tokens: Some(100),
            max_concurrent_requests: Some(1),
            expires_at_ms: None,
            requests_per_minute: Some(1),
        };
        let (token, device) = fixture.connect(&limits, NOW).await;
        let request = input("idempotent", 100);
        let (first, second) = tokio::join!(
            fixture.reserve(&token, source, &request, NOW + 6000),
            fixture.reserve(&token, source, &request, NOW + 6000),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.newly_accepted, second.newly_accepted);
        assert_eq!(first.request.request_id, second.request.request_id);
        assert_eq!(
            fixture.usage(&device.device_id, NOW + 6000).await,
            (0, 100, 1)
        );
        fixture
            .settle(&first.request.request_id, None, NOW + 6001)
            .await;
    }
}

async fn lowering(fixture: &Fixture) {
    let (token, device) = fixture.connect(&ModelDeviceLimits::default(), NOW).await;
    let first = fixture
        .reserve(&token, Source::First, &input("active", 100), NOW + 6000)
        .await
        .unwrap();
    let account = fixture
        .reserve(&token, Source::Account, &input("active", 100), NOW + 6000)
        .await
        .unwrap();
    let limits = ModelDeviceLimits {
        monthly_tokens: Some(1),
        max_concurrent_requests: Some(1),
        expires_at_ms: None,
        requests_per_minute: None,
    };
    fixture
        .store
        .update_model_device_limits(&fixture.owner, &device.device_id, &limits, NOW + 6001)
        .await
        .unwrap();
    for request in [&first.request, &account.request] {
        fixture
            .store
            .mark_model_request_attempted(&request.request_id, NOW + 6002)
            .await
            .unwrap();
        fixture
            .store
            .check_model_request_authorized(&request.request_id, NOW + 6003)
            .await
            .unwrap();
    }
    for source in [Source::First, Source::Account] {
        let retry = fixture
            .reserve(&token, source, &input("active", 100), NOW + 6004)
            .await
            .unwrap();
        assert!(!retry.newly_accepted);
        assert_quota(
            fixture
                .reserve(&token, source, &input("after-lowering", 1), NOW + 6004)
                .await,
        );
    }
    assert_eq!(
        fixture.usage(&device.device_id, NOW + 6004).await,
        (0, 200, 2)
    );
    for request in [&first.request, &account.request] {
        fixture
            .settle(&request.request_id, Some(10), NOW + 6005)
            .await;
    }
    fixture
        .store
        .update_model_device_limits(
            &fixture.owner,
            &device.device_id,
            &ModelDeviceLimits::default(),
            NOW + 6006,
        )
        .await
        .unwrap();
    let next = fixture
        .reserve(
            &token,
            Source::Account,
            &input("cleared", 10_001),
            NOW + 6007,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.usage(&device.device_id, NOW + 6007).await,
        (20, 10_001, 1)
    );
    fixture
        .settle(&next.request.request_id, None, NOW + 6008)
        .await;
}

async fn grant_still_applies(fixture: &Fixture) {
    let limits = ModelDeviceLimits {
        monthly_tokens: Some(20_000),
        max_concurrent_requests: Some(10),
        expires_at_ms: None,
        requests_per_minute: None,
    };
    let (token, _) = fixture.connect(&limits, NOW).await;
    assert_quota(
        fixture
            .reserve(
                &token,
                Source::First,
                &input("grant-tokens", 10_001),
                NOW + 6000,
            )
            .await,
    );
    let first = fixture
        .reserve(&token, Source::First, &input("slot-one", 1), NOW + 6000)
        .await
        .unwrap();
    let second = fixture
        .reserve(&token, Source::First, &input("slot-two", 1), NOW + 6000)
        .await
        .unwrap();
    assert_quota(
        fixture
            .reserve(
                &token,
                Source::First,
                &input("grant-concurrency", 1),
                NOW + 6000,
            )
            .await,
    );
    fixture
        .settle(&first.request.request_id, None, NOW + 6001)
        .await;
    fixture
        .settle(&second.request.request_id, None, NOW + 6001)
        .await;
}
