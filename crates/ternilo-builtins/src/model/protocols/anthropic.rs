use serde_json::{Value, json};
use ternilo_protocol::{
    HarnessError, MessageRole, ModelFinishReason, ModelRequest, ModelResponse, ModelUsage,
    ProviderProtocol,
};

use super::{
    super::{ProviderModel, StreamCompletion, StreamEvent},
    native,
};

pub(super) fn request_body(
    model: &ProviderModel,
    request: &ModelRequest,
) -> Result<Value, HarnessError> {
    let mut messages = Vec::new();
    for message in &request.messages {
        let role = if message.role == MessageRole::Assistant {
            "assistant"
        } else {
            "user"
        };
        let mut blocks = Vec::new();
        match message.role {
            MessageRole::Assistant => {
                if let Some(saved) = native::replay_blocks(message, ProviderProtocol::AnthropicMessages, &model.model) {
                    blocks = saved;
                } else {
                    if !message.content.is_empty() { blocks.push(json!({"type": "text", "text": message.content})); }
                    blocks.extend(message.tool_calls.iter().map(|call| json!({"type": "tool_use", "id": call.id, "name": call.name, "input": call.arguments})));
                }
            }
            MessageRole::User => {
                if !message.content.is_empty() { blocks.push(json!({"type": "text", "text": message.content})); }
                for attachment in &message.attachments { blocks.push(native::attachment_value(attachment, model.protocol)?); }
            }
            MessageRole::Tool => blocks.push(json!({
                "type": "tool_result",
                "tool_use_id": message.tool_call_id.as_deref().ok_or_else(|| HarnessError::invalid("Claude tool result is missing its call id"))?,
                "content": message.content,
            })),
        }
        native::append_message(&mut messages, role, "content", blocks);
    }
    let mut body = json!({"model": model.model, "messages": messages, "max_tokens": model.max_tokens.unwrap_or(4096), "stream": true});
    if !request.system_prompt.is_empty() {
        body["system"] = json!(request.system_prompt);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(|tool| json!({"name": tool.name, "description": tool.description, "input_schema": tool.input_schema})).collect::<Vec<_>>());
    }
    if let Some(temperature) = model.temperature {
        body["temperature"] = json!(temperature);
    }
    crate::apply_native_reasoning(
        &mut body,
        model.protocol,
        model.reasoning_effort.as_deref(),
        u64::from(model.max_tokens.unwrap_or(4096)),
    )?;
    Ok(body)
}

fn ingest_usage(value: &Value, completion: &mut StreamCompletion) {
    let Some(usage) = value.get("usage") else {
        return;
    };
    let current = completion.usage.get_or_insert(ModelUsage {
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: 0,
        cache_write_tokens: None,
        reasoning_tokens: 0,
    });
    if let Some(input) = usage["input_tokens"].as_u64() {
        current.cached_input_tokens = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
        current.cache_write_tokens = usage["cache_creation_input_tokens"].as_u64();
        current.input_tokens = input
            .saturating_add(current.cached_input_tokens)
            .saturating_add(current.cache_write_tokens.unwrap_or(0));
    }
    if let Some(output) = usage["output_tokens"].as_u64() {
        current.output_tokens = output;
    }
}

fn finish_reason(value: &Value, completion: &mut StreamCompletion) -> Result<(), HarnessError> {
    if let Some(reason) = value["stop_reason"].as_str() {
        completion.finish_reason = Some(match reason {
            "end_turn" | "stop_sequence" | "refusal" => ModelFinishReason::Stop,
            "tool_use" => ModelFinishReason::ToolCalls,
            "max_tokens" | "model_context_window_exceeded" => ModelFinishReason::MaxTokens,
            other => {
                return Err(HarnessError::execution(format!(
                    "Claude generation stopped: {other}"
                )));
            }
        });
    }
    Ok(())
}

fn start_block(index: usize, block: &Value, completion: &mut StreamCompletion) -> StreamEvent {
    completion.native_blocks.insert(index, block.clone());
    let mut text = String::new();
    let mut reasoning = String::new();
    match block["type"].as_str() {
        Some("text") => block["text"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut text),
        Some("thinking") => block["thinking"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut reasoning),
        Some("tool_use") => {
            let pending = completion.tool_calls.entry(index).or_default();
            block["id"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut pending.id);
            block["name"]
                .as_str()
                .unwrap_or_default()
                .clone_into(&mut pending.name);
        }
        _ => {}
    }
    completion.content.push_str(&text);
    completion.reasoning_content.push_str(&reasoning);
    StreamEvent::Deltas { reasoning, text }
}

fn stop_block(index: usize, completion: &mut StreamCompletion) -> Result<(), HarnessError> {
    if let Some(pending) = completion.tool_calls.get_mut(&index) {
        if pending.arguments.is_empty() {
            pending.arguments = completion.native_blocks[&index]
                .get("input")
                .cloned()
                .unwrap_or_else(|| json!({}))
                .to_string();
        }
        let arguments: Value = serde_json::from_str(&pending.arguments).map_err(|error| {
            HarnessError::execution(format!("Claude returned invalid tool JSON: {error}"))
        })?;
        completion
            .native_blocks
            .get_mut(&index)
            .expect("started content block")["input"] = arguments;
    }
    Ok(())
}

pub(super) fn decode_stream_data(
    value: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    native::check_error(value)?;
    completion.native_protocol = Some(ProviderProtocol::AnthropicMessages);
    match value["type"].as_str().unwrap_or_default() {
        "message_start" => ingest_usage(&value["message"], completion),
        "message_delta" => {
            ingest_usage(value, completion);
            finish_reason(&value["delta"], completion)?;
        }
        "message_stop" => {
            if completion.finish_reason.is_none() {
                return Err(HarnessError::execution(
                    "Claude stream omitted its stop reason",
                ));
            }
            return Ok(StreamEvent::Done);
        }
        "content_block_start" | "content_block_delta" | "content_block_stop" => {
            let index = value["index"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| HarnessError::execution("Claude content event omitted its index"))?;
            match value["type"].as_str() {
                Some("content_block_start") => {
                    return Ok(start_block(index, &value["content_block"], completion));
                }
                Some("content_block_stop") => stop_block(index, completion)?,
                _ => return append_delta(index, &value["delta"], completion),
            }
        }
        _ => {}
    }
    Ok(StreamEvent::Metadata)
}

fn append_delta(
    index: usize,
    delta: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    let block = completion
        .native_blocks
        .get_mut(&index)
        .ok_or_else(|| HarnessError::execution("Claude delta arrived before its content block"))?;
    let (field, visible, thinking) = match delta["type"].as_str() {
        Some("text_delta") => ("text", true, false),
        Some("thinking_delta") => ("thinking", false, true),
        Some("signature_delta") => ("signature", false, false),
        Some("input_json_delta") => {
            let pending = completion.tool_calls.get_mut(&index).ok_or_else(|| {
                HarnessError::execution("Claude tool arguments arrived without a tool call")
            })?;
            pending
                .arguments
                .push_str(delta["partial_json"].as_str().unwrap_or_default());
            return Ok(StreamEvent::Metadata);
        }
        _ => return Ok(StreamEvent::Metadata),
    };
    let text = delta[field].as_str().unwrap_or_default();
    block[field] = json!(format!(
        "{}{text}",
        block[field].as_str().unwrap_or_default()
    ));
    if visible {
        completion.content.push_str(text);
    }
    if thinking {
        completion.reasoning_content.push_str(text);
    }
    Ok(StreamEvent::Deltas {
        reasoning: if thinking {
            text.to_owned()
        } else {
            String::new()
        },
        text: if visible {
            text.to_owned()
        } else {
            String::new()
        },
    })
}

pub(super) fn decode_completion(
    bytes: &[u8],
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let value = native::read_json(bytes)?;
    let mut completion = StreamCompletion {
        native_protocol: Some(ProviderProtocol::AnthropicMessages),
        ..StreamCompletion::default()
    };
    let blocks = value["content"]
        .as_array()
        .ok_or_else(|| HarnessError::execution("Claude response omitted content blocks"))?;
    for (index, block) in blocks.iter().enumerate() {
        start_block(index, block, &mut completion);
        stop_block(index, &mut completion)?;
    }
    ingest_usage(&value, &mut completion);
    finish_reason(&value, &mut completion)?;
    if completion.finish_reason.is_none() {
        return Err(HarnessError::execution(
            "Claude response omitted its stop reason",
        ));
    }
    completion.finish(provider, model)
}
