use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use ternilo_protocol::{HarnessError, ProviderModelDefaults, ProviderProtocol};

pub(super) const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

pub(super) struct PreparedRequest {
    pub(super) model_id: String,
    pub(super) body: Value,
    pub(super) payload_hash: String,
    pub(super) stream: bool,
    input_bytes: usize,
    media: bool,
}

impl PreparedRequest {
    pub(super) fn parse(body: Value, protocol: ProviderProtocol) -> Result<Self, HarnessError> {
        let object = body
            .as_object()
            .ok_or_else(|| HarnessError::invalid("request must be a JSON object"))?;
        let model_id = object
            .get("model")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| HarnessError::invalid("model must name a published model"))?
            .to_owned();
        let stream = optional_bool(object, "stream")?.unwrap_or(false);
        if optional_bool(object, "store")?.unwrap_or(false) {
            return Err(HarnessError::invalid(
                "this model service is stateless; store must be false",
            ));
        }
        match protocol {
            ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
                super::native::validate(object, protocol)?;
            }
            ProviderProtocol::OpenAiChatCompletions => {
                validate_tools(object, protocol)?;
                validate_tool_choice(object, protocol)?;
                validate_chat(object)?;
            }
            ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
                validate_tools(object, protocol)?;
                validate_tool_choice(object, protocol)?;
                validate_responses(object)?;
            }
        }
        let mut canonical_body = body.clone();
        canonical_body.sort_all_objects();
        let canonical = serde_json::to_vec(&canonical_body)
            .map_err(|error| HarnessError::invalid(format!("encode model request: {error}")))?;
        let mut digest = Sha256::new();
        digest.update(endpoint_path(protocol));
        digest.update([0]);
        digest.update(&canonical);
        let media = has_media(&body);
        Ok(Self {
            model_id,
            body,
            payload_hash: hex(digest.finalize().as_ref()),
            stream,
            input_bytes: canonical.len(),
            media,
        })
    }

    pub(super) fn prepare_upstream(
        &mut self,
        protocol: ProviderProtocol,
        upstream_model: &str,
        defaults: &ProviderModelDefaults,
    ) -> Result<u64, HarnessError> {
        if matches!(
            protocol,
            ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages
        ) {
            let output = super::native::prepare_upstream(
                &mut self.body,
                protocol,
                upstream_model,
                defaults,
            )?;
            let input = if self.media {
                defaults.context_window
            } else {
                u64::try_from(self.input_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_add(4096)
            };
            return input
                .checked_add(output)
                .ok_or_else(|| HarnessError::invalid("model request budget overflow"));
        }
        let object = self.body.as_object_mut().expect("validated request object");
        apply_reasoning(object, protocol, defaults)?;
        for name in ["max_tokens", "max_completion_tokens", "max_output_tokens"] {
            if object.get(name).is_some_and(Value::is_null) {
                object.remove(name);
            }
        }
        let output_field = match protocol {
            ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
                "max_output_tokens"
            }
            ProviderProtocol::OpenAiChatCompletions
                if object.contains_key("max_completion_tokens") =>
            {
                "max_completion_tokens"
            }
            ProviderProtocol::OpenAiChatCompletions => "max_tokens",
            ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
                unreachable!("native request handled above")
            }
        };
        let output = positive_integer(object, output_field)?.unwrap_or(defaults.max_output_tokens);
        if output > defaults.max_output_tokens {
            return Err(HarnessError::invalid(format!(
                "{output_field} exceeds this model's published limit of {}",
                defaults.max_output_tokens,
            )));
        }
        let choices = positive_integer(object, "n")?.unwrap_or(1);
        if choices > 16 {
            return Err(HarnessError::invalid("n must be between 1 and 16"));
        }
        object.insert("model".to_owned(), json!(upstream_model));
        object.insert(output_field.to_owned(), json!(output));
        object.insert("store".to_owned(), json!(false));
        if self.stream && protocol == ProviderProtocol::OpenAiChatCompletions {
            let options = object.entry("stream_options").or_insert_with(|| json!({}));
            options
                .as_object_mut()
                .ok_or_else(|| HarnessError::invalid("stream_options must be an object"))?
                .insert("include_usage".to_owned(), json!(true));
        }
        let input = if self.media {
            defaults.context_window
        } else {
            u64::try_from(self.input_bytes)
                .ok()
                .and_then(|bytes| bytes.checked_add(4_096))
                .ok_or_else(|| HarnessError::invalid("model request budget overflow"))?
        };
        choices
            .checked_mul(output)
            .and_then(|output| input.checked_add(output))
            .ok_or_else(|| HarnessError::invalid("model request budget overflow"))
    }
}

pub(super) const fn endpoint_path(protocol: ProviderProtocol) -> &'static str {
    match protocol {
        ProviderProtocol::OpenAiChatCompletions => "chat/completions",
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => "responses",
        ProviderProtocol::GoogleGemini => "generateContent",
        ProviderProtocol::AnthropicMessages => "messages",
    }
}

fn optional_bool(object: &Map<String, Value>, name: &str) -> Result<Option<bool>, HarnessError> {
    object
        .get(name)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| HarnessError::invalid(format!("{name} must be a boolean")))
        })
        .transpose()
}

fn positive_integer(object: &Map<String, Value>, name: &str) -> Result<Option<u64>, HarnessError> {
    object
        .get(name)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .filter(|value| *value > 0)
                .ok_or_else(|| HarnessError::invalid(format!("{name} must be a positive integer")))
        })
        .transpose()
}

fn reject_fields(object: &Map<String, Value>, names: &[&str]) -> Result<(), HarnessError> {
    for name in names {
        if object.get(*name).is_some_and(|value| !value.is_null()) {
            return Err(HarnessError::invalid(format!(
                "{name} is not supported by this stateless model endpoint",
            )));
        }
    }
    Ok(())
}

fn validate_chat(object: &Map<String, Value>) -> Result<(), HarnessError> {
    reject_fields(
        object,
        &[
            "web_search_options",
            "audio",
            "modalities",
            "max_output_tokens",
            "reasoning",
        ],
    )?;
    if object
        .get("max_tokens")
        .is_some_and(|value| !value.is_null())
        && object
            .get("max_completion_tokens")
            .is_some_and(|value| !value.is_null())
    {
        return Err(HarnessError::invalid(
            "specify only one of max_tokens and max_completion_tokens",
        ));
    }
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| HarnessError::invalid("messages must be a nonempty array"))?;
    for message in messages {
        let message = message
            .as_object()
            .ok_or_else(|| HarnessError::invalid("each message must be an object"))?;
        reject_fields(message, &["audio"])?;
        validate_content(message.get("content"), false)?;
    }
    Ok(())
}

fn validate_responses(object: &Map<String, Value>) -> Result<(), HarnessError> {
    reject_fields(
        object,
        &[
            "previous_response_id",
            "conversation",
            "prompt",
            "context_management",
            "max_tool_calls",
            "max_tokens",
            "max_completion_tokens",
            "web_search_options",
            "reasoning_effort",
            "n",
        ],
    )?;
    if optional_bool(object, "background")?.unwrap_or(false) {
        return Err(HarnessError::invalid(
            "background responses are not supported",
        ));
    }
    let input = object
        .get("input")
        .ok_or_else(|| HarnessError::invalid("input is required"))?;
    if input.is_string() {
        return Ok(());
    }
    let input = input
        .as_array()
        .ok_or_else(|| HarnessError::invalid("input must be a string or an array"))?;
    for item in input {
        let item = item
            .as_object()
            .ok_or_else(|| HarnessError::invalid("each input item must be an object"))?;
        match item.get("type").and_then(Value::as_str) {
            None | Some("message") => validate_content(item.get("content"), true)?,
            Some("function_call" | "reasoning") => {}
            Some("function_call_output") => validate_content(item.get("output"), true)?,
            Some(kind) => {
                return Err(HarnessError::invalid(format!(
                    "input item type {kind:?} is not supported; send full stateless messages and function results",
                )));
            }
        }
    }
    Ok(())
}

fn apply_reasoning(
    object: &mut Map<String, Value>,
    protocol: ProviderProtocol,
    defaults: &ProviderModelDefaults,
) -> Result<(), HarnessError> {
    let requested = match protocol {
        ProviderProtocol::OpenAiChatCompletions => object.get("reasoning_effort"),
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            let reasoning = object.get("reasoning").filter(|value| !value.is_null());
            if reasoning.is_some_and(|value| !value.is_object()) {
                return Err(HarnessError::invalid("reasoning must be an object"));
            }
            reasoning.and_then(|value| value.get("effort"))
        }
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
            return Err(HarnessError::invalid(
                "native reasoning must use its native endpoint",
            ));
        }
    }
    .filter(|value| !value.is_null());
    if let Some(requested) = requested {
        let effort = requested
            .as_str()
            .ok_or_else(|| HarnessError::invalid("reasoning effort must be a string"))?;
        let Some(reasoning) = &defaults.reasoning else {
            return Err(HarnessError::invalid(
                "this published model does not support reasoning effort",
            ));
        };
        if !reasoning
            .efforts
            .values()
            .any(|value| value.as_deref() == Some(effort))
        {
            return Err(HarnessError::invalid(
                "reasoning effort is not allowed by this published model",
            ));
        }
        return Ok(());
    }
    let default = defaults
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.efforts.get(&reasoning.default_effort))
        .and_then(Option::as_ref);
    match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            object.remove("reasoning_effort");
            if let Some(effort) = default {
                object.insert("reasoning_effort".to_owned(), json!(effort));
            }
        }
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
            unreachable!("native reasoning handled separately")
        }
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            if let Some(effort) = default {
                let reasoning = object.entry("reasoning").or_insert_with(|| json!({}));
                if reasoning.is_null() {
                    *reasoning = json!({});
                }
                reasoning
                    .as_object_mut()
                    .expect("validated reasoning object")
                    .insert("effort".to_owned(), json!(effort));
            } else if let Some(reasoning) =
                object.get_mut("reasoning").and_then(Value::as_object_mut)
            {
                reasoning.remove("effort");
            }
        }
    }
    Ok(())
}

fn validate_content(content: Option<&Value>, responses: bool) -> Result<(), HarnessError> {
    let Some(content) = content.filter(|content| !content.is_null()) else {
        return Ok(());
    };
    if content.is_string() {
        return Ok(());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| HarnessError::invalid("message content must be text or an array"))?;
    for block in blocks {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let valid = if responses {
            matches!(
                kind,
                "input_text" | "output_text" | "refusal" | "input_image"
            )
        } else {
            matches!(kind, "text" | "image_url" | "refusal")
        };
        if !valid || block.get("file_id").is_some_and(|value| !value.is_null()) {
            return Err(HarnessError::invalid(format!(
                "content type {kind:?} is not supported; send text or an image URL/data URL without upstream file references",
            )));
        }
    }
    Ok(())
}

fn validate_tools(
    object: &Map<String, Value>,
    protocol: ProviderProtocol,
) -> Result<(), HarnessError> {
    let Some(tools) = object.get("tools").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let tools = tools
        .as_array()
        .ok_or_else(|| HarnessError::invalid("tools must be an array"))?;
    for tool in tools {
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(HarnessError::invalid(
                "only client-executed function tools are supported by this model endpoint",
            ));
        }
        let function = match protocol {
            ProviderProtocol::OpenAiChatCompletions => tool.get("function"),
            ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => Some(tool),
            ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
                return Err(HarnessError::invalid(
                    "native tools require their native endpoint",
                ));
            }
        };
        if function
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(HarnessError::invalid("each function tool requires a name"));
        }
    }
    Ok(())
}

fn validate_tool_choice(
    object: &Map<String, Value>,
    protocol: ProviderProtocol,
) -> Result<(), HarnessError> {
    let Some(choice) = object.get("tool_choice").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if matches!(choice.as_str(), Some("none" | "auto" | "required")) {
        return Ok(());
    }
    let function = choice.get("type").and_then(Value::as_str) == Some("function");
    let name = match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            choice.get("function").and_then(|value| value.get("name"))
        }
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            choice.get("name")
        }
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
            return Err(HarnessError::invalid(
                "native tool choice requires its native endpoint",
            ));
        }
    };
    if function
        && name
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
    {
        Ok(())
    } else {
        Err(HarnessError::invalid(
            "tool_choice must select none, auto, required, or one client-executed function",
        ))
    }
}

fn has_media(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            matches!(
                object.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image" | "image")
            ) || object.contains_key("inlineData")
                || object.contains_key("fileData")
                || object.values().any(has_media)
        }
        Value::Array(values) => values.iter().any(has_media),
        _ => false,
    }
}

pub(super) fn rewrite_model(value: &mut Value, public_model: &str) {
    if let Some(object) = value.as_object_mut() {
        if object.contains_key("model") {
            object.insert("model".to_owned(), json!(public_model));
        }
        if object.get("error").is_some_and(|error| !error.is_null()) {
            object.insert(
                "error".to_owned(),
                json!({"code":"upstream_model_error","message":"upstream model reported an error"}),
            );
        }
        if let Some(response) = object.get_mut("response") {
            rewrite_model(response, public_model);
        }
        if let Some(message) = object.get_mut("message") {
            rewrite_model(message, public_model);
        }
        if object.contains_key("modelVersion") {
            object.insert("modelVersion".to_owned(), json!(public_model));
        }
    }
}

pub(super) fn take_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (offset, length) = buffer.windows(2).enumerate().find_map(|(index, pair)| {
        if pair == b"\n\n" {
            Some((index, 2))
        } else if buffer.get(index..index + 4) == Some(b"\r\n\r\n") {
            Some((index, 4))
        } else {
            None
        }
    })?;
    let event = buffer[..offset].to_vec();
    buffer.drain(..offset + length);
    Some(event)
}

pub(super) fn sse_data(event: &[u8]) -> Result<Option<String>, HarnessError> {
    let event = std::str::from_utf8(event)
        .map_err(|_| HarnessError::execution("upstream sent invalid UTF-8 in an SSE event"))?;
    let data = event
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(|data| data.strip_prefix(' ').unwrap_or(data))
        })
        .collect::<Vec<_>>();
    Ok((!data.is_empty()).then(|| data.join("\n")))
}

pub(super) fn sse_frame(value: &Value, protocol: ProviderProtocol) -> Vec<u8> {
    let mut frame = String::new();
    if matches!(
        protocol.api_protocol(),
        ProviderProtocol::OpenAiResponses | ProviderProtocol::AnthropicMessages
    ) && let Some(kind) = value.get("type").and_then(Value::as_str)
    {
        frame.push_str("event: ");
        frame.push_str(kind);
        frame.push('\n');
    }
    frame.push_str("data: ");
    frame.push_str(&value.to_string());
    frame.push_str("\n\n");
    frame.into_bytes()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            write!(text, "{byte:02x}").expect("writing into a String cannot fail");
            text
        })
}
