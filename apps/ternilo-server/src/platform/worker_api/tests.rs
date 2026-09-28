use std::{collections::BTreeSet, sync::Arc};

use salvo_core::{
    Service,
    test::{ResponseExt, TestClient},
};
use salvo_extra::affix_state;
use ternilo_cloud::{CloudRunDraft, CloudSessionDraft, RunLease};
use ternilo_control::{ControlStore, NativeRegistration, SecretCipher};
use ternilo_protocol::{AgentId, PermissionPreset, Profile, SessionId, SessionMode};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorKind,
};

use super::*;

#[path = "admission_tests.rs"]
mod admission;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify the real HTTP credential-to-claim-to-run path, canonical DTO rejection and live revocation in one service."
)]
async fn http_worker_protocol_uses_canonical_leases_and_rechecks_revocation() {
    let now = now_ms().unwrap();
    let control =
        ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([14; 32]), 1)
            .await
            .unwrap();
    let account = control
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".to_owned(),
                username: "owner".to_owned(),
                password: "worker-owner-password".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    let tenant = account.session.personal_tenant_id;
    let project_id = account.session.personal_project_id;
    let user = account.session.user;
    let workspace = control
        .create_cloud_workspace(&user, &tenant, &project_id, "Worker files", now)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(SessionId::new("worker-session")),
                agent_id: AgentId::new("agent"),
                title: "Worker session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
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
    let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id,
                workspace_id: workspace.workspace_id,
                agent_id: session.agent_id,
                session_id: session.session_id.clone(),
                run_id: None,
                limits: policy.maximum_limits,
                permissions: session.permissions,
                mode: SessionMode::Execute,
                profile: Profile::default(),
                input: "/write result.txt worker".to_owned(),
                references: vec![],
                reference_contexts: vec![],
                attachments: vec![],
                reserved_model_tokens: 100,
            },
            tenant.clone(),
            user.user_id.clone(),
            user.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let reservation = control
        .reserve_quota(
            &user,
            &tenant,
            Some(compiled.spec.metadata.run_id.as_str()),
            100,
            Duration::from_secs(600),
            now,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&compiled, &reservation.reservation_id, now)
        .await
        .unwrap();
    let grant = cloud
        .create_worker_credential(&ExecutorId::new("http-worker"), "http-storage", now)
        .await
        .unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let state = AppState {
        cloud_events: ternilo_cloud::CloudSessionEventFeed::from_database(
            control.database().clone(),
        )
        .await
        .unwrap(),
        edge: Arc::new(
            crate::platform::edge::EdgeGateway::new(control.edge_store())
                .await
                .unwrap(),
        ),
        store: control,
        cloud: cloud.clone(),
        security: Arc::default(),
        setup_token_hash: None,
        managed_execution_enabled: true,
        shutdown: receiver,
        worker_policy: Arc::new(policy),
        catalog: Arc::new(catalog),
    };
    let service = Service::new(
        Router::new()
            .hoop(affix_state::inject(state.clone()))
            .push(router()),
    );
    let base = "http://server.test/internal/worker/v1";
    assert_eq!(
        TestClient::get(format!("{base}/configuration"))
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    let bearer = format!("Bearer {}", grant.token);
    let discovered: WorkerConfiguration = TestClient::get(format!("{base}/configuration"))
        .add_header("Authorization", &bearer, true)
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(discovered.worker_id, grant.worker_id);
    assert_eq!(discovered.expected_root_id, None);
    let registration = WorkerRegisterRequest {
        capacity: ternilo_cloud::WorkerCapacity {
            max_active_runs: 1,
            max_resident_runs: 3,
        },
        hello: ExecutorHello {
            protocol_version: EXECUTOR_PROTOCOL_VERSION,
            executor_id: grant.worker_id.clone(),
            executor_kind: ExecutorKind::CloudWorker,
            instance_nonce: "http-worker-process".to_owned(),
            catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
            capabilities: BTreeSet::from([
                ExecutorCapability::CloudRun,
                ExecutorCapability::AddressedSessionCommands,
                ExecutorCapability::WorkspaceFiles,
            ]),
        },
        storage_id: "http-storage".to_owned(),
        root_id: "http-volume".to_owned(),
    };
    let registered: WorkerRegistration = TestClient::post(format!("{base}/register"))
        .add_header("Authorization", &bearer, true)
        .json(&registration)
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(registered.capacity, registration.capacity);
    let claim: WorkerReply = TestClient::post(format!("{base}/rpc"))
        .add_header("Authorization", &bearer, true)
        .json(&WorkerRpcRequest {
            identity: registered.identity.clone(),
            request: WorkerRequest::ClaimRun,
        })
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    let WorkerReply::Claim { claim: Some(claim) } = claim else {
        panic!("expected a canonical run claim")
    };
    assert_eq!(claim.spec, compiled.spec);
    let mut forged = serde_json::to_value(WorkerRpcRequest {
        identity: registered.identity.clone(),
        request: WorkerRequest::StartRun {
            run: RunLease::from(&claim),
        },
    })
    .unwrap();
    forged["request"]["run"]["spec"] = serde_json::json!({"input":"replace canonical input"});
    assert_eq!(
        TestClient::post(format!("{base}/rpc"))
            .add_header("Authorization", &bearer, true)
            .json(&forged)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let started: WorkerReply = TestClient::post(format!("{base}/rpc"))
        .add_header("Authorization", &bearer, true)
        .json(&WorkerRpcRequest {
            identity: registered.identity.clone(),
            request: WorkerRequest::StartRun {
                run: RunLease::from(&claim),
            },
        })
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    let WorkerReply::Started { run: Some(run) } = started else {
        panic!("expected a started canonical run")
    };
    assert_eq!(run.claim.spec.input, "/write result.txt worker");
    verify_subagent_acceptance(&service, &bearer, &registered.identity, &run).await;
    let mut wrong_fence = RunLease::from(&run);
    wrong_fence.writer_fencing_token += 1;
    assert_eq!(
        TestClient::post(format!("{base}/rpc"))
            .add_header("Authorization", &bearer, true)
            .json(&WorkerRpcRequest {
                identity: registered.identity.clone(),
                request: WorkerRequest::RenewRun { run: wrong_fence }
            })
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    cloud
        .revoke_worker_credential(&grant.worker_id, now_ms().unwrap())
        .await
        .unwrap();
    assert_eq!(
        TestClient::post(format!("{base}/rpc"))
            .add_header("Authorization", &bearer, true)
            .json(&WorkerRpcRequest {
                identity: registered.identity,
                request: WorkerRequest::RenewRun {
                    run: RunLease::from(&run)
                }
            })
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        TestClient::get(format!("{base}/configuration"))
            .add_header("Authorization", &bearer, true)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::UNAUTHORIZED)
    );
    shutdown.send_replace(true);
    state.edge.shutdown().await;
}

async fn worker_request(
    service: &Service,
    bearer: &str,
    identity: &ternilo_cloud::CloudWorkerIdentity,
    request: WorkerRequest,
) -> salvo_core::prelude::Response {
    TestClient::post("http://server.test/internal/worker/v1/rpc")
        .add_header("Authorization", bearer, true)
        .json(&WorkerRpcRequest {
            identity: identity.clone(),
            request,
        })
        .send(service)
        .await
}

async fn verify_subagent_acceptance(
    service: &Service,
    bearer: &str,
    identity: &ternilo_cloud::CloudWorkerIdentity,
    parent: &ternilo_cloud::StartedRun,
) {
    let session_id = SessionId::new("http-child-session");
    let run_id = ternilo_protocol::RunId::new("http-child-run");
    let mut created = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::CreateSubagent {
            run: RunLease::from(parent),
            child_session_id: session_id.clone(),
            metadata: ternilo_protocol::SubagentSessionMetadata {
                subagent_id: ternilo_protocol::SubagentId::new("http-child"),
                provider: "in-process".to_owned(),
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
            },
            label: "HTTP child".to_owned(),
        },
    )
    .await;
    assert_eq!(created.status_code, Some(StatusCode::OK));
    assert!(
        matches!(created.take_json::<WorkerReply>().await.unwrap(), WorkerReply::SessionId { session_id: id } if id == session_id)
    );
    let mut accepted = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::EnqueueSubagent {
            run: RunLease::from(parent),
            child_session_id: session_id.clone(),
            child_run_id: run_id.clone(),
            input: "/code \"child\"".to_owned(),
        },
    )
    .await;
    assert_eq!(accepted.status_code, Some(StatusCode::OK));
    let WorkerReply::SubagentAccepted { run: ticket } = accepted.take_json().await.unwrap() else {
        panic!("subagent enqueue must return a durable acceptance ticket");
    };
    assert_eq!(ticket.session_id, session_id);
    assert_eq!(ticket.run_id, run_id);
    let mut readable = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::ReadSubagent {
            run: RunLease::from(parent),
            child_session_id: ticket.session_id.clone(),
            child_run_id: ticket.run_id.clone(),
        },
    )
    .await;
    assert_eq!(readable.status_code, Some(StatusCode::OK));
    assert!(
        matches!(readable.take_json::<WorkerReply>().await.unwrap(), WorkerReply::Subagent { run: Some(child) } if child.state == ternilo_cloud::CloudRunState::Queued)
    );
    let forged = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::ReadSubagent {
            run: RunLease::from(parent),
            child_session_id: ticket.session_id.clone(),
            child_run_id: parent.claim.run_id.clone(),
        },
    )
    .await;
    assert_eq!(forged.status_code, Some(StatusCode::FORBIDDEN));
    admission::verify_capacity_handoff(service, bearer, identity, parent, &ticket).await;
    let mut cancelled = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::CancelSubagent {
            run: RunLease::from(parent),
            child_session_id: ticket.session_id,
            child_run_id: ticket.run_id,
        },
    )
    .await;
    assert_eq!(cancelled.status_code, Some(StatusCode::OK));
    assert!(matches!(
        cancelled.take_json::<WorkerReply>().await.unwrap(),
        WorkerReply::RunState {
            state: ternilo_cloud::CloudRunState::Cancelled
        }
    ));
}
