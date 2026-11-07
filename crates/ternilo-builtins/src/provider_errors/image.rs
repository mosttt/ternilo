use super::ProviderHttpFailure;
use serde_json::Value;

const COMPLEXITY: ProviderHttpFailure = ProviderHttpFailure {
    code: "image_complexity_rejected",
    retryable: false,
    message: "Model API error (image_complexity_rejected): the upstream image model rejected the complexity parameter. Ask the Provider or relay administrator to check its image model and complexity setting, or choose another Provider. Image complexity is separate from chat reasoning effort.",
};

pub(super) fn from_error(error: &Value) -> Option<ProviderHttpFailure> {
    (error["type"] == "image_generation_user_error"
        && error["code"] == "invalid_value"
        && error["param"] == "complexity")
        .then_some(COMPLEXITY)
}

pub(super) fn from_public_message(message: &str) -> Option<ProviderHttpFailure> {
    (message == COMPLEXITY.message).then_some(COMPLEXITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::ProviderProtocol;

    #[test]
    fn structured_image_rejection_survives_http_and_stream_wrappers_without_private_text() {
        let mut value = serde_json::json!({"error": {
            "type": "image_generation_user_error", "code": "invalid_value", "param": "complexity",
            "message": "Complexity 'medium' is not supported for this image model. private-key"
        }, "sequence_number": 118});
        for status in [400, 422, 502] {
            assert_eq!(
                ProviderHttpFailure::from_response(
                    ProviderProtocol::OpenAiResponses,
                    "https://relay.example/v1/responses",
                    status,
                    &serde_json::to_vec(&value).unwrap(),
                ),
                Some(COMPLEXITY)
            );
        }
        assert_eq!(
            ProviderHttpFailure::from_response_error(&value),
            Some(COMPLEXITY)
        );
        assert_eq!(
            ProviderHttpFailure::from_response_error(&value["error"]),
            Some(COMPLEXITY)
        );
        let failure = ProviderHttpFailure::from_response_error(&value).unwrap();
        assert!(!failure.retryable);
        assert!(!failure.message.contains("private-key"));
        assert_eq!(
            ProviderHttpFailure::from_public_message(COMPLEXITY.message),
            Some(COMPLEXITY)
        );
        assert!(
            ProviderHttpFailure::from_public_message(&format!(
                "{} private-key",
                COMPLEXITY.message
            ))
            .is_none()
        );
        value["error"]["param"] = "reasoning".into();
        assert!(ProviderHttpFailure::from_response_error(&value).is_none());
    }
}
