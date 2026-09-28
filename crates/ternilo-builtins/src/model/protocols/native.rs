use serde_json::{Value, json};
use ternilo_protocol::{Attachment, HarnessError, ModelMessage, ProviderProtocol};

pub(super) fn replay_blocks(
    message: &ModelMessage,
    protocol: ProviderProtocol,
    model: &str,
) -> Option<Vec<Value>> {
    message
        .provider_state
        .as_ref()
        .filter(|state| state.protocol == protocol && state.model == model)
        .map(|state| state.blocks.clone())
}

pub(super) fn attachment_value(
    attachment: &Attachment,
    protocol: ProviderProtocol,
) -> Result<Value, HarnessError> {
    if !attachment.media_type.starts_with("image/") {
        return Ok(json!({"type": "text", "text": format!(
            "<attachment name={:?} media_type={:?}>\n{}\n</attachment>",
            attachment.name, attachment.media_type, attachment.content,
        )}));
    }
    let data = attachment
        .content
        .strip_prefix("data:")
        .and_then(|value| value.split_once(";base64,"));
    match (protocol, data) {
        (ProviderProtocol::GoogleGemini, Some((mime_type, data))) => {
            Ok(json!({"inlineData": {"mimeType": mime_type, "data": data}}))
        }
        (ProviderProtocol::GoogleGemini, None) => Err(HarnessError::invalid(
            "Gemini images must be resolved to base64 data URLs before submission",
        )),
        (_, Some((media_type, data))) => Ok(json!({
            "type": "image", "source": {"type": "base64", "media_type": media_type, "data": data},
        })),
        (_, None) if attachment.content.starts_with("https://") => Ok(json!({
            "type": "image", "source": {"type": "url", "url": attachment.content},
        })),
        _ => Err(HarnessError::invalid(
            "native model images require a base64 data URL or supported HTTPS URL",
        )),
    }
}

pub(super) fn append_message(
    messages: &mut Vec<Value>,
    role: &str,
    field: &str,
    blocks: Vec<Value>,
) {
    if blocks.is_empty() {
        return;
    }
    if let Some(previous) = messages.last_mut().filter(|value| value["role"] == role) {
        previous[field]
            .as_array_mut()
            .expect("message block array")
            .extend(blocks);
    } else {
        messages.push(json!({"role": role, (field): blocks}));
    }
}

pub(super) fn read_json(bytes: &[u8]) -> Result<Value, HarnessError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        HarnessError::execution(format!("decode native model response: {error}"))
    })?;
    check_error(&value)?;
    Ok(value)
}

pub(super) fn check_error(value: &Value) -> Result<(), HarnessError> {
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        return Err(HarnessError::execution(format!(
            "native model error: {}",
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream rejected the request")
        )));
    }
    Ok(())
}
