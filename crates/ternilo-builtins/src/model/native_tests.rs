use std::fmt::Write;

use super::*;
use serde_json::{Value, json};
use ternilo_protocol::{MessageRole, ModelFinishReason, ModelMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn model(protocol: ProviderProtocol, effort: Option<&str>) -> ProviderModel {
    ProviderModel {
        hosted_tools: None,
        provider: "native".to_owned(),
        endpoint: "http://unused.test".to_owned(),
        protocol,
        model: "native-model".to_owned(),
        context_window: Some(64_000),
        api_key_env: None,
        api_key_override: None,
        max_tokens: Some(8192),
        temperature: None,
        reasoning_effort: effort.map(str::to_owned),
        max_attempts: 1,
        retry_base_delay_ms: 1,
        client: reqwest::Client::new(),
        environment: None,
        attachments: None,
        sessions: None,
    }
}

fn request() -> ModelRequest {
    serde_json::from_value(json!({
        "run_id":"native-run", "system_prompt":"Use the tool, then answer.", "step":1,
        "messages":[{"role":"user","content":"Read fixture.txt"}],
        "tools":[{"name":"read_file","description":"Read a file","input_schema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}],
    })).unwrap()
}

fn history(response: &ModelResponse) -> ModelRequest {
    let persisted: ModelResponse =
        serde_json::from_slice(&serde_json::to_vec(response).unwrap()).unwrap();
    let mut request = request();
    request.messages.push(ModelMessage {
        role: MessageRole::Assistant,
        content: persisted.content,
        reasoning_content: persisted.reasoning_content,
        provider_state: persisted.provider_state,
        attachments: Vec::new(),
        tool_call_id: None,
        tool_calls: persisted.tool_calls,
    });
    request.messages.push(serde_json::from_value(json!({
        "role":"tool", "tool_call_id":response.tool_calls[0].id, "content":"The fixture content",
    })).unwrap());
    request
}

#[test]
fn responses_summary_opt_in_respects_disabled_reasoning_and_protocol_boundaries() {
    for (protocol, effort, expected) in [
        (
            ProviderProtocol::OpenAiResponses,
            Some("high"),
            Some("auto"),
        ),
        (ProviderProtocol::OpenAiResponses, Some("none"), None),
        (ProviderProtocol::OpenAiResponses, None, None),
        (ProviderProtocol::DeepSeekResponses, Some("high"), None),
        (ProviderProtocol::OpenAiChatCompletions, Some("high"), None),
    ] {
        let body = protocols::request_body(&model(protocol, effort), &request()).unwrap();
        assert_eq!(body["reasoning"]["summary"].as_str(), expected);
    }
}

fn gemini_events() -> Vec<Value> {
    vec![
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Inspect ","thought":true}]}}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"the file","thought":true},{"functionCall":{"name":"read_file","args":{"path":"fixture.txt"}},"thoughtSignature":"gemini-signed-state"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":3,"thoughtsTokenCount":8,"cachedContentTokenCount":5}}),
    ]
}

fn claude_events() -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{"id":"msg-fixture","model":"native-model","usage":{"input_tokens":20,"output_tokens":1,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Inspect the file"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"claude-signed-state"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque-redacted"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu-fixture","name":"read_file","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"fixture.txt\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":11}}),
        json!({"type":"message_stop"}),
    ]
}

#[test]
fn native_tool_history_retains_signed_blocks_after_persistence() {
    for (protocol, events) in [
        (ProviderProtocol::GoogleGemini, gemini_events()),
        (ProviderProtocol::AnthropicMessages, claude_events()),
    ] {
        let mut completion = StreamCompletion::default();
        let mut emitted = String::new();
        for event in events {
            if let StreamEvent::Deltas { reasoning, .. } =
                protocols::decode_stream_data(event, protocol, &mut completion).unwrap()
            {
                emitted.push_str(&reasoning);
            }
        }
        let response = completion.finish("native", "native-model").unwrap();
        assert_eq!(
            response.reasoning_content.as_deref(),
            Some("Inspect the file")
        );
        assert_eq!(emitted, "Inspect the file");
        assert_eq!(
            response.tool_calls[0].arguments,
            json!({"path":"fixture.txt"})
        );
        assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
        let body =
            protocols::request_body(&model(protocol, Some("high")), &history(&response)).unwrap();
        let blocks = &response.provider_state.as_ref().unwrap().blocks;
        if protocol == ProviderProtocol::GoogleGemini {
            assert_eq!(body["contents"][1]["parts"], json!(blocks));
            assert_eq!(
                body["contents"][2]["parts"][0]["functionResponse"]["name"],
                "read_file"
            );
            assert_eq!(
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                "high"
            );
            assert_eq!(response.usage.as_ref().unwrap().input_tokens, 20);
        } else {
            assert_eq!(body["messages"][1]["content"], json!(blocks));
            assert_eq!(
                body["messages"][1]["content"][0]["signature"],
                "claude-signed-state"
            );
            assert_eq!(body["messages"][1]["content"][1]["data"], "opaque-redacted");
            assert_eq!(
                body["messages"][2]["content"][0]["tool_use_id"],
                "toolu-fixture"
            );
            assert_eq!(body["thinking"]["type"], "adaptive");
            assert_eq!(response.usage.as_ref().unwrap().input_tokens, 27);
        }
        assert_eq!(response.usage.as_ref().unwrap().output_tokens, 11);
        let mut different_model = model(protocol, None);
        different_model.model = "other-model".to_owned();
        assert!(
            !protocols::request_body(&different_model, &history(&response))
                .unwrap()
                .to_string()
                .contains("signed-state")
        );
    }
}

#[test]
fn native_images_and_manual_thinking_use_native_wire_contracts() {
    let mut request = request();
    request.messages[0].attachments.push(serde_json::from_value(json!({
        "name":"pixel.png", "media_type":"image/png", "content":"data:image/png;base64,cGl4ZWw=",
    })).unwrap());
    let gemini = protocols::request_body(
        &model(ProviderProtocol::GoogleGemini, Some("2048")),
        &request,
    )
    .unwrap();
    assert_eq!(
        gemini["contents"][0]["parts"][1]["inlineData"],
        json!({"mimeType":"image/png","data":"cGl4ZWw="})
    );
    assert_eq!(
        gemini["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        2048
    );
    assert_eq!(
        gemini["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"],
        request.tools[0].input_schema
    );
    let claude = protocols::request_body(
        &model(ProviderProtocol::AnthropicMessages, Some("2048")),
        &request,
    )
    .unwrap();
    assert_eq!(
        claude["messages"][0]["content"][1]["source"],
        json!({"type":"base64","media_type":"image/png","data":"cGl4ZWw="})
    );
    assert_eq!(
        claude["thinking"],
        json!({"type":"enabled","budget_tokens":2048})
    );
    assert_eq!(
        claude["tools"][0]["input_schema"],
        request.tools[0].input_schema
    );
    assert!(
        protocols::request_body(
            &model(ProviderProtocol::AnthropicMessages, Some("8192")),
            &request
        )
        .is_err()
    );
    assert!(
        protocols::request_body(
            &model(ProviderProtocol::GoogleGemini, Some("max")),
            &request
        )
        .is_err()
    );
}

#[test]
fn native_nonstream_completions_and_error_frames_are_not_silently_accepted() {
    for protocol in [
        ProviderProtocol::GoogleGemini,
        ProviderProtocol::AnthropicMessages,
    ] {
        assert!(
            transport::decode_stream_event(
                b"data: [DONE]",
                protocol,
                &mut StreamCompletion::default()
            )
            .is_err()
        );
    }
    let claude = json!({"content":[{"type":"thinking","thinking":"Reason","signature":"signed"},{"type":"text","text":"Answer"}],"stop_reason":"max_tokens","usage":{"input_tokens":5,"output_tokens":6}});
    let response = protocols::decode_completion(
        &serde_json::to_vec(&claude).unwrap(),
        ProviderProtocol::AnthropicMessages,
        "native",
        "native-model",
    )
    .unwrap();
    assert_eq!(response.content, "Answer");
    assert_eq!(response.finish_reason, ModelFinishReason::MaxTokens);
    let gemini = json!({"candidates":[{"content":{"parts":[{"text":"Answer","thoughtSignature":"signed"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":6}});
    let response = protocols::decode_completion(
        &serde_json::to_vec(&gemini).unwrap(),
        ProviderProtocol::GoogleGemini,
        "native",
        "native-model",
    )
    .unwrap();
    assert_eq!(response.content, "Answer");
    for (protocol, value) in [
        (
            ProviderProtocol::GoogleGemini,
            json!({"promptFeedback":{"blockReason":"SAFETY"}}),
        ),
        (
            ProviderProtocol::GoogleGemini,
            json!({"candidates":[{"content":{"parts":[]}}]}),
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"type":"error","error":{"message":"overloaded"}}),
        ),
        (ProviderProtocol::AnthropicMessages, json!({"content":[]})),
    ] {
        assert!(
            protocols::decode_completion(
                &serde_json::to_vec(&value).unwrap(),
                protocol,
                "native",
                "native-model"
            )
            .is_err()
        );
    }
}

#[derive(Default)]
struct Output(tokio::sync::Mutex<Vec<String>>);

impl ModelOutput for Output {
    fn emit<'a>(
        &'a self,
        text: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.lock().await.push(text);
            Ok(())
        })
    }
}

async fn upstream(
    replies: Vec<(String, String)>,
) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (content_type, reply) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 2048];
            let (headers, offset, length) = loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(offset) = bytes.windows(4).position(|pair| pair == b"\r\n\r\n") {
                    let headers = String::from_utf8(bytes[..offset].to_vec()).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                    if bytes.len() >= offset + 4 + length {
                        break (headers, offset + 4, length);
                    }
                }
            };
            let body = if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[offset..offset + length]).unwrap()
            };
            requests.push((headers, body));
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\nrequest-id: native-fixture\r\n\r\n").as_bytes()).await.unwrap();
            for chunk in reply.as_bytes().chunks(19) {
                socket.write_all(chunk).await.unwrap();
            }
            socket.shutdown().await.unwrap();
        }
        requests
    });
    (format!("http://{address}/v1"), task)
}

#[tokio::test]
async fn native_http_streams_use_real_auth_endpoints_and_wait_for_complete_usage() {
    for (protocol, events) in [
        (ProviderProtocol::GoogleGemini, gemini_events()),
        (ProviderProtocol::AnthropicMessages, claude_events()),
    ] {
        let mut stream = String::new();
        for event in events {
            write!(stream, "data: {event}\r\n\r\n").unwrap();
        }
        let (base_url, server) = upstream(vec![("text/event-stream".to_owned(), stream)]).await;
        let route = ProviderModelRoute {
            hosted_tools: None,
            provider: "native".to_owned(),
            base_url,
            protocol,
            model: "native-model".to_owned(),
            context_window: Some(64_000),
            timeout_ms: 5000,
            max_tokens: Some(8192),
            temperature: None,
            reasoning_effort: Some("high".to_owned()),
            max_attempts: 1,
            retry_base_delay_ms: 1,
        };
        let response = complete_provider_model(
            route,
            Some("fixture-key".to_owned()),
            request(),
            Arc::new(Output::default()),
            RunCancellation::default(),
            None,
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        let headers = requests[0].0.to_lowercase();
        assert!(!headers.contains("authorization:"));
        assert_eq!(
            response.provider_request_id.as_deref(),
            Some("native-fixture")
        );
        assert_eq!(response.usage.unwrap().output_tokens, 11);
        if protocol == ProviderProtocol::GoogleGemini {
            assert!(
                headers.starts_with("post /v1/models/native-model:streamgeneratecontent?alt=sse ")
            );
            assert!(headers.contains("x-goog-api-key: fixture-key"));
            assert!(!requests[0].1.as_object().unwrap().contains_key("model"));
        } else {
            assert!(headers.starts_with("post /v1/messages "));
            assert!(headers.contains("x-api-key: fixture-key"));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
        }
    }
}

#[tokio::test]
async fn native_discovery_paginates_and_does_not_infer_missing_thinking_levels() {
    for protocol in [
        ProviderProtocol::GoogleGemini,
        ProviderProtocol::AnthropicMessages,
    ] {
        let pages = if protocol == ProviderProtocol::GoogleGemini {
            vec![
                json!({"models":[{"name":"models/embedding","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"next"}),
                json!({"models":[{"name":"models/native-model","displayName":"Gemini fixture","inputTokenLimit":100_000,"outputTokenLimit":8192,"thinking":true,"supportedGenerationMethods":["generateContent"]}]}),
            ]
        } else {
            vec![
                json!({"data":[{"id":"native-model","display_name":"Claude fixture","max_input_tokens":100_000,"max_tokens":8192,"capabilities":{"thinking":{"supported":true,"types":{"adaptive":{"supported":true}}},"effort":{"high":{"supported":true},"low":{"supported":true},"max":{"supported":false}}}}],"has_more":true,"last_id":"next"}),
                json!({"data":[{"id":"other-model"}],"has_more":false}),
            ]
        };
        let (base, server) = upstream(
            pages
                .iter()
                .map(|page| ("application/json".to_owned(), page.to_string()))
                .collect(),
        )
        .await;
        let models = crate::discover_provider_models(
            &reqwest::Client::new(),
            &base,
            protocol,
            Some("fixture-key"),
        )
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        let headers = requests[0].0.to_ascii_lowercase();
        assert!(headers.starts_with("get /v1/models "));
        assert!(!headers.contains("authorization:"));
        if protocol == ProviderProtocol::AnthropicMessages {
            assert!(headers.contains("x-api-key: fixture-key"));
            assert!(headers.contains("anthropic-version: 2023-06-01"));
        } else {
            assert!(headers.contains("x-goog-api-key: fixture-key"));
        }
        assert!(
            requests[1]
                .0
                .contains(if protocol == ProviderProtocol::GoogleGemini {
                    "pageToken=next"
                } else {
                    "after_id=next"
                })
        );
        let ternilo_protocol::ProviderModelSettings::Automatic { upstream, .. } =
            &models[0].settings
        else {
            panic!("automatic settings");
        };
        assert_eq!(upstream.context_window, Some(100_000));
        if protocol == ProviderProtocol::GoogleGemini {
            assert_eq!(upstream.reasoning, None);
        } else {
            let Some(ternilo_protocol::ProviderReasoningSetting::Enabled { configuration }) =
                &upstream.reasoning
            else {
                panic!("explicit adaptive levels");
            };
            assert_eq!(configuration.efforts.len(), 2);
        }
    }
}

#[test]
fn hosted_web_requests_replace_same_named_local_tools_and_keep_other_functions() {
    let mut model = model(ProviderProtocol::AnthropicMessages, None);
    model.hosted_tools = Some(ternilo_protocol::HostedWebTools {
        web_search: true,
        web_fetch: true,
        max_uses: 2,
        max_content_tokens: 1000,
        allowed_domains: vec!["example.com/docs".to_owned()],
        blocked_domains: Vec::new(),
    });
    let mut request = request();
    request.tools.push(serde_json::from_value(json!({"name":"web_fetch","description":"Local fetch","input_schema":{"type":"object"}})).unwrap());
    let body = protocols::request_body(&model, &request).unwrap();
    let tools = body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0]["name"], "read_file");
    assert_eq!(tools[1]["type"], "web_search_20250305");
    assert_eq!(tools[1]["max_uses"], 2);
    assert_eq!(tools[1]["allowed_domains"], json!(["example.com/docs"]));
    assert_eq!(tools[2]["type"], "web_fetch_20250910");
    assert_eq!(tools[2]["citations"], json!({"enabled":true}));
    assert_eq!(tools[2]["max_content_tokens"], 1000);
    request.step = 0;
    let auxiliary = protocols::request_body(&model, &request).unwrap();
    assert_eq!(auxiliary["tool_choice"], json!({"type":"none"}));
    assert!(
        !auxiliary["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool.get("type").is_some())
    );
}

#[test]
fn hosted_web_stream_preserves_inputs_encrypted_results_citations_and_pause_after_storage() {
    let result = json!({"type":"web_search_tool_result","tool_use_id":"srv-1","content":[{"type":"web_search_result","url":"https://example.com","title":"Source","encrypted_content":"keep-exactly"}]});
    let citation = json!({"type":"web_search_result_location","url":"https://example.com","title":"Source","encrypted_index":"index-exactly","cited_text":"source excerpt"});
    let mut completion = StreamCompletion::default();
    for value in [
        json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srv-1","name":"web_search","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"example\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":result}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":"Answer","citations":[]}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":citation}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"pause_turn"},"usage":{"output_tokens":5}}),
        json!({"type":"message_stop"}),
    ] {
        protocols::decode_stream_data(value, ProviderProtocol::AnthropicMessages, &mut completion)
            .unwrap();
    }
    let response = completion.finish("native", "native-model").unwrap();
    assert_eq!(response.finish_reason, ModelFinishReason::Pause);
    assert!(
        response.tool_calls.is_empty(),
        "server tools never dispatch on the execution host"
    );
    let state = response.provider_state.as_ref().unwrap();
    assert_eq!(state.blocks[0]["input"], json!({"query":"example"}));
    assert_eq!(state.blocks[1], result);
    assert_eq!(state.blocks[2]["citations"], json!([citation]));
    let persisted: ModelResponse =
        serde_json::from_slice(&serde_json::to_vec(&response).unwrap()).unwrap();
    let mut continued = request();
    continued.messages.push(ModelMessage {
        role: MessageRole::Assistant,
        content: persisted.content,
        reasoning_content: persisted.reasoning_content,
        provider_state: persisted.provider_state,
        attachments: Vec::new(),
        tool_call_id: None,
        tool_calls: Vec::new(),
    });
    let body = protocols::request_body(
        &model(ProviderProtocol::AnthropicMessages, None),
        &continued,
    )
    .unwrap();
    assert_eq!(body["messages"][1]["content"], json!(state.blocks));
}
