//! `DeepSeek` Responses carries full reasoning text, not `OpenAI` reasoning summaries.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use ternilo_protocol::{
    HarnessError, MessageRole, ModelProviderState, ModelRequest, ModelResponse, ProviderProtocol,
};

use super::super::{ProviderModel, StreamCompletion, StreamEvent};
use super::{native, responses_wire};

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
    retain_search_history(event, completion)?;
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
    if let Some(blocks) = value.get("output").and_then(Value::as_array)
        && blocks.iter().any(has_search_state)
    {
        response.provider_state = Some(Box::new(ModelProviderState {
            protocol: ProviderProtocol::DeepSeekResponses,
            model: model.to_owned(),
            blocks: blocks.clone(),
        }));
    }
    let mut reasoning = ReasoningState::default();
    reasoning.ingest_response(&value);
    if !reasoning.parts.is_empty() {
        response.reasoning_content = Some(reasoning.text());
    }
    Ok(response)
}

pub(in crate::model) fn request_input(request: &ModelRequest, model: &str) -> Vec<Value> {
    let mut input = Vec::new();
    for message in &request.messages {
        if message.role == MessageRole::Assistant
            && let Some(blocks) =
                native::replay_blocks(message, ProviderProtocol::DeepSeekResponses, model)
        {
            input.extend(blocks);
            continue;
        }
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
    let mut body =
        responses_wire::request_body(model, request, &request_input(request, &model.model));
    if request.step != 0
        && let Some(hosted) = &model.hosted_tools
    {
        crate::hosted_tools::apply_request_hosted_tools(
            &mut body,
            hosted,
            model.protocol,
            &model.model,
            request,
        );
    }
    body
}

fn retain_search_history(
    event: &Value,
    completion: &mut StreamCompletion,
) -> Result<(), HarnessError> {
    match event["type"].as_str() {
        Some("response.output_item.added" | "response.output_item.done") => {
            if let Some(item) = event.get("item").filter(|item| item.is_object()) {
                let index = event_index(event, "output_index")?;
                completion.native_blocks.insert(index, item.clone());
            }
        }
        Some(
            "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed",
        ) => {
            if let Some(item) = completion
                .native_blocks
                .get_mut(&event_index(event, "output_index")?)
                && item["type"] == "web_search_call"
            {
                item["status"] = json!(event["type"].as_str().unwrap().rsplit('.').next().unwrap());
            }
        }
        Some("response.completed" | "response.incomplete") => {
            if let Some(items) = event
                .pointer("/response/output")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty())
            {
                completion.native_blocks = items.iter().cloned().enumerate().collect();
            }
        }
        _ => {}
    }
    if completion.native_blocks.values().any(has_search_state) {
        completion.native_protocol = Some(ProviderProtocol::DeepSeekResponses);
    }
    Ok(())
}

fn has_search_state(item: &Value) -> bool {
    item["type"] == "web_search_call"
        || (item["type"] == "message"
            && item
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts.iter().any(|part| {
                        part.get("annotations")
                            .and_then(Value::as_array)
                            .is_some_and(|annotations| {
                                annotations
                                    .iter()
                                    .any(|annotation| annotation["type"] == "url_citation")
                            })
                    })
                }))
}
