use super::forwarding::{ForwardRequest, validate_cluster_origin};
use super::*;
use crate::platform::state::AppState;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use salvo_core::{
    Service,
    test::{ResponseExt as _, TestClient},
};
use serde_json::json;
use ternilo_cloud::{CloudSessionEventFeed, CloudStore};
use ternilo_control::{ControlStore, NativeRegistration, SecretCipher};
use ternilo_protocol::{
    ErrorCode, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
};
#[path = "../../../../../crates/ternilo-control/tests/support/postgres.rs"]
mod postgres_runtime;

struct Fixture {
    store: ControlStore,
    edge: Arc<EdgeGateway>,
    route: RouteKey,
    connection: ConnectedExecutor,
    incoming: mpsc::Receiver<ControlFrame>,
    user: ControlUser,
    key: String,
    browser_token: String,
}

impl Fixture {
    #[expect(
        clippy::too_many_lines,
        reason = "Create real accounts, enrollment, mapping, credentials and a fenced test Node together."
    )]
    async fn new(url: &str, schema_owner: Option<&str>) -> Self {
        let key = STANDARD.encode([79; 32]);
        let now = now_ms().unwrap();
        let store = ControlStore::connect(url, schema_owner, SecretCipher::from_key([79; 32]), 4)
            .await
            .unwrap();
        if let Some(owner_url) = schema_owner {
            let database = ternilo_storage::Database::connect(owner_url, 1)
                .await
                .unwrap();
            crate::gateway_journal::GatewayJournal::open(database.clone())
                .await
                .unwrap();
            database.close().await;
        }
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "peer@example.test".into(),
                    username: "peer-owner".into(),
                    password: "peer-contract-password".into(),
                },
                now,
            )
            .await
            .unwrap();
        let browser_token = owner.access_token;
        let user = owner.session.user;
        let tenant = store
            .create_tenant(
                &user,
                "peer-team",
                "Peer team",
                ternilo_control::TenantQuota::default(),
                now,
            )
            .await
            .unwrap()
            .tenant_id;
        let executor = ExecutorId::new("peer-node");
        let enrollment = store
            .create_owned_enrollment(
                &user,
                &tenant,
                None,
                executor.clone(),
                Duration::from_secs(60),
                now,
            )
            .await
            .unwrap();
        let credential = store
            .consume_enrollment(&enrollment.token, now)
            .await
            .unwrap();
        let principal = store
            .authenticate_node(&credential.token, now)
            .await
            .unwrap();
        let edge = Arc::new(
            EdgeGateway::new(store.edge_store())
                .await
                .unwrap()
                .with_cluster("http://127.0.0.1:4321", &key)
                .await
                .unwrap(),
        );
        let route = RouteKey::new(tenant, executor);
        let lease = edge
            .journal
            .acquire(&route, &edge.instance_id, now, EXECUTOR_LEASE_TTL_MS)
            .await
            .unwrap()
            .unwrap();
        let hello = ExecutorHello {
            protocol_version: EXECUTOR_PROTOCOL_VERSION,
            executor_id: route.executor_id.clone(),
            executor_kind: ExecutorKind::EdgeNode,
            instance_nonce: "peer-contract".into(),
            catalog_revision: "peer-contract".into(),
            capabilities: [
                ExecutorCapability::ApplicationRpc,
                ExecutorCapability::RunCancellation,
                ExecutorCapability::SessionEventDelta,
                ExecutorCapability::LiveInvalidations,
            ]
            .into_iter()
            .collect(),
        };
        edge.store
            .register_executor(&route.tenant_id, &hello, now)
            .await
            .unwrap();
        let project = store
            .list_projects(&user, &route.tenant_id)
            .await
            .unwrap()
            .remove(0);
        let workspace = store
            .create_local_workspace(
                &user,
                &route.tenant_id,
                &project.project_id,
                "Peer workspace",
                (
                    &route.executor_id,
                    &ternilo_protocol::WorkspaceId::new("node-workspace"),
                ),
                now,
            )
            .await
            .unwrap();
        store
            .create_edge_session_mapping(
                &user,
                &route.tenant_id,
                &workspace.workspace_id,
                &route.executor_id,
                &SessionId::new("node-session"),
                Some(&SessionId::new("peer-session")),
                ternilo_control::EdgeSessionMetadata {
                    server_model: None,
                    parent_session_id: None,
                    subagent: None,
                    title: "Peer session".into(),
                    archived_at_ms: None,
                    blank: true,
                    permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
                    model: json!({}),
                    agent_preset: "standard".into(),
                    preset_plugins: vec![],
                    profile_plugins: vec![],
                    mode: ternilo_protocol::SessionMode::Execute,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
                now,
            )
            .await
            .unwrap();
        let (sender, incoming) = mpsc::channel(8);
        let connection = ConnectedExecutor {
            principal: principal.clone(),
            hello,
            scope: principal.scope.clone(),
            connection_id: ConnectionId::new("peer-connection"),
            sender,
            lease: lease.clone(),
        };
        edge.executors
            .write()
            .await
            .insert(route.clone(), connection.clone());
        edge.journal
            .publish_peer(
                &route,
                &lease,
                &edge.cluster.as_ref().unwrap().origin,
                &principal,
                now,
            )
            .await
            .unwrap();
        Self {
            store,
            edge,
            route,
            connection,
            incoming,
            user,
            key,
            browser_token,
        }
    }

    fn request(&self, operation: ApplicationOperation) -> ForwardRequest {
        ForwardRequest {
            request_id: random_hex_128(),
            target_instance_id: self.edge.instance_id.clone(),
            tenant_id: self.route.tenant_id.clone(),
            executor_id: self.route.executor_id.clone(),
            fencing_token: self.connection.lease.fencing_token,
            issued_at_ms: now_ms().unwrap(),
            timeout_ms: 10_000,
            body: ExecutorCommandBody::Application { request: operation },
            author: None,
            authorization: None,
        }
    }

    async fn service(&self) -> (Service, tokio::sync::watch::Sender<bool>) {
        let cloud = CloudStore::from_database(self.store.database().clone())
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let worker_policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            store: self.store.clone(),
            cloud,
            cloud_events: CloudSessionEventFeed::from_database(self.store.database().clone())
                .await
                .unwrap(),
            edge: self.edge.clone(),
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: false,
            shutdown: receiver,
            worker_policy: Arc::new(worker_policy),
            catalog: Arc::new(catalog),
        };
        (Service::new(crate::platform::web_router(state)), shutdown)
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Prove online-only SQL avoids decoding offline data, then validate full loading and shared bindings through authenticated HTTP."
)]
async fn online_computer_state_does_not_load_offline_workspace_or_session_metadata() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    let (service, _shutdown) = fixture.service().await;
    let now = now_ms().unwrap();
    let offline = ExecutorId::new("offline-node");
    let enrollment = fixture
        .store
        .create_owned_enrollment(
            &fixture.user,
            &fixture.route.tenant_id,
            None,
            offline.clone(),
            Duration::from_secs(60),
            now,
        )
        .await
        .unwrap();
    fixture
        .store
        .consume_enrollment(&enrollment.token, now)
        .await
        .unwrap();
    let project = fixture
        .store
        .list_projects(&fixture.user, &fixture.route.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let workspace = fixture
        .store
        .create_local_workspace(
            &fixture.user,
            &fixture.route.tenant_id,
            &project.project_id,
            "Offline workspace",
            (
                &offline,
                &ternilo_protocol::WorkspaceId::new("offline-workspace"),
            ),
            now,
        )
        .await
        .unwrap();
    let metadata = ternilo_control::EdgeSessionMetadata {
        server_model: None,
        parent_session_id: None,
        subagent: None,
        title: "Offline session".into(),
        archived_at_ms: None,
        blank: false,
        permissions: ternilo_protocol::PermissionPreset::ReadOnly,
        model: json!({}),
        agent_preset: "standard".into(),
        preset_plugins: vec![],
        profile_plugins: vec![],
        mode: ternilo_protocol::SessionMode::Execute,
        created_at_ms: now,
        updated_at_ms: now,
    };
    fixture
        .store
        .create_edge_session_mapping(
            &fixture.user,
            &fixture.route.tenant_id,
            &workspace.workspace_id,
            &offline,
            &SessionId::new("node-offline-session"),
            Some(&SessionId::new("offline-session")),
            metadata.clone(),
            now,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE control_edge_sessions SET metadata_json='{}' WHERE session_id='offline-session'",
    )
    .execute(fixture.store.database().pool())
    .await
    .unwrap();
    let get = TestClient::get("http://server.test/api/v1/state?online_computers_only=true")
        .add_header(
            "Authorization",
            format!("Bearer {}", fixture.browser_token),
            true,
        )
        .add_header("x-ternilo-tenant", fixture.route.tenant_id.as_str(), true)
        .send(&service);
    let node = async {
        let ControlFrame::Command { command } = fixture.incoming.recv().await.unwrap() else {
            panic!("expected snapshot")
        };
        assert!(matches!(
            command.body,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::Snapshot
            }
        ));
        fixture
            .edge
            .accept_reply(
                &fixture.route,
                &fixture.connection,
                CommandReply::success(
                    command.command_id,
                    now_ms().unwrap(),
                    json!({"workspaces":[],"sessions":[]}),
                ),
            )
            .await
            .unwrap();
    };
    let (mut response, ()) = Box::pin(tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(get, node)
    }))
    .await
    .unwrap();
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let filtered: Value = response.take_json().await.unwrap();
    assert_eq!(filtered["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(
        filtered["sessions"][0]["identity"]["session_id"],
        "peer-session"
    );
    assert!(!filtered.to_string().contains("Offline workspace"));
    sqlx::query(
        "UPDATE control_edge_sessions SET metadata_json=$1 WHERE session_id='offline-session'",
    )
    .bind(ternilo_storage::Json(metadata))
    .execute(fixture.store.database().pool())
    .await
    .unwrap();
    let get = TestClient::get("http://server.test/api/v1/state")
        .add_header(
            "Authorization",
            format!("Bearer {}", fixture.browser_token),
            true,
        )
        .add_header("x-ternilo-tenant", fixture.route.tenant_id.as_str(), true)
        .send(&service);
    let node = async {
        let ControlFrame::Command { command } = fixture.incoming.recv().await.unwrap() else {
            panic!("expected snapshot")
        };
        fixture
            .edge
            .accept_reply(
                &fixture.route,
                &fixture.connection,
                CommandReply::success(
                    command.command_id,
                    now_ms().unwrap(),
                    json!({"workspaces":[],"sessions":[]}),
                ),
            )
            .await
            .unwrap();
    };
    let (mut response, ()) = Box::pin(tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(get, node)
    }))
    .await
    .unwrap();
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let full: Value = response.take_json().await.unwrap();
    assert_eq!(full["workspaces"].as_array().unwrap().len(), 2);
    assert_eq!(full["sessions"].as_array().unwrap().len(), 2);
    assert!(full.to_string().contains("Offline workspace"));
    fixture.edge.shutdown().await;
}

#[test]
fn cluster_origin_requires_verified_tls_outside_literal_loopback() {
    for value in [
        "https://server-a.internal:8443",
        "http://127.0.0.1:4321",
        "http://[::1]:4321",
    ] {
        validate_cluster_origin(value).unwrap();
    }
    for value in [
        "http://localhost:4321",
        "http://0.0.0.0:4321",
        "http://10.0.0.1:4321",
        "https://host/path",
        "https://user:pass@host",
        "https://host?token=secret",
    ] {
        assert!(validate_cluster_origin(value).is_err(), "{value}");
    }
}

#[tokio::test]
async fn cluster_http_authentication_and_replay_do_not_persist_private_calls() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    let (service, _shutdown) = fixture.service().await;
    let input = fixture.request(ApplicationOperation::CredentialSet {
        name: "API_KEY".into(),
        value: "peer-secret-sentinel".into(),
    });
    let bytes = serde_json::to_vec(&input).unwrap();
    let signature = fixture.edge.cluster.as_ref().unwrap().sign(&bytes);
    let path = "http://server.test/api/v1/internal/node/call";
    assert_eq!(
        TestClient::post(path)
            .json(&input)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let mut tampered = bytes.clone();
    tampered.push(b' ');
    assert_eq!(
        TestClient::post(path)
            .add_header("x-ternilo-peer-signature", &signature, true)
            .bytes(tampered)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let request = TestClient::post(path)
        .add_header("x-ternilo-peer-signature", &signature, true)
        .bytes(bytes.clone())
        .send(&service);
    let node = async {
        let ControlFrame::Command { command } = fixture.incoming.recv().await.unwrap() else {
            panic!("expected Node command")
        };
        assert_eq!(command.body, input.body);
        fixture
            .edge
            .accept_reply(
                &fixture.route,
                &fixture.connection,
                CommandReply::success(command.command_id, now_ms().unwrap(), json!({"saved":true})),
            )
            .await
            .unwrap();
    };
    let (mut response, ()) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(request, node)
    })
    .await
    .unwrap();
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let value: Value = response.take_json().await.unwrap();
    assert_eq!(value["result"]["Ok"]["saved"], true);
    let mut replay = TestClient::post(path)
        .add_header("x-ternilo-peer-signature", &signature, true)
        .bytes(bytes)
        .send(&service)
        .await;
    let value: Value = replay.take_json().await.unwrap();
    assert_eq!(value["result"]["Err"]["code"], "conflict");
    assert!(fixture.incoming.try_recv().is_err());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gateway_commands")
        .fetch_one(fixture.store.database().pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "private request and response must never enter durable delivery"
    );
    let peer_rows: Vec<String> =
        sqlx::query_scalar("SELECT principal_json FROM gateway_peer_routes")
            .fetch_all(fixture.store.database().pool())
            .await
            .unwrap();
    assert!(
        peer_rows
            .iter()
            .all(|row| !row.contains("peer-secret-sentinel"))
    );
    fixture.edge.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify stale input authority, connection fences and the real attributed receipt in one scenario."
)]
async fn cluster_submission_preserves_actor_and_rejects_expired_or_changed_connections() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    let mut input = fixture.request(ApplicationOperation::SessionSubmit {
        session_id: SessionId::new("node-session"),
        request: SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: None,
            content: SubmissionContent::Prompt {
                input: "peer task".into(),
            },
            references: vec![],
            attachments: vec![],
        },
    });
    input.author = Some(InputAuthor::Account {
        user_id: fixture.user.user_id.clone(),
        username: fixture.user.username.clone(),
    });
    input.authorization = Some(
        fixture
            .edge
            .store
            .node_input_authorization(&fixture.connection.principal, &fixture.user.user_id)
            .await
            .unwrap(),
    );
    let mut stale_authority = fixture.request(ApplicationOperation::SessionSubmit {
        session_id: SessionId::new("node-session"),
        request: SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: None,
            content: SubmissionContent::Prompt {
                input: "old input".into(),
            },
            references: vec![],
            attachments: vec![],
        },
    });
    stale_authority.author = input.author.clone();
    stale_authority.authorization = input.authorization.clone();
    stale_authority
        .authorization
        .as_mut()
        .unwrap()
        .status_revision += 1;
    assert!(
        fixture
            .edge
            .receive_forwarded(stale_authority)
            .await
            .is_err()
    );
    let mut expired = fixture.request(ApplicationOperation::LiveActivities);
    expired.issued_at_ms -= 60_000;
    assert!(fixture.edge.receive_forwarded(expired).await.is_err());
    let mut stale = fixture.request(ApplicationOperation::LiveActivities);
    stale.fencing_token += 1;
    assert!(fixture.edge.receive_forwarded(stale).await.is_err());
    assert!(fixture.incoming.try_recv().is_err());
    let call = fixture.edge.receive_forwarded(input);
    let node = async {
        let ControlFrame::Command { command } = fixture.incoming.recv().await.unwrap() else {
            panic!("expected command")
        };
        let provenance = command.input_provenance.as_ref().unwrap();
        assert!(
            matches!(&provenance.author, InputAuthor::Account {user_id,..} if user_id==&fixture.user.user_id)
        );
        assert!(command.input_authorization.is_some());
        assert_eq!(command.scope, fixture.connection.scope);
        let receipt = ternilo_protocol::SessionSubmission {
            id: provenance.input_id.clone(),
            run_id: provenance.run_id.clone().unwrap(),
            content: SubmissionContent::Prompt {
                input: "peer task".into(),
            },
            provenance: Some(provenance.clone()),
            references: vec![],
            attachments: vec![],
            placement: ternilo_protocol::SubmissionPlacement::Queued,
            created_at_ms: now_ms().unwrap(),
            updated_at_ms: now_ms().unwrap(),
        };
        fixture
            .edge
            .accept_reply(
                &fixture.route,
                &fixture.connection,
                CommandReply::success(
                    command.command_id,
                    now_ms().unwrap(),
                    serde_json::to_value(receipt).unwrap(),
                ),
            )
            .await
            .unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(call, node)
    }))
    .await
    .unwrap();
    assert_eq!(
        result.unwrap()["provenance"]["author"]["user_id"],
        fixture.user.user_id.as_str()
    );
    fixture.edge.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise two independent pools through event commits, rollback, revocation and connection replacement."
)]
async fn cluster_two_gateways_follow_committed_events_leases_and_revoked_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("peer.sqlite").display()
    );
    let fixture = Fixture::new(&url, None).await;
    let store_b = ControlStore::connect(&url, None, SecretCipher::from_key([79; 32]), 4)
        .await
        .unwrap();
    let gateway_b = EdgeGateway::new(store_b.edge_store())
        .await
        .unwrap()
        .with_cluster("http://127.0.0.1:4322", &fixture.key)
        .await
        .unwrap();
    assert!(
        gateway_b
            .is_connected(&fixture.route.tenant_id, &fixture.route.executor_id)
            .await
    );
    let mut hints = gateway_b.subscribe_live();
    let event = SessionEvent {
        seq: 0,
        occurred_at_ms: now_ms().unwrap(),
        run_id: RunId::new("peer-run"),
        kind: SessionEventKind::TurnStarted,
    };
    fixture
        .edge
        .journal
        .merge_events(
            &fixture.edge.store,
            &fixture.route,
            &fixture.connection.lease,
            &SessionId::new("node-session"),
            std::slice::from_ref(&event),
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let hint = tokio::time::timeout(Duration::from_secs(3), hints.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(hint,EdgeLiveNotification::Rescan{tenant_id,executor_id} if tenant_id==fixture.route.tenant_id && executor_id==fixture.route.executor_id)
    );
    assert_eq!(
        gateway_b
            .cached_events(
                &fixture.route.tenant_id,
                &fixture.route.executor_id,
                &SessionId::new("node-session")
            )
            .await
            .unwrap(),
        vec![event]
    );
    let revision = fixture
        .edge
        .journal
        .live_routes()
        .await
        .unwrap()
        .remove(0)
        .revision;
    let bad = SessionEvent {
        seq: 5,
        occurred_at_ms: now_ms().unwrap(),
        run_id: RunId::new("peer-run"),
        kind: SessionEventKind::TurnCancelled,
    };
    assert!(
        fixture
            .edge
            .journal
            .merge_events(
                &fixture.edge.store,
                &fixture.route,
                &fixture.connection.lease,
                &SessionId::new("node-session"),
                &[bad],
                now_ms().unwrap()
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .edge
            .journal
            .live_routes()
            .await
            .unwrap()
            .remove(0)
            .revision,
        revision,
        "rolled-back events must not publish hints"
    );
    fixture
        .store
        .revoke_owned_executor(
            &fixture.user,
            &fixture.route.tenant_id,
            &fixture.route.executor_id,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !gateway_b
            .is_connected(&fixture.route.tenant_id, &fixture.route.executor_id)
            .await
    );
    fixture.edge.shutdown().await;
    assert!(
        gateway_b
            .journal
            .peer_route(&fixture.route, now_ms().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let now = now_ms().unwrap();
    let newer = gateway_b
        .journal
        .acquire(&fixture.route, &gateway_b.instance_id, now, 60_000)
        .await
        .unwrap()
        .unwrap();
    assert!(newer.fencing_token > fixture.connection.lease.fencing_token);
    assert!(
        fixture
            .edge
            .journal
            .check_lease(&fixture.route, &fixture.connection.lease, now)
            .await
            .is_err()
    );
    assert!(
        gateway_b
            .journal
            .peer_route(&fixture.route, now)
            .await
            .unwrap()
            .is_none(),
        "an old endpoint cannot match a new lease"
    );
    gateway_b.shutdown().await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn cluster_postgres_routes_remain_scoped_and_runtime_cannot_change_schema() {
    use sqlx::Executor as _;
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, "node_peer_runtime", "node-peer-password").await;
    let mut runtime = reqwest::Url::parse(&url).unwrap();
    runtime.set_username("node_peer_runtime").unwrap();
    runtime.set_password(Some("node-peer-password")).unwrap();
    let fixture = Fixture::new(runtime.as_str(), Some(&url)).await;
    assert!(
        fixture
            .edge
            .journal
            .peer_route(&fixture.route, now_ms().unwrap())
            .await
            .unwrap()
            .is_some()
    );
    let unscoped: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gateway_peer_routes")
        .fetch_one(fixture.store.database().pool())
        .await
        .unwrap();
    assert_eq!(
        unscoped, 0,
        "peer endpoints and credentials require a tenant scope"
    );
    let hints = fixture.edge.journal.live_routes().await.unwrap();
    assert_eq!(
        hints.len(),
        1,
        "the shared hint feed contains only identifiers and revisions"
    );
    assert!(
        sqlx::query("CREATE TABLE peer_unauthorized_ddl (id BIGINT)")
            .execute(fixture.store.database().pool())
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query("UPDATE gateway_live_routes SET revision=revision+1")
            .execute(fixture.store.database().pool())
            .await
            .unwrap()
            .rows_affected(),
        0
    );
    fixture.edge.shutdown().await;
    admin.close().await;
}

#[tokio::test]
async fn cluster_http_forwarding_crosses_two_servers_without_retrying_ambiguous_results() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    fixture
        .edge
        .journal
        .publish_peer(
            &fixture.route,
            &fixture.connection.lease,
            &origin,
            &fixture.connection.principal,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let (service, _shutdown) = fixture.service().await;
    let server =
        salvo_core::Server::new(salvo_core::conn::tcp::TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(async move { server.try_serve(service).await.unwrap() });
    let gateway_b = EdgeGateway::new(fixture.store.edge_store())
        .await
        .unwrap()
        .with_cluster("http://127.0.0.1:4322", &fixture.key)
        .await
        .unwrap();
    let call = gateway_b.call(
        &fixture.route.tenant_id,
        &fixture.route.executor_id,
        ApplicationOperation::CredentialSet {
            name: "API_KEY".into(),
            value: "transport-secret".into(),
        },
    );
    let node = async {
        let ControlFrame::Command { command } = fixture.incoming.recv().await.unwrap() else {
            panic!("expected command")
        };
        fixture
            .edge
            .accept_reply(
                &fixture.route,
                &fixture.connection,
                CommandReply::success(
                    command.command_id,
                    now_ms().unwrap(),
                    json!({"forwarded":true}),
                ),
            )
            .await
            .unwrap();
    };
    let (result, ()) = Box::pin(tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(call, node)
    }))
    .await
    .unwrap();
    assert_eq!(result.unwrap()["forwarded"], true);
    // A delivered ephemeral command with no return path is never redelivered.
    let call = gateway_b.call_with_timeout(
        &fixture.route.tenant_id,
        &fixture.route.executor_id,
        ApplicationOperation::CredentialSet {
            name: "API_KEY".into(),
            value: "uncertain-secret".into(),
        },
        Duration::from_millis(500),
    );
    let node = async {
        assert!(matches!(
            fixture.incoming.recv().await,
            Some(ControlFrame::Command { .. })
        ));
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(6), async { tokio::join!(call, node) })
            .await
            .unwrap();
    assert!(
        matches!(result,Err(error) if matches!(error.code,ErrorCode::Execution | ErrorCode::Unavailable))
    );
    assert!(fixture.incoming.try_recv().is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM gateway_commands")
            .fetch_one(fixture.store.database().pool())
            .await
            .unwrap(),
        0
    );
    handle.stop_graceful(None);
    serving.await.unwrap();
    fixture.edge.shutdown().await;
}

#[path = "model_forwarding/tests.rs"]
mod computer_stream_tests;
