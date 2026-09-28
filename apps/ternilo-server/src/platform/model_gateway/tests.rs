use std::{collections::BTreeSet, sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use salvo_core::{conn::tcp::TcpAcceptor, http::StatusCode, prelude::Json, server::ServerHandle};
use salvo_extra::affix_state;
use serde_json::{Value, json};
use ternilo_cloud::{
    CloudRunDraft, CloudSessionDraft, CloudStore, StartedRun, WorkerRegisterRequest,
};
use ternilo_control::{
    ControlStore, InstanceMode, ModelGrantInput, ModelGrantSubject, ModelProviderInput,
    ModelPublicationInput, ModelServiceRequest, NativeRegistration, NativeSessionGrant, PageQuery,
    SecretCipher,
};
use ternilo_protocol::{
    AgentId, ModelRequest, PermissionPreset, ProviderModel, ProviderModelDefaults,
    ProviderModelSettings, ProviderProfile, ProviderProtocol, RunModelBinding, RunModelSnapshot,
    SessionId, SessionMode,
};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind,
};
use tokio::sync::Mutex;

use super::*;
use crate::platform::{edge::EdgeGateway, state::AppState};

struct RunningServer {
    base: String,
    handle: ServerHandle,
    task: tokio::task::JoinHandle<()>,
}

impl RunningServer {
    async fn start(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
        let handle = server.handle();
        let task = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
        Self {
            base: format!("http://{address}"),
            handle,
            task,
        }
    }

    async fn close(self) {
        self.handle.stop_graceful(Some(Duration::from_secs(1)));
        self.task.await.unwrap();
    }
}

#[derive(Clone, Default)]
struct UpstreamState(Arc<Mutex<Vec<Value>>>);

#[handler]
async fn fake_upstream(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    let body: Value = request.parse_json().await.unwrap();
    assert_eq!(
        request.headers()["authorization"],
        "Bearer workload-upstream-secret"
    );
    let state = depot.get_typed::<UpstreamState>().unwrap();
    let case = body["messages"][0]["content"].as_str().unwrap().to_owned();
    let mut captured = state.0.lock().await;
    captured.push(body.clone());
    let attempt = captured
        .iter()
        .filter(|item| item["messages"][0]["content"] == case)
        .count();
    drop(captured);
    response.headers_mut().insert(
        "x-request-id",
        format!("upstream-{case}-{attempt}").parse().unwrap(),
    );
    if (case == "retry" || case == "unknown-retry") && attempt == 1 {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
        let mut error = json!({"error":{"message":"workload-upstream-secret temporary failure"}});
        if case == "retry" {
            error["usage"] = json!({"prompt_tokens":7,"completion_tokens":2});
        }
        response.render(Json(error));
    } else if matches!(case.as_str(), "eof" | "revoke" | "disconnect") {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        let event = json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}],"usage":{"prompt_tokens":12,"completion_tokens":3,"completion_tokens_details":{"reasoning_tokens":2}}});
        let bytes = format!("data: {event}\n\n").into_bytes();
        let stream = futures_util::stream::once(async { Ok::<_, std::io::Error>(bytes) });
        if case == "eof" {
            response.stream(stream);
        } else {
            response.stream(stream.chain(futures_util::stream::pending()));
        }
    } else {
        response.render(Json(json!({
            "choices":[{"message":{"content":"complete"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":70},"completion_tokens_details":{"reasoning_tokens":10}}
        })));
    }
}

struct Fixture {
    state: AppState,
    owner: NativeSessionGrant,
    actor: ternilo_control::ControlUser,
    server: RunningServer,
    upstream: RunningServer,
    captured: UpstreamState,
    shutdown: tokio::sync::watch::Sender<bool>,
    token: String,
    request: WorkerModelRequest,
    client: reqwest::Client,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_model_source(false).await
    }

    async fn with_model_source(byok: bool) -> Self {
        Self::configured(byok, false).await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Build independent ownership, permission and execution boundaries through their public store APIs."
    )]
    async fn configured(byok: bool, shared: bool) -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([93; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "owner@example.test".to_owned(),
                    username: "owner".to_owned(),
                    password: "worker-model-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let captured = UpstreamState::default();
        let upstream = RunningServer::start(
            Router::new()
                .hoop(affix_state::inject(captured.clone()))
                .push(Router::with_path("v1/chat/completions").post(fake_upstream)),
        )
        .await;
        let defaults = ProviderModelDefaults {
            context_window: 32_000,
            max_output_tokens: 512,
            reasoning: None,
        };
        let profile = ProviderProfile {
            id: "workload".to_owned(),
            display_name: "Workload provider".to_owned(),
            base_url: format!("{}/v1", upstream.base),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: None,
            defaults: defaults.clone(),
            models: vec![ProviderModel {
                id: "internal-model".to_owned(),
                display_name: None,
                settings: ProviderModelSettings::Inherit,
            }],
            timeout_ms: 20_000,
            max_attempts: 3,
            retry_base_delay_ms: 10,
        };
        store
            .save_model_provider(
                &owner.session.user,
                &ModelProviderInput {
                    profile: profile.clone(),
                    enabled: true,
                    api_key: Some("workload-upstream-secret".to_owned()),
                    clear_api_key: false,
                },
                now,
            )
            .await
            .unwrap();
        store
            .save_model_publication(
                &owner.session.user,
                &ModelPublicationInput {
                    model_id: "public-model".to_owned(),
                    display_name: "Public model".to_owned(),
                    provider_id: "workload".to_owned(),
                    upstream_model: "internal-model".to_owned(),
                    enabled: true,
                },
                now,
            )
            .await
            .unwrap();
        let grant = store
            .save_model_grant(
                &owner.session.user,
                None,
                &ModelGrantInput {
                    name: "Execution allowance".to_owned(),
                    subject: ModelGrantSubject::User {
                        id: owner.session.user.user_id.as_str().to_owned(),
                    },
                    model_ids: vec!["public-model".to_owned()],
                    monthly_tokens: 1_000_000,
                    max_concurrent_requests: 2,
                    expires_at_ms: None,
                    allow_resource_sharing: true,
                },
                now,
            )
            .await
            .unwrap();
        let mut binding = RunModelBinding::Platform {
            grant_id: grant.grant_id,
            model_id: "public-model".to_owned(),
            beneficiary_user_id: owner.session.user.user_id.clone(),
        };
        if byok {
            store
                .upsert_user_provider_profile(
                    &owner.session.user,
                    &owner.session.personal_tenant_id,
                    ProviderProfile {
                        id: "personal".to_owned(),
                        api_key_ref: Some("PERSONAL_MODEL_KEY".to_owned()),
                        ..profile
                    },
                    now,
                )
                .await
                .unwrap();
            store
                .put_user_credential(
                    &owner.session.user,
                    &owner.session.personal_tenant_id,
                    "PERSONAL_MODEL_KEY",
                    "workload-upstream-secret",
                    now,
                )
                .await
                .unwrap();
            binding = RunModelBinding::UserProvider {
                tenant_id: owner.session.personal_tenant_id.clone(),
                owner_user_id: owner.session.user.user_id.clone(),
                provider_id: "personal".to_owned(),
                model: "internal-model".to_owned(),
            };
        }
        let snapshot = RunModelSnapshot {
            binding: binding.clone(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            defaults,
            reasoning_effort: None,
            display_name: "Public model".to_owned(),
            source_name: "Execution allowance".to_owned(),
        };
        let cloud = CloudStore::from_database(store.database().clone())
            .await
            .unwrap();
        let user = &owner.session.user;
        let (tenant, project_id) = if shared {
            let tenant = store
                .create_tenant(
                    user,
                    "model-team",
                    "Model team",
                    ternilo_control::TenantQuota::default(),
                    now,
                )
                .await
                .unwrap()
                .tenant_id;
            let project_id = store
                .list_projects(user, &tenant)
                .await
                .unwrap()
                .remove(0)
                .project_id;
            (tenant, project_id)
        } else {
            (
                owner.session.personal_tenant_id.clone(),
                owner.session.personal_project_id.clone(),
            )
        };
        let workspace = store
            .create_cloud_workspace(user, &tenant, &project_id, "Worker models", now)
            .await
            .unwrap();
        let session = cloud
            .create_session(
                CloudSessionDraft {
                    project_id: project_id.clone(),
                    workspace_id: workspace.workspace_id.clone(),
                    session_id: Some(SessionId::new("model-session")),
                    agent_id: AgentId::new("agent"),
                    title: "Model session".to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: Some(snapshot.clone()),
                    reserved_model_tokens: 100_000,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &tenant,
                &user.user_id,
                now,
            )
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let actor = if shared {
            let actor = store
                .upsert_user(
                    &ternilo_control::OidcPrincipal {
                        issuer: "https://identity.example".to_owned(),
                        subject: "collaborator".to_owned(),
                        email: None,
                        display_name: Some("Collaborator".to_owned()),
                    },
                    "test-collaborator",
                    now,
                )
                .await
                .unwrap();
            store
                .set_membership(
                    user,
                    &tenant,
                    &actor.user_id,
                    ternilo_control::TenantRole::Member,
                    now,
                )
                .await
                .unwrap();
            store
                .set_resource_share(
                    user,
                    &tenant,
                    ternilo_control::ResourceKind::Session,
                    session.session_id.as_str(),
                    &actor.user_id,
                    Some(ternilo_control::ResourcePermissions {
                        view: true,
                        submit: true,
                        stop: false,
                        configure: false,
                    }),
                    now,
                )
                .await
                .unwrap();
            actor
        } else {
            user.clone()
        };
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let compiled = policy
            .compile_run(
                CloudRunDraft {
                    project_id,
                    workspace_id: workspace.workspace_id,
                    agent_id: session.agent_id,
                    session_id: session.session_id,
                    run_id: None,
                    limits: policy.maximum_limits,
                    permissions: session.permissions,
                    mode: SessionMode::Execute,
                    profile: ternilo_cloud::cloud_profile(Some(&snapshot)),
                    input: "Run a model call".to_owned(),
                    references: vec![],
                    reference_contexts: vec![],
                    attachments: vec![],
                    reserved_model_tokens: 100_000,
                },
                tenant.clone(),
                user.user_id.clone(),
                actor.user_id.clone(),
                &catalog,
            )
            .unwrap();
        cloud
            .enqueue_session_submission_as(
                &actor.user_id,
                &compiled,
                &ternilo_protocol::SessionSubmissionRequest {
                    delivery: ternilo_protocol::SubmissionDelivery::Queue,
                    run_id: Some(compiled.spec.metadata.run_id.clone()),
                    content: ternilo_protocol::SubmissionContent::Prompt {
                        input: compiled.spec.input.clone(),
                    },
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
                now,
            )
            .await
            .unwrap();
        let credential = cloud
            .create_worker_credential(&ExecutorId::new("model-worker"), "model-storage", now)
            .await
            .unwrap();
        let lease = Duration::from_secs(60);
        let identity = cloud
            .register_authenticated_worker(
                &credential.token,
                &WorkerRegisterRequest {
                    capacity: ternilo_cloud::WorkerCapacity::default(),
                    hello: ExecutorHello {
                        protocol_version: EXECUTOR_PROTOCOL_VERSION,
                        executor_id: credential.worker_id.clone(),
                        executor_kind: ExecutorKind::CloudWorker,
                        instance_nonce: "model-worker-process".to_owned(),
                        catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
                        capabilities: BTreeSet::from([
                            ExecutorCapability::CloudRun,
                            ExecutorCapability::AddressedSessionCommands,
                            ExecutorCapability::WorkspaceFiles,
                        ]),
                    },
                    storage_id: "model-storage".to_owned(),
                    root_id: "model-root".to_owned(),
                },
                lease,
                now,
            )
            .await
            .unwrap();
        let worker_store = cloud
            .authenticated_worker(&credential.token, &identity, now)
            .await
            .unwrap();
        let claim = worker_store
            .claim_run(identity.worker_id.as_str(), lease, now)
            .await
            .unwrap()
            .unwrap();
        let run: StartedRun = worker_store
            .start_run(claim, identity.worker_id.as_str(), lease, now)
            .await
            .unwrap()
            .unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud_events: ternilo_cloud::CloudSessionEventFeed::from_database(
                store.database().clone(),
            )
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
        let server = RunningServer::start(
            Router::new()
                .hoop(affix_state::inject(state.clone()))
                .push(router())
                .push(crate::platform::models::access_router()),
        )
        .await;
        Self {
            state,
            owner,
            actor,
            server,
            upstream,
            captured,
            shutdown,
            token: credential.token,
            request: WorkerModelRequest {
                identity,
                run: ternilo_cloud::RunLease::from(&run),
                request_id: 1,
                binding,
                request: ModelRequest {
                    run_id: ternilo_protocol::RunId::new("model-test-run"),
                    system_prompt: "retry".to_owned(),
                    messages: vec![],
                    tools: vec![],
                    step: 1,
                },
            },
            client: reqwest::Client::new(),
        }
    }

    async fn send(&self, case: &str, request_id: u64) -> reqwest::Response {
        let mut request = self.request.clone();
        request.request_id = request_id;
        request.request.system_prompt = case.to_owned();
        self.send_body(&serde_json::to_value(request).unwrap())
            .await
    }

    async fn send_body(&self, body: &Value) -> reqwest::Response {
        self.client
            .post(format!("{}/internal/worker/v1/model", self.server.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .unwrap()
    }

    async fn ledger(&self, id: &str) -> ModelServiceRequest {
        self.state
            .store
            .list_model_service_requests(
                &self.owner.session.user,
                None,
                &PageQuery {
                    query: Some(id.to_owned()),
                    ..PageQuery::default()
                },
            )
            .await
            .unwrap()
            .requests
            .into_iter()
            .find(|request| request.request_id == id)
            .unwrap()
    }

    async fn settled(&self, id: &str) -> ModelServiceRequest {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let request = self.ledger(id).await;
                if request.state != ModelRequestState::Pending {
                    return request;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("accepted model request must settle")
    }

    async fn close(self) {
        self.shutdown.send_replace(true);
        self.server.close().await;
        self.state.edge.shutdown().await;
        self.upstream.close().await;
    }
}

fn request_id(response: &reqwest::Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned()
}

fn frames(body: &str) -> Vec<WorkerModelFrame> {
    body.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn read_partial(response: &mut reqwest::Response) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            bytes.extend_from_slice(&response.chunk().await.unwrap().expect("model delta"));
            if let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
                let frame: WorkerModelFrame = serde_json::from_slice(&bytes[..end]).unwrap();
                assert!(matches!(frame, WorkerModelFrame::Delta { delta } if delta == "partial"));
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn retries_meter_each_real_attempt_and_unknown_usage_is_not_free() {
    let fixture = Fixture::new().await;
    for (case, ordinal) in [("retry", 1), ("unknown-retry", 2)] {
        let response = fixture.send(case, ordinal).await;
        let id = request_id(&response);
        let body = response.text().await.unwrap();
        assert!(!body.contains("workload-upstream-secret"));
        assert!(!body.contains("internal-model"));
        let frames = frames(&body);
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, WorkerModelFrame::RetryScheduled { retry: 1, .. }))
        );
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, WorkerModelFrame::RetryStarted { retry: 1 }))
        );
        let WorkerModelFrame::Complete { response } = frames.last().unwrap() else {
            panic!("{body}")
        };
        assert_eq!(response.model, "public-model");
        assert_eq!(response.attempts, 2);
        assert_eq!(response.provider_request_id.as_deref(), Some(id.as_str()));
        let ledger = fixture.ledger(&id).await;
        assert_eq!(ledger.state, ModelRequestState::Completed);
        assert_eq!(ledger.attempts.len(), 2);
        assert_eq!(ledger.attempts[1].accounted_tokens, Some(120));
        assert_eq!(ledger.actor_user_id, fixture.owner.session.user.user_id);
        assert_eq!(ledger.workload.unwrap().run_id, fixture.request.run.run_id);
        if case == "retry" {
            assert_eq!(ledger.accounted_tokens, Some(129));
        } else {
            assert_eq!(ledger.accounted_tokens, None);
            assert_eq!(ledger.attempts[0].accounted_tokens, None);
            assert!(ledger.attempts[0].reserved_tokens > 4_096);
        }
        let duplicate = fixture.send(case, ordinal).await;
        assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    }
    let history: Value = fixture
        .client
        .get(format!(
            "{}/api/v1/model-access/requests",
            fixture.server.base
        ))
        .bearer_auth(&fixture.owner.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(history["requests"].as_array().unwrap().len(), 2);
    assert_eq!(
        history["requests"][0]["attempts"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        history["requests"][0]["attempts"][0]["error_code"],
        "upstream_failed"
    );
    assert_eq!(
        history["requests"][0]["workload"]["run_id"],
        fixture.request.run.run_id.as_str()
    );
    for private in [
        "lease_token",
        "writer_fencing_token",
        "worker_id",
        "execution_reservation_id",
        "provider_id",
        "upstream_request_id",
        "upstream_model",
        "raw_usage",
        "workload-upstream-secret",
    ] {
        assert!(
            !history.to_string().contains(private),
            "private workload field leaked: {private}"
        );
    }
    assert_eq!(fixture.captured.0.lock().await.len(), 4);
    let usage = fixture
        .state
        .store
        .model_service_usage(&fixture.owner.session.user, None, now_ms().unwrap())
        .await
        .unwrap();
    assert_eq!(usage.request_count, 2);
    assert_eq!(usage.used_tokens, 249);
    assert_eq!(usage.unknown_requests, 1);
    assert!(usage.reserved_tokens > 4_096);
    fixture.close().await;
}

#[tokio::test]
async fn partial_eof_and_live_grant_revocation_settle_server_observed_usage() {
    let fixture = Fixture::new().await;
    let response = fixture.send("eof", 1).await;
    let id = request_id(&response);
    let body = response.text().await.unwrap();
    assert!(matches!(
        frames(&body).last(),
        Some(WorkerModelFrame::Error { .. })
    ));
    let ledger = fixture.ledger(&id).await;
    assert_eq!(ledger.state, ModelRequestState::Failed);
    assert_eq!(ledger.accounted_tokens, Some(15));
    assert_eq!(ledger.attempts.len(), 1);
    let mut response = fixture.send("revoke", 2).await;
    let id = request_id(&response);
    read_partial(&mut response).await;
    let mut stream = response.bytes_stream();
    let RunModelBinding::Platform { grant_id, .. } = &fixture.request.binding else {
        unreachable!()
    };
    fixture
        .state
        .store
        .revoke_model_grant(&fixture.owner.session.user, grant_id, now_ms().unwrap())
        .await
        .unwrap();
    let mut remainder = Vec::new();
    tokio::time::timeout(Duration::from_secs(6), async {
        while let Some(chunk) = stream.next().await {
            remainder.extend_from_slice(&chunk.unwrap());
        }
    })
    .await
    .unwrap();
    assert!(String::from_utf8(remainder).unwrap().contains("error"));
    let ledger = fixture.ledger(&id).await;
    assert_eq!(ledger.state, ModelRequestState::Cancelled);
    assert_eq!(ledger.accounted_tokens, Some(15));
    assert_eq!(ledger.attempts.len(), 1);
    assert_eq!(fixture.captured.0.lock().await.len(), 2);
    fixture.close().await;
}

#[tokio::test]
async fn canonical_model_binding_rejects_forgery_and_disconnect_settles_usage() {
    let fixture = Fixture::new().await;
    let mut body = serde_json::to_value(&fixture.request).unwrap();
    body["actor_user_id"] = json!("another-user");
    assert_eq!(
        fixture.send_body(&body).await.status(),
        StatusCode::BAD_REQUEST
    );
    body.as_object_mut().unwrap().remove("actor_user_id");
    body["binding"]["model_id"] = json!("another-model");
    assert_eq!(
        fixture.send_body(&body).await.status(),
        StatusCode::FORBIDDEN
    );
    assert!(fixture.captured.0.lock().await.is_empty());
    fixture
        .state
        .store
        .set_instance_mode(
            &fixture.owner.session.user,
            InstanceMode::SingleUser,
            2,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let mut response = fixture.send("disconnect", 1).await;
    let id = request_id(&response);
    read_partial(&mut response).await;
    drop(response);
    let ledger = fixture.settled(&id).await;
    assert_eq!(ledger.state, ModelRequestState::Cancelled);
    assert_eq!(ledger.accounted_tokens, Some(15));
    assert_eq!(fixture.captured.0.lock().await.len(), 1);
    fixture.close().await;
}

#[tokio::test]
async fn user_provider_uses_the_same_attempt_ledger_without_a_platform_allowance() {
    let fixture = Fixture::with_model_source(true).await;
    let response = fixture.send("retry", 1).await;
    let id = request_id(&response);
    let body = response.text().await.unwrap();
    let frames = frames(&body);
    let WorkerModelFrame::Complete { response } = frames.last().unwrap() else {
        panic!("{body}")
    };
    assert_eq!(response.provider, "personal");
    assert_eq!(response.attempts, 2);
    assert!(!body.contains("workload-upstream-secret"));
    let ledger = fixture.ledger(&id).await;
    assert_eq!(
        ledger.source,
        ternilo_control::ModelRequestSource::UserProvider
    );
    assert!(ledger.grant_id.is_none());
    assert!(ledger.key_id.is_none());
    assert_eq!(ledger.accounted_tokens, Some(129));
    assert_eq!(ledger.attempts.len(), 2);
    let entitlements = fixture
        .state
        .store
        .list_model_entitlements(
            &fixture.owner.session.user,
            &PageQuery::default(),
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(entitlements.entitlements[0].grant.quota.used_tokens, 0);
    assert_eq!(entitlements.entitlements[0].grant.quota.reserved_tokens, 0);
    assert_eq!(fixture.captured.0.lock().await.len(), 2);
    fixture.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise original sharing authority across a real parent run, a background subagent and live revocation through HTTP."
)]
async fn shared_actor_subagent_uses_original_authority_without_copying_resource_grants() {
    let mut fixture = Fixture::configured(false, true).await;
    let now = now_ms().unwrap();
    let worker = fixture
        .state
        .cloud
        .authenticated_worker(&fixture.token, &fixture.request.identity, now)
        .await
        .unwrap();
    let (_, parent) = worker
        .authorized_run(&fixture.request.run, now)
        .await
        .unwrap();
    assert_ne!(
        parent.claim.actor_user_id,
        parent.claim.spec.metadata.user_id
    );
    let response = fixture.send("parent", 1).await;
    let parent_request_id = request_id(&response);
    assert!(matches!(
        frames(&response.text().await.unwrap()).last(),
        Some(WorkerModelFrame::Complete { .. })
    ));
    let parent_usage = fixture.ledger(&parent_request_id).await;
    assert_eq!(parent_usage.actor_user_id, parent.claim.actor_user_id);
    assert_eq!(
        parent_usage.model_beneficiary_user_id,
        fixture.owner.session.user.user_id
    );
    let child_id = SessionId::new("background-child");
    worker
        .create_subagent_for_worker(
            fixture.request.identity.worker_id.as_str(),
            &parent,
            &child_id,
            &ternilo_protocol::SubagentSessionMetadata {
                subagent_id: ternilo_protocol::SubagentId::new("researcher"),
                provider: "in-process".to_owned(),
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
            },
            "Background research",
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let mut child_spec = parent.claim.spec.clone();
    child_spec.metadata.session_id = child_id.clone();
    child_spec.metadata.run_id = ternilo_protocol::RunId::new("background-run");
    child_spec.input = "Background work".to_owned();
    worker
        .enqueue_subagent_for_worker(
            fixture.request.identity.worker_id.as_str(),
            &parent,
            &child_id,
            &child_spec,
            &child_spec.input,
            1,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let child_claim = worker
        .claim_run(
            fixture.request.identity.worker_id.as_str(),
            Duration::from_secs(60),
            now_ms().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let child = worker
        .start_run(
            child_claim,
            fixture.request.identity.worker_id.as_str(),
            Duration::from_secs(60),
            now_ms().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.claim.actor_user_id, parent.claim.actor_user_id);
    assert_eq!(
        child.claim.authorization_session_id,
        parent.claim.session_id
    );
    let shares = fixture
        .state
        .store
        .list_resource_shares(
            &fixture.owner.session.user,
            &parent.claim.tenant_id,
            ternilo_control::ResourceKind::Session,
            child_id.as_str(),
            &PageQuery::default(),
        )
        .await
        .unwrap();
    assert!(shares.shares.is_empty());
    let direct = fixture
        .state
        .store
        .resource_access(
            &fixture.actor,
            &parent.claim.tenant_id,
            ternilo_control::ResourceKind::Session,
            child_id.as_str(),
        )
        .await;
    assert!(direct.is_err() || !direct.unwrap().permissions.submit);
    assert_busy_child_followup_is_accepted(&fixture, &parent, &child).await;
    fixture.request.run = ternilo_cloud::RunLease::from(&child);
    let mut response = fixture.send("revoke", 1).await;
    let child_request_id = request_id(&response);
    read_partial(&mut response).await;
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &parent.claim.tenant_id,
            ternilo_control::ResourceKind::Session,
            parent.claim.session_id.as_str(),
            &parent.claim.actor_user_id,
            None,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.send("new-child-call", 2).await.status(),
        StatusCode::FORBIDDEN
    );
    let tail = tokio::time::timeout(Duration::from_secs(6), response.text())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        frames(&tail).last(),
        Some(WorkerModelFrame::Error { .. })
    ));
    let settled = fixture.ledger(&child_request_id).await;
    assert_eq!(settled.state, ModelRequestState::Cancelled);
    assert_eq!(settled.accounted_tokens, Some(15));
    assert_eq!(settled.actor_user_id, parent.claim.actor_user_id);
    let workload = settled.workload.unwrap();
    assert_eq!(workload.session_id, child_id);
    assert_eq!(workload.authorization_session_id, parent.claim.session_id);
    assert_eq!(fixture.captured.0.lock().await.len(), 2);
    fixture.close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify busy child follow-up acceptance, independent author and unchanged running authority through the actual HTTP routes."
)]
async fn assert_busy_child_followup_is_accepted(
    fixture: &Fixture,
    parent: &StartedRun,
    child: &StartedRun,
) {
    use salvo_core::test::{ResponseExt as _, TestClient};
    let service = salvo_core::Service::new(crate::platform::web_router(fixture.state.clone()));
    let url = format!(
        "http://server.test/api/v1/sessions/{}/subagents/researcher/followup",
        parent.claim.session_id
    );
    let actor_session = fixture
        .state
        .store
        .create_browser_session(fixture.actor.clone(), now_ms().unwrap())
        .await
        .unwrap();
    let denied = TestClient::post(&url)
        .add_header(
            "Authorization",
            format!("Bearer {}", actor_session.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", parent.claim.tenant_id.as_str(), true)
        .json(&json!({"message":"Unpermitted child follow-up"}))
        .send(&service)
        .await;
    assert_eq!(
        denied.status_code,
        Some(StatusCode::FORBIDDEN),
        "parent model authority does not grant direct child submission rights"
    );
    let message = "Owner follow-up while the delegated task is running";
    let mut response = TestClient::post(&url)
        .add_header(
            "Authorization",
            format!("Bearer {}", fixture.owner.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", parent.claim.tenant_id.as_str(), true)
        .json(&json!({"message":message}))
        .send(&service)
        .await;
    let status = response.status_code;
    let body: Value = response.take_json().await.unwrap();
    assert_eq!(status, Some(StatusCode::OK), "{body}");
    assert_eq!(body["status"], "running");
    let mut accepted_id = None;
    for _ in 0..2 {
        let mut response = TestClient::get(format!(
            "http://server.test/api/v1/sessions/{}/queue",
            child.claim.session_id
        ))
        .add_header(
            "Authorization",
            format!("Bearer {}", fixture.owner.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", parent.claim.tenant_id.as_str(), true)
        .send(&service)
        .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let inbox: ternilo_protocol::SessionInboxSnapshot = response.take_json().await.unwrap();
        assert_eq!(
            inbox.items.len(),
            2,
            "one follow-up adds exactly one durable input and rejected input adds none"
        );
        let queued = inbox
            .items
            .iter()
            .find(|item| item.content.input() == message)
            .unwrap();
        assert_eq!(
            queued.placement,
            ternilo_protocol::SubmissionPlacement::Queued
        );
        let provenance = queued.provenance.as_ref().unwrap();
        assert_eq!(provenance.input_id, queued.id);
        assert_eq!(
            provenance.author,
            ternilo_protocol::InputAuthor::Account {
                user_id: fixture.owner.session.user.user_id.clone(),
                username: fixture.owner.session.user.username.clone(),
            }
        );
        if let Some(previous) = &accepted_id {
            assert_eq!(
                &queued.id, previous,
                "refreshing does not resubmit the follow-up"
            );
        }
        accepted_id = Some(queued.id.clone());
        let running = inbox
            .items
            .iter()
            .find(|item| item.run_id == child.claim.run_id)
            .unwrap();
        assert_eq!(
            running.placement,
            ternilo_protocol::SubmissionPlacement::Running
        );
        assert!(matches!(
            running.provenance.as_ref().map(|value| &value.author),
            Some(ternilo_protocol::InputAuthor::Automation {
                source: ternilo_protocol::AutomatedInputSource::Subagent
            })
        ));
    }
    let queued = fixture
        .state
        .cloud
        .queued_submission_run(
            &parent.claim.tenant_id,
            &fixture.owner.session.user.user_id,
            &child.claim.session_id,
            &accepted_id.unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(queued.actor_user_id, fixture.owner.session.user.user_id);
    assert_eq!(queued.authorization_session_id, child.claim.session_id);
    let active = fixture
        .state
        .cloud
        .active_subagent_run(
            &parent.claim.tenant_id,
            &fixture.owner.session.user.user_id,
            &child.claim.session_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        active.run_id, child.claim.run_id,
        "Stop must select the running child ahead of its queued follow-up"
    );
    let unchanged = fixture
        .state
        .cloud
        .get_run(&child.claim.tenant_id, &child.claim.run_id)
        .await
        .unwrap();
    assert_eq!(unchanged.actor_user_id, parent.claim.actor_user_id);
    assert_eq!(unchanged.authorization_session_id, parent.claim.session_id);
    assert_eq!(unchanged.user_id, fixture.owner.session.user.user_id);
}
