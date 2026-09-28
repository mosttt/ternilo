use std::collections::BTreeSet;

use futures_util::StreamExt as _;
use serde_json::Value;
use ternilo_control::{ModelRequestState, ServiceModelUsage};
use ternilo_protocol::ProviderProtocol;

use super::{Delivery, error::GatewayError, protocol, usage};

pub(super) struct Completion {
    pub(super) state: ModelRequestState,
    pub(super) usage: Option<ServiceModelUsage>,
    pub(super) upstream_request_id: Option<String>,
    pub(super) tail: Vec<Vec<u8>>,
    pub(super) started: bool,
    finished_choices: BTreeSet<u64>,
    expected_choices: u64,
    native_usage: Value,
    native_finished: bool,
}

impl Default for Completion {
    fn default() -> Self {
        Self {
            state: ModelRequestState::Failed,
            usage: None,
            upstream_request_id: None,
            tail: Vec::new(),
            started: false,
            finished_choices: BTreeSet::new(),
            expected_choices: 1,
            native_usage: serde_json::json!({}),
            native_finished: false,
        }
    }
}

pub(super) async fn execute(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: Option<&str>,
    prepared: &protocol::PreparedRequest,
    protocol: ProviderProtocol,
    output: &tokio::sync::mpsc::Sender<Delivery>,
    completion: &mut Completion,
) -> Result<(), GatewayError> {
    let request = ternilo_builtins::provider_request(
        client.post(endpoint).json(&prepared.body),
        protocol,
        api_key,
    );
    let response = request.send().await.map_err(|_| {
        GatewayError::upstream(
            "upstream_connection_error",
            "could not complete the upstream model connection",
        )
    })?;
    completion.upstream_request_id = response
        .headers()
        .get("x-request-id")
        .or_else(|| response.headers().get("request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if !response.status().is_success() {
        // Upstream bodies can echo credentials and internal routes. Keep them private.
        return Err(GatewayError::upstream(
            "upstream_rejected_request",
            &format!(
                "upstream model rejected the request (HTTP {})",
                response.status().as_u16()
            ),
        ));
    }
    let is_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if prepared.stream != is_stream {
        return Err(GatewayError::upstream(
            "upstream_protocol_error",
            "upstream did not honor the requested streaming format",
        ));
    }
    if is_stream {
        completion.expected_choices = prepared.body.get("n").and_then(Value::as_u64).unwrap_or(1);
        output
            .send(Delivery::Start)
            .await
            .map_err(|_| GatewayError::cancelled("client disconnected"))?;
        completion.started = true;
        read_stream(response, protocol, &prepared.model_id, output, completion).await
    } else {
        let bytes = bounded_body(response).await?;
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            GatewayError::upstream("upstream_protocol_error", "upstream sent invalid JSON")
        })?;
        completion.usage = usage::parse_usage(&value, protocol);
        validate_completion(&value, protocol)?;
        completion.state = terminal_state(&value, protocol);
        protocol::rewrite_model(&mut value, &prepared.model_id);
        completion.tail.push(value.to_string().into_bytes());
        Ok(())
    }
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, GatewayError> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            GatewayError::upstream(
                "upstream_stream_error",
                "upstream response ended unexpectedly",
            )
        })?;
        if bytes.len().saturating_add(chunk.len()) > protocol::MAX_RESPONSE_BYTES {
            return Err(GatewayError::upstream(
                "upstream_response_too_large",
                "upstream response exceeded the supported size",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_stream(
    response: reqwest::Response,
    protocol: ProviderProtocol,
    model_id: &str,
    output: &tokio::sync::mpsc::Sender<Delivery>,
    completion: &mut Completion,
) -> Result<(), GatewayError> {
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            GatewayError::upstream(
                "upstream_stream_error",
                "upstream model stream ended unexpectedly",
            )
        })?;
        if buffer.len().saturating_add(chunk.len()) > protocol::MAX_RESPONSE_BYTES {
            return Err(GatewayError::upstream(
                "upstream_event_too_large",
                "upstream event exceeded the supported size",
            ));
        }
        buffer.extend_from_slice(&chunk);
        while let Some(event) = protocol::take_sse_event(&mut buffer) {
            if stream_event(&event, protocol, model_id, output, completion).await? {
                return Ok(());
            }
        }
    }
    if protocol == ProviderProtocol::GoogleGemini
        && completion.native_finished
        && buffer.iter().all(u8::is_ascii_whitespace)
    {
        completion.state = ModelRequestState::Completed;
        return Ok(());
    }
    // An unterminated last event is not a successful stream terminator.
    Err(GatewayError::upstream(
        "upstream_stream_incomplete",
        "upstream stream ended before its completion event",
    ))
}

async fn stream_event(
    event: &[u8],
    protocol: ProviderProtocol,
    model_id: &str,
    output: &tokio::sync::mpsc::Sender<Delivery>,
    completion: &mut Completion,
) -> Result<bool, GatewayError> {
    let Some(data) = protocol::sse_data(event).map_err(|_| {
        GatewayError::upstream(
            "upstream_protocol_error",
            "upstream event is not valid UTF-8",
        )
    })?
    else {
        return Ok(false);
    };
    if data == "[DONE]" {
        if protocol != ProviderProtocol::OpenAiChatCompletions {
            return Err(GatewayError::upstream(
                "upstream_protocol_error",
                "Responses stream used a Chat completion terminator",
            ));
        }
        if (0..completion.expected_choices)
            .any(|index| !completion.finished_choices.contains(&index))
        {
            return Err(GatewayError::upstream(
                "upstream_stream_incomplete",
                "upstream stream did not finish every requested choice",
            ));
        }
        completion.state = ModelRequestState::Completed;
        completion.tail.push(b"data: [DONE]\n\n".to_vec());
        return Ok(true);
    }
    let mut value = parse_stream_value(&data)?;
    if matches!(
        protocol,
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages
    ) {
        return native_stream_event(value, protocol, model_id, output, completion).await;
    }
    let terminal = match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            if let Some(usage) = usage::parse_usage(&value, protocol) {
                completion.usage = Some(usage);
            }
            let choices = value
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    GatewayError::upstream(
                        "upstream_protocol_error",
                        "Chat stream event omitted choices",
                    )
                })?;
            for choice in choices {
                if choice
                    .get("finish_reason")
                    .is_some_and(|value| !value.is_null())
                    && let Some(index) = choice.get("index").and_then(Value::as_u64)
                {
                    completion.finished_choices.insert(index);
                }
            }
            false
        }
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            matches!(
                value.get("type").and_then(Value::as_str),
                Some("response.completed" | "response.incomplete" | "response.failed")
            )
        }
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
            unreachable!("native stream handled separately")
        }
    };
    if terminal {
        let response = value.get("response").ok_or_else(|| {
            GatewayError::upstream(
                "upstream_protocol_error",
                "completion event omitted its response object",
            )
        })?;
        completion.usage = usage::parse_usage(response, protocol);
        validate_completion(response, protocol)?;
        completion.state = terminal_state(response, protocol);
    }
    protocol::rewrite_model(&mut value, model_id);
    let frame = protocol::sse_frame(&value, protocol);
    if terminal {
        completion.tail.push(frame);
    } else {
        output
            .send(Delivery::Bytes(frame))
            .await
            .map_err(|_| GatewayError::cancelled("client disconnected"))?;
    }
    Ok(terminal)
}

fn parse_stream_value(data: &str) -> Result<Value, GatewayError> {
    let value: Value = serde_json::from_str(data).map_err(|_| {
        GatewayError::upstream(
            "upstream_protocol_error",
            "upstream stream contained invalid JSON",
        )
    })?;
    if value.get("error").is_some_and(|value| !value.is_null())
        || value.get("type").and_then(Value::as_str) == Some("error")
    {
        return Err(GatewayError::upstream(
            "upstream_model_error",
            "upstream model reported an error",
        ));
    }
    Ok(value)
}

fn validate_completion(value: &Value, protocol: ProviderProtocol) -> Result<(), GatewayError> {
    let valid = match protocol {
        ProviderProtocol::OpenAiChatCompletions => value
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                !choices.is_empty()
                    && choices.iter().all(|choice| {
                        choice.get("message").is_some_and(Value::is_object)
                            && choice
                                .get("finish_reason")
                                .is_some_and(|reason| !reason.is_null())
                    })
            }),
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            value.get("output").is_some_and(Value::is_array)
                && matches!(
                    value.get("status").and_then(Value::as_str),
                    Some("completed" | "incomplete" | "failed")
                )
        }
        ProviderProtocol::GoogleGemini => value["candidates"]
            .as_array()
            .and_then(|items| items.first())
            .is_some_and(|candidate| {
                matches!(
                    candidate["finishReason"].as_str(),
                    Some("STOP" | "MAX_TOKENS")
                )
            }),
        ProviderProtocol::AnthropicMessages => {
            value["content"].is_array() && native_stop_reason(&value["stop_reason"])
        }
    };
    if valid {
        Ok(())
    } else {
        Err(GatewayError::upstream(
            "upstream_protocol_error",
            "upstream returned an invalid completion object",
        ))
    }
}

fn terminal_state(value: &Value, protocol: ProviderProtocol) -> ModelRequestState {
    if protocol.api_protocol() == ProviderProtocol::OpenAiResponses
        && value.get("status").and_then(Value::as_str) == Some("failed")
    {
        ModelRequestState::Failed
    } else {
        ModelRequestState::Completed
    }
}

fn native_stop_reason(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some(
            "end_turn"
                | "stop_sequence"
                | "refusal"
                | "tool_use"
                | "max_tokens"
                | "model_context_window_exceeded"
        )
    )
}

async fn native_stream_event(
    mut value: Value,
    protocol: ProviderProtocol,
    model_id: &str,
    output: &tokio::sync::mpsc::Sender<Delivery>,
    completion: &mut Completion,
) -> Result<bool, GatewayError> {
    let current_usage = value
        .get("usageMetadata")
        .or_else(|| value.get("usage"))
        .or_else(|| value.pointer("/message/usage"));
    if let Some(usage) = current_usage.and_then(Value::as_object) {
        completion
            .native_usage
            .as_object_mut()
            .expect("usage object")
            .extend(usage.clone());
        completion.usage = usage::parse_usage(
            &serde_json::json!({"usage": completion.native_usage}),
            protocol,
        );
    }
    let terminal = if protocol == ProviderProtocol::GoogleGemini {
        if value.pointer("/promptFeedback/blockReason").is_some() {
            return Err(GatewayError::upstream(
                "upstream_model_error",
                "Gemini blocked the prompt",
            ));
        }
        if let Some(candidate) = value["candidates"]
            .as_array()
            .and_then(|items| items.first())
            && let Some(reason) = candidate["finishReason"].as_str()
        {
            if !matches!(reason, "STOP" | "MAX_TOKENS") {
                return Err(GatewayError::upstream(
                    "upstream_model_error",
                    "Gemini did not complete generation",
                ));
            }
            completion.native_finished = true;
        }
        false
    } else {
        if let Some(reason) = value
            .pointer("/delta/stop_reason")
            .filter(|value| !value.is_null())
        {
            if !native_stop_reason(reason) {
                return Err(GatewayError::upstream(
                    "upstream_model_error",
                    "Claude did not complete generation",
                ));
            }
            completion.native_finished = true;
        }
        let stopped = value["type"] == "message_stop";
        if stopped && !completion.native_finished {
            return Err(GatewayError::upstream(
                "upstream_stream_incomplete",
                "Claude stream omitted its stop reason",
            ));
        }
        if stopped {
            completion.state = ModelRequestState::Completed;
        }
        stopped
    };
    protocol::rewrite_model(&mut value, model_id);
    let frame = protocol::sse_frame(&value, protocol);
    if terminal || protocol == ProviderProtocol::GoogleGemini && completion.native_finished {
        completion.tail.push(frame);
    } else {
        output
            .send(Delivery::Bytes(frame))
            .await
            .map_err(|_| GatewayError::cancelled("client disconnected"))?;
    }
    Ok(terminal)
}
