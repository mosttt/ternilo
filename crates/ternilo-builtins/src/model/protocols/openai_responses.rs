//! Standard `OpenAI` Responses reasoning summaries and request history.

use super::super::{ProviderModel, StreamCompletion, StreamEvent};
use super::responses_wire;
use serde_json::Value;
use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse};

pub(super) fn request_body(model: &ProviderModel, request: &ModelRequest) -> Value {
    let mut body =
        responses_wire::request_body(model, request, &responses_wire::responses_input(request));
    if model
        .reasoning_effort
        .as_deref()
        .is_some_and(|effort| effort != "none")
    {
        body["reasoning"]["summary"] = Value::String("auto".to_owned());
    }
    body
}

pub(super) fn decode_responses_stream_data(
    event: &Value,
    completion: &mut StreamCompletion,
) -> Result<StreamEvent, HarnessError> {
    match event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "response.reasoning_summary_text.delta" => {
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            completion.reasoning_content.push_str(&delta);
            Ok(StreamEvent::Deltas {
                reasoning: delta,
                text: String::new(),
            })
        }
        "response.reasoning_summary_text.done" => {
            if completion.reasoning_content.is_empty()
                && let Some(text) = event.get("text").and_then(Value::as_str)
            {
                text.clone_into(&mut completion.reasoning_content);
            }
            Ok(StreamEvent::Metadata)
        }
        _ => {
            let decoded = responses_wire::decode_responses_stream_data(event, completion)?;
            if matches!(decoded, StreamEvent::Done) {
                ingest_responses_reasoning(&event["response"], completion);
            }
            Ok(decoded)
        }
    }
}

fn ingest_responses_reasoning(response: &Value, completion: &mut StreamCompletion) {
    let summaries = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .flat_map(|item| {
            item.get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if !summaries.is_empty() {
        completion.reasoning_content = summaries.join("\n\n");
    }
}

pub(in crate::model) fn decode_responses_completion(
    bytes: &[u8],
    provider: &str,
    model: &str,
) -> Result<ModelResponse, HarnessError> {
    let response: Value = serde_json::from_slice(bytes)
        .map_err(|error| HarnessError::execution(format!("parse model response: {error}")))?;
    let mut completion = responses_wire::decode_response_value(&response, provider, model)?;
    let mut state = StreamCompletion::default();
    ingest_responses_reasoning(&response, &mut state);
    completion.reasoning_content =
        (!state.reasoning_content.is_empty()).then_some(state.reasoning_content);
    Ok(completion)
}
