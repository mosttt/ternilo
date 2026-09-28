use std::sync::Arc;

use salvo_core::{
    Service,
    http::StatusCode,
    prelude::Response,
    test::{ResponseExt as _, TestClient},
};
use serde_json::{Value, json};
use ternilo_cloud::{CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, InstanceMode, ModelRequestInput, ModelRequestSettlement, ModelRequestState,
    NativeRegistration, NativeSessionGrant, OidcPrincipal, PlatformRole, SecretCipher,
    ServiceModelUsage,
};
use ternilo_protocol::ProviderProtocol;

use crate::platform::{edge::EdgeGateway, http::now_ms, state::AppState};

mod device_limits;
mod scope_tests;

struct Fixture {
    state: AppState,
    service: Service,
    owner: NativeSessionGrant,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new() -> Self {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([71; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-test-password".to_owned(),
                },
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        store
            .set_instance_mode(
                &owner.session.user,
                InstanceMode::MultiUser,
                1,
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        let cloud = CloudStore::from_database(store.database().clone())
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
                .await
                .unwrap(),
            edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
            store,
            cloud,
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: false,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        let service = Service::new(crate::platform::web_router(state.clone()));
        Self {
            state,
            service,
            owner,
            _shutdown: shutdown,
        }
    }

    async fn account(&self, name: &str, role: PlatformRole) -> NativeSessionGrant {
        let user = self
            .state
            .store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://identity.example.test".to_owned(),
                    subject: name.to_owned(),
                    email: Some(format!("{name}@example.test")),
                    display_name: Some(name.to_owned()),
                },
                &format!("test-{name}"),
                now_ms().unwrap(),
            )
            .await
            .unwrap();
        if role != PlatformRole::User {
            self.state
                .store
                .set_account_role(
                    &self.owner.session.user,
                    &user.user_id,
                    role,
                    1,
                    now_ms().unwrap(),
                )
                .await
                .unwrap();
        }
        self.state
            .store
            .create_browser_session(user, now_ms().unwrap())
            .await
            .unwrap()
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> Response {
        let url = format!("http://server.test{path}");
        let request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "PUT" => TestClient::put(url),
            "PATCH" => TestClient::patch(url),
            "DELETE" => TestClient::delete(url),
            _ => panic!("unsupported test method"),
        }
        .add_header("Authorization", format!("Bearer {token}"), true);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        request.send(&self.service).await
    }

    async fn publish(&self) {
        let response = self
            .request(
                "POST",
                "/api/v1/admin/models/providers",
                &self.owner.access_token,
                Some(provider_input()),
            )
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let response = self.request("POST", "/api/v1/admin/models/publications", &self.owner.access_token, Some(json!({
            "model_id":"public-model", "display_name":"Published model", "provider_id":"upstream",
            "upstream_model":"internal-model", "enabled":true,
        }))).await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
    }
}

fn provider_input() -> Value {
    json!({"profile": {
        "id":"upstream", "display_name":"Managed upstream", "base_url":"https://upstream.example.test/v1",
        "protocol":"openai-responses", "defaults":{"context_window":128_000,"max_output_tokens":4096},
        "models":[{"id":"internal-model","settings":{"mode":"inherit"}}],
        "timeout_ms":30_000,"max_attempts":1,"retry_base_delay_ms":100,
    }, "enabled":true,"api_key":"private-upstream-secret","clear_api_key":false})
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify role boundaries and write-only key edits through one complete HTTP configuration flow."
)]
async fn model_management_roles_and_write_only_provider_keys_are_enforced_over_http() {
    let f = Fixture::new().await;
    let operator = f.account("operator", PlatformRole::Operator).await;
    let auditor = f.account("auditor", PlatformRole::Auditor).await;
    let user = f.account("user", PlatformRole::User).await;
    f.publish().await;
    for account in [&f.owner, &operator, &auditor] {
        let mut response = f
            .request(
                "GET",
                "/api/v1/admin/models/providers",
                &account.access_token,
                None,
            )
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        let body = response.take_json::<Value>().await.unwrap();
        assert!(!body.to_string().contains("private-upstream-secret"));
        assert_eq!(body["providers"][0]["has_api_key"], true);
    }
    let response = f
        .request(
            "GET",
            "/api/v1/admin/models/providers",
            &user.access_token,
            None,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    let response = f
        .request(
            "PUT",
            "/api/v1/admin/models/providers/upstream",
            &auditor.access_token,
            Some(provider_input()),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    let response = f
        .request(
            "PUT",
            "/api/v1/admin/models/providers/wrong-id",
            &f.owner.access_token,
            Some(provider_input()),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::BAD_REQUEST));
    let mut input = provider_input();
    input["api_key"] = Value::Null;
    let response = f
        .request(
            "PUT",
            "/api/v1/admin/models/providers/upstream",
            &operator.access_token,
            Some(input.clone()),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_eq!(
        f.state
            .store
            .resolve_model_provider_secret(&f.owner.session.user, "upstream")
            .await
            .unwrap()
            .unwrap()
            .as_str(),
        "private-upstream-secret"
    );
    input["clear_api_key"] = json!(true);
    let response = f
        .request(
            "PUT",
            "/api/v1/admin/models/providers/upstream",
            &operator.access_token,
            Some(input),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert!(
        f.state
            .store
            .resolve_model_provider_secret(&f.owner.session.user, "upstream")
            .await
            .unwrap()
            .is_none()
    );
    let response = f
        .request(
            "POST",
            "/api/v1/admin/models/groups",
            &operator.access_token,
            Some(json!({"name":"Blocked", "description":null})),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    let mut response = f
        .request(
            "POST",
            "/api/v1/admin/models/groups",
            &f.owner.access_token,
            Some(json!({"name":"Readers", "description":null})),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::CREATED));
    let group = response.take_json::<Value>().await.unwrap();
    let id = group["group_id"].as_str().unwrap();
    let response = f
        .request(
            "PUT",
            &format!(
                "/api/v1/admin/models/groups/{id}/members/{}",
                user.session.user.user_id
            ),
            &f.owner.access_token,
            None,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::NO_CONTENT));
    let mut response = f
        .request(
            "GET",
            &format!("/api/v1/admin/models/groups/{id}/members?limit=1"),
            &f.owner.access_token,
            None,
        )
        .await;
    assert_eq!(
        response.take_json::<Value>().await.unwrap()["users"][0]["user_id"],
        user.session.user.user_id.as_str()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep personal account, Key, budget and HTTP privacy checks in one complete service scenario."
)]
async fn personal_model_keys_and_request_history_have_independent_scopes() {
    let f = Fixture::new().await;
    let alice = f.account("alice", PlatformRole::User).await;
    let bob = f.account("bob", PlatformRole::User).await;
    f.publish().await;
    let mut response = f.request("POST", "/api/v1/admin/models/grants", &f.owner.access_token, Some(json!({
        "name":"Alice budget", "subject":{"kind":"user","id":alice.session.user.user_id},
        "model_ids":["public-model"],"monthly_tokens":1_000_000,"max_concurrent_requests":2,"expires_at_ms":null,
    }))).await;
    assert_eq!(response.status_code, Some(StatusCode::CREATED));
    let grant = response.take_json::<Value>().await.unwrap();
    let grant_id = grant["grant_id"].as_str().unwrap();
    let mut response = f
        .request(
            "GET",
            "/api/v1/model-access/catalog",
            &alice.access_token,
            None,
        )
        .await;
    let catalog = response.take_json::<Value>().await.unwrap();
    assert_eq!(
        catalog["entitlements"][0]["models"][0]["model_id"],
        "public-model"
    );
    assert!(!catalog.to_string().contains("upstream"));
    let mut response = f
        .request(
            "GET",
            "/api/v1/model-access/catalog",
            &bob.access_token,
            None,
        )
        .await;
    assert_eq!(
        response.take_json::<Value>().await.unwrap()["entitlements"],
        json!([])
    );
    let key_input = json!({"name":"Laptop", "grant_id":grant_id,"model_ids":["public-model"],
        "monthly_tokens":500_000,"max_concurrent_requests":1,"expires_at_ms":null});
    let response = f
        .request(
            "POST",
            "/api/v1/model-access/keys",
            &bob.access_token,
            Some(key_input.clone()),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    let mut response = f
        .request(
            "POST",
            "/api/v1/model-access/keys",
            &alice.access_token,
            Some(key_input),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::CREATED));
    let created = response.take_json::<Value>().await.unwrap();
    let token = created["token"].as_str().unwrap();
    let key_id = created["key"]["key_id"].as_str().unwrap();
    let mut response = f
        .request(
            "GET",
            "/api/v1/model-access/keys",
            &alice.access_token,
            None,
        )
        .await;
    let listed = response.take_json::<Value>().await.unwrap();
    assert!(!listed.to_string().contains(token));
    assert!(listed["keys"][0].get("token").is_none());
    let response = f
        .request("GET", "/api/v1/model-access/catalog", token, None)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
    let response = f
        .request("GET", "/v1/models", &alice.access_token, None)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
    let permit = f
        .state
        .store
        .reserve_model_request(
            token,
            &ModelRequestInput {
                request_key: "history-request".to_owned(),
                payload_hash: "a".repeat(64),
                model_id: "public-model".to_owned(),
                protocol: ProviderProtocol::OpenAiResponses,
                reserved_tokens: 1000,
            },
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    f.state
        .store
        .mark_model_request_attempted(&permit.request.request_id, now_ms().unwrap())
        .await
        .unwrap();
    f.state
        .store
        .settle_model_request(
            &permit.request.request_id,
            &ModelRequestSettlement {
                state: ModelRequestState::Completed,
                usage: Some(ServiceModelUsage {
                    input_tokens: Some(80),
                    output_tokens: Some(20),
                    raw_usage: Some(json!({"private_debug":"provider-diagnostic"})),
                    ..ServiceModelUsage::default()
                }),
                upstream_request_id: Some("upstream-trace".to_owned()),
                error_code: None,
            },
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let mut response = f
        .request(
            "GET",
            "/api/v1/model-access/requests",
            &alice.access_token,
            None,
        )
        .await;
    let history = response.take_json::<Value>().await.unwrap();
    assert_eq!(history["requests"][0]["accounted_tokens"], 100);
    for private in [
        "provider_id",
        "upstream_model",
        "upstream_request_id",
        "raw_usage",
        "provider-diagnostic",
    ] {
        assert!(
            !history.to_string().contains(private),
            "private upstream field leaked: {private}"
        );
    }
    let mut response = f
        .request(
            "GET",
            "/api/v1/model-access/requests",
            &bob.access_token,
            None,
        )
        .await;
    assert_eq!(
        response.take_json::<Value>().await.unwrap()["requests"],
        json!([])
    );
    let response = f
        .request(
            "DELETE",
            &format!("/api/v1/model-access/keys/{key_id}"),
            &bob.access_token,
            None,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    let response = f
        .request(
            "DELETE",
            &format!("/api/v1/model-access/keys/{key_id}"),
            &alice.access_token,
            None,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::NO_CONTENT));
    let response = f.request("GET", "/v1/models", token, None).await;
    assert_eq!(response.status_code, Some(StatusCode::UNAUTHORIZED));
}
