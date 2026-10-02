use super::*;
use serde_json::{Value, json};
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{
    ErrorCode, ModelRequest, ProviderModel, ProviderModelDefaults, ProviderModelSettings,
    ProviderProfile, ProviderProtocol, RunId, RunLimits, TenantId, UserId,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const SOURCE_KEY: &str = "source-computer-only-test-key";

struct Fixture {
    directory: tempfile::TempDir,
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    request: ComputerModelRequest,
}

impl Fixture {
    async fn new(address: std::net::SocketAddr) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let application = Arc::new(
            LocalApplication::open(
                ternilo_local::catalog().unwrap(),
                ternilo_local::local_profile(),
                HostPolicy::local(RunLimits::default()),
                directory.path().to_owned(),
            )
            .await
            .unwrap(),
        );
        application
            .set_credential("FORWARDING_TEST_KEY".to_owned(), SOURCE_KEY.to_owned())
            .await
            .unwrap();
        let defaults = ProviderModelDefaults {
            context_window: 32_000,
            max_output_tokens: 128,
            reasoning: None,
        };
        application
            .upsert_provider_profile(ProviderProfile {
                id: "source-provider".to_owned(),
                display_name: "Source".to_owned(),
                base_url: format!("http://{address}/v1"),
                protocol: ProviderProtocol::OpenAiChatCompletions,
                api_key_ref: Some("FORWARDING_TEST_KEY".to_owned()),
                defaults: defaults.clone(),
                models: vec![ProviderModel {
                    id: "test-model".to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Inherit,
                }],
                timeout_ms: 5_000,
                max_attempts: 1,
                retry_base_delay_ms: 10,
            })
            .await
            .unwrap();
        Self {
            directory,
            application,
            scope: ExecutorScope {
                tenant_id: TenantId::new("team"),
                user_id: UserId::new("owner"),
            },
            request: ComputerModelRequest {
                provider_id: "source-provider".to_owned(),
                model: "test-model".to_owned(),
                protocol: ProviderProtocol::OpenAiChatCompletions,
                defaults,
                reasoning_effort: None,
                max_attempts: 3,
                request: ModelRequest {
                    run_id: RunId::new("remote-run"),
                    system_prompt: "remote system prompt".to_owned(),
                    messages: vec![],
                    tools: vec![],
                    step: 1,
                },
            },
        }
    }

    fn forwarding(&self) -> (ForwardedModels, mpsc::Receiver<ExecutorFrame>) {
        let (sender, receiver) = mpsc::channel(64);
        (
            ForwardedModels::new(Arc::clone(&self.application), self.scope.clone(), sender),
            receiver,
        )
    }
}

async fn accept_request(listener: &tokio::net::TcpListener) -> (tokio::net::TcpStream, Value) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let header = std::str::from_utf8(&bytes[..offset]).unwrap();
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= offset + 4 + length {
                    assert!(
                        header.contains(&format!("Bearer {SOURCE_KEY}")),
                        "only source-local credentials authorize the actual upstream request"
                    );
                    assert!(header.starts_with("POST /v1/chat/completions "));
                    return (
                        socket,
                        serde_json::from_slice(&bytes[offset + 4..offset + 4 + length]).unwrap(),
                    );
                }
            }
        }
    })
    .await
    .expect("source request must reach the upstream within five seconds")
}

async fn send_chunk(socket: &mut tokio::net::TcpStream, chunk: &str) {
    socket
        .write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
        .await
        .unwrap();
}

async fn next(
    receiver: &mut mpsc::Receiver<ExecutorFrame>,
    id: &ModelRequestId,
) -> ModelGatewayFrame {
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = receiver.recv().await.unwrap();
            if matches!(frame, ExecutorFrame::ModelOutput { .. }) {
                break frame;
            }
            assert!(!serde_json::to_string(&frame).unwrap().contains(SOURCE_KEY));
        }
    })
    .await
    .unwrap();
    assert!(!serde_json::to_string(&frame).unwrap().contains(SOURCE_KEY));
    let ExecutorFrame::ModelOutput { request_id, frame } = frame else {
        panic!("expected model output")
    };
    assert_eq!(request_id, *id);
    *frame
}

#[tokio::test]
async fn source_calls_upstream_streams_results_and_returns_tools_without_executing_them() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut fixture = Fixture::new(listener.local_addr().unwrap()).await;
    // The current source configuration may reduce a previously selected limit.
    fixture.request.defaults.max_output_tokens = 512;
    let (continue_tx, continue_rx) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut socket, request) = accept_request(&listener).await;
        assert_eq!(request["model"], "test-model");
        assert_eq!(request["max_tokens"], 128);
        assert_eq!(request["messages"][0]["content"], "remote system prompt");
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
        send_chunk(
            &mut socket,
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking on source\"}}]}\n\n",
        )
        .await;
        continue_rx.await.unwrap();
        let value = json!({"choices":[{"delta":{"content":"continue on execution computer", "tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"result.txt\",\"content\":\"ok\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":11,"completion_tokens":7}});
        send_chunk(&mut socket, &format!("data: {value}\n\ndata: [DONE]\n\n")).await;
        socket.write_all(b"0\r\n\r\n").await.unwrap();
    });
    let (mut models, mut receiver) = fixture.forwarding();
    let id = ModelRequestId::new("stream-test");
    let wire = serde_json::to_value(&fixture.request).unwrap();
    assert!(wire.get("base_url").is_none());
    assert!(wire.get("api_key").is_none());
    models
        .start(id.clone(), &fixture.scope, fixture.request.clone())
        .unwrap();
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::ReasoningDelta { delta } if delta == "thinking on source")
    );
    continue_tx.send(()).unwrap();
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Delta { delta } if delta == "continue on execution computer")
    );
    let ModelGatewayFrame::Complete { response } = next(&mut receiver, &id).await else {
        panic!("expected completion")
    };
    assert_eq!(response.tool_calls[0].name, "write_file");
    assert_eq!(response.tool_calls[0].arguments["path"], "result.txt");
    assert_eq!(response.usage.unwrap().input_tokens, 11);
    assert!(fixture.application.snapshot().await.sessions.is_empty());
    assert!(!fixture.directory.path().join("result.txt").exists());
    models.reap().await.unwrap();
    assert!(models.is_empty());
    upstream.await.unwrap();
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_and_connection_shutdown_close_the_source_upstream_request() {
    for disconnect in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fixture = Fixture::new(listener.local_addr().unwrap()).await;
        let (started, start_rx) = tokio::sync::oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = accept_request(&listener).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
            started.send(()).unwrap();
            let mut byte = [0];
            let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
                .await
                .expect("cancel must close the pending upstream");
            assert!(matches!(closed, Ok(0) | Err(_)));
        });
        let (mut models, mut receiver) = fixture.forwarding();
        let id = ModelRequestId::new("cancel-test");
        models
            .start(id.clone(), &fixture.scope, fixture.request.clone())
            .unwrap();
        start_rx.await.unwrap();
        if disconnect {
            models.shutdown().await;
        } else {
            models.cancel(&id).unwrap();
            assert!(
                matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::Cancelled)
            );
            models.reap().await.unwrap();
        }
        assert!(models.active.is_empty());
        upstream.await.unwrap();
        fixture.application.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn invalid_scope_missing_credentials_and_changed_models_do_not_reach_upstream() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(listener.local_addr().unwrap()).await;
    let (mut models, mut receiver) = fixture.forwarding();
    let id = ModelRequestId::new("validation-test");
    let mut wrong_scope = fixture.scope.clone();
    wrong_scope.user_id = UserId::new("someone-else");
    assert_eq!(
        models
            .start(id.clone(), &wrong_scope, fixture.request.clone())
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let mut changed = fixture.request.clone();
    changed.protocol = ProviderProtocol::AnthropicMessages;
    models.start(id.clone(), &fixture.scope, changed).unwrap();
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::PolicyDenied)
    );
    models.reap().await.unwrap();
    fixture
        .application
        .remove_credential("FORWARDING_TEST_KEY")
        .await
        .unwrap();
    models
        .start(id.clone(), &fixture.scope, fixture.request.clone())
        .unwrap();
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::Unavailable)
    );
    models.reap().await.unwrap();
    let mut injected = serde_json::to_value(&fixture.request).unwrap();
    injected["base_url"] = json!("http://requester-supplied-endpoint.invalid");
    assert!(serde_json::from_value::<ComputerModelRequest>(injected).is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn upstream_errors_cannot_echo_source_credentials_to_the_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(listener.local_addr().unwrap()).await;
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = accept_request(&listener).await;
        let body = json!({"error":{"message":SOURCE_KEY}}).to_string();
        socket.write_all(format!("HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    });
    let (mut models, mut receiver) = fixture.forwarding();
    let id = ModelRequestId::new("redaction-test");
    models
        .start(id.clone(), &fixture.scope, fixture.request.clone())
        .unwrap();
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::Execution)
    );
    models.reap().await.unwrap();
    upstream.await.unwrap();
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn retry_backoff_is_reported_and_cancellable_without_another_upstream_call() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(listener.local_addr().unwrap()).await;
    let mut provider = fixture.application.provider_profiles().await.remove(0);
    provider.max_attempts = 3;
    provider.retry_base_delay_ms = 5_000;
    fixture
        .application
        .upsert_provider_profile(provider)
        .await
        .unwrap();
    let (mut models, mut receiver) = fixture.forwarding();
    let id = ModelRequestId::new("retry-cancel-test");
    models
        .start(id.clone(), &fixture.scope, fixture.request.clone())
        .unwrap();
    assert_eq!(
        models
            .start(id.clone(), &fixture.scope, fixture.request.clone())
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    let (mut socket, _) = accept_request(&listener).await;
    let body = json!({"error":{"message":SOURCE_KEY}}).to_string();
    socket.write_all(format!("HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    assert!(matches!(
        next(&mut receiver, &id).await,
        ModelGatewayFrame::RetryScheduled {
            retry: 1,
            max_retries: 2,
            ..
        }
    ));
    models.cancel(&id).unwrap();
    assert!(matches!(
        next(&mut receiver, &id).await,
        ModelGatewayFrame::RetryCancelled { retry: 1 }
    ));
    assert!(
        matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::Cancelled)
    );
    models.reap().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise the real Node WebSocket handshake, streaming completion and cancellation together."
)]
async fn websocket_completion_keeps_the_computer_connected_and_cancel_reaches_upstream() {
    use crate::node::{ConnectionConfig, ReplyCache, connect_once};
    use futures_util::{SinkExt as _, StreamExt as _};
    use ternilo_transport::{
        ConnectionId, ControlFrame, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorId,
    };
    use tokio_tungstenite::tungstenite::Message;
    use tokio_util::task::TaskTracker;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(listener.local_addr().unwrap()).await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut first, _) = accept_request(&listener).await;
        let body =
            "data: {\"choices\":[{\"delta\":{\"content\":\"first result\"}}]}\n\ndata: [DONE]\n\n";
        first.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        drop(first);
        let (mut second, _) = accept_request(&listener).await;
        second.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
        started_tx.send(()).unwrap();
        let mut byte = [0];
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), second.read(&mut byte))
                .await
                .unwrap(),
            Ok(0) | Err(_)
        ));
    });
    let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // The service creates this entry point before opening its LocalApplication.
    tokio::fs::write(fixture.directory.path().join("config.json"), b"{}")
        .await
        .unwrap();
    let address = format!(
        "ws://{}/executor/connect",
        gateway_listener.local_addr().unwrap()
    );
    let application = Arc::clone(&fixture.application);
    let node = tokio::spawn(async move {
        let replies = Arc::new(
            ReplyCache::open(application.data_dir().join("data/db/test-replies.sqlite3"))
                .await
                .unwrap(),
        );
        let result = connect_once(ConnectionConfig {
            gateway_url: &address,
            token: "fixture-node-token",
            executor_id: &ExecutorId::new("source-computer"),
            instance_nonce: "test-source",
            catalog_revision: "test-catalog",
            application,
            replies,
            commands: TaskTracker::new(),
        })
        .await;
        assert!(result.is_ok(), "source Node connection failed: {result:?}");
        result
    });
    let (socket, _) = gateway_listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
    let hello = socket.next().await.unwrap().unwrap();
    let ExecutorFrame::Hello { hello } = serde_json::from_str(hello.to_text().unwrap()).unwrap()
    else {
        panic!("expected hello")
    };
    assert!(
        hello
            .capabilities
            .contains(&ExecutorCapability::ModelForwarding)
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlFrame::Welcome {
                protocol_version: EXECUTOR_PROTOCOL_VERSION,
                connection_id: ConnectionId::new("model-test-connection"),
                scope: fixture.scope.clone(),
                heartbeat_interval_ms: 1_000,
                event_cursors: vec![],
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let start = socket.next().await.unwrap().unwrap();
    let ExecutorFrame::UploadSyncStarted { stream_id, .. } =
        serde_json::from_str(start.to_text().unwrap()).unwrap()
    else {
        panic!("expected upload handshake")
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlFrame::UploadsAcknowledged {
                stream_id,
                last_seq: None,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let mut started_rx = Some(started_rx);
    for index in 0..2 {
        let id = ModelRequestId::new(format!("websocket-model-{index}"));
        socket
            .send(Message::Text(
                serde_json::to_string(&ControlFrame::ModelRequest {
                    request_id: id.clone(),
                    scope: fixture.scope.clone(),
                    request: Box::new(fixture.request.clone()),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        if index == 1 {
            tokio::time::timeout(Duration::from_secs(5), started_rx.take().unwrap())
                .await
                .unwrap()
                .unwrap();
            socket
                .send(Message::Text(
                    serde_json::to_string(&ControlFrame::ModelCancel {
                        request_id: id.clone(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }
        let terminal = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let frame = socket.next().await.unwrap().unwrap();
                if !frame.is_text() {
                    continue;
                }
                let frame: ExecutorFrame = serde_json::from_str(frame.to_text().unwrap()).unwrap();
                assert!(!serde_json::to_string(&frame).unwrap().contains(SOURCE_KEY));
                if let ExecutorFrame::ModelOutput { request_id, frame } = frame {
                    assert_eq!(request_id, id);
                    if frame.is_terminal() {
                        break *frame;
                    }
                }
            }
        })
        .await
        .unwrap();
        if index == 0 {
            assert!(
                matches!(terminal, ModelGatewayFrame::Complete { response } if response.content == "first result")
            );
        } else {
            assert!(
                matches!(terminal, ModelGatewayFrame::Error { error } if error.code == ErrorCode::Cancelled)
            );
        }
    }
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlFrame::Shutdown {
                reason: "test finished".to_owned(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), node)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    upstream.await.unwrap();
    assert!(fixture.application.snapshot().await.sessions.is_empty());
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn model_retries_wait_for_server_permission_and_respect_denial() {
    for allow in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fixture = Fixture::new(listener.local_addr().unwrap()).await;
        let mut provider = fixture.application.provider_profiles().await.remove(0);
        provider.max_attempts = 2;
        fixture
            .application
            .upsert_provider_profile(provider)
            .await
            .unwrap();
        let (mut models, mut receiver) = fixture.forwarding();
        let id = ModelRequestId::new("retry-permission");
        models
            .start(id.clone(), &fixture.scope, fixture.request.clone())
            .unwrap();
        let (mut first, _) = accept_request(&listener).await;
        first.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    receiver.recv().await,
                    Some(ExecutorFrame::ModelAttempt {
                        event: ternilo_protocol::ComputerModelAttempt::Started { attempt: 2 },
                        ..
                    })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
        models
            .permit(
                &id,
                2,
                (!allow).then(|| HarnessError::policy("model access revoked")),
            )
            .unwrap();
        if allow {
            let (mut retry, _) = accept_request(&listener).await;
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"authorized retry\"}}]}\n\ndata: [DONE]\n\n";
            retry.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            assert!(matches!(
                next(&mut receiver, &id).await,
                ModelGatewayFrame::Delta { .. }
            ));
            assert!(
                matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Complete { response } if response.attempts == 2)
            );
        } else {
            assert!(
                matches!(next(&mut receiver, &id).await, ModelGatewayFrame::Error { error } if error.code == ErrorCode::PolicyDenied)
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err()
            );
        }
        models.reap().await.unwrap();
        fixture.application.shutdown().await.unwrap();
    }
}
