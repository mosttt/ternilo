use super::*;

const PATH: &str = "/admin/instance/authentication";

fn turnstile(revision: u64) -> Value {
    json!({"revision": revision, "public_url": "https://server.example.test", "oidc_providers": [],
        "turnstile": {"site_key": "site-key", "secret_key": "private-turnstile-key"}})
}

#[tokio::test]
async fn settings_are_owner_only_and_never_return_secrets() {
    let fixture = Fixture::new().await;
    for role in [PlatformRole::Admin, PlatformRole::User] {
        let session = fixture
            .account(
                if role == PlatformRole::Admin {
                    "staff"
                } else {
                    "member"
                },
                role,
            )
            .await;
        for method in ["GET", "PUT"] {
            let response = fixture
                .json(
                    method,
                    PATH,
                    &session.access_token,
                    (method == "PUT").then(|| turnstile(0)),
                )
                .await;
            assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
        }
    }
    let token = &fixture.owner.access_token;
    let mut response = fixture.json("PUT", PATH, token, Some(turnstile(0))).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let settings: Value = response.take_json().await.unwrap();
    assert_eq!(settings["turnstile"]["has_secret_key"], true);
    assert!(!settings.to_string().contains("private-turnstile-key"));
    assert!(settings["turnstile"].get("secret_key").is_none());
    let mut public = TestClient::get("http://server.test/auth/config")
        .send(&fixture.service)
        .await;
    let config: Value = public.take_json().await.unwrap();
    assert_eq!(config["turnstile"], json!({"site_key": "site-key"}));
    assert!(!config.to_string().contains("private-turnstile-key"));
}

#[tokio::test]
async fn turnstile_cannot_be_bypassed_by_direct_native_requests_and_disabling_takes_effect() {
    let fixture = Fixture::new().await;
    let token = &fixture.owner.access_token;
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(turnstile(0)))
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    for path in ["login", "register", "invitations/accept"] {
        let mut body = json!({"username": "owner", "password": "owner-test-password"});
        if path != "login" {
            body["email"] = "fresh@example.test".into();
        }
        if path.contains("invitations") {
            body["token"] = "invitation-token".into();
        }
        let mut response = TestClient::post(format!("http://server.test/api/v1/auth/{path}"))
            .json(&body)
            .send(&fixture.service)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
        assert_eq!(
            response.take_json::<Value>().await.unwrap()["error"]["message"],
            "complete the Turnstile verification"
        );
    }
    let page = TestClient::get("http://server.test/")
        .send(&fixture.service)
        .await;
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-src 'self' blob: https://challenges.cloudflare.com")
    );
    let mut disabled = turnstile(1);
    disabled["turnstile"] = Value::Null;
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(disabled))
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let response = fixture
        .json(
            "POST",
            "/auth/login",
            "",
            Some(json!({"username":"owner","password":"owner-test-password"})),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let page = TestClient::get("http://server.test/")
        .send(&fixture.service)
        .await;
    assert!(
        !page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("cloudflare")
    );
}

#[tokio::test]
async fn secret_retention_requires_the_same_provider_and_updates_reject_stale_revisions() {
    let fixture = Fixture::new().await;
    let token = &fixture.owner.access_token;
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(turnstile(0)))
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let mut retained = turnstile(1);
    retained["turnstile"]["secret_key"] = Value::Null;
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(retained.clone()))
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(retained.clone()))
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    retained["revision"] = 2.into();
    retained["turnstile"]["site_key"] = "different-site".into();
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(retained))
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let other = crate::platform::security::SecurityState::default();
    let loaded = other.current(&fixture.store).await.unwrap();
    assert_eq!(loaded.revision, 2);
    assert_eq!(
        loaded
            .settings
            .turnstile
            .as_ref()
            .unwrap()
            .secret_key
            .as_deref(),
        Some("private-turnstile-key")
    );
    let mut invalid = turnstile(2);
    invalid["oidc"] = json!({"issuer":"http://untrusted.example.test", "audience":"test", "client_id":"test", "scopes":"openid"});
    assert_eq!(
        fixture
            .json("PUT", PATH, token, Some(invalid))
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        fixture
            .store
            .authentication_settings_revision()
            .await
            .unwrap(),
        2
    );
}
