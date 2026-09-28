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
    let mut contents = Vec::new();
    for message in &request.messages {
        let role = if message.role == MessageRole::Assistant {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        match message.role {
            MessageRole::Assistant => {
                if let Some(blocks) =
                    native::replay_blocks(message, ProviderProtocol::GoogleGemini, &model.model)
                {
                    parts = blocks;
                } else {
                    if !message.content.is_empty() {
                        parts.push(json!({"text": message.content}));
                    }
                    parts.extend(message.tool_calls.iter().map(
                        |call| json!({"functionCall": {"name": call.name, "args": call.arguments}}),
                    ));
                }
            }
            MessageRole::User => {
                if !message.content.is_empty() {
                    parts.push(json!({"text": message.content}));
                }
                for attachment in &message.attachments {
                    let mut part =
                        native::attachment_value(attachment, ProviderProtocol::GoogleGemini)?;
                    part.as_object_mut()
                        .expect("attachment object")
                        .remove("type");
                    parts.push(part);
                }
            }
            MessageRole::Tool => {
                let id = message.tool_call_id.as_deref().ok_or_else(|| {
                    HarnessError::invalid("Gemini tool result is missing its call id")
                })?;
                let call = request
                    .messages
                    .iter()
                    .rev()
                    .flat_map(|item| &item.tool_calls)
                    .find(|call| call.id == id)
                    .ok_or_else(|| {
                        HarnessError::invalid("Gemini tool result has no matching function call")
                    })?;
                let mut result =
                    json!({"name": call.name, "response": {"output": message.content}});
                if !id.starts_with("gemini-call-") {
                    result["id"] = json!(id);
                }
                parts.push(json!({"functionResponse": result}));
            }
        }
        native::append_message(&mut contents, role, "parts", parts);
    }
    let mut body = json!({"contents": contents, "generationConfig": {"candidateCount": 1}});
    if !request.system_prompt.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": request.system_prompt}]});
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations": request.tools.iter().map(|tool| json!({
            "name": tool.name, "description": tool.description, "parametersJsonSchema": tool.input_schema,
        })).collect::<Vec<_>>()}]);
    }
    if let Some(max_tokens) = model.max_tokens {
        body["generationConfig"]["maxOutputTokens"] = json!(max_tokens);
    }
    if let Some(temperature) = model.temperature {
        body["generationConfig"]["temperature"] = json!(temperature);
    }
    crate::apply_native_reasoning(
        &mut body,
        model.protocol,
        model.reasoning_effort.as_deref(),
        u64::from(model.max_tokens.unwrap_or(4096)),
    )?;
    Ok(body)
}

pub(super) fn decode_stream_data(
    value: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    native::check_error(value)?;
    completion.native_protocol = Some(ProviderProtocol::GoogleGemini);
    if let Some(usage) = value.get("usageMetadata") {
        completion.usage = Some(ModelUsage {
            input_tokens: usage["promptTokenCount"].as_u64().unwrap_or(0),
            output_tokens: usage["candidatesTokenCount"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(usage["thoughtsTokenCount"].as_u64().unwrap_or(0)),
            cached_input_tokens: usage["cachedContentTokenCount"].as_u64().unwrap_or(0),
            cache_write_tokens: None,
            reasoning_tokens: usage["thoughtsTokenCount"].as_u64().unwrap_or(0),
        });
    }
    if let Some(reason) = value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(HarnessError::execution(format!(
            "Gemini blocked the prompt: {reason}"
        )));
    }
    let mut text = String::new();
    let mut reasoning = String::new();
    if let Some(candidate) = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        for part in candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            completion
                .native_blocks
                .insert(completion.native_blocks.len(), part.clone());
            if let Some(delta) = part.get("text").and_then(Value::as_str) {
                if part["thought"].as_bool() == Some(true) {
                    reasoning.push_str(delta);
                    completion.reasoning_content.push_str(delta);
                } else {
                    text.push_str(delta);
                    completion.content.push_str(delta);
                }
            }
            if let Some(call) = part.get("functionCall") {
                let index = completion.tool_calls.len();
                let pending = completion.tool_calls.entry(index).or_default();
                pending.id = call["id"]
                    .as_str()
                    .map_or_else(|| format!("gemini-call-{index}"), str::to_owned);
                call["name"]
                    .as_str()
                    .unwrap_or_default()
                    .clone_into(&mut pending.name);
                pending.arguments = call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string();
            }
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            completion.finish_reason = Some(match reason {
                "STOP" if !completion.tool_calls.is_empty() => ModelFinishReason::ToolCalls,
                "STOP" => ModelFinishReason::Stop,
                "MAX_TOKENS" => ModelFinishReason::MaxTokens,
                other => {
                    return Err(HarnessError::execution(format!(
                        "Gemini generation stopped: {other}"
                    )));
                }
            });
        }
    }
    Ok(StreamEvent::Deltas { reasoning, text })
}

pub(super) fn decode_completion(
    bytes: &[u8],
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let value = native::read_json(bytes)?;
    let mut completion = StreamCompletion::default();
    decode_stream_data(&value, &mut completion)?;
    if completion.finish_reason.is_none() {
        return Err(HarnessError::execution(
            "Gemini response omitted its finish reason",
        ));
    }
    completion.finish(provider, model)
}
