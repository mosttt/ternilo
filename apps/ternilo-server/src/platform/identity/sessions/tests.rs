use std::sync::Arc;

use salvo_core::{
    Service,
    http::StatusCode,
    prelude::Response,
    test::{ResponseExt, TestClient},
};
use serde_json::Value;
use ternilo_cloud::{CloudLiveNotification, CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    AccountStatusAction, ControlStore, InstanceMode, NativeRegistration, NativeSessionGrant,
    OidcPrincipal, SecretCipher,
};

use super::*;
use crate::platform::edge::EdgeGateway;

#[path = "live_tests.rs"]
mod live_tests;

fn reauthentication_targets(
    notifications: &mut tokio::sync::broadcast::Receiver<CloudLiveNotification>,
) -> Vec<ternilo_protocol::UserId> {
    let mut targets = Vec::new();
    while let Ok(notification) = notifications.try_recv() {
        if let CloudLiveNotification::Reauthenticate { user_id } = notification {
            targets.push(user_id);
        }
    }
    targets
}

struct Fixture {
    state: AppState,
    service: Service,
    owner: NativeSessionGrant,
    member: NativeSessionGrant,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new() -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([74; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "sessions-owner@example.test".to_owned(),
                    username: "sessions-owner".to_owned(),
                    password: "sessions-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let member = store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://sessions.example.test".to_owned(),
                    subject: "member".to_owned(),
                    email: Some("member@example.test".to_owned()),
                    display_name: Some("Member".to_owned()),
                },
                "member",
                now,
            )
            .await
            .unwrap();
        let member = store.create_browser_session(member, now).await.unwrap();
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
            member,
            shutdown,
        }
    }

    async fn request(&self, method: &str, path: &str, token: Option<&str>) -> Response {
        let url = format!("http://server.test/api/v1/auth/{path}");
        let request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "DELETE" => TestClient::delete(url),
            other => panic!("unsupported method {other}"),
        };
        let request = if let Some(token) = token {
            request.add_header("Authorization", format!("Bearer {token}"), true)
        } else {
            request
        };
        request.send(&self.service).await
    }

    async fn list(&self, grant: &NativeSessionGrant) -> Value {
        let mut response = self
            .request("GET", "sessions", Some(&grant.access_token))
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        response.take_json().await.unwrap()
    }

    async fn denied(&self, token: Option<&str>, status: StatusCode) {
        for (method, path) in [
            ("GET", "sessions"),
            ("POST", "sessions/revoke-others"),
            ("DELETE", "sessions/knbs_unknown"),
        ] {
            let response = self.request(method, path, token).await;
            assert_eq!(response.status_code, Some(status), "{method} {path}");
            assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        }
    }

    async fn close(self) {
        self.shutdown.send_replace(true);
        self.state.edge.shutdown().await;
    }
}

#[tokio::test]
async fn sessions_require_current_login_and_public_ids_are_not_credentials() {
    let fixture = Fixture::new().await;
    fixture.denied(None, StatusCode::UNAUTHORIZED).await;
    fixture
        .denied(Some("kns_invalid"), StatusCode::UNAUTHORIZED)
        .await;
    fixture
        .denied(Some("external.jwt.invalid"), StatusCode::UNAUTHORIZED)
        .await;
    let listed = fixture.list(&fixture.member).await;
    assert_eq!(listed["current_login"], "native");
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["is_current"], true);
    assert_eq!(sessions[0].as_object().unwrap().len(), 4);
    let public_id = sessions[0]["session_id"].as_str().unwrap();
    fixture
        .denied(Some(public_id), StatusCode::UNAUTHORIZED)
        .await;
    assert!(!listed.to_string().contains("token"));
    fixture.close().await;
}

async fn assert_revoke_isolation(fixture: &Fixture) {
    let mut notifications = fixture.state.cloud_events.subscribe();
    let owner = fixture.list(&fixture.owner).await;
    let member = fixture.list(&fixture.member).await;
    let owner_path = format!(
        "sessions/{}",
        owner["sessions"][0]["session_id"].as_str().unwrap()
    );
    let member_path = format!(
        "sessions/{}",
        member["sessions"][0]["session_id"].as_str().unwrap()
    );
    for (path, token) in [
        (&owner_path, &fixture.member.access_token),
        (&member_path, &fixture.owner.access_token),
    ] {
        let mut response = fixture.request("DELETE", path, Some(token)).await;
        assert_eq!(response.status_code, Some(StatusCode::CONFLICT));
        let cross: Value = response.take_json().await.unwrap();
        let mut unknown = fixture
            .request("DELETE", "sessions/knbs_unknown", Some(token))
            .await;
        assert_eq!(cross, unknown.take_json::<Value>().await.unwrap());
    }
    let mut unchanged = fixture
        .request(
            "POST",
            "sessions/revoke-others",
            Some(&fixture.member.access_token),
        )
        .await;
    assert_eq!(unchanged.status_code, Some(StatusCode::OK));
    assert_eq!(
        unchanged.take_json::<Value>().await.unwrap(),
        serde_json::json!({"revoked_count":0,"current_revoked":false})
    );
    assert!(reauthentication_targets(&mut notifications).is_empty());
}

#[tokio::test]
async fn sessions_revoke_individual_other_and_current_without_cross_account_access() {
    let fixture = Fixture::new().await;
    assert_revoke_isolation(&fixture).await;
    let mut notifications = fixture.state.cloud_events.subscribe();
    let second = fixture
        .state
        .store
        .create_browser_session(fixture.member.session.user.clone(), now_ms().unwrap())
        .await
        .unwrap();
    let second_list = fixture.list(&second).await;
    let second_path = format!(
        "sessions/{}",
        second_list["sessions"][0]["session_id"].as_str().unwrap()
    );
    let mut response = fixture
        .request("DELETE", &second_path, Some(&fixture.member.access_token))
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_eq!(
        response.take_json::<Value>().await.unwrap(),
        serde_json::json!({"revoked_count":1,"current_revoked":false})
    );
    assert_eq!(
        reauthentication_targets(&mut notifications),
        vec![fixture.member.session.user.user_id.clone()]
    );
    fixture
        .denied(Some(&second.access_token), StatusCode::UNAUTHORIZED)
        .await;
    assert_eq!(
        fixture
            .request("DELETE", &second_path, Some(&fixture.member.access_token))
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    assert!(reauthentication_targets(&mut notifications).is_empty());
    let third = fixture
        .state
        .store
        .create_browser_session(fixture.member.session.user.clone(), now_ms().unwrap())
        .await
        .unwrap();
    let mut response = fixture
        .request(
            "POST",
            "sessions/revoke-others",
            Some(&fixture.member.access_token),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_eq!(
        response.take_json::<Value>().await.unwrap(),
        serde_json::json!({"revoked_count":1,"current_revoked":false})
    );
    assert_eq!(
        reauthentication_targets(&mut notifications),
        vec![fixture.member.session.user.user_id.clone()]
    );
    fixture
        .denied(Some(&third.access_token), StatusCode::UNAUTHORIZED)
        .await;
    assert_eq!(
        fixture.list(&fixture.owner).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let member_path = format!(
        "sessions/{}",
        fixture.list(&fixture.member).await["sessions"][0]["session_id"]
            .as_str()
            .unwrap()
    );
    let mut response = fixture
        .request("DELETE", &member_path, Some(&fixture.member.access_token))
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_eq!(
        response.take_json::<Value>().await.unwrap(),
        serde_json::json!({"revoked_count":1,"current_revoked":true})
    );
    assert_eq!(
        reauthentication_targets(&mut notifications),
        vec![fixture.member.session.user.user_id.clone()]
    );
    fixture
        .denied(Some(&fixture.member.access_token), StatusCode::UNAUTHORIZED)
        .await;
    assert_eq!(
        fixture
            .request("POST", "logout", Some(&fixture.member.access_token))
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    fixture.close().await;
}

#[tokio::test]
async fn sessions_reject_expired_login_and_hide_expired_targets() {
    let fixture = Fixture::new().await;
    let expired = fixture
        .state
        .store
        .create_browser_session(fixture.member.session.user.clone(), 1_000)
        .await
        .unwrap();
    fixture
        .denied(Some(&expired.access_token), StatusCode::UNAUTHORIZED)
        .await;
    assert_eq!(
        fixture.list(&fixture.member).await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn sessions_obey_account_ban_and_paused_access_while_logout_remains_available() {
    let fixture = Fixture::new().await;
    let store = &fixture.state.store;
    store
        .set_instance_mode(
            &fixture.owner.session.user,
            InstanceMode::SingleUser,
            2,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    fixture
        .denied(Some(&fixture.member.access_token), StatusCode::FORBIDDEN)
        .await;
    store
        .set_instance_mode(
            &fixture.owner.session.user,
            InstanceMode::MultiUser,
            3,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    store
        .set_account_status(
            &fixture.owner.session.user,
            &fixture.member.session.user.user_id,
            AccountStatusAction::Ban,
            1,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    fixture
        .denied(Some(&fixture.member.access_token), StatusCode::UNAUTHORIZED)
        .await;
    assert_eq!(
        fixture
            .request("POST", "logout", Some(&fixture.member.access_token))
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    fixture.close().await;
}
