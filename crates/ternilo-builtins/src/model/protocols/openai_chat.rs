//! Chat Completions messages, streams and usage.

use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_protocol::{
    HarnessError, MessageRole, ModelFinishReason, ModelMessage, ModelRequest, ModelResponse,
    ModelUsage, ToolCall, ToolSpec,
};

use super::super::{StreamCompletion, StreamEvent};

pub(super) fn decode_chat_stream_data(
    data: Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    let chunk: StreamingResponse = serde_json::from_value(data)
        .map_err(|error| HarnessError::execution(format!("parse model stream event: {error}")))?;
    if let Some(usage) = chunk.usage {
        completion.usage = Some(usage.into());
    }
    let mut visible = String::new();
    let mut reasoning = String::new();
    for choice in chunk.choices {
        if let Some(reason) = choice.finish_reason.as_deref() {
            completion.finish_reason = Some(parse_finish_reason(reason)?);
        }
        let delta = choice.delta.reasoning_text();
        if !delta.is_empty() {
            completion.reasoning_content.push_str(&delta);
            reasoning.push_str(&delta);
        }
        if let Some(content) = choice.delta.content {
            completion.content.push_str(&content);
            visible.push_str(&content);
        }
        for tool in choice.delta.tool_calls {
            let pending = completion.tool_calls.entry(tool.index).or_default();
            if let Some(id) = tool.id {
                pending.id.push_str(&id);
            }
            if let Some(function) = tool.function {
                if let Some(name) = function.name {
                    pending.name.push_str(&name);
                }
                if let Some(arguments) = function.arguments {
                    pending.arguments.push_str(&arguments);
                }
            }
        }
    }
    Ok(StreamEvent::Deltas {
        reasoning,
        text: visible,
    })
}

fn parse_finish_reason(reason: &str) -> Result<ModelFinishReason, HarnessError> {
    match reason {
        "stop" => Ok(ModelFinishReason::Stop),
        "tool_calls" | "function_call" => Ok(ModelFinishReason::ToolCalls),
        "length" | "max_tokens" | "max_output_tokens" => Ok(ModelFinishReason::MaxTokens),
        other => Err(HarnessError::execution(format!(
            "unsupported model finish reason {other:?}"
        ))),
    }
}

pub(in crate::model) fn decode_chat_completion(
    bytes: &[u8],
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let completion: CompletionResponse = serde_json::from_slice(bytes)
        .map_err(|error| HarnessError::execution(format!("parse model response: {error}")))?;
    let choice = completion
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| HarnessError::execution("model response contains no choices"))?;
    let finish_reason = choice
        .finish_reason
        .as_deref()
        .map(parse_finish_reason)
        .transpose()?;
    let message = choice.message;
    let reasoning_content = {
        let reasoning = message.reasoning_text();
        (!reasoning.is_empty()).then_some(reasoning)
    };
    let content = message.content.unwrap_or_default();
    let tool_calls = message
        .tool_calls
        .into_iter()
        .map(|call| {
            let arguments = serde_json::from_str(&call.function.arguments).map_err(|error| {
                HarnessError::execution(format!(
                    "model tool arguments for {:?} are not JSON: {error}",
                    call.function.name
                ))
            })?;
            Ok(ToolCall {
                id: call.id,
                name: call.function.name,
                arguments,
                presentation: None,
            })
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
    Ok(ModelResponse {
        provider: provider.to_owned(),
        model: model.to_owned(),
        content,
        reasoning_content,
        provider_state: None,
        tool_calls,
        usage: completion.usage.map(Into::into),
        finish_reason: finish_reason.unwrap_or(ModelFinishReason::Stop),
        provider_request_id: None,
        attempts: 1,
        request_digest: None,
        replayed: false,
    })
}

pub(in crate::model) fn message_value(message: &ModelMessage) -> Value {
    match message.role {
        MessageRole::User => {
            if message.attachments.is_empty() {
                json!({ "role": "user", "content": message.content })
            } else {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": message.content,
                    }));
                }
                content.extend(message.attachments.iter().map(|attachment| {
                    if attachment.media_type.starts_with("image/") {
                        json!({
                            "type": "image_url",
                            "image_url": { "url": attachment.content },
                        })
                    } else {
                        json!({
                            "type": "text",
                            "text": format!(
                                "<attachment name={:?} media_type={:?}>\n{}\n</attachment>",
                                attachment.name, attachment.media_type, attachment.content
                            ),
                        })
                    }
                }));
                json!({ "role": "user", "content": content })
            }
        }
        MessageRole::Tool => json!({
            "role": "tool",
            "content": message.content,
            "tool_call_id": message.tool_call_id,
        }),
        MessageRole::Assistant => {
            let mut value = json!({
                "role": "assistant",
                "content": message.content,
            });
            if let Some(reasoning) = message
                .reasoning_content
                .as_ref()
                .filter(|reasoning| !reasoning.is_empty())
            {
                value["reasoning_content"] = json!(reasoning);
            }
            if !message.tool_calls.is_empty() {
                value["tool_calls"] = Value::Array(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments.to_string(),
                                }
                            })
                        })
                        .collect(),
                );
            }
            value
        }
    }
}

pub(super) fn tool_value(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}
fn reasoning_value_text(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| reasoning_value_text(Some(part)))
            .collect(),
        Value::Object(fields) => fields
            .get("text")
            .or_else(|| fields.get("content"))
            .or_else(|| fields.get("summary"))
            .map(|part| reasoning_value_text(Some(part)))
            .unwrap_or_default(),
        Value::Null | Value::Bool(_) | Value::Number(_) => String::new(),
    }
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<CompletionChoice>,
    usage: Option<ProviderUsage>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: CompletionMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct CompletionMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<Value>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    thinking: Option<Value>,
    #[serde(default)]
    reasoning_text: Option<Value>,
    #[serde(default)]
    reasoning_details: Option<Value>,
    #[serde(default, deserialize_with = "null_default")]
    tool_calls: Vec<CompletionToolCall>,
}

impl CompletionMessage {
    fn reasoning_text(&self) -> String {
        [
            self.reasoning_content.as_ref(),
            self.reasoning.as_ref(),
            self.thinking.as_ref(),
            self.reasoning_text.as_ref(),
            self.reasoning_details.as_ref(),
        ]
        .into_iter()
        .map(reasoning_value_text)
        .find(|text| !text.is_empty())
        .unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct CompletionToolCall {
    id: String,
    function: CompletionFunction,
}

#[derive(Deserialize)]
struct CompletionFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct StreamingResponse {
    #[serde(default, deserialize_with = "null_default")]
    choices: Vec<StreamingChoice>,
    usage: Option<ProviderUsage>,
}

#[derive(Deserialize)]
struct ProviderUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    prompt_tokens_details: Option<PromptTokenDetails>,
    completion_tokens_details: Option<CompletionTokenDetails>,
    #[serde(default)]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct CompletionTokenDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

impl From<ProviderUsage> for ModelUsage {
    fn from(value: ProviderUsage) -> Self {
        let cached_input_tokens = value
            .prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .or(value.prompt_cache_hit_tokens)
            .unwrap_or(0);
        let cache_write_tokens = value
            .prompt_tokens_details
            .and_then(|details| details.cache_write_tokens)
            .or(value.cache_write_tokens)
            .or(value.cache_creation_input_tokens);
        Self {
            input_tokens: value.prompt_tokens,
            output_tokens: value.completion_tokens,
            cached_input_tokens,
            cache_write_tokens,
            reasoning_tokens: value
                .completion_tokens_details
                .map_or(0, |details| details.reasoning_tokens),
        }
    }
}

#[derive(Deserialize)]
struct StreamingChoice {
    delta: StreamingDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct StreamingDelta {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<Value>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    thinking: Option<Value>,
    #[serde(default)]
    reasoning_text: Option<Value>,
    #[serde(default)]
    reasoning_details: Option<Value>,
    #[serde(default, deserialize_with = "null_default")]
    tool_calls: Vec<StreamingToolCall>,
}

impl StreamingDelta {
    fn reasoning_text(&self) -> String {
        [
            self.reasoning_content.as_ref(),
            self.reasoning.as_ref(),
            self.thinking.as_ref(),
            self.reasoning_text.as_ref(),
            self.reasoning_details.as_ref(),
        ]
        .into_iter()
        .map(reasoning_value_text)
        .find(|text| !text.is_empty())
        .unwrap_or_default()
    }
}

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Deserialize)]
struct StreamingToolCall {
    index: usize,
    id: Option<String>,
    function: Option<StreamingFunction>,
}

#[derive(Deserialize)]
struct StreamingFunction {
    name: Option<String>,
    arguments: Option<String>,
}

pub(super) fn request_body(model: &super::super::ProviderModel, request: &ModelRequest) -> Value {
    let mut messages = vec![json!({
        "role": "system",
        "content": request.system_prompt,
    })];
    messages.extend(request.messages.iter().map(message_value));
    let tools = request.tools.iter().map(tool_value).collect::<Vec<_>>();
    let mut body = json!({
        "model": model.model,
        "messages": messages,
        "tools": tools,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if request.tools.is_empty() {
        body.as_object_mut()
            .expect("completion body is an object")
            .remove("tools");
    }
    if let Some(max_tokens) = model.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(temperature) = model.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(reasoning_effort) = &model.reasoning_effort {
        body["reasoning_effort"] = json!(reasoning_effort);
    }
    body
}
