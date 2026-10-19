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
        reasoning_recommendation: None,
        prompt_cache: false,
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
        automatic_reasoning_efforts: ternilo_protocol::AutomaticReasoningEfforts::new(),
        max_attempts: 3,
        retry_base_delay_ms: 1,
    }
}

fn rejected() -> Reply {
    Reply { status:503,body:json!({"error":{"message":"temporary error"},"usage":{"prompt_tokens":7,"completion_tokens":2}}).to_string(),stream:false,stall:false }
}

#[tokio::test]
async fn deepseek_http_failures_keep_safe_causes_and_the_documented_retry_behavior() {
    for status in [400, 401, 402, 422, 429, 500, 503] {
        let attempts = if matches!(status, 429 | 500 | 503) {
            3
        } else {
            1
        };
        let replies = (0..attempts).map(|_| Reply {
            status,
            body: json!({"error":{"message":"Insufficient Balance; private-upstream-key private-route"}}).to_string(),
            stream: false,
            stall: false,
        }).collect();
        let (base_url, server) = upstream(replies).await;
        let recorder = Arc::new(Recorder::default());
        let error = complete_provider_model(
            route(base_url.clone(), ProviderProtocol::DeepSeekResponses),
            Some("private-upstream-key".into()),
            request(),
            recorder.clone(),
            RunCancellation::default(),
            Some(recorder.clone()),
        )
        .await
        .unwrap_err();
        let expected = crate::ProviderHttpFailure::from_response(
            ProviderProtocol::DeepSeekResponses,
            &base_url,
            status,
            &[],
        )
        .unwrap();
        assert_eq!(error.message, expected.message);
        assert!(!error.message.contains("private-"));
        assert_eq!(server.await.unwrap().len(), attempts);
        let reports = recorder.reports.lock().unwrap();
        assert_eq!(reports.len(), attempts);
        assert!(
            reports
                .iter()
                .all(|report| report.http_status == Some(status))
        );
        assert_eq!(
            reports.last().unwrap().upstream_request_id,
            Some(format!("attempt-{attempts}"))
        );
    }
}

fn completed() -> Reply {
    Reply { status:200,body:json!({"choices":[{"message":{"content":"complete"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":80},"completion_tokens_details":{"reasoning_tokens":10}}}).to_string(),stream:false,stall:false }
}

#[tokio::test]
async fn openai_billing_failures_stop_retrying_but_rate_limits_still_retry() {
    for protocol in [
        ProviderProtocol::OpenAiChatCompletions,
        ProviderProtocol::OpenAiResponses,
    ] {
        for code in [
            "credit_balance_exhausted",
            "organization_spend_limit_exceeded",
            "project_spend_limit_exceeded",
            "organization_usage_limit_exceeded",
            "insufficient_quota",
            "rate_limit_exceeded",
            "slow_down",
        ] {
            let attempts = if matches!(code, "rate_limit_exceeded" | "slow_down") {
                3
            } else {
                1
            };
            let replies = (0..attempts).map(|_| Reply {
                status: 429,
                body: json!({"error":{"code":code,"type":"insufficient_quota","message":"private-api-key"}}).to_string(),
                stream: false,
                stall: false,
            }).collect();
            let (base_url, server) = upstream(replies).await;
            let recorder = Arc::new(Recorder::default());
            let error = complete_provider_model(
                route(base_url, protocol),
                None,
                request(),
                recorder.clone(),
                RunCancellation::default(),
                Some(recorder.clone()),
            )
            .await
            .unwrap_err();
            assert_eq!(
                crate::ProviderHttpFailure::from_public_message(&error.message)
                    .unwrap()
                    .code,
                code
            );
            assert!(!error.message.contains("private-api-key"));
            assert_eq!(server.await.unwrap().len(), attempts);
            assert_eq!(recorder.reports.lock().unwrap().len(), attempts);
        }
    }
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

#[tokio::test]
async fn automatic_reasoning_changes_the_real_request_across_all_protocols_only_when_enabled() {
    use ternilo_protocol::ReasoningEffort::{High, Low};
    for protocol in [
        ProviderProtocol::OpenAiChatCompletions,
        ProviderProtocol::OpenAiResponses,
        ProviderProtocol::DeepSeekResponses,
        ProviderProtocol::GoogleGemini,
        ProviderProtocol::AnthropicMessages,
    ] {
        let body = match protocol {
            ProviderProtocol::OpenAiChatCompletions => {
                serde_json::from_str::<Value>(&completed().body).unwrap()
            }
            ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
                json!({"id":"r","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"complete"}]}]})
            }
            ProviderProtocol::GoogleGemini => {
                json!({"candidates":[{"content":{"role":"model","parts":[{"text":"complete"}]},"finishReason":"STOP"}]})
            }
            ProviderProtocol::AnthropicMessages => {
                json!({"id":"r","content":[{"type":"text","text":"complete"}],"stop_reason":"end_turn"})
            }
        };
        let replies = (0..3)
            .map(|_| Reply {
                status: 200,
                body: body.to_string(),
                stream: false,
                stall: false,
            })
            .collect();
        let (url, server) = upstream(replies).await;
        let mut model_route = route(url, protocol);
        model_route.reasoning_effort = Some("high".into());
        model_route.automatic_reasoning_efforts =
            [(Low, Some("low".into())), (High, Some("high".into()))].into();
        for recommended in [None, Some(Low), Some(High)] {
            let mut request = request();
            request.reasoning_recommendation = recommended;
            let response = complete_provider_model(
                model_route.clone(),
                None,
                request,
                Arc::new(Recorder::default()),
                RunCancellation::new(),
                None,
            )
            .await
            .unwrap();
            assert_eq!(
                response
                    .reasoning_selection
                    .as_ref()
                    .and_then(|selection| selection.selected),
                recommended
            );
        }
        let requests = server.await.unwrap();
        for (request, expected) in requests.iter().zip(["high", "low", "high"]) {
            let actual = match protocol {
                ProviderProtocol::OpenAiChatCompletions => &request["reasoning_effort"],
                ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
                    &request["reasoning"]["effort"]
                }
                ProviderProtocol::GoogleGemini => {
                    &request["generationConfig"]["thinkingConfig"]["thinkingLevel"]
                }
                ProviderProtocol::AnthropicMessages => &request["output_config"]["effort"],
            };
            assert_eq!(actual, expected, "{protocol:?}");
        }
        assert_eq!(model_route.reasoning_effort.as_deref(), Some("high"));
    }
}

#[tokio::test]
async fn unsupported_automatic_budgets_keep_the_existing_request_usable() {
    let replies = (0..2).map(|_| Reply {
        status: 200,
        body: json!({"id":"r","content":[{"type":"text","text":"complete"}],"stop_reason":"end_turn"}).to_string(),
        stream: false,
        stall: false,
    }).collect();
    let (url, server) = upstream(replies).await;
    let mut model_route = route(url, ProviderProtocol::AnthropicMessages);
    model_route
        .automatic_reasoning_efforts
        .insert(ternilo_protocol::ReasoningEffort::Low, Some("1024".into()));
    for recommended in [None, Some(ternilo_protocol::ReasoningEffort::Low)] {
        let mut request = request();
        request.reasoning_recommendation = recommended;
        let response = complete_provider_model(
            model_route.clone(),
            None,
            request,
            Arc::new(Recorder::default()),
            RunCancellation::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            response
                .reasoning_selection
                .as_ref()
                .and_then(|selection| selection.selected),
            None
        );
        assert_eq!(
            response.reasoning_selection.is_some(),
            recommended.is_some()
        );
    }
    for request in server.await.unwrap() {
        assert!(request.get("thinking").is_none());
        assert_eq!(request["max_tokens"], 512);
    }
}
