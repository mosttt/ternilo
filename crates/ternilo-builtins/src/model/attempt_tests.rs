use std::{sync::Mutex, time::Duration};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;
use serde_json::{Value, json};
use ternilo_protocol::ModelRetryFailure;

#[derive(Default)]
struct Recorder {
    trace: Mutex<Vec<String>>,
    text_emitted: tokio::sync::Notify,
    reports: Mutex<Vec<ModelAttemptReport>>,
    deny_attempt: Option<u32>,
    cancel_on_text: Option<RunCancellation>,
    cancel_on_retry: Option<RunCancellation>,
}

impl ModelAttemptObserver for Recorder {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(format!("before:{attempt}"));
            if self.deny_attempt == Some(attempt) {
                return Err(HarnessError::policy("attempt budget is exhausted"));
            }
            Ok(())
        })
    }

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.trace
                .lock()
                .unwrap()
                .push(format!("after:{}", report.attempt));
            self.reports.lock().unwrap().push(report);
            Ok(())
        })
    }
}

impl ModelOutput for Recorder {
    fn emit<'a>(
        &'a self,
        text: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(format!("text:{text}"));
            self.text_emitted.notify_one();
            if let Some(cancellation) = &self.cancel_on_text {
                cancellation.cancel();
            }
            Ok(())
        })
    }

    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        _: u32,
        _: u64,
        _: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(format!("scheduled:{retry}"));
            if let Some(cancellation) = &self.cancel_on_retry {
                cancellation.cancel();
            }
            Ok(())
        })
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(format!("started:{retry}"));
            Ok(())
        })
    }

    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(format!("cancelled:{retry}"));
            Ok(())
        })
    }
}

struct Reply {
    status: u16,
    body: String,
    stream: bool,
    stall: bool,
}

async fn upstream(replies: Vec<Reply>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let (offset, length) = loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "client must send a complete request");
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..offset]).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .unwrap();
                    if bytes.len() >= offset + 4 + length {
                        break (offset + 4, length);
                    }
                }
            };
            requests.push(serde_json::from_slice(&bytes[offset..offset + length]).unwrap());
            let content_type = if reply.stream {
                "text/event-stream"
            } else {
                "application/json"
            };
            let headers = format!(
                "HTTP/1.1 {} Test\r\nContent-Type: {content_type}\r\nConnection: close\r\nx-request-id: attempt-{}\r\n\r\n",
                reply.status,
                requests.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(reply.body.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            if reply.stall {
                let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
                    .await
                    .expect("cancelled model closes its upstream stream")
                    .unwrap();
                assert_eq!(read, 0);
            }
        }
        requests
    });
    (format!("http://{address}/v1"), server)
}

fn request() -> ModelRequest {
    ModelRequest {
        run_id: ternilo_protocol::RunId::new("model-test-run"),
        system_prompt: "System".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        step: 1,
    }
}

fn route(base_url: String, protocol: ProviderProtocol) -> ProviderModelRoute {
    ProviderModelRoute {
        hosted_tools: None,
        provider: "provider".to_owned(),
        base_url,
        protocol,
        model: "model".to_owned(),
        context_window: Some(32_000),
        timeout_ms: 10_000,
        max_tokens: Some(512),
        temperature: None,
        reasoning_effort: None,
        max_attempts: 3,
        retry_base_delay_ms: 1,
    }
}

fn rejected() -> Reply {
    Reply { status:503,body:json!({"error":{"message":"temporary error"},"usage":{"prompt_tokens":7,"completion_tokens":2}}).to_string(),stream:false,stall:false }
}

fn completed() -> Reply {
    Reply { status:200,body:json!({"choices":[{"message":{"content":"complete"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":10}}}).to_string(),stream:false,stall:false }
}

#[tokio::test]
async fn every_http_attempt_is_authorized_and_observed_before_retry_events() {
    let (url, server) = upstream(vec![rejected(), completed()]).await;
    let recorder = Arc::new(Recorder::default());
    let response = complete_provider_model(
        route(url, ProviderProtocol::OpenAiChatCompletions),
        None,
        request(),
        recorder.clone(),
        RunCancellation::new(),
        Some(recorder.clone()),
    )
    .await
    .unwrap();
    assert_eq!(response.attempts, 2);
    assert_eq!(response.provider_request_id.as_deref(), Some("attempt-2"));
    assert_eq!(server.await.unwrap().len(), 2);
    assert_eq!(
        *recorder.trace.lock().unwrap(),
        [
            "before:1",
            "after:1",
            "scheduled:1",
            "started:1",
            "before:2",
            "text:complete",
            "after:2"
        ]
    );
    let reports = recorder.reports.lock().unwrap();
    assert_eq!(reports[0].http_status, Some(503));
    assert!(reports[0].error.is_some());
    assert_eq!(reports[0].usage.as_ref().unwrap()["prompt_tokens"], 7);
    assert_eq!(
        reports[1].usage.as_ref().unwrap()["completion_tokens_details"]["reasoning_tokens"],
        10
    );
    assert!(reports[1].error.is_none());
}

#[tokio::test]
async fn a_denied_retry_never_reaches_the_upstream() {
    let (url, server) = upstream(vec![rejected()]).await;
    let recorder = Arc::new(Recorder {
        deny_attempt: Some(2),
        ..Recorder::default()
    });
    let error = complete_provider_model(
        route(url, ProviderProtocol::OpenAiChatCompletions),
        None,
        request(),
        recorder.clone(),
        RunCancellation::new(),
        Some(recorder.clone()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(recorder.reports.lock().unwrap().len(), 1);
    assert_eq!(
        *recorder.trace.lock().unwrap(),
        [
            "before:1",
            "after:1",
            "scheduled:1",
            "started:1",
            "before:2"
        ]
    );
}

#[tokio::test]
async fn cancelling_a_retry_wait_preserves_the_finished_attempt_and_retry_event() {
    let (url, server) = upstream(vec![rejected()]).await;
    let cancellation = RunCancellation::new();
    let recorder = Arc::new(Recorder {
        cancel_on_retry: Some(cancellation.clone()),
        ..Recorder::default()
    });
    let error = complete_provider_model(
        route(url, ProviderProtocol::OpenAiChatCompletions),
        None,
        request(),
        recorder.clone(),
        cancellation,
        Some(recorder.clone()),
    )
    .await
    .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(
        *recorder.trace.lock().unwrap(),
        ["before:1", "after:1", "scheduled:1", "cancelled:1"]
    );
    assert_eq!(
        recorder.reports.lock().unwrap()[0].usage.as_ref().unwrap()["prompt_tokens"],
        7
    );
}

#[tokio::test]
async fn cancellation_after_a_usage_event_preserves_native_counts_without_a_terminal_event() {
    for protocol in [
        ProviderProtocol::OpenAiChatCompletions,
        ProviderProtocol::OpenAiResponses,
    ] {
        let native_usage = if protocol == ProviderProtocol::OpenAiChatCompletions {
            json!({"prompt_tokens":90,"cache_creation_input_tokens":5})
        } else {
            json!({"input_tokens":90,"input_tokens_details":{"cache_write_tokens":5}})
        };
        let event = if protocol == ProviderProtocol::OpenAiChatCompletions {
            json!({"choices":[{"index":0,"delta":{"content":"usage observed"},"finish_reason":null}],"usage":native_usage})
        } else {
            json!({"type":"response.output_text.delta","delta":"usage observed","usage":native_usage})
        };
        let (url, server) = upstream(vec![Reply {
            status: 200,
            body: format!("data: {event}\n\n"),
            stream: true,
            stall: true,
        }])
        .await;
        let cancellation = RunCancellation::new();
        let recorder = Arc::new(Recorder {
            cancel_on_text: Some(cancellation.clone()),
            ..Recorder::default()
        });
        let error = complete_provider_model(
            route(url, protocol),
            None,
            request(),
            recorder.clone(),
            cancellation,
            Some(recorder.clone()),
        )
        .await
        .unwrap_err();
        assert!(error.is_cancelled());
        assert_eq!(server.await.unwrap().len(), 1);
        let reports = recorder.reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].usage, Some(native_usage));
        assert_eq!(reports[0].upstream_request_id.as_deref(), Some("attempt-1"));
        assert!(reports[0].error.as_ref().unwrap().is_cancelled());
    }
}

#[tokio::test]
async fn broken_stream_does_not_replay_partial_output_and_keeps_observed_usage() {
    let event = json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":null}],"usage":{"prompt_tokens":12,"completion_tokens":3}});
    let (url, server) = upstream(vec![Reply {
        status: 200,
        body: format!("data: {event}\n\n"),
        stream: true,
        stall: false,
    }])
    .await;
    let recorder = Arc::new(Recorder::default());
    let error = complete_provider_model(
        route(url, ProviderProtocol::OpenAiChatCompletions),
        None,
        request(),
        recorder.clone(),
        RunCancellation::new(),
        Some(recorder.clone()),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("completion event"));
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(
        *recorder.trace.lock().unwrap(),
        ["before:1", "text:partial", "after:1"]
    );
    assert_eq!(
        recorder.reports.lock().unwrap()[0].usage.as_ref().unwrap()["completion_tokens"],
        3
    );
}

#[test]
fn partial_usage_events_preserve_previously_observed_fields() {
    let mut report = ModelAttemptReport::new(1);
    report
        .observe(&json!({"usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":9}}}));
    report.observe(&json!({"response":{"usage":{"input_tokens":null,"output_tokens":3,"input_tokens_details":{"cache_write_tokens":2}}}}));
    assert_eq!(
        report.usage,
        Some(
            json!({"input_tokens":12,"output_tokens":3,"input_tokens_details":{"cached_tokens":9,"cache_write_tokens":2}})
        )
    );
}

#[tokio::test]
async fn stream_timeout_retains_received_text_and_does_not_retry_started_output() {
    let (base_url, server) = upstream(vec![Reply {
        status: 200,
        body: "data: {\"choices\":[{\"delta\":{\"content\":\"Keep this partial answer\"},\"finish_reason\":null}]}\n\n".to_owned(),
        stream: true,
        stall: true,
    }]).await;
    let mut configured = route(base_url, ProviderProtocol::OpenAiChatCompletions);
    configured.timeout_ms = 200;
    let recorder = Arc::new(Recorder::default());
    let failure = complete_provider_model(
        configured,
        None,
        request(),
        recorder.clone(),
        RunCancellation::default(),
        Some(recorder.clone()),
    )
    .await
    .unwrap_err();
    assert!(
        failure.message.contains("model request timed out"),
        "{}",
        failure.message
    );
    assert!(
        recorder
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "text:Keep this partial answer")
    );
    assert_eq!(recorder.reports.lock().unwrap().len(), 1);
    assert!(!recorder.reports.lock().unwrap()[0].retryable);
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn zero_timeout_keeps_a_stream_open_until_cancelled() {
    let (base_url, server) = upstream(vec![Reply {
        status: 200,
        body: "data: {\"choices\":[{\"delta\":{\"content\":\"Still running\"},\"finish_reason\":null}]}\n\n".to_owned(),
        stream: true,
        stall: true,
    }]).await;
    let mut configured = route(base_url, ProviderProtocol::OpenAiChatCompletions);
    configured.timeout_ms = 0;
    let cancellation = RunCancellation::default();
    let recorder = Arc::new(Recorder::default());
    let running = complete_provider_model(
        configured,
        None,
        request(),
        recorder.clone(),
        cancellation.clone(),
        Some(recorder.clone()),
    );
    tokio::pin!(running);
    // Observe the idle stream after its first output, independent of startup scheduling.
    tokio::select! {
        result = &mut running => panic!("stream completed before its first output: {result:?}"),
        () = recorder.text_emitted.notified() => {},
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("stream did not produce its first output"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut running)
            .await
            .is_err()
    );
    assert!(
        recorder
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "text:Still running")
    );
    cancellation.cancel();
    let failure = tokio::time::timeout(Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.code, ternilo_protocol::ErrorCode::Cancelled);
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn session_provider_persists_each_attempt_but_connected_servers_keep_their_own_ledger() {
    use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy};
    use ternilo_protocol::{
        AgentId, RunId, RunLimits, SessionEventKind, SessionId, SessionIdentity, TenantId, UserId,
    };
    for source in ["direct_provider", "connected_server"] {
        let mut partial_failure = rejected();
        partial_failure.body =
            json!({"error":{"message":"temporary error"},"usage":{"prompt_tokens":7}}).to_string();
        let (url, server) = upstream(vec![partial_failure, completed()]).await;
        let mut profile = crate::local_profile();
        let model = profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "model")
            .unwrap();
        model.kind = KIND.to_owned();
        model.config = json!({ "provider":"usage-provider","base_url":url,"model":"usage-model","usage_source":source,"retry_base_delay_ms":1,"max_attempts":2 });
        let session_id = SessionId::new("usage-session");
        let harness = HarnessSession::boot(
            &crate::catalog().unwrap(),
            &profile,
            HostEnvironment::memory(
                SessionIdentity {
                    tenant_id: TenantId::new("local"),
                    user_id: UserId::new("local-user"),
                    agent_id: AgentId::new("agent"),
                    session_id: session_id.clone(),
                },
                None,
                HostPolicy::local(RunLimits::default()),
            ),
        )
        .await
        .unwrap();
        let outcome = harness
            .run(RunId::new("usage-run"), "answer")
            .await
            .unwrap();
        assert_eq!(server.await.unwrap().len(), 2);
        let starts = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::ProviderUsageStarted {
                    source_session_id,
                    attempt,
                    route,
                    ..
                } => Some((event.seq, source_session_id, *attempt, route)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let finishes = outcome
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::ProviderUsageFinished {
                    started_seq,
                    usage,
                    error_code,
                    ..
                } => Some((*started_seq, usage, error_code)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if source == "direct_provider" {
            assert_eq!(starts.len(), 2);
            assert_eq!(finishes.len(), 2);
            for (index, (seq, origin, attempt, route)) in starts.iter().enumerate() {
                assert_eq!(origin.as_ref(), Some(&session_id));
                assert_eq!(*attempt as usize, index + 1);
                assert_eq!(route.provider, "usage-provider");
                assert_eq!(finishes[index].0, *seq);
            }
            assert_eq!(finishes[0].1.as_ref().unwrap().input_tokens, Some(7));
            assert_eq!(finishes[0].1.as_ref().unwrap().output_tokens, None);
            assert!(finishes[0].2.is_some());
            assert_eq!(finishes[1].1.as_ref().unwrap().input_tokens, Some(100));
            assert_eq!(finishes[1].1.as_ref().unwrap().output_tokens, Some(20));
            assert_eq!(
                finishes[1].1.as_ref().unwrap().cached_input_tokens,
                Some(80)
            );
            assert_eq!(finishes[1].1.as_ref().unwrap().reasoning_tokens, Some(10));
            assert!(finishes[1].2.is_none());
        } else {
            assert!(
                starts.is_empty() && finishes.is_empty(),
                "official Server model sources are not counted as direct device calls"
            );
        }
        harness.shutdown().await.unwrap();
    }
}
