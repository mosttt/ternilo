//! Shared Responses wire envelopes, messages, tools and usage; reasoning belongs to each adapter.

use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_protocol::{
    HarnessError, MessageRole, ModelFinishReason, ModelMessage, ModelRequest, ModelResponse,
    ModelUsage, ToolSpec,
};

use super::super::{StreamCompletion, StreamEvent};

pub(super) fn decode_responses_stream_data(
    event: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    match event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "response.output_text.delta" => {
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            completion.content.push_str(&delta);
            Ok(StreamEvent::Deltas {
                reasoning: String::new(),
                text: delta,
            })
        }
        "response.output_item.added" | "response.output_item.done" => {
            if let (Some(index), Some(item)) = (
                event.get("output_index").and_then(Value::as_u64),
                event.get("item"),
            ) {
                set_responses_tool_call(responses_output_index(index)?, item, completion);
            }
            Ok(StreamEvent::Metadata)
        }
        "response.function_call_arguments.delta" => {
            if let Some(index) = event.get("output_index").and_then(Value::as_u64) {
                let index = responses_output_index(index)?;
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                completion
                    .tool_calls
                    .entry(index)
                    .or_default()
                    .arguments
                    .push_str(delta);
            }
            Ok(StreamEvent::Metadata)
        }
        "response.function_call_arguments.done" => {
            if let Some(index) = event.get("output_index").and_then(Value::as_u64) {
                let index = responses_output_index(index)?;
                let arguments = event
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                arguments
                    .clone_into(&mut completion.tool_calls.entry(index).or_default().arguments);
            }
            Ok(StreamEvent::Metadata)
        }
        "response.completed" => {
            let response = event.get("response").unwrap_or(&Value::Null);
            ingest_responses_usage(response, completion)?;
            ingest_responses_tools(response, completion);
            Ok(StreamEvent::Done)
        }
        "response.incomplete" => {
            let response = event.get("response").unwrap_or(&Value::Null);
            if responses_reached_max_tokens(response) {
                ingest_responses_usage(response, completion)?;
                ingest_responses_tools(response, completion);
                completion.finish_reason = Some(ModelFinishReason::MaxTokens);
                Ok(StreamEvent::Done)
            } else {
                Err(HarnessError::execution(responses_error_message(response)))
            }
        }
        "response.failed" => {
            let response = event.get("response").unwrap_or(&Value::Null);
            Err(HarnessError::execution(responses_error_message(response)))
        }
        "error" => Err(HarnessError::execution(
            event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Responses API stream failed"),
        )),
        _ => Ok(StreamEvent::Metadata),
    }
}

fn set_responses_tool_call(index: usize, item: &Value, completion: &mut StreamCompletion) {
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let pending = completion.tool_calls.entry(index).or_default();
    if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
        call_id.clone_into(&mut pending.id);
    }
    if let Some(name) = item.get("name").and_then(Value::as_str) {
        name.clone_into(&mut pending.name);
    }
    if let Some(arguments) = item.get("arguments").and_then(Value::as_str)
        && !arguments.is_empty()
    {
        arguments.clone_into(&mut pending.arguments);
    }
}

pub(super) fn responses_output_index(index: u64) -> Result<usize, HarnessError> {
    usize::try_from(index)
        .map_err(|_| HarnessError::execution("Responses API output index exceeds platform limit"))
}

fn ingest_responses_tools(response: &Value, completion: &mut StreamCompletion) {
    for (index, item) in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        set_responses_tool_call(index, item, completion);
    }
}

fn ingest_responses_usage(
    response: &Value,
    completion: &mut StreamCompletion,
) -> Result<(), HarnessError> {
    if let Some(usage) = response.get("usage").filter(|value| !value.is_null()) {
        completion.usage = Some(
            serde_json::from_value::<ResponsesUsage>(usage.clone())
                .map_err(|error| {
                    HarnessError::execution(format!("parse Responses API usage: {error}"))
                })?
                .into(),
        );
    }
    Ok(())
}

fn responses_error_message(response: &Value) -> String {
    response
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| {
            response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
        })
        .unwrap_or("Responses API did not complete")
        .to_owned()
}

fn responses_reached_max_tokens(response: &Value) -> bool {
    matches!(
        response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str),
        Some("max_output_tokens" | "max_tokens")
    )
}

pub(super) fn decode_response_value(
    response: &Value,
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let status = response.get("status").and_then(Value::as_str);
    let max_tokens = status == Some("incomplete") && responses_reached_max_tokens(response);
    if status != Some("completed") && !max_tokens {
        return Err(HarnessError::execution(responses_error_message(response)));
    }
    let mut completion = StreamCompletion::default();
    for (index, item) in response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(Value::as_str) == Some("output_text")
                        && let Some(text) = part.get("text").and_then(Value::as_str)
                    {
                        completion.content.push_str(text);
                    }
                }
            }
            Some("function_call") => set_responses_tool_call(index, item, &mut completion),
            _ => {}
        }
    }
    ingest_responses_usage(response, &mut completion)?;
    if max_tokens {
        completion.finish_reason = Some(ModelFinishReason::MaxTokens);
    }
    completion.finish(provider, model)
}

pub(in crate::model) fn responses_input(request: &ModelRequest) -> Vec<Value> {
    request.messages.iter().flat_map(message_input).collect()
}

pub(super) fn message_input(message: &ModelMessage) -> Vec<Value> {
    let mut input = Vec::new();
    match message.role {
        MessageRole::User => {
            let mut content = Vec::new();
            if !message.content.is_empty() {
                content.push(json!({
                    "type": "input_text",
                    "text": message.content,
                }));
            }
            content.extend(message.attachments.iter().map(|attachment| {
                if attachment.media_type.starts_with("image/") {
                    json!({
                        "type": "input_image",
                        "image_url": attachment.content,
                    })
                } else {
                    json!({
                        "type": "input_text",
                        "text": format!(
                            "<attachment name={:?} media_type={:?}>\n{}\n</attachment>",
                            attachment.name, attachment.media_type, attachment.content
                        ),
                    })
                }
            }));
            input.push(json!({ "role": "user", "content": content }));
        }
        MessageRole::Assistant => {
            if !message.content.is_empty() {
                input.push(json!({
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": message.content }],
                }));
            }
            input.extend(message.tool_calls.iter().map(|call| {
                json!({
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": call.arguments.to_string(),
                })
            }));
        }
        MessageRole::Tool => input.push(json!({
            "type": "function_call_output",
            "call_id": message.tool_call_id,
            "output": message.content,
        })),
    }
    input
}

pub(super) fn responses_tool_value(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.input_schema,
    })
}

#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    input_tokens_details: Option<ResponsesInputTokenDetails>,
    output_tokens_details: Option<ResponsesOutputTokenDetails>,
}

#[derive(Deserialize)]
struct ResponsesInputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ResponsesOutputTokenDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

impl From<ResponsesUsage> for ModelUsage {
    fn from(value: ResponsesUsage) -> Self {
        let input_details = value.input_tokens_details;
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: input_details
                .as_ref()
                .map_or(0, |details| details.cached_tokens),
            cache_write_tokens: input_details.and_then(|details| details.cache_write_tokens),
            reasoning_tokens: value
                .output_tokens_details
                .map_or(0, |details| details.reasoning_tokens),
        }
    }
}

pub(super) fn request_body(
    model: &super::super::ProviderModel,
    request: &ModelRequest,
    input: &[Value],
) -> Value {
    let tools = request
        .tools
        .iter()
        .map(responses_tool_value)
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": model.model,
        "instructions": request.system_prompt,
        "input": input,
        "tools": tools,
        "stream": true,
        "store": false,
    });
    if request.tools.is_empty() {
        body.as_object_mut()
            .expect("response body is an object")
            .remove("tools");
    }
    if let Some(max_tokens) = model.max_tokens {
        body["max_output_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = model.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(reasoning_effort) = &model.reasoning_effort {
        body["reasoning"] = json!({ "effort": reasoning_effort });
    }
    body
}
