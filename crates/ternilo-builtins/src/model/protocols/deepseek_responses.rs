//! `DeepSeek` Responses carries full reasoning text, not `OpenAI` reasoning summaries.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use ternilo_protocol::{HarnessError, MessageRole, ModelRequest, ModelResponse};

use super::super::{ProviderModel, StreamCompletion, StreamEvent};
use super::responses_wire;

#[derive(Default)]
pub(in crate::model) struct ReasoningState {
    parts: BTreeMap<(usize, usize), String>,
}

impl ReasoningState {
    fn append(&mut self, key: (usize, usize), delta: &str) -> String {
        if delta.is_empty() {
            return String::new();
        }
        let separator =
            !self.parts.contains_key(&key) && self.parts.values().any(|text| !text.is_empty());
        self.parts.entry(key).or_default().push_str(delta);
        if separator {
            format!("\n\n{delta}")
        } else {
            delta.to_owned()
        }
    }

    fn finish_part(&mut self, key: (usize, usize), text: &str) -> String {
        let existing = self.parts.get(&key).map_or("", String::as_str);
        let suffix = text.strip_prefix(existing).unwrap_or_default().to_owned();
        let delta = self.append(key, &suffix);
        text.clone_into(self.parts.entry(key).or_default());
        delta
    }

    fn text(&self) -> String {
        self.parts
            .values()
            .filter(|text| !text.is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn ingest_response(&mut self, response: &Value) {
        let mut parts = BTreeMap::new();
        for (output_index, item) in response
            .get("output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            for (content_index, text) in reasoning_parts(item).into_iter().enumerate() {
                parts.insert((output_index, content_index), text.to_owned());
            }
        }
        if !parts.is_empty() {
            self.parts = parts;
        }
    }
}

fn reasoning_parts(item: &Value) -> Vec<&str> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return Vec::new();
    }
    match item.get("content") {
        Some(Value::String(text)) if !text.is_empty() => vec![text],
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("reasoning_text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn event_index(event: &Value, name: &str) -> Result<usize, HarnessError> {
    responses_wire::responses_output_index(event.get(name).and_then(Value::as_u64).unwrap_or(0))
}

fn reasoning_event(delta: String) -> StreamEvent {
    if delta.is_empty() {
        StreamEvent::Metadata
    } else {
        StreamEvent::Deltas {
            reasoning: delta,
            text: String::new(),
        }
    }
}

pub(in crate::model) fn decode_stream_data(
    event: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "response.reasoning_text.delta" => {
            let key = (
                event_index(event, "output_index")?,
                event_index(event, "content_index")?,
            );
            let delta = completion.deepseek_reasoning.append(
                key,
                event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
            completion.reasoning_content.push_str(&delta);
            Ok(reasoning_event(delta))
        }
        "response.reasoning_text.done" | "response.content_part.done" => {
            let part = if kind == "response.content_part.done" {
                let part = event.get("part").unwrap_or(&Value::Null);
                if part.get("type").and_then(Value::as_str) != Some("reasoning_text") {
                    return responses_wire::decode_responses_stream_data(event, completion);
                }
                part
            } else {
                event
            };
            let key = (
                event_index(event, "output_index")?,
                event_index(event, "content_index")?,
            );
            let Some(text) = part.get("text").and_then(Value::as_str) else {
                return Ok(StreamEvent::Metadata);
            };
            let delta = completion.deepseek_reasoning.finish_part(key, text);
            completion.reasoning_content = completion.deepseek_reasoning.text();
            Ok(reasoning_event(delta))
        }
        "response.output_item.done"
            if event.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
        {
            let index = event_index(event, "output_index")?;
            let mut delta = String::new();
            for (part, text) in reasoning_parts(&event["item"]).into_iter().enumerate() {
                delta.push_str(
                    &completion
                        .deepseek_reasoning
                        .finish_part((index, part), text),
                );
            }
            completion.reasoning_content = completion.deepseek_reasoning.text();
            Ok(reasoning_event(delta))
        }
        _ => {
            let decoded = responses_wire::decode_responses_stream_data(event, completion)?;
            if matches!(decoded, StreamEvent::Done) {
                completion
                    .deepseek_reasoning
                    .ingest_response(&event["response"]);
                if !completion.deepseek_reasoning.parts.is_empty() {
                    completion.reasoning_content = completion.deepseek_reasoning.text();
                }
            }
            Ok(decoded)
        }
    }
}

pub(in crate::model) fn decode_completion(
    bytes: &[u8],
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        HarnessError::execution(format!("parse DeepSeek Responses completion: {error}"))
    })?;
    let mut response = responses_wire::decode_response_value(&value, provider, model)?;
    let mut reasoning = ReasoningState::default();
    reasoning.ingest_response(&value);
    if !reasoning.parts.is_empty() {
        response.reasoning_content = Some(reasoning.text());
    }
    Ok(response)
}

pub(in crate::model) fn request_input(request: &ModelRequest) -> Vec<Value> {
    let mut input = Vec::new();
    for message in &request.messages {
        if message.role == MessageRole::Assistant
            && let Some(reasoning) = message
                .reasoning_content
                .as_deref()
                .filter(|text| !text.is_empty())
        {
            input.push(json!({"type":"reasoning", "summary":[], "content":[{"type":"reasoning_text", "text":reasoning}]}));
        }
        input.extend(responses_wire::message_input(message));
    }
    input
}

pub(super) fn request_body(model: &ProviderModel, request: &ModelRequest) -> Value {
    responses_wire::request_body(model, request, &request_input(request))
}
