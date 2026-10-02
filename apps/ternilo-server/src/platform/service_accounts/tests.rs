use salvo_core::{
    Service,
    http::StatusCode,
    prelude::Response,
    test::{ResponseExt, TestClient},
};
use serde_json::{Value, json};
use std::sync::Arc;
use ternilo_cloud::{CloudSessionEventFeed, CloudStore};
use ternilo_control::{ControlStore, NativeRegistration, NativeSessionGrant, SecretCipher};

use crate::platform::{edge::EdgeGateway, state::AppState};

struct Fixture {
    service: Service,
    owner: NativeSessionGrant,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new() -> Self {
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([58; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    username: "service-owner".into(),
                    email: "service-owner@example.test".into(),
                    password: "service-test-password".into(),
                },
                super::now_ms().unwrap(),
            )
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud: CloudStore::from_database(store.database().clone())
                .await
                .unwrap(),
            cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
                .await
                .unwrap(),
            edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
            store,
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: false,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        Self {
            service: Service::new(crate::platform::web_router(state)),
            owner,
            _shutdown: shutdown,
        }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        tenant: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let url = format!("http://server.test/api/v1/{path}");
        let mut request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "PATCH" => TestClient::patch(url),
            "DELETE" => TestClient::delete(url),
            _ => panic!("unsupported test method"),
        }
        .add_header("Authorization", format!("Bearer {token}"), true);
        if let Some(tenant) = tenant {
            request = request.add_header("X-Ternilo-Tenant", tenant, true);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send(&self.service).await
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One HTTP lifecycle verifies issuing, scope isolation, denial of management, disable and permanent revocation."
)]
async fn service_credentials_are_tenant_scoped_nonhuman_and_revocable_through_the_real_router() {
    let f = Fixture::new().await;
    let tenant = f.owner.session.personal_tenant_id.as_str();
    let owner = &f.owner.access_token;
    let root = format!("tenants/{tenant}/service-accounts");
    let mut created = f
        .request("POST", &root, owner, None, Some(json!({"name":"报告任务"})))
        .await;
    assert_eq!(created.status_code, Some(StatusCode::CREATED));
    assert_eq!(created.headers().get("cache-control").unwrap(), "no-store");
    let account: Value = created.take_json().await.unwrap();
    let account = &account["service_account"];
    let id = account["service_account_id"].as_str().unwrap();
    assert_ne!(id, f.owner.session.user.user_id.as_str());
    let path = format!("{root}/{id}");
    let credentials = format!("{path}/credentials");
    let mut issued=f.request("POST",&credentials,owner,None,Some(json!({"name":"每日读取","scopes":["resource.read"],"expires_at_ms":super::now_ms().unwrap()+60_000}))).await;
    assert_eq!(issued.status_code, Some(StatusCode::CREATED));
    assert_eq!(issued.headers().get("cache-control").unwrap(), "no-store");
    let grant: Value = issued.take_json().await.unwrap();
    let token = grant["access_token"].as_str().unwrap();
    let mut me = f.request("GET", "me", token, Some(tenant), None).await;
    assert_eq!(me.status_code, Some(StatusCode::OK));
    let identity: Value = me.take_json().await.unwrap();
    assert_eq!(identity["user_id"], id);
    assert_ne!(identity["user_id"], f.owner.session.user.user_id.as_str());
    assert_eq!(
        f.request("GET", "me", token, Some("another-tenant"), None)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        f.request("GET", "projects", token, Some(tenant), None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        f.request(
            "GET",
            &format!("tenants/{tenant}/projects"),
            token,
            Some("another-tenant"),
            None
        )
        .await
        .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    for denied in [
        "auth/session",
        "auth/sessions",
        "admin/instance",
        &root,
        &credentials,
        "providers",
        "credentials",
    ] {
        assert_eq!(
            f.request("GET", denied, token, Some(tenant), None)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN),
            "{denied}"
        );
    }
    assert_eq!(
        f.request("POST", "sessions", token, Some(tenant), Some(json!({})))
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let mut listed = f.request("GET", &credentials, owner, None, None).await;
    assert_eq!(listed.status_code, Some(StatusCode::OK));
    let list: Value = listed.take_json().await.unwrap();
    assert_eq!(list["credentials"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains(token));
    assert!(!list.to_string().contains("token_hash"));
    let mut disabled = f
        .request(
            "PATCH",
            &path,
            owner,
            None,
            Some(
                json!({"name":"报告任务","notes":"暂时停止","enabled":false,"expected_revision":1}),
            ),
        )
        .await;
    assert_eq!(disabled.status_code, Some(StatusCode::OK));
    let disabled: Value = disabled.take_json().await.unwrap();
    assert_eq!(
        f.request("GET", "me", token, Some(tenant), None)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(f.request("PATCH",&path,owner,None,Some(json!({"name":"报告任务","notes":"","enabled":true,"expected_revision":disabled["service_account"]["revision"]}))).await.status_code,Some(StatusCode::OK));
    assert_eq!(
        f.request("GET", "me", token, Some(tenant), None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    let revoke = format!(
        "{credentials}/{}",
        grant["credential"]["credential_id"].as_str().unwrap()
    );
    assert_eq!(
        f.request("DELETE", &revoke, owner, None, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    assert_eq!(
        f.request("GET", "me", token, Some(tenant), None)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        f.request("GET", "auth/session", owner, None, None)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
}
