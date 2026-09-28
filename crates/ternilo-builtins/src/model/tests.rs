use super::*;
use protocols::openai_chat::{decode_chat_completion, message_value};
use protocols::openai_responses::decode_responses_completion;
use protocols::responses_wire::responses_input;
use serde_json::json;
use ternilo_protocol::{MessageRole, ModelMessage, ToolSpec};
use ternilo_protocol::{ModelFinishReason, ModelRetryFailure, ModelUsage, ToolCall};
use transport::{decode_stream_event, take_sse_event};

#[derive(Clone, Debug, Eq, PartialEq)]
enum RetryObservation {
    Scheduled {
        retry: u32,
        maximum: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    },
    Started(u32),
    Cancelled(u32),
}

#[derive(Default)]
struct RecordingOutput {
    observations: tokio::sync::Mutex<Vec<RetryObservation>>,
}

impl ModelOutput for RecordingOutput {
    fn emit<'a>(
        &'a self,
        _: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        maximum: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.observations
                .lock()
                .await
                .push(RetryObservation::Scheduled {
                    retry,
                    maximum,
                    delay_ms,
                    failure,
                });
            Ok(())
        })
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.observations
                .lock()
                .await
                .push(RetryObservation::Started(retry));
            Ok(())
        })
    }

    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.observations
                .lock()
                .await
                .push(RetryObservation::Cancelled(retry));
            Ok(())
        })
    }
}

fn retrying_model(delay_ms: u64) -> ProviderModel {
    ProviderModel {
        provider: "test-provider".to_owned(),
        endpoint: "http://127.0.0.1/unused".to_owned(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        model: "test".to_owned(),
        context_window: None,
        api_key_env: None,
        api_key_override: None,
        max_tokens: None,
        temperature: None,
        reasoning_effort: None,
        max_attempts: 3,
        retry_base_delay_ms: delay_ms,
        client: reqwest::Client::new(),
        environment: None,
        attachments: None,
        sessions: None,
    }
}

#[tokio::test]
async fn retry_wait_publishes_started_and_cancelled_lifecycles() {
    let model = retrying_model(1);
    let started = RecordingOutput::default();
    model
        .wait_for_retry(
            &started,
            1,
            ModelRetryFailure {
                message: "busy".to_owned(),
                code: Some("http_503".to_owned()),
            },
            &RunCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *started.observations.lock().await,
        vec![
            RetryObservation::Scheduled {
                retry: 1,
                maximum: 2,
                delay_ms: 1,
                failure: ModelRetryFailure {
                    message: "busy".to_owned(),
                    code: Some("http_503".to_owned()),
                },
            },
            RetryObservation::Started(1),
        ]
    );

    let cancelled = RecordingOutput::default();
    let cancellation = RunCancellation::new();
    cancellation.cancel();
    let error = retrying_model(60_000)
        .wait_for_retry(
            &cancelled,
            2,
            ModelRetryFailure {
                message: "offline".to_owned(),
                code: Some("transport".to_owned()),
            },
            &cancellation,
        )
        .await
        .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(
        *cancelled.observations.lock().await,
        vec![
            RetryObservation::Scheduled {
                retry: 2,
                maximum: 2,
                delay_ms: 60_000_u64.saturating_mul(2),
                failure: ModelRetryFailure {
                    message: "offline".to_owned(),
                    code: Some("transport".to_owned()),
                },
            },
            RetryObservation::Cancelled(2),
        ]
    );
}

#[test]
fn streaming_tool_call_fragments_are_assembled_by_index() {
    let mut completion = StreamCompletion::default();
    let first = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read_","arguments":"{\"pa"}}]}}]}"#;
    let second = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"th\":\"README.md\"}"}}]}}]}"#;
    let usage = br#"data: {"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":96}}}"#;
    assert!(matches!(
        decode_stream_event(
            first,
            ProviderProtocol::OpenAiChatCompletions,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Deltas { .. }
    ));
    assert!(matches!(
        decode_stream_event(
            second,
            ProviderProtocol::OpenAiChatCompletions,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Deltas { .. }
    ));
    assert!(matches!(
        decode_stream_event(
            usage,
            ProviderProtocol::OpenAiChatCompletions,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Deltas { .. }
    ));
    let response = completion.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call-1");
    assert_eq!(response.tool_calls[0].name, "read_file");
    assert_eq!(
        response.tool_calls[0].arguments,
        json!({ "path": "README.md" })
    );
    assert_eq!(
        response.usage,
        Some(ModelUsage {
            input_tokens: 120,
            output_tokens: 8,
            cached_input_tokens: 96,
            cache_write_tokens: None,
            reasoning_tokens: 0,
        })
    );
}

#[test]
fn sse_parser_accepts_crlf_event_boundaries() {
    let mut buffer = b"data: one\r\n\r\ndata: two\n\nremaining".to_vec();
    assert_eq!(take_sse_event(&mut buffer).unwrap(), b"data: one");
    assert_eq!(take_sse_event(&mut buffer).unwrap(), b"data: two");
    assert_eq!(buffer, b"remaining");
}

#[test]
fn streaming_parser_accepts_null_collections_from_compatible_endpoints() {
    let mut completion = StreamCompletion::default();
    let role = br#"data: {"choices":[{"delta":{"role":"assistant","content":"","tool_calls":null}}],"usage":null}"#;
    let usage = br#"data: {"choices":null,"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":null}}"#;
    assert!(matches!(
        decode_stream_event(role, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(usage, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text.is_empty()
    ));
    assert_eq!(
        completion
            .finish("test-provider", "test-model")
            .unwrap()
            .usage,
        Some(ModelUsage {
            input_tokens: 10,
            output_tokens: 2,
            cached_input_tokens: 0,
            cache_write_tokens: None,
            reasoning_tokens: 0,
        })
    );
}

#[test]
fn streaming_parser_maps_deepseek_cache_usage() {
    let mut completion = StreamCompletion::default();
    let usage = br#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":8,"prompt_cache_hit_tokens":75,"prompt_cache_miss_tokens":25,"cache_creation_input_tokens":9}}"#;
    assert!(matches!(
        decode_stream_event(usage, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text.is_empty()
    ));
    let response = completion.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.provider, "test-provider");
    assert_eq!(response.model, "test-model");
    assert_eq!(response.finish_reason, ModelFinishReason::Stop);
    let usage = response.usage.unwrap();
    assert_eq!(usage.cached_input_tokens, 75);
    assert_eq!(usage.cache_write_tokens, Some(9));
}

#[test]
fn chat_stream_keeps_reasoning_separate_and_maps_reasoning_usage() {
    let mut completion = StreamCompletion::default();
    let empty = br#"data: {"choices":[{"delta":{"content":null,"reasoning_content":""}}]}"#;
    let reasoning =
        br#"data: {"choices":[{"delta":{"content":null,"reasoning_content":"think"}}]}"#;
    let compatible = br#"data: {"choices":[{"delta":{"content":null,"reasoning_content":null,"reasoning_details":[{"type":"reasoning.text","text":"ing"}]}}]}"#;
    let answer = br#"data: {"choices":[{"delta":{"content":"answer","reasoning_content":null}}]}"#;
    let usage = br#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":42,"completion_tokens_details":{"reasoning_tokens":24}}}"#;

    assert!(matches!(
        decode_stream_event(empty, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(reasoning, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning == "think" && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(compatible, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning == "ing" && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(answer, ProviderProtocol::OpenAiChatCompletions, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text == "answer"
    ));
    decode_stream_event(
        usage,
        ProviderProtocol::OpenAiChatCompletions,
        &mut completion,
    )
    .unwrap();

    let response = completion.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.reasoning_content.as_deref(), Some("thinking"));
    assert_eq!(response.content, "answer");
    assert_eq!(response.usage.unwrap().reasoning_tokens, 24);
}

#[test]
fn chat_completion_and_history_preserve_reasoning_content() {
    let response = decode_chat_completion(
            br#"{"choices":[{"finish_reason":"length","message":{"content":"answer","reasoning":"private","tool_calls":null}}],"usage":{"prompt_tokens":3,"completion_tokens":5,"prompt_tokens_details":{"cache_write_tokens":2},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
            "test-provider",
            "test-model",
        )
        .unwrap();
    assert_eq!(response.provider, "test-provider");
    assert_eq!(response.model, "test-model");
    assert_eq!(response.finish_reason, ModelFinishReason::MaxTokens);
    assert_eq!(response.reasoning_content.as_deref(), Some("private"));
    let usage = response.usage.unwrap();
    assert_eq!(usage.reasoning_tokens, 2);
    assert_eq!(usage.cache_write_tokens, Some(2));

    let value = message_value(&ModelMessage {
        role: MessageRole::Assistant,
        content: String::new(),
        reasoning_content: Some("preserve me".into()),
        provider_state: None,
        attachments: Vec::new(),
        tool_call_id: None,
        tool_calls: vec![ToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: json!({ "path": "README.md" }),
            presentation: None,
        }],
    });
    assert_eq!(value["content"], "");
    assert_eq!(value["reasoning_content"], "preserve me");
    assert_eq!(value["tool_calls"][0]["id"], "call-1");
}

#[test]
fn responses_request_uses_native_messages_tools_and_reasoning_shape() {
    let model = ProviderModel {
        provider: "test-provider".to_owned(),
        endpoint: "https://api.example/v1/responses".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        model: "reasoning-model".to_owned(),
        context_window: None,
        api_key_env: None,
        api_key_override: None,
        max_tokens: Some(8_192),
        temperature: None,
        reasoning_effort: Some("ultra".to_owned()),
        max_attempts: 1,
        retry_base_delay_ms: 1,
        client: reqwest::Client::new(),
        environment: None,
        attachments: None,
        sessions: None,
    };
    let body = protocols::request_body(
        &model,
        &ModelRequest {
            run_id: ternilo_protocol::RunId::new("model-test-run"),
            system_prompt: "system".to_owned(),
            messages: vec![ModelMessage {
                role: MessageRole::User,
                content: "hello".to_owned(),
                reasoning_content: None,
                provider_state: None,
                attachments: Vec::new(),
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
            tools: vec![ToolSpec {
                name: "read_file".to_owned(),
                description: "Read a file".to_owned(),
                input_schema: json!({ "type": "object" }),
            }],
            step: 1,
        },
    )
    .unwrap();
    assert_eq!(body["instructions"], "system");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["input"][0]["content"][0]["text"], "hello");
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert!(body["tools"][0].get("function").is_none());
    assert_eq!(body["reasoning"]["effort"], "ultra");
    assert_eq!(body["reasoning"]["summary"], "auto");
    assert_eq!(body["max_output_tokens"], 8_192);
    assert_eq!(body["store"], false);
}

#[test]
fn image_only_user_messages_do_not_emit_empty_text_parts() {
    let message = ModelMessage {
        role: MessageRole::User,
        content: String::new(),
        reasoning_content: None,
        provider_state: None,
        attachments: vec![ternilo_protocol::Attachment {
            name: "clipboard.png".to_owned(),
            media_type: "image/png".to_owned(),
            content: "data:image/png;base64,AA==".to_owned(),
        }],
        tool_call_id: None,
        tool_calls: Vec::new(),
    };
    let chat = message_value(&message);
    assert_eq!(chat["content"].as_array().unwrap().len(), 1);
    assert_eq!(chat["content"][0]["type"], "image_url");

    let responses = responses_input(&ModelRequest {
        run_id: ternilo_protocol::RunId::new("model-test-run"),
        system_prompt: String::new(),
        messages: vec![message],
        tools: Vec::new(),
        step: 1,
    });
    assert_eq!(responses[0]["content"].as_array().unwrap().len(), 1);
    assert_eq!(responses[0]["content"][0]["type"], "input_image");
}

#[test]
fn responses_stream_assembles_text_tools_usage_and_cache() {
    let mut completion = StreamCompletion::default();
    let text = br#"data: {"type":"response.output_text.delta","delta":"hello"}"#;
    let added = br#"data: {"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call-1","name":"read_file","arguments":""}}"#;
    let arguments = br#"data: {"type":"response.function_call_arguments.done","output_index":1,"arguments":"{\"path\":\"README.md\"}"}"#;
    let completed = br#"data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"hello"}]},{"type":"function_call","call_id":"call-1","name":"read_file","arguments":"{\"path\":\"README.md\"}"}],"usage":{"input_tokens":120,"output_tokens":8,"input_tokens_details":{"cached_tokens":96,"cache_write_tokens":4}}}}"#;
    assert!(matches!(
        decode_stream_event(text, ProviderProtocol::OpenAiResponses, &mut completion)
            .unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning.is_empty() && text == "hello"
    ));
    assert!(matches!(
        decode_stream_event(added, ProviderProtocol::OpenAiResponses, &mut completion).unwrap(),
        StreamEvent::Metadata
    ));
    assert!(matches!(
        decode_stream_event(
            arguments,
            ProviderProtocol::OpenAiResponses,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Metadata
    ));
    assert!(matches!(
        decode_stream_event(
            completed,
            ProviderProtocol::OpenAiResponses,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Done
    ));
    let response = completion.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.content, "hello");
    assert_eq!(response.tool_calls[0].id, "call-1");
    assert_eq!(response.tool_calls[0].name, "read_file");
    assert_eq!(
        response.tool_calls[0].arguments,
        json!({ "path": "README.md" })
    );
    assert_eq!(
        response.usage,
        Some(ModelUsage {
            input_tokens: 120,
            output_tokens: 8,
            cached_input_tokens: 96,
            cache_write_tokens: Some(4),
            reasoning_tokens: 0,
        })
    );
    assert_eq!(response.finish_reason, ModelFinishReason::ToolCalls);
}

#[test]
fn responses_incomplete_at_max_tokens_is_a_partial_success() {
    let response = decode_responses_completion(
            br#"{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"partial answer"}]}],"usage":{"input_tokens":20,"output_tokens":7,"input_tokens_details":{"cached_tokens":5,"cache_write_tokens":3}}}"#,
            "test-provider",
            "test-model",
        )
        .unwrap();
    assert_eq!(response.content, "partial answer");
    assert_eq!(response.finish_reason, ModelFinishReason::MaxTokens);
    assert_eq!(response.usage.unwrap().cache_write_tokens, Some(3));

    let mut streaming = StreamCompletion::default();
    let event = br#"data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"partial answer"}]}],"usage":{"input_tokens":20,"output_tokens":7,"input_tokens_details":{"cache_write_tokens":3}}}}"#;
    assert!(matches!(
        decode_stream_event(event, ProviderProtocol::OpenAiResponses, &mut streaming).unwrap(),
        StreamEvent::Done
    ));
    let response = streaming.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.finish_reason, ModelFinishReason::MaxTokens);
    assert_eq!(response.usage.unwrap().cache_write_tokens, Some(3));
}

#[test]
fn responses_stream_and_completion_preserve_summary_and_reasoning_usage() {
    let mut completion = StreamCompletion::default();
    let first = br#"data: {"type":"response.reasoning_summary_text.delta","delta":"inspect "}"#;
    let second = br#"data: {"type":"response.reasoning_summary_text.delta","delta":"state"}"#;
    let completed = br#"data: {"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"inspect state"}]},{"type":"message","content":[{"type":"output_text","text":"done"}]}],"usage":{"input_tokens":21,"output_tokens":13,"input_tokens_details":{"cached_tokens":8},"output_tokens_details":{"reasoning_tokens":7}}}}"#;

    assert!(matches!(
        decode_stream_event(first, ProviderProtocol::OpenAiResponses, &mut completion).unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning == "inspect " && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(second, ProviderProtocol::OpenAiResponses, &mut completion).unwrap(),
        StreamEvent::Deltas { reasoning, text } if reasoning == "state" && text.is_empty()
    ));
    assert!(matches!(
        decode_stream_event(
            completed,
            ProviderProtocol::OpenAiResponses,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Done
    ));
    let response = completion.finish("test-provider", "test-model").unwrap();
    assert_eq!(response.reasoning_content.as_deref(), Some("inspect state"));
    assert_eq!(response.usage.unwrap().reasoning_tokens, 7);

    let response = decode_responses_completion(
            br#"{"status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"summary one"},{"type":"summary_text","text":"summary two"}]},{"type":"message","content":[{"type":"output_text","text":"answer"}]}],"usage":{"input_tokens":5,"output_tokens":4,"input_tokens_details":null,"output_tokens_details":{"reasoning_tokens":3}}}"#,
            "test-provider",
            "test-model",
        )
        .unwrap();
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some("summary one\n\nsummary two")
    );
    assert_eq!(response.content, "answer");
    assert_eq!(response.usage.unwrap().reasoning_tokens, 3);
}
