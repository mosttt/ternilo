use std::{future::Future, pin::Pin, sync::Mutex};

use ternilo_cloud::{RunLease, WorkerModelFrame};
use ternilo_protocol::{ModelFinishReason, TenantId};
use ternilo_transport::ExecutorId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[cfg(target_os = "linux")]
#[path = "supervisor_timing_tests.rs"]
mod supervisor_timing;

#[path = "subagent_admission_tests.rs"]
mod subagent_admission;

#[path = "client_admission_tests.rs"]
mod execution_admission;

#[derive(Default)]
struct Output(Mutex<Vec<String>>);

impl ModelOutput for Output {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        self.0.lock().unwrap().push(format!("text:{delta}"));
        Box::pin(async { Ok(()) })
    }

    fn emit_reasoning<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        self.0.lock().unwrap().push(format!("reasoning:{delta}"));
        Box::pin(async { Ok(()) })
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        self.0.lock().unwrap().push(format!("retry:{retry}"));
        Box::pin(async { Ok(()) })
    }
}

fn request() -> WorkerModelRequest {
    WorkerModelRequest {
        identity: CloudWorkerIdentity {
            worker_id: ExecutorId::new("worker-one"),
            instance_nonce: "instance-one".to_owned(),
            generation: 3,
        },
        run: RunLease {
            tenant_id: TenantId::new("tenant-one"),
            run_id: RunId::new("run-one"),
            lease_token: 7,
            writer_fencing_token: 9,
        },
        request_id: 1,
        binding: ternilo_protocol::RunModelBinding::UserProvider {
            tenant_id: TenantId::new("tenant-one"),
            owner_user_id: ternilo_protocol::UserId::new("owner-one"),
            provider_id: "primary".to_owned(),
            model: "test-model".to_owned(),
        },
        request: ModelRequest {
            run_id: ternilo_protocol::RunId::new("model-test-run"),
            system_prompt: "system".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            step: 1,
        },
    }
}

fn model_response() -> ModelResponse {
    ModelResponse {
        provider: "primary".to_owned(),
        model: "test-model".to_owned(),
        content: "你好".to_owned(),
        reasoning_content: Some("思考".to_owned()),
        provider_state: None,
        tool_calls: Vec::new(),
        usage: None,
        finish_reason: ModelFinishReason::Stop,
        provider_request_id: None,
        attempts: 1,
        replayed: false,
        request_digest: None,
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, serde_json::Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1_024];
    loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0, "client closed before sending its request");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return (
                    headers,
                    serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                );
            }
        }
    }
}

async fn streaming_server(
    body: Vec<u8>,
) -> (String, tokio::task::JoinHandle<(String, serde_json::Value)>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        for chunk in body.chunks(5) {
            stream.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
        stream.shutdown().await.unwrap();
        request
    });
    (origin, task)
}

#[tokio::test]
async fn model_http_stream_preserves_unicode_reasoning_and_retries_with_canonical_leases() {
    let response = model_response();
    let frames = [
        WorkerModelFrame::ReasoningDelta {
            delta: "思考".to_owned(),
        },
        WorkerModelFrame::RetryStarted { retry: 1 },
        WorkerModelFrame::Delta {
            delta: "你好".to_owned(),
        },
        WorkerModelFrame::Complete {
            response: response.clone(),
        },
    ];
    let mut body = b"\n".to_vec();
    for frame in frames {
        body.extend(serde_json::to_vec(&frame).unwrap());
        body.push(b'\n');
    }
    let (origin, server) = streaming_server(body).await;
    let client = WorkerClient::new(&origin, "worker-only-token".to_owned()).unwrap();
    let output = Output::default();
    let result = client.stream_model(&request(), &output).await.unwrap();
    assert_eq!(result, response);
    assert_eq!(
        *output.0.lock().unwrap(),
        ["reasoning:思考", "retry:1", "text:你好"]
    );
    let (headers, body) = server.await.unwrap();
    assert!(headers.starts_with("POST /internal/worker/v1/model HTTP/1.1"));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("authorization: bearer worker-only-token")
    );
    assert_eq!(
        body["run"],
        serde_json::json!({"tenant_id":"tenant-one","run_id":"run-one","lease_token":7,"writer_fencing_token":9})
    );
    assert!(body.get("spec").is_none());
    assert!(body.get("secret_master_key").is_none());
    assert!(body.get("provider_key").is_none());
    assert_eq!(
        body["binding"],
        serde_json::to_value(request().binding).unwrap()
    );
    assert!(body.get("actor_user_id").is_none());
    assert!(body.get("route_id").is_none());
}

#[tokio::test]
async fn model_eof_without_terminal_response_keeps_the_result_indeterminate() {
    let body = serde_json::to_vec(&WorkerModelFrame::Delta {
        delta: "partial".to_owned(),
    })
    .unwrap();
    let (origin, server) = streaming_server(body).await;
    let client = WorkerClient::new(&origin, "worker-only-token".to_owned()).unwrap();
    let output = Output::default();
    let error = client.stream_model(&request(), &output).await.unwrap_err();
    assert!(
        error
            .message
            .contains("result is unknown and was not replayed")
    );
    assert_eq!(*output.0.lock().unwrap(), ["text:partial"]);
    server.await.unwrap();
}

#[tokio::test]
async fn a_server_error_keeps_its_typed_policy_denial() {
    let frame = WorkerModelFrame::Error {
        error: HarnessError::policy("canonical lease was revoked"),
    };
    let (origin, server) = streaming_server(serde_json::to_vec(&frame).unwrap()).await;
    let client = WorkerClient::new(&origin, "worker-only-token".to_owned()).unwrap();
    let error = client
        .stream_model(&request(), &Output::default())
        .await
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
    assert_eq!(error.message, "canonical lease was revoked");
    server.await.unwrap();
}

#[test]
fn server_origin_does_not_accept_embedded_credentials_or_ambiguous_paths() {
    for origin in [
        "file:///tmp/server",
        "https://token@server.example",
        "https://server.example/prefix",
        "https://server.example?token=value",
        "https://server.example#fragment",
    ] {
        assert!(WorkerClient::new(origin, "worker-only-token".to_owned()).is_err());
    }
    assert!(WorkerClient::new("https://server.example", "token\n".to_owned()).is_err());
    let client =
        WorkerClient::new("https://server.example", "worker-only-token".to_owned()).unwrap();
    assert!(client.current_identity().is_err());
}

#[tokio::test]
async fn cancelling_model_work_closes_the_pending_server_stream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: 10000\r\n\r\n").await.unwrap();
        accepted.send(()).unwrap();
        let mut byte = [0];
        let result = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(
            matches!(result, Ok(0)) || result.is_err(),
            "cancelled client must close its unfinished response"
        );
    });
    let client = WorkerClient::new(&origin, "worker-only-token".to_owned()).unwrap();
    *client.identity.write().unwrap() = Some(request().identity);
    let envelope: ternilo_cloud::ExecutionEnvelope =
        serde_json::from_str(include_str!("../../../examples/execution-envelope.json")).unwrap();
    let run = StartedRun {
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
            lease_token: 1,
            spec_digest: [0; 32],
            spec: envelope.spec,
        },
        fencing_token: 1,
        prior_events: Vec::new(),
    };
    let cancellation = RunCancellation::new();
    let request_cancellation = cancellation.clone();
    let work = tokio::spawn(async move {
        client
            .model_complete(
                &run,
                1,
                &request().binding,
                request().request,
                Arc::new(Output::default()),
                request_cancellation,
            )
            .await
    });
    received.await.unwrap();
    cancellation.cancel();
    let error = work.await.unwrap().unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    server.await.unwrap();
}
