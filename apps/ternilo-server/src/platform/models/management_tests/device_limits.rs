use salvo_core::test::ResponseExt as _;

use super::{
    Fixture, NativeSessionGrant, PlatformRole, Response, StatusCode, TestClient, Value, json,
    now_ms,
};
use ternilo_protocol::{ModelDeviceIdentity, ModelDevicePoll};

async fn json_response(mut response: Response, status: StatusCode) -> Value {
    let actual = response.status_code;
    let body = response.take_json::<Value>().await.unwrap();
    assert_eq!(actual, Some(status), "{body}");
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    body
}

async fn authorize(
    fixture: &Fixture,
    account: &NativeSessionGrant,
    limits: Option<Value>,
) -> (String, ModelDeviceIdentity) {
    let authorization = fixture
        .state
        .store
        .begin_model_device_authorization("Scoped laptop", now_ms().unwrap() - 6_000)
        .await
        .unwrap();
    let mut decision = json!({"user_code":authorization.user_code,"scope":{"kind":"account"}});
    if let Some(limits) = limits {
        decision["limits"] = limits;
    }
    let response = fixture
        .request(
            "POST",
            "/api/v1/model-access/device-authorization",
            &account.access_token,
            Some(decision),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::NO_CONTENT));
    let response = TestClient::post("http://server.test/api/v1/model-device/token")
        .json(&json!({"device_code":authorization.device_code}))
        .send(&fixture.service)
        .await;
    match serde_json::from_value(json_response(response, StatusCode::OK).await).unwrap() {
        ModelDevicePoll::Authorized { token, session } => (token, session.identity),
        result => panic!("expected an approved device, received {result:?}"),
    }
}

async fn update(
    fixture: &Fixture,
    account: &NativeSessionGrant,
    device: &ModelDeviceIdentity,
    limits: Value,
    status: StatusCode,
) -> Value {
    json_response(
        fixture
            .request(
                "PATCH",
                &format!("/api/v1/model-access/devices/{}", device.device_id),
                &account.access_token,
                Some(limits),
            )
            .await,
        status,
    )
    .await
}

fn limited(expires_at_ms: u64) -> Value {
    json!({"monthly_tokens":100_000,"max_concurrent_requests":2,"requests_per_minute":30,"expires_at_ms":expires_at_ms})
}

#[tokio::test]
async fn device_limits_round_trip_through_approval_management_and_the_device_catalog() {
    let fixture = Fixture::new().await;
    let limits = limited(now_ms().unwrap() + 60_000);
    let (token, identity) = authorize(&fixture, &fixture.owner, Some(limits.clone())).await;
    assert_eq!(serde_json::to_value(&identity).unwrap()["limits"], limits);
    let list = json_response(
        fixture
            .request(
                "GET",
                "/api/v1/model-access/devices",
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(list["devices"][0]["limits"], limits);
    assert!(!list.to_string().contains(&token));
    assert!(!list.to_string().contains("token_hash"));
    let usage = json_response(
        fixture
            .request(
                "GET",
                &format!("/api/v1/model-access/devices/{}/usage", identity.device_id),
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(usage["month"].as_str().unwrap().len(), 7);
    for counter in ["used_tokens", "reserved_tokens", "active_requests"] {
        assert_eq!(usage[counter], 0);
    }
    let changed = json!({"monthly_tokens":500,"max_concurrent_requests":1,"expires_at_ms":null});
    let result = update(
        &fixture,
        &fixture.owner,
        &identity,
        changed.clone(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(result["limits"], changed);
    assert_eq!(result["device_id"], identity.device_id);
    assert_eq!(
        result["scope"],
        serde_json::to_value(&identity.scope).unwrap()
    );
    let public = json_response(
        fixture
            .request("GET", "/v1/model-device", &token, None)
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(public["identity"]["limits"], changed);
    assert!(!public.to_string().contains(&token));
    fixture.state.store.database().close().await;
}

#[tokio::test]
async fn device_limit_updates_reject_invalid_values_without_changing_the_saved_policy() {
    let fixture = Fixture::new().await;
    let (_, identity) = authorize(&fixture, &fixture.owner, None).await;
    for invalid in [
        json!({"monthly_tokens":0}),
        json!({"monthly_tokens":-1}),
        json!({"monthly_tokens":1.5}),
        json!({"monthly_tokens":u64::MAX}),
        json!({"monthly_tokens":9_007_199_254_740_992_u64}),
        json!({"max_concurrent_requests":0}),
        json!({"max_concurrent_requests":10_001}),
        json!({"expires_at_ms":now_ms().unwrap() - 1}),
        json!({"expires_at_ms":u64::MAX}),
        json!({"expires_at_ms":253_402_300_800_000_u64}),
        json!({"scope":{"kind":"account"}}),
    ] {
        update(
            &fixture,
            &fixture.owner,
            &identity,
            invalid,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let list = json_response(
        fixture
            .request(
                "GET",
                "/api/v1/model-access/devices",
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        list["devices"][0]["limits"],
        serde_json::to_value(&identity.limits).unwrap()
    );
    fixture.state.store.database().close().await;
}

#[tokio::test]
async fn platform_roles_and_device_bearers_do_not_grant_access_to_another_devices_limits() {
    let fixture = Fixture::new().await;
    let member = fixture.account("limited-member", PlatformRole::User).await;
    let outsider = fixture.account("other-member", PlatformRole::User).await;
    let (token, identity) = authorize(&fixture, &member, None).await;
    let limits = limited(now_ms().unwrap() + 60_000);
    let path = format!("/api/v1/model-access/devices/{}/usage", identity.device_id);
    for unauthorized in [&fixture.owner, &outsider] {
        update(
            &fixture,
            unauthorized,
            &identity,
            limits.clone(),
            StatusCode::FORBIDDEN,
        )
        .await;
        json_response(
            fixture
                .request("GET", &path, &unauthorized.access_token, None)
                .await,
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    for method in ["GET", "PATCH"] {
        let target = if method == "GET" {
            path.clone()
        } else {
            path.trim_end_matches("/usage").to_owned()
        };
        let body = (method == "PATCH").then(|| limits.clone());
        let response = fixture.request(method, &target, &token, body).await;
        assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
    }
    let result = update(&fixture, &member, &identity, limits.clone(), StatusCode::OK).await;
    assert_eq!(result["limits"], limits);
    fixture.state.store.database().close().await;
}

#[tokio::test]
async fn revoked_devices_cannot_be_reactivated_by_editing_limits_but_keep_their_usage() {
    let fixture = Fixture::new().await;
    let (token, identity) = authorize(&fixture, &fixture.owner, None).await;
    let path = format!("/api/v1/model-access/devices/{}", identity.device_id);
    let response = fixture
        .request("DELETE", &path, &fixture.owner.access_token, None)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::NO_CONTENT));
    update(
        &fixture,
        &fixture.owner,
        &identity,
        limited(now_ms().unwrap() + 60_000),
        StatusCode::CONFLICT,
    )
    .await;
    let usage = json_response(
        fixture
            .request(
                "GET",
                &format!("{path}/usage"),
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(usage["used_tokens"], 0);
    let public = fixture
        .request("GET", "/v1/model-device", &token, None)
        .await;
    assert_eq!(public.status_code, Some(StatusCode::UNAUTHORIZED));
    fixture.state.store.database().close().await;
}

#[tokio::test]
async fn expired_device_access_returns_an_authentication_challenge_without_revival() {
    let fixture = Fixture::new().await;
    let (token, identity) = authorize(&fixture, &fixture.owner, None).await;
    update(
        &fixture,
        &fixture.owner,
        &identity,
        limited(now_ms().unwrap() + 500),
        StatusCode::OK,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    for method in ["GET", "DELETE"] {
        let response = fixture
            .request(method, "/v1/model-device", &token, None)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
        assert_eq!(
            response.headers().get("www-authenticate").unwrap(),
            "Bearer"
        );
    }
    update(
        &fixture,
        &fixture.owner,
        &identity,
        limited(now_ms().unwrap() + 60_000),
        StatusCode::CONFLICT,
    )
    .await;
    let usage = json_response(
        fixture
            .request(
                "GET",
                &format!("/api/v1/model-access/devices/{}/usage", identity.device_id),
                &fixture.owner.access_token,
                None,
            )
            .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(usage["active_requests"], 0);
    fixture.state.store.database().close().await;
}
