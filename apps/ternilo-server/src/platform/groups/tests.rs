use std::{collections::BTreeSet, sync::Arc, time::Duration};

use futures_util::{SinkExt as _, StreamExt as _};
use salvo_core::{
    Service,
    conn::tcp::TcpAcceptor,
    test::{ResponseExt, TestClient},
};
use serde_json::{Value, json};
use ternilo_cloud::{CloudSessionDraft, CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, NativeSessionGrant, OidcPrincipal,
    SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, LIVE_PROTOCOL_VERSION, LiveClientFrame, LiveServerFrame, PermissionPreset, SessionId,
    SessionLiveReadMask, SessionMode, TenantId,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use super::*;
use crate::platform::{edge::EdgeGateway, state::AppState};

struct Fixture {
    state: AppState,
    service: Service,
    owner: NativeSessionGrant,
    tenant: TenantId,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new() -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([51; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "owner-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                &owner.session.user,
                "team",
                "Team",
                TenantQuota::default(),
                now,
            )
            .await
            .unwrap()
            .tenant_id;
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
            managed_execution_enabled: true,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        let service = Service::new(crate::platform::web_router(state.clone()));
        Self {
            state,
            service,
            owner,
            tenant,
            shutdown,
        }
    }

    async fn account(&self, name: &str, role: TenantRole) -> NativeSessionGrant {
        let now = now_ms().unwrap();
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
                now,
            )
            .await
            .unwrap();
        self.state
            .store
            .set_membership(
                &self.owner.session.user,
                &self.tenant,
                &user.user_id,
                role,
                now,
            )
            .await
            .unwrap();
        self.state
            .store
            .create_browser_session(user, now)
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
        let url = format!("http://server.test/api/v1{path}");
        let request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "PUT" => TestClient::put(url),
            "PATCH" => TestClient::patch(url),
            "DELETE" => TestClient::delete(url),
            other => panic!("unsupported test method {other}"),
        }
        .add_header("Authorization", format!("Bearer {token}"), true)
        .add_header("x-ternilo-tenant", self.tenant.as_str(), true);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        request.send(&self.service).await
    }

    async fn group(&self, name: &str) -> String {
        let mut response = self
            .request(
                "POST",
                &format!("/tenants/{}/groups", self.tenant),
                &self.owner.access_token,
                Some(json!({"name":name,"description":"Shared access"})),
            )
            .await;
        assert_eq!(response.status_code, Some(StatusCode::CREATED));
        response.take_json::<Value>().await.unwrap()["group_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn session(&self) -> SessionId {
        self.session_for(&self.owner.session.user, "shared-session")
            .await
    }

    async fn session_for(&self, owner: &ControlUser, id: &str) -> SessionId {
        let store = &self.state.store;
        let now = now_ms().unwrap();
        let project = store.list_projects(owner, &self.tenant).await.unwrap()[0]
            .project_id
            .clone();
        let workspace = store
            .create_cloud_workspace(owner, &self.tenant, &project, "Shared workspace", now)
            .await
            .unwrap();
        self.state
            .cloud
            .create_session(
                CloudSessionDraft {
                    project_id: project,
                    workspace_id: workspace.workspace_id,
                    session_id: Some(SessionId::new(id)),
                    agent_id: AgentId::new("agent"),
                    title: "Shared session".to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 100,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &self.tenant,
                &owner.user_id,
                now,
            )
            .await
            .unwrap()
            .session_id
    }

    async fn close(self) {
        self.shutdown.send_replace(true);
        self.state.edge.shutdown().await;
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise authenticated group CRUD, pagination, membership and space boundaries over HTTP."
)]
async fn group_http_is_paged_and_requires_team_management() {
    let fixture = Fixture::new().await;
    let member = fixture.account("member", TenantRole::Member).await;
    let viewer = fixture.account("viewer", TenantRole::Viewer).await;
    let base = format!("/tenants/{}/groups", fixture.tenant);
    for account in [&member, &viewer] {
        for (method, body) in [("GET", None), ("POST", Some(json!({"name":"Denied"})))] {
            assert_eq!(
                fixture
                    .request(method, &base, &account.access_token, body)
                    .await
                    .status_code,
                Some(StatusCode::FORBIDDEN)
            );
        }
    }
    let mut expected = BTreeSet::new();
    for name in ["Alpha", "Beta", "Gamma"] {
        expected.insert(fixture.group(name).await);
    }
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    loop {
        let path = cursor.as_ref().map_or_else(
            || format!("{base}?limit=1"),
            |value| format!("{base}?limit=1&cursor={value}"),
        );
        let mut response = fixture
            .request("GET", &path, &fixture.owner.access_token, None)
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let page: Value = response.take_json().await.unwrap();
        let groups = page["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(seen.insert(groups[0]["group_id"].as_str().unwrap().to_owned()));
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, expected);
    for query in [
        "limit=0",
        "limit=101",
        "cursor=not-a-cursor",
        "limit=oops",
        "typo=value",
    ] {
        assert_eq!(
            fixture
                .request(
                    "GET",
                    &format!("{base}?{query}"),
                    &fixture.owner.access_token,
                    None
                )
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    let group = seen.first().unwrap();
    let group_path = format!("{base}/{group}");
    let updated: Value = fixture
        .request(
            "PATCH",
            &group_path,
            &fixture.owner.access_token,
            Some(json!({"name":"Renamed","description":"Updated"})),
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(updated["name"], "Renamed");
    let found: Value = fixture
        .request(
            "GET",
            &format!("{base}?query=Renamed"),
            &fixture.owner.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(found["groups"].as_array().unwrap().len(), 1);
    let mut notifications = fixture.state.edge.subscribe_live();
    for account in [&member, &viewer] {
        let path = format!("{group_path}/members/{}", account.session.user.user_id);
        assert_eq!(
            fixture
                .request("PUT", &path, &fixture.owner.access_token, None)
                .await
                .status_code,
            Some(StatusCode::NO_CONTENT)
        );
    }
    assert!(
        matches!(notifications.try_recv().unwrap(), crate::platform::edge::EdgeLiveNotification::ResourcesChanged { tenant_id } if tenant_id == fixture.tenant)
    );
    let details: Value = fixture
        .request("GET", &group_path, &fixture.owner.access_token, None)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(details["member_count"], 2);
    let members: Value = fixture
        .request(
            "GET",
            &format!("{group_path}/members?limit=1&query=member"),
            &fixture.owner.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(members["memberships"].as_array().unwrap().len(), 1);
    assert!(members["next_cursor"].is_null());
    let team_members: Value = fixture
        .request(
            "GET",
            &format!("/tenants/{}/members?limit=1", fixture.tenant),
            &fixture.owner.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(team_members["memberships"].as_array().unwrap().len(), 1);
    assert!(team_members["next_cursor"].is_string());
    let personal = &fixture.owner.session.personal_tenant_id;
    assert_eq!(
        fixture
            .request(
                "POST",
                &format!("/tenants/{personal}/groups"),
                &fixture.owner.access_token,
                Some(json!({"name":"Not a team"}))
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .request("DELETE", &group_path, &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    let deleted: Value = fixture
        .request(
            "GET",
            &format!("{base}?query=Renamed"),
            &fixture.owner.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(deleted["groups"], json!([]));
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify group resource grants, owner-only candidate discovery, effective permissions and independent direct grants."
)]
async fn group_sharing_http_uses_current_membership_and_preserves_direct_grants() {
    let fixture = Fixture::new().await;
    let member = fixture.account("member", TenantRole::Member).await;
    let group = fixture.group("Developers").await;
    let member_session = fixture
        .session_for(&member.session.user, "member-session")
        .await;
    let member_candidates: Value = fixture
        .request(
            "GET",
            &format!("/sessions/{member_session}/sharing/candidates?kind=group"),
            &member.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(
        member_candidates["candidates"][0]["group"]["group_id"],
        group
    );
    assert_eq!(
        fixture
            .request(
                "GET",
                &format!("/tenants/{}/groups", fixture.tenant),
                &member.access_token,
                None,
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let session = fixture.session().await;
    let sharing = format!("/sessions/{session}/sharing");
    let membership = format!(
        "/tenants/{}/groups/{group}/members/{}",
        fixture.tenant, member.session.user.user_id
    );
    let read = json!({"view":true,"submit":false,"stop":false,"configure":false});
    let write = json!({"view":true,"submit":true,"stop":true,"configure":true});
    let mut response = fixture
        .request(
            "GET",
            &format!("{sharing}/candidates?kind=group&query=Develop&limit=1"),
            &fixture.owner.access_token,
            None,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let candidates: Value = response.take_json().await.unwrap();
    assert_eq!(candidates["candidates"][0]["kind"], "group");
    assert_eq!(candidates["candidates"][0]["group"]["group_id"], group);
    for query in ["kind=unknown", "kind=user&limit=101", "limit=1"] {
        assert_eq!(
            fixture
                .request(
                    "GET",
                    &format!("{sharing}/candidates?{query}"),
                    &fixture.owner.access_token,
                    None
                )
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    assert_eq!(
        fixture
            .request(
                "PUT",
                &format!("{sharing}/group/{group}"),
                &fixture.owner.access_token,
                Some(write)
            )
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    assert_eq!(
        fixture
            .request("GET", &sharing, &member.access_token, None)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .request("PUT", &membership, &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    let shared: Value = fixture
        .request("GET", &sharing, &member.access_token, None)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(shared["access"]["permissions"]["configure"], true);
    assert!(!shared["access"]["sources"].as_array().unwrap().is_empty());
    assert_eq!(shared["shares"], json!([]));
    assert!(shared.get("members").is_none());
    assert_eq!(
        fixture
            .request(
                "GET",
                &format!("{sharing}/candidates?kind=user"),
                &member.access_token,
                None
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .request(
                "PUT",
                &format!("{sharing}/user/{}", member.session.user.user_id),
                &member.access_token,
                Some(read.clone())
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let own: Value = fixture
        .request(
            "GET",
            &format!("{sharing}?limit=1"),
            &fixture.owner.access_token,
            None,
        )
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(own["shares"][0]["subject"]["kind"], "group");
    let direct = format!("{sharing}/user/{}", member.session.user.user_id);
    assert_eq!(
        fixture
            .request("PUT", &direct, &fixture.owner.access_token, Some(read))
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    assert_eq!(
        fixture
            .request("DELETE", &membership, &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    let remaining: Value = fixture
        .request("GET", &sharing, &member.access_token, None)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(remaining["access"]["permissions"]["view"], true);
    assert_eq!(remaining["access"]["permissions"]["submit"], false);
    assert_eq!(remaining["access"]["permissions"]["configure"], false);
    assert_eq!(
        fixture
            .request("DELETE", &direct, &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    assert_eq!(
        fixture
            .request("GET", &sharing, &member.access_token, None)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    fixture.close().await;
}

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_frame(socket: &mut ClientSocket) -> LiveServerFrame {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let message = socket.next().await.unwrap().unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    })
    .await
    .expect("Live frame timed out")
}

#[tokio::test]
async fn group_revocation_removes_live_subscription_and_workbench_session() {
    assert_live_revocation(true).await;
}

#[tokio::test]
async fn idle_live_subscription_rechecks_permissions_without_broadcast() {
    assert_live_revocation(false).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify revocation reaches an existing authenticated WebSocket subscription without requiring another session event."
)]
async fn assert_live_revocation(notify: bool) {
    let fixture = Fixture::new().await;
    let member = fixture.account("live-member", TenantRole::Member).await;
    let group = fixture.group("Live viewers").await;
    let session = fixture.session().await;
    let membership = format!(
        "/tenants/{}/groups/{group}/members/{}",
        fixture.tenant, member.session.user.user_id
    );
    assert_eq!(
        fixture
            .request("PUT", &membership, &fixture.owner.access_token, None)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    assert_eq!(
        fixture
            .request(
                "PUT",
                &format!("/sessions/{session}/sharing/group/{group}"),
                &fixture.owner.access_token,
                Some(json!({"view":true}))
            )
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move {
        server.try_serve(router).await.unwrap();
    });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/live"))
        .await
        .unwrap();
    let hello = LiveClientFrame::Hello {
        protocol_version: LIVE_PROTOCOL_VERSION,
        bearer_token: Some(member.access_token.clone()),
        tenant_id: Some(fixture.tenant.clone()),
    };
    socket
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut socket).await,
        LiveServerFrame::Ready { .. }
    ));
    let baseline = next_frame(&mut socket).await;
    assert!(
        matches!(baseline, LiveServerFrame::Workbench { activity, .. } if activity.iter().any(|item| item.session_id == session))
    );
    let subscribe = LiveClientFrame::Subscribe {
        subscription_id: 1,
        session_id: session.clone(),
        after_seq: None,
        metadata: SessionLiveReadMask::default(),
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&subscribe).unwrap().into(),
        ))
        .await
        .unwrap();
    loop {
        match next_frame(&mut socket).await {
            LiveServerFrame::EventBatch {
                subscription_id: 1, ..
            } => break,
            LiveServerFrame::Activity { .. } | LiveServerFrame::Workbench { .. } => {}
            frame => panic!("unexpected frame before subscription baseline: {frame:?}"),
        }
    }
    if notify {
        assert_eq!(
            fixture
                .request("DELETE", &membership, &fixture.owner.access_token, None)
                .await
                .status_code,
            Some(StatusCode::NO_CONTENT),
        );
    } else {
        // A direct store write simulates a missed or remote invalidation broadcast.
        fixture
            .state
            .store
            .set_permission_group_member(
                &fixture.owner.session.user,
                &fixture.tenant,
                &group,
                &member.session.user.user_id,
                false,
                now_ms().unwrap(),
            )
            .await
            .unwrap();
    }
    let mut revoked = false;
    let mut removed = false;
    for _ in 0..8 {
        match next_frame(&mut socket).await {
            LiveServerFrame::Error {
                subscription_id: Some(1),
                code: ternilo_protocol::ErrorCode::PolicyDenied,
                ..
            } => revoked = true,
            LiveServerFrame::Workbench { activity, .. } => {
                removed = activity.iter().all(|item| item.session_id != session);
            }
            LiveServerFrame::Activity { .. } => {}
            frame => panic!("unexpected frame after revocation: {frame:?}"),
        }
        if revoked && removed {
            break;
        }
    }
    assert!(revoked && removed);
    let resubscribe = LiveClientFrame::Subscribe {
        subscription_id: 2,
        session_id: session,
        after_seq: None,
        metadata: SessionLiveReadMask::default(),
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&resubscribe).unwrap().into(),
        ))
        .await
        .unwrap();
    loop {
        match next_frame(&mut socket).await {
            LiveServerFrame::Error {
                subscription_id: Some(2),
                ..
            } => break,
            LiveServerFrame::Workbench { activity, .. } => {
                assert!(
                    activity
                        .iter()
                        .all(|item| item.session_id.as_str() != "shared-session")
                );
            }
            frame => panic!("unexpected frame while resubscribing after revocation: {frame:?}"),
        }
    }
    socket.close(None).await.unwrap();
    fixture.close().await;
    handle.stop_graceful(Some(Duration::from_secs(2)));
    tokio::time::timeout(Duration::from_secs(3), serving)
        .await
        .unwrap()
        .unwrap();
}
