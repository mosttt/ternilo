use super::*;
use chrono::TimeZone;

pub(super) async fn shared_budget(fixture: &Fixture) {
    let limits = ModelDeviceLimits {
        monthly_tokens: Some(300),
        max_concurrent_requests: Some(2),
        requests_per_minute: None,
        expires_at_ms: None,
    };
    let (token, device) = fixture.connect(&limits, NOW).await;
    let id = &device.device_id;
    let now = NOW + 6000;
    let first = fixture
        .reserve(&token, Source::First, &input("first", 100), now)
        .await
        .unwrap();
    let second = fixture
        .reserve(&token, Source::Second, &input("second", 100), now)
        .await
        .unwrap();
    assert_quota(
        fixture
            .reserve(&token, Source::Account, &input("third", 100), now)
            .await,
    );
    let retry = fixture
        .reserve(&token, Source::First, &input("first", 100), now)
        .await
        .unwrap();
    assert!(!retry.newly_accepted);
    assert_eq!(retry.request.request_id, first.request.request_id);
    assert_eq!(fixture.usage(id, now).await, (0, 200, 2));
    fixture
        .store
        .mark_model_request_attempted(&first.request.request_id, now)
        .await
        .unwrap();
    fixture
        .settle(&first.request.request_id, Some(40), now)
        .await;
    let account = fixture
        .reserve(&token, Source::Account, &input("third", 150), now)
        .await
        .unwrap();
    assert_eq!(fixture.usage(id, now).await, (40, 250, 2));
    fixture
        .store
        .mark_model_request_attempted(&second.request.request_id, now)
        .await
        .unwrap();
    fixture.settle(&second.request.request_id, None, now).await;
    fixture
        .store
        .mark_model_request_attempted(&account.request.request_id, now)
        .await
        .unwrap();
    fixture
        .settle(&account.request.request_id, Some(20), now)
        .await;
    assert_eq!(fixture.usage(id, now).await, (60, 100, 0));
    for source in [Source::First, Source::Second, Source::Account] {
        assert_quota(
            fixture
                .reserve(&token, source, &input("over-budget", 141), now)
                .await,
        );
    }
    assert_eq!(
        fixture.usage(id, now).await,
        (60, 100, 0),
        "rejected requests create no ledger rows"
    );
    fixture
        .settle(&second.request.request_id, Some(80), now + 1)
        .await;
    fixture
        .settle(&second.request.request_id, Some(80), now + 2)
        .await;
    assert_eq!(fixture.usage(id, now + 2).await, (140, 0, 0));
    grant_totals(fixture, now).await;
    let accepted = fixture
        .reserve(
            &token,
            Source::Account,
            &input("exact-budget", 160),
            now + 3,
        )
        .await
        .unwrap();
    assert_eq!(fixture.usage(id, now + 3).await, (140, 160, 1));
    fixture
        .settle(&accepted.request.request_id, None, now + 4)
        .await;
    assert_eq!(fixture.usage(id, now + 4).await, (140, 0, 0));
    independent_device(fixture, id, now).await;
}

async fn grant_totals(fixture: &Fixture, now: u64) {
    let first_usage = fixture
        .store
        .get_model_grant(&fixture.admin, &fixture.grants[0], now)
        .await
        .unwrap();
    let second_usage = fixture
        .store
        .get_model_grant(&fixture.admin, &fixture.grants[1], now)
        .await
        .unwrap();
    assert_eq!(first_usage.quota.used_tokens, 40);
    assert_eq!(second_usage.quota.used_tokens, 80);
}

async fn independent_device(fixture: &Fixture, id: &str, now: u64) {
    let (other, identity) = fixture.connect(&ModelDeviceLimits::default(), NOW).await;
    let other_request = fixture
        .reserve(&other, Source::Account, &input("other-device", 1000), now)
        .await
        .unwrap();
    assert_eq!(fixture.usage(&identity.device_id, now).await, (0, 1000, 1));
    assert_eq!(fixture.usage(id, now).await, (140, 0, 0));
    fixture
        .settle(&other_request.request.request_id, None, now)
        .await;
}

pub(super) fn assert_quota(result: Result<ModelRequestPermit, ModelAccessError>) {
    assert_eq!(
        result.err().expect("request should exceed the quota").kind,
        ModelAccessErrorKind::QuotaExceeded
    );
}

pub(super) async fn expiry(fixture: &Fixture) {
    let limits = ModelDeviceLimits {
        expires_at_ms: Some(NOW + 7000),
        ..Default::default()
    };
    let (token, device) = fixture.connect(&limits, NOW).await;
    let store = &fixture.store;
    let first = fixture
        .reserve(&token, Source::First, &input("expiring", 100), NOW + 6000)
        .await
        .unwrap();
    let account = fixture
        .reserve(&token, Source::Account, &input("expiring", 150), NOW + 6000)
        .await
        .unwrap();
    for request in [&first.request, &account.request] {
        store
            .mark_model_request_attempted(&request.request_id, NOW + 6001)
            .await
            .unwrap();
        store
            .check_model_request_authorized(&request.request_id, NOW + 6999)
            .await
            .unwrap();
        assert_eq!(
            store
                .check_model_request_authorized(&request.request_id, NOW + 7000)
                .await
                .unwrap_err()
                .kind,
            ModelAccessErrorKind::Unauthorized
        );
    }
    assert_eq!(
        store
            .model_device_session(&token, None, NOW + 7000)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    assert_eq!(
        store
            .list_device_models(&token, &fixture.grants[0], NOW + 7000)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    assert_eq!(
        store
            .list_device_account_models(&token, "device-upstream", NOW + 7000)
            .await
            .unwrap_err()
            .kind,
        ModelAccessErrorKind::Unauthorized
    );
    for source in [Source::First, Source::Second, Source::Account] {
        assert_eq!(
            fixture
                .reserve(&token, source, &input("expired", 10), NOW + 7000)
                .await
                .err()
                .unwrap()
                .kind,
            ModelAccessErrorKind::Unauthorized
        );
    }
    fixture
        .settle(&first.request.request_id, None, NOW + 7001)
        .await;
    fixture
        .settle(&account.request.request_id, None, NOW + 7001)
        .await;
    assert_eq!(
        fixture.usage(&device.device_id, NOW + 7001).await,
        (0, 250, 0)
    );
    fixture
        .settle(&first.request.request_id, Some(30), NOW + 9000)
        .await;
    fixture
        .settle(&account.request.request_id, Some(50), NOW + 9000)
        .await;
    assert_eq!(
        fixture.usage(&device.device_id, NOW + 9000).await,
        (80, 0, 0)
    );
}

pub(super) async fn month_boundary(fixture: &Fixture) {
    let next = u64::try_from(
        chrono::Utc
            .with_ymd_and_hms(2027, 2, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap();
    let limits = ModelDeviceLimits {
        monthly_tokens: Some(100),
        max_concurrent_requests: Some(1),
        requests_per_minute: None,
        expires_at_ms: None,
    };
    let (token, device) = fixture.connect(&limits, next - 6000).await;
    let id = &device.device_id;
    let previous = fixture
        .reserve(&token, Source::Account, &input("last-month", 100), next - 1)
        .await
        .unwrap();
    fixture
        .store
        .mark_model_request_attempted(&previous.request.request_id, next - 1)
        .await
        .unwrap();
    let current = fixture
        .store
        .model_device_usage(&fixture.owner, id, next)
        .await
        .unwrap();
    assert_eq!(current.month, "2027-02");
    assert_eq!(
        (
            current.used_tokens,
            current.reserved_tokens,
            current.active_requests
        ),
        (0, 0, 1)
    );
    assert_quota(
        fixture
            .reserve(&token, Source::Second, &input("new-month", 100), next)
            .await,
    );
    fixture
        .settle(&previous.request.request_id, None, next)
        .await;
    assert_eq!(fixture.usage(id, next - 1).await, (0, 100, 0));
    assert_eq!(fixture.usage(id, next).await, (0, 0, 0));
    let accepted = fixture
        .reserve(&token, Source::Second, &input("new-month", 100), next)
        .await
        .unwrap();
    fixture
        .settle(&previous.request.request_id, Some(40), next + 1)
        .await;
    assert_eq!(fixture.usage(id, next - 1).await, (40, 0, 1));
    assert_eq!(fixture.usage(id, next + 1).await, (0, 100, 1));
    fixture
        .store
        .mark_model_request_attempted(&accepted.request.request_id, next + 1)
        .await
        .unwrap();
    let expired = accepted.request.expires_at_ms;
    assert_eq!(fixture.usage(id, expired).await, (0, 100, 0));
    assert_quota(
        fixture
            .reserve(
                &token,
                Source::Account,
                &input("unknown-expired", 1),
                expired,
            )
            .await,
    );
    fixture
        .store
        .expire_model_requests(expired, 100)
        .await
        .unwrap();
    assert_eq!(fixture.usage(id, expired).await, (0, 100, 0));
    fixture
        .settle(&accepted.request.request_id, Some(60), expired + 1)
        .await;
    assert_eq!(fixture.usage(id, expired + 1).await, (60, 0, 0));
}
