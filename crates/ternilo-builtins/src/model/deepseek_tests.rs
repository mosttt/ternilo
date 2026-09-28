use super::*;
use protocols::{decode_completion, responses_wire::responses_input};
use serde_json::{Value, json};
use ternilo_protocol::{MessageRole, ModelMessage};
use ternilo_protocol::{ModelFinishReason, ToolCall};
use transport::decode_stream_event;

fn stream(event: &Value, completion: &mut StreamCompletion) -> StreamEvent {
    decode_stream_event(
        format!("data: {event}").as_bytes(),
        ProviderProtocol::DeepSeekResponses,
        completion,
    )
    .unwrap()
}

#[test]
fn deepseek_reasoning_stream_survives_all_repeated_completion_frames() {
    let mut completion = StreamCompletion::default();
    let mut emitted = String::new();
    for event in [
        json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":"Inspect "}),
        json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":"the workspace."}),
        json!({"type":"response.reasoning_text.done","output_index":0,"content_index":0,"text":"Inspect the workspace."}),
        json!({"type":"response.content_part.done","output_index":0,"content_index":0,"part":{"type":"reasoning_text","text":"Inspect the workspace."}}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","content":[{"type":"reasoning_text","text":"Inspect the workspace."}],"summary":[]}}),
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Done."}),
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning","content":[{"type":"reasoning_text","text":"Inspect the workspace."}],"summary":[]}],"usage":{"input_tokens":12,"output_tokens":9,"output_tokens_details":{"reasoning_tokens":6}}}}),
    ] {
        if let StreamEvent::Deltas { reasoning, .. } = stream(&event, &mut completion) {
            emitted.push_str(&reasoning);
        }
    }
    assert_eq!(emitted, "Inspect the workspace.");
    let response = completion.finish("deepseek", "deepseek-flash").unwrap();
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some(emitted.as_str())
    );
    assert_eq!(response.content, "Done.");
    assert_eq!(response.usage.unwrap().reasoning_tokens, 6);
}

#[test]
fn deepseek_done_only_and_separate_reasoning_parts_are_not_lost_or_repeated() {
    let mut completion = StreamCompletion::default();
    let mut emitted = String::new();
    for (item, part, text) in [
        (0, 0, "First"),
        (0, 1, "Second"),
        (2, 0, "Third"),
        (2, 0, "Third"),
    ] {
        if let StreamEvent::Deltas { reasoning, .. } = stream(
            &json!({
                "type":"response.reasoning_text.done", "output_index":item,"content_index":part,"text":text,
            }),
            &mut completion,
        ) {
            emitted.push_str(&reasoning);
        }
    }
    assert_eq!(emitted, "First\n\nSecond\n\nThird");
    assert_eq!(
        completion
            .finish("deepseek", "model")
            .unwrap()
            .reasoning_content
            .as_deref(),
        Some(emitted.as_str())
    );
}

#[test]
fn deepseek_final_reasoning_and_truncation_are_preserved_without_changing_standard_openai() {
    let value = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[
        {"type":"reasoning","content":[{"type":"reasoning_text","text":"Visible reasoning."}],"summary":[]},
        {"type":"message","content":[{"type":"output_text","text":"Partial answer."}]}
    ],"usage":{"input_tokens":10,"output_tokens":8,"output_tokens_details":{"reasoning_tokens":6}}});
    let bytes = serde_json::to_vec(&value).unwrap();
    let response = decode_completion(
        &bytes,
        ProviderProtocol::DeepSeekResponses,
        "deepseek",
        "model",
    )
    .unwrap();
    assert_eq!(
        response.reasoning_content.as_deref(),
        Some("Visible reasoning.")
    );
    assert_eq!(response.content, "Partial answer.");
    assert_eq!(response.finish_reason, ModelFinishReason::MaxTokens);
    let standard =
        decode_completion(&bytes, ProviderProtocol::OpenAiResponses, "openai", "model").unwrap();
    assert!(
        standard.reasoning_content.is_none(),
        "standard OpenAI only interprets its summary representation"
    );
    let mut completion = StreamCompletion::default();
    assert!(matches!(
        decode_stream_event(
            br#"data: {"type":"response.reasoning_text.delta","delta":"provider extension"}"#,
            ProviderProtocol::OpenAiResponses,
            &mut completion
        )
        .unwrap(),
        StreamEvent::Metadata
    ));
    assert!(completion.reasoning_content.is_empty());
}

#[test]
fn deepseek_tool_history_passes_back_full_reasoning_only_for_the_selected_adapter() {
    let request = ModelRequest {
        run_id: ternilo_protocol::RunId::new("model-test-run"),
        step: 1,
        system_prompt: String::new(),
        messages: vec![
            ModelMessage {
                role: MessageRole::Assistant,
                content: "Reading the file.".to_owned(),
                reasoning_content: Some("The path must be inspected.".to_owned()),
                provider_state: None,
                tool_call_id: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".to_owned(),
                    name: "read_file".to_owned(),
                    arguments: json!({"path":"README.md"}),
                    presentation: None,
                }],
                attachments: Vec::new(),
            },
            ModelMessage {
                role: MessageRole::Tool,
                content: "File content".to_owned(),
                reasoning_content: None,
                provider_state: None,
                tool_call_id: Some("call-1".to_owned()),
                tool_calls: Vec::new(),
                attachments: Vec::new(),
            },
        ],
        tools: Vec::new(),
    };
    let deepseek = protocols::deepseek_responses::request_input(&request);
    assert_eq!(
        deepseek[0],
        json!({"type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"The path must be inspected."}]})
    );
    assert_eq!(deepseek[1]["role"], "assistant");
    assert_eq!(deepseek[2]["call_id"], "call-1");
    assert_eq!(deepseek[3]["type"], "function_call_output");
    assert_eq!(&deepseek[1..], responses_input(&request).as_slice());
}

#[test]
fn deepseek_keeps_partial_reasoning_when_terminal_frames_omit_text() {
    let mut completion = StreamCompletion::default();
    for event in [
        json!({"type":"response.reasoning_text.delta","output_index":0,"content_index":0,"delta":"Partial reasoning."}),
        json!({"type":"response.reasoning_text.done","output_index":0,"content_index":0}),
        json!({"type":"response.completed","response":{"status":"completed","output":[]}}),
    ] {
        stream(&event, &mut completion);
    }
    assert_eq!(
        completion
            .finish("deepseek", "model")
            .unwrap()
            .reasoning_content
            .as_deref(),
        Some("Partial reasoning.")
    );
}

#[test]
fn deepseek_does_not_treat_openai_summaries_as_full_reasoning_history() {
    let mut completion = StreamCompletion::default();
    for event in [
        json!({"type":"response.reasoning_summary_text.delta","delta":"Summary only."}),
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"Summary only."}]}]}}),
    ] {
        stream(&event, &mut completion);
    }
    assert!(
        completion
            .finish("deepseek", "model")
            .unwrap()
            .reasoning_content
            .is_none()
    );
}
