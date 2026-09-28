use super::*;
use chrono::TimeZone;

fn year_boundary() -> u64 {
    u64::try_from(
        chrono::Utc
            .with_ymd_and_hms(2028, 1, 1, 0, 0, 0)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap()
}

pub(super) async fn contract(fixture: &Fixture) {
    let boundary = year_boundary();
    let limits = ModelDeviceLimits {
        requests_per_minute: Some(2),
        ..Default::default()
    };
    let (token, device) = fixture.connect(&limits, boundary - 6000).await;
    let first = fixture
        .reserve(
            &token,
            Source::First,
            &input("rate-first", 10),
            boundary - 1,
        )
        .await
        .unwrap();
    let retry = fixture
        .reserve(&token, Source::First, &input("rate-first", 10), boundary)
        .await
        .unwrap();
    assert!(!retry.newly_accepted);
    complete(fixture, &first.request.request_id, Some(1), boundary).await;
    let second = fixture
        .reserve(
            &token,
            Source::Account,
            &input("rate-account", 10),
            boundary,
        )
        .await
        .unwrap();
    complete(fixture, &second.request.request_id, None, boundary + 1).await;
    let error = fixture
        .reserve(
            &token,
            Source::Second,
            &input("rate-blocked", 10),
            boundary + 1,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ModelAccessErrorKind::QuotaExceeded);
    assert!(error.error.to_string().contains("request rate limit"));
    let retry = fixture
        .reserve(
            &token,
            Source::Account,
            &input("rate-account", 10),
            boundary + 2,
        )
        .await
        .unwrap();
    assert!(!retry.newly_accepted);
    assert_eq!(retry.request.request_id, second.request.request_id);
    accounting::assert_quota(
        fixture
            .reserve(
                &token,
                Source::Second,
                &input("rate-blocked", 10),
                boundary + 59_998,
            )
            .await,
    );
    let third = fixture
        .reserve(
            &token,
            Source::Second,
            &input("rate-blocked", 10),
            boundary + 59_999,
        )
        .await
        .unwrap();
    assert!(third.newly_accepted);
    complete(
        fixture,
        &third.request.request_id,
        Some(1),
        boundary + 59_999,
    )
    .await;
    let fourth = fixture
        .reserve(
            &token,
            Source::First,
            &input("rate-after-window", 10),
            boundary + 60_000,
        )
        .await
        .unwrap();
    complete(
        fixture,
        &fourth.request.request_id,
        Some(1),
        boundary + 60_000,
    )
    .await;
    independent_and_clear(fixture, &token, &device.device_id, &limits, boundary).await;
}

async fn complete(fixture: &Fixture, request: &str, tokens: Option<u64>, now: u64) {
    fixture
        .store
        .mark_model_request_attempted(request, now)
        .await
        .unwrap();
    fixture.settle(request, tokens, now).await;
}

async fn independent_and_clear(
    fixture: &Fixture,
    token: &str,
    id: &str,
    limits: &ModelDeviceLimits,
    boundary: u64,
) {
    let (independent, _) = fixture.connect(limits, boundary + 60_000).await;
    let permit = fixture
        .reserve(
            &independent,
            Source::Account,
            &input("rate-independent", 10),
            boundary + 65_000,
        )
        .await
        .unwrap();
    complete(
        fixture,
        &permit.request.request_id,
        Some(1),
        boundary + 65_000,
    )
    .await;
    fixture
        .store
        .update_model_device_limits(
            &fixture.owner,
            id,
            &ModelDeviceLimits::default(),
            boundary + 65_001,
        )
        .await
        .unwrap();
    let permit = fixture
        .reserve(
            token,
            Source::Account,
            &input("rate-cleared", 10),
            boundary + 65_001,
        )
        .await
        .unwrap();
    complete(
        fixture,
        &permit.request.request_id,
        Some(1),
        boundary + 65_001,
    )
    .await;
    fixture
        .store
        .update_model_device_limits(&fixture.owner, id, limits, boundary + 65_002)
        .await
        .unwrap();
    accounting::assert_quota(
        fixture
            .reserve(
                token,
                Source::Second,
                &input("rate-reenabled", 10),
                boundary + 65_002,
            )
            .await,
    );
}
