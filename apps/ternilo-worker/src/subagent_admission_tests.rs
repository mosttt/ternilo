use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use ternilo_kernel::{SubagentAdmission, SubagentRunStart, SubagentSessionHost};
use tokio::{io::DuplexStream, sync::Mutex as AsyncMutex};

use super::*;
use crate::{
    HostSubagentRuns, PipeCloudHost, PipeHostClient,
    child_protocol::{ChildProtocol, CloudHostOutcome, CloudHostRequest, ParentProtocol},
    serve_cloud_host_request, serve_ordered_cloud_host_request,
};

struct PipeFixture {
    host: Arc<PipeCloudHost>,
    output: tokio::io::Lines<tokio::io::BufReader<DuplexStream>>,
}

impl PipeFixture {
    fn new() -> Self {
        use tokio::io::AsyncBufReadExt as _;
        let (writer, reader) = tokio::io::duplex(16 * 1024);
        Self {
            host: Arc::new(PipeCloudHost {
                client: PipeHostClient {
                    protocol: Arc::new(ChildProtocol::with_output(writer)),
                    pending: Arc::new(AsyncMutex::new(BTreeMap::new())),
                    next_request_id: Arc::new(AtomicU64::new(1)),
                    input_closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                },
                cleanup: Arc::new(crate::subagent_cleanup::SubagentCleanup::default()),
            }),
            output: tokio::io::BufReader::new(reader).lines(),
        }
    }

    async fn request(&mut self, operation: &str) -> serde_json::Value {
        let line = tokio::time::timeout(Duration::from_secs(2), self.output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["type"], "host_request");
        assert_eq!(frame["request"]["operation"], operation);
        frame
    }

    async fn reply(&self, request: &serde_json::Value, outcome: CloudHostOutcome) {
        self.host
            .client
            .receive_reply(request["request_id"].as_u64().unwrap(), outcome)
            .await;
    }

    fn run(
        &self,
        cancellation: RunCancellation,
        start: SubagentRunStart,
    ) -> tokio::task::JoinHandle<Result<RunOutcome, HarnessError>> {
        let host = Arc::clone(&self.host);
        tokio::spawn(async move {
            host.run(
                SessionId::new("child-session"),
                RunId::new("logical-run"),
                "inspect".to_owned(),
                cancellation,
                start,
            )
            .await
        })
    }
}

fn accepted() -> AcceptedSubagentRun {
    AcceptedSubagentRun {
        session_id: SessionId::new("child-session"),
        run_id: RunId::new("accepted-run"),
    }
}

fn acceptance() -> CloudHostOutcome {
    CloudHostOutcome::Ok {
        value: serde_json::to_value(accepted()).unwrap(),
    }
}

#[tokio::test]
async fn admission_confirms_a_durable_ticket_before_background_execution_can_continue() {
    let mut pipe = PipeFixture::new();
    let start = SubagentRunStart::new();
    let task = pipe.run(RunCancellation::new(), start.clone());
    let enqueue = pipe.request("enqueue_subagent").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), start.wait())
            .await
            .is_err()
    );
    pipe.reply(&enqueue, acceptance()).await;
    assert_eq!(
        start.wait().await.unwrap(),
        SubagentAdmission::Scheduled(accepted())
    );
    let wait = pipe.request("wait_subagent").await;
    assert_eq!(wait["request"]["session_id"], "child-session");
    assert_eq!(wait["request"]["run_id"], "accepted-run");
    assert!(!task.is_finished(), "admission is distinct from completion");
    pipe.reply(
        &wait,
        CloudHostOutcome::Ok {
            value: serde_json::json!({
                "answer": "accepted child completed", "steps": 1, "tool_calls": 0, "events": [],
            }),
        },
    )
    .await;
    assert_eq!(
        task.await.unwrap().unwrap().answer,
        "accepted child completed"
    );
}

#[tokio::test]
async fn rejected_admission_fails_start_without_requesting_a_completion() {
    let mut pipe = PipeFixture::new();
    let start = SubagentRunStart::new();
    let task = pipe.run(RunCancellation::new(), start.clone());
    let enqueue = pipe.request("enqueue_subagent").await;
    let error = HarnessError::policy("admission denied");
    pipe.reply(
        &enqueue,
        CloudHostOutcome::Error {
            error: error.clone(),
        },
    )
    .await;
    assert_eq!(start.wait().await.unwrap_err(), error);
    assert_eq!(task.await.unwrap().unwrap_err(), error);
    assert!(pipe.host.client.pending.lock().await.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), pipe.output.next_line())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_during_admission_waits_for_the_ordered_cancel_confirmation() {
    let mut pipe = PipeFixture::new();
    let start = SubagentRunStart::new();
    let cancellation = RunCancellation::new();
    let task = pipe.run(cancellation.clone(), start.clone());
    let enqueue = pipe.request("enqueue_subagent").await;
    cancellation.cancel();
    let cancel = pipe.request("cancel_subagent").await;
    assert_eq!(cancel["request"]["run_id"], "logical-run");
    pipe.reply(&enqueue, acceptance()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), start.wait())
            .await
            .is_err()
    );
    pipe.reply(
        &cancel,
        CloudHostOutcome::Ok {
            value: serde_json::json!("cancelled"),
        },
    )
    .await;
    assert_eq!(
        task.await.unwrap().unwrap_err().code,
        ternilo_protocol::ErrorCode::Cancelled
    );
    assert_eq!(
        start.wait().await.unwrap_err().code,
        ternilo_protocol::ErrorCode::Cancelled
    );
    assert!(pipe.host.client.pending.lock().await.is_empty());
}

#[tokio::test]
async fn dropping_an_unconfirmed_driver_compensates_after_its_enqueue_succeeds() {
    let mut pipe = PipeFixture::new();
    let task = pipe.run(RunCancellation::new(), SubagentRunStart::new());
    let enqueue = pipe.request("enqueue_subagent").await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    pipe.reply(&enqueue, acceptance()).await;
    let cancel = pipe.request("cancel_subagent").await;
    assert_eq!(cancel["request"]["session_id"], "child-session");
    assert_eq!(cancel["request"]["run_id"], "logical-run");
    pipe.reply(
        &cancel,
        CloudHostOutcome::Ok {
            value: serde_json::json!("cancelled"),
        },
    )
    .await;
    pipe.host.cleanup.drain().await.unwrap();
    assert!(pipe.host.client.pending.lock().await.is_empty());
}

#[tokio::test]
async fn accepted_reply_queued_before_driver_abort_still_cancels_the_accepted_run() {
    let mut pipe = PipeFixture::new();
    let task = pipe.run(RunCancellation::new(), SubagentRunStart::new());
    let enqueue = pipe.request("enqueue_subagent").await;
    let sender = pipe
        .host
        .client
        .pending
        .lock()
        .await
        .remove(&enqueue["request_id"].as_u64().unwrap())
        .unwrap();
    assert!(sender.send(acceptance()).is_ok());
    // No await separates successful delivery from abort: this driver cannot consume the ticket.
    pipe.host.cleanup.preserve_delivered();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    pipe.host.cleanup.finish_parent(true);
    let cancel = pipe.request("cancel_subagent").await;
    assert_eq!(cancel["request"]["run_id"], "logical-run");
    let draining = pipe.host.cleanup.drain();
    tokio::pin!(draining);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut draining)
            .await
            .is_err()
    );
    pipe.reply(
        &cancel,
        CloudHostOutcome::Ok {
            value: serde_json::json!("cancelled"),
        },
    )
    .await;
    draining.await.unwrap();
}

#[tokio::test]
async fn consumed_admission_keeps_its_cleanup_guard_while_waiting_for_completion() {
    let mut pipe = PipeFixture::new();
    let start = SubagentRunStart::new();
    let task = pipe.run(RunCancellation::new(), start.clone());
    let enqueue = pipe.request("enqueue_subagent").await;
    pipe.reply(&enqueue, acceptance()).await;
    assert_eq!(
        start.wait().await.unwrap(),
        SubagentAdmission::Scheduled(accepted())
    );
    let _wait = pipe.request("wait_subagent").await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let cancel = pipe.request("cancel_subagent").await;
    pipe.reply(
        &cancel,
        CloudHostOutcome::Error {
            error: HarnessError::invalid("Server rejected cancellation"),
        },
    )
    .await;
    let error = pipe.host.cleanup.drain().await.unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::InvalidInput);
    assert_eq!(error.message, "Server rejected cancellation");
}

#[tokio::test]
async fn shutdown_drain_waits_for_live_guards_and_reports_a_closed_input() {
    let mut pipe = PipeFixture::new();
    let task = pipe.run(RunCancellation::new(), SubagentRunStart::new());
    let _enqueue = pipe.request("enqueue_subagent").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), pipe.host.cleanup.drain())
            .await
            .is_err()
    );
    pipe.host.client.input_closed.store(true, Ordering::Release);
    pipe.host.client.pending.lock().await.clear();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), pipe.host.cleanup.drain())
            .await
            .unwrap()
            .unwrap_err()
            .message,
        "cloud parent input is closed"
    );
}

#[tokio::test]
async fn dropping_the_cleanup_owner_aborts_a_pending_cancel_without_a_reference_cycle() {
    let mut pipe = PipeFixture::new();
    let task = pipe.run(RunCancellation::new(), SubagentRunStart::new());
    let _enqueue = pipe.request("enqueue_subagent").await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let cancel = pipe.request("cancel_subagent").await;
    let cleanup = Arc::downgrade(&pipe.host.cleanup);
    let protocol = Arc::downgrade(&pipe.host.client.protocol);
    let mut sender = pipe
        .host
        .client
        .pending
        .lock()
        .await
        .remove(&cancel["request_id"].as_u64().unwrap())
        .unwrap();
    drop(pipe.host);
    assert!(
        cleanup.upgrade().is_none(),
        "a pending cleanup task must not own its JoinSet owner"
    );
    tokio::time::timeout(Duration::from_secs(2), sender.closed())
        .await
        .unwrap();
    assert!(
        protocol.upgrade().is_none(),
        "dropping the owner must abort the pending request and release its client"
    );
}

#[tokio::test]
async fn successful_parent_preserves_only_delivered_runs_across_both_guard_drop_orders() {
    for (delivered, drop_first) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut pipe = PipeFixture::new();
        let start = SubagentRunStart::new();
        let task = pipe.run(RunCancellation::new(), start.clone());
        let enqueue = pipe.request("enqueue_subagent").await;
        pipe.reply(&enqueue, acceptance()).await;
        start.wait().await.unwrap();
        let _wait = pipe.request("wait_subagent").await;
        if delivered {
            start.mark_delivered();
        }
        pipe.host.cleanup.preserve_delivered();
        if !drop_first {
            pipe.host.cleanup.finish_parent(true);
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        if drop_first {
            pipe.host.cleanup.finish_parent(true);
        }
        if delivered {
            pipe.host.cleanup.drain().await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), pipe.output.next_line())
                    .await
                    .is_err()
            );
        } else {
            let cancel = pipe.request("cancel_subagent").await;
            pipe.reply(
                &cancel,
                CloudHostOutcome::Ok {
                    value: serde_json::json!("cancelled"),
                },
            )
            .await;
            pipe.host.cleanup.drain().await.unwrap();
        }
    }
}

#[tokio::test]
async fn failed_shutdown_and_explicit_cancellation_override_background_preservation() {
    for explicit_cancel in [false, true] {
        let mut pipe = PipeFixture::new();
        let start = SubagentRunStart::new();
        let cancellation = RunCancellation::new();
        let task = pipe.run(cancellation.clone(), start.clone());
        let enqueue = pipe.request("enqueue_subagent").await;
        pipe.reply(&enqueue, acceptance()).await;
        start.wait().await.unwrap();
        let _wait = pipe.request("wait_subagent").await;
        start.mark_delivered();
        pipe.host.cleanup.preserve_delivered();
        assert!(start.is_preserved());
        if explicit_cancel {
            cancellation.cancel();
        }
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        pipe.host.cleanup.finish_parent(explicit_cancel);
        let cancel = pipe.request("cancel_subagent").await;
        pipe.reply(
            &cancel,
            CloudHostOutcome::Ok {
                value: serde_json::json!("cancelled"),
            },
        )
        .await;
        pipe.host.cleanup.drain().await.unwrap();
    }
}

fn fixture_run() -> StartedRun {
    let envelope: ternilo_cloud::ExecutionEnvelope =
        serde_json::from_str(include_str!("../../../examples/execution-envelope.json")).unwrap();
    StartedRun {
        claim: CloudRunClaim {
            provenance: None,
            tenant_id: envelope.spec.metadata.tenant_id.clone(),
            actor_user_id: envelope.spec.metadata.user_id.clone(),
            authorization_session_id: envelope.spec.metadata.session_id.clone(),
            run_id: envelope.spec.metadata.run_id.clone(),
            session_id: envelope.spec.metadata.session_id.clone(),
            workspace_use: ternilo_cloud::WorkspaceUseTicket {
                storage_id: "test-storage".to_owned(),
                root_id: "test-root".to_owned(),
                tenant_id: envelope.spec.metadata.tenant_id.clone(),
                workspace_id: envelope.spec.metadata.workspace_id.clone(),
                family_id: "test-family".to_owned(),
                worker_id: "test-worker".to_owned(),
                worker_generation: 1,
                occupation_epoch: 1,
                run_id: envelope.spec.metadata.run_id.clone(),
                lease_token: 7,
            },
            lease_token: 7,
            spec_digest: [0; 32],
            spec: envelope.spec,
        },
        fencing_token: 9,
        prior_events: Vec::new(),
    }
}

#[tokio::test]
async fn unaccepted_waits_are_rejected_and_logical_cancellation_is_a_local_noop() {
    let client = WorkerClient::new("http://127.0.0.1:1", "fixture-token".to_owned()).unwrap();
    let parent = fixture_run();
    let policy =
        serde_json::from_str(include_str!("../../../examples/worker-policy.json")).unwrap();
    let runs = HostSubagentRuns::new(BTreeMap::from([(
        ("child-session".to_owned(), "logical-run".to_owned()),
        accepted(),
    )]));
    for request in [
        CloudHostRequest::WaitSubagent {
            session_id: SessionId::new("child-session"),
            run_id: RunId::new("logical-run"),
        },
        CloudHostRequest::WaitSubagent {
            session_id: SessionId::new("another-session"),
            run_id: RunId::new("accepted-run"),
        },
    ] {
        let error = serve_cloud_host_request(&client, &parent, &policy, 1, &runs, request)
            .await
            .unwrap_err();
        assert!(matches!(
            error.code,
            ternilo_protocol::ErrorCode::PolicyDenied | ternilo_protocol::ErrorCode::InvalidInput
        ));
        assert!(
            !error.message.contains("registered"),
            "forged dependency must be rejected before the HTTP client is used"
        );
    }
    let result = serve_cloud_host_request(
        &client,
        &parent,
        &policy,
        1,
        &runs,
        CloudHostRequest::CancelSubagent {
            session_id: SessionId::new("another-session"),
            run_id: RunId::new("logical-run"),
        },
    )
    .await
    .unwrap();
    assert_eq!(result, serde_json::json!("not_accepted"));
}

#[tokio::test]
async fn a_closed_child_pipe_cancels_the_run_accepted_by_server_before_returning_an_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut accepted = None;
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (_, body) = read_request(&mut stream).await;
            let rpc: WorkerRpcRequest = serde_json::from_value(body).unwrap();
            let reply = match rpc.request {
                WorkerRequest::EnqueueSubagent {
                    child_session_id,
                    child_run_id,
                    ..
                } => {
                    let run = AcceptedSubagentRun {
                        session_id: child_session_id,
                        run_id: child_run_id,
                    };
                    accepted = Some(run.clone());
                    WorkerReply::SubagentAccepted { run }
                }
                WorkerRequest::CancelSubagent {
                    child_session_id,
                    child_run_id,
                    ..
                } => {
                    assert_eq!(accepted.as_ref().unwrap().session_id, child_session_id);
                    assert_eq!(accepted.as_ref().unwrap().run_id, child_run_id);
                    WorkerReply::RunState {
                        state: CloudRunState::Cancelled,
                    }
                }
                operation => panic!("unexpected operation: {operation:?}"),
            };
            let encoded = serde_json::to_vec(&reply).unwrap();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", encoded.len()).as_bytes()).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
        accepted.unwrap()
    });
    let client = WorkerClient::new(&origin, "fixture-token".to_owned()).unwrap();
    *client.identity.write().unwrap() = Some(request().identity);
    let mut child = tokio::process::Command::new("/bin/cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let protocol = ParentProtocol::new(child.stdin.take().unwrap());
    protocol.close().await.unwrap();
    let policy =
        serde_json::from_str(include_str!("../../../examples/worker-policy.json")).unwrap();
    let runs = HostSubagentRuns::new(BTreeMap::new());
    let error = serve_ordered_cloud_host_request(
        &client,
        &fixture_run(),
        &policy,
        1,
        &runs,
        &protocol,
        CloudHostRequest::EnqueueSubagent {
            session_id: SessionId::new("child-session"),
            run_id: RunId::new("logical-run"),
            input: "inspect".to_owned(),
        },
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("child input is closed"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap()
            .session_id
            .as_str(),
        "child-session"
    );
    assert!(child.wait().await.unwrap().success());
}
