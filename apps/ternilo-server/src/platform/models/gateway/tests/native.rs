use super::*;
use std::fmt::Write;

#[handler]
pub(super) async fn fake_upstream(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) {
    let gemini = request.uri().path().contains("/models/");
    let body = request.parse_json::<Value>().await.unwrap();
    assert!(request.headers().get(header::AUTHORIZATION).is_none());
    assert_eq!(
        request
            .headers()
            .get(if gemini {
                "x-goog-api-key"
            } else {
                "x-api-key"
            })
            .unwrap(),
        "upstream-test-secret"
    );
    if !gemini {
        assert_eq!(request.headers()["anthropic-version"], "2023-06-01");
    }
    depot
        .get_typed::<UpstreamState>()
        .unwrap()
        .requests
        .lock()
        .await
        .push(body.clone());
    let stream = if gemini {
        request.uri().path().ends_with(":streamGenerateContent")
    } else {
        body["stream"].as_bool() == Some(true)
    };
    let usage = if gemini {
        json!({"promptTokenCount":20,"candidatesTokenCount":4,"thoughtsTokenCount":6,"cachedContentTokenCount":2})
    } else {
        json!({"input_tokens":15,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":10})
    };
    let result = if gemini {
        json!({"modelVersion":"internal-model","candidates":[{"content":{"role":"model","parts":[{"text":"Answer","thoughtSignature":"native-signature"}]},"finishReason":"STOP"}]})
    } else {
        json!({"id":"msg-test","type":"message","role":"assistant","model":"internal-model","content":[{"type":"thinking","thinking":"Reasoning","signature":"native-signature"},{"type":"text","text":"Answer"}],"stop_reason":"end_turn","usage":usage})
    };
    if !stream {
        let mut result = result;
        if gemini {
            result["usageMetadata"] = usage;
        }
        response.render(Json(result));
        return;
    }
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
    let events = if gemini {
        vec![result, json!({"usageMetadata":usage})]
    } else {
        vec![
            json!({"type":"message_start","message":{"id":"msg-test","model":"internal-model","usage":{"input_tokens":15,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"Reasoning","signature":"native-signature"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"Answer"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}),
            json!({"type":"message_stop"}),
        ]
    };
    let mut bytes = String::new();
    for event in events {
        write!(bytes, "data: {event}\n\n").unwrap();
    }
    response.stream(futures_util::stream::iter(vec![Ok::<_, std::io::Error>(
        bytes.into_bytes(),
    )]));
}

#[tokio::test]
async fn native_endpoints_keep_signatures_settle_usage_and_list_the_right_catalog() {
    let fixture = Fixture::with_native(true).await;
    for (protocol, header_name, model, path) in [
        (
            ProviderProtocol::GoogleGemini,
            "x-goog-api-key",
            "gemini",
            "models/gemini",
        ),
        (
            ProviderProtocol::AnthropicMessages,
            "x-api-key",
            "claude",
            "messages",
        ),
    ] {
        let catalog = fixture
            .client
            .get(format!("{}/v1/models", fixture.server.base))
            .header(header_name, &fixture.key.token)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(
            catalog[if model == "gemini" { "models" } else { "data" }]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for stream in [false, true] {
            let suffix = if model == "gemini" {
                if stream {
                    ":streamGenerateContent?alt=sse"
                } else {
                    ":generateContent"
                }
            } else {
                ""
            };
            let body = if model == "gemini" {
                json!({"contents":[{"role":"user","parts":[{"text":"Hello"}]}]})
            } else {
                json!({"model":model,"stream":stream,"messages":[{"role":"user","content":"Hello"}]})
            };
            let response = fixture
                .client
                .post(format!("{}/v1/{path}{suffix}", fixture.server.base))
                .header(header_name, &fixture.key.token)
                .json(&body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let request_id = response
                .headers()
                .get("x-ternilo-request-id")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let text = response.text().await.unwrap();
            assert_eq!(status, StatusCode::OK, "{text}");
            assert!(text.contains("native-signature"));
            assert!(!text.contains("internal-model"));
            let record = fixture.ledger(&request_id).await;
            assert_eq!(record["protocol"], protocol.as_str());
            assert_eq!(record["state"], "completed");
            assert_eq!(record["usage"]["input_tokens"], 20);
            assert_eq!(record["usage"]["output_tokens"], 10);
        }
    }
    fixture.close().await;
}

#[test]
fn native_request_validation_respects_published_reasoning_and_forbids_hosted_tools() {
    let mut settings = defaults();
    settings.reasoning = Some(ProviderModelReasoning {
        default_effort: ReasoningEffort::High,
        efforts: [(ReasoningEffort::High, Some("high".to_owned()))]
            .into_iter()
            .collect(),
    });
    for (protocol, body, pointer) in [
        (
            ProviderProtocol::GoogleGemini,
            json!({"model":"public","contents":[{"role":"user","parts":[{"text":"test"}]}]}),
            "/generationConfig/thinkingConfig/thinkingLevel",
        ),
        (
            ProviderProtocol::AnthropicMessages,
            json!({"model":"public","messages":[{"role":"user","content":"test"}]}),
            "/output_config/effort",
        ),
    ] {
        let mut prepared = PreparedRequest::parse(body.clone(), protocol).unwrap();
        prepared
            .prepare_upstream(protocol, "internal-model", &settings)
            .unwrap();
        assert_eq!(prepared.body.pointer(pointer).unwrap(), "high");
        let mut hosted = body;
        hosted["tools"] = if protocol == ProviderProtocol::GoogleGemini {
            json!([{"googleSearch":{}}])
        } else {
            json!([{"type":"web_search_20250305","name":"web_search"}])
        };
        assert!(PreparedRequest::parse(hosted, protocol).is_err());
    }
}
