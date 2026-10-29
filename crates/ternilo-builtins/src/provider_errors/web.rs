use super::ProviderHttpFailure;
use serde_json::Value;

const FAILURES: &[(u16, ProviderHttpFailure)] = &[
    (
        400,
        ProviderHttpFailure {
            code: "unsupported_web_search",
            retryable: false,
            message: "Model API error (HTTP 400; unsupported_web_search): the upstream provider rejected built-in web search for this model. Choose another model, disable search, or select an external integration, then retry.",
        },
    ),
    (
        400,
        ProviderHttpFailure {
            code: "unsupported_web_fetch",
            retryable: false,
            message: "Model API error (HTTP 400; unsupported_web_fetch): the upstream provider rejected built-in page reading for this model. Choose another model, disable reading, or select an external integration, then retry.",
        },
    ),
    (
        422,
        ProviderHttpFailure {
            code: "unsupported_web_search",
            retryable: false,
            message: "Model API error (HTTP 422; unsupported_web_search): the upstream provider rejected built-in web search for this model. Choose another model, disable search, or select an external integration, then retry.",
        },
    ),
    (
        422,
        ProviderHttpFailure {
            code: "unsupported_web_fetch",
            retryable: false,
            message: "Model API error (HTTP 422; unsupported_web_fetch): the upstream provider rejected built-in page reading for this model. Choose another model, disable reading, or select an external integration, then retry.",
        },
    ),
];

pub(super) fn from_response(status: u16, body: &[u8]) -> Option<ProviderHttpFailure> {
    if !matches!(status, 400 | 422) {
        return None;
    }
    let value: Value = serde_json::from_slice(body).ok()?;
    let error = &value["error"];
    let code = error["code"].as_str().unwrap_or_default();
    let message = error["message"].as_str().unwrap_or_default().to_lowercase();
    let parameter = error["param"].as_str().unwrap_or_default();
    let unsupported = matches!(
        code,
        "unsupported_tool" | "unsupported_value" | "unsupported_parameter"
    ) || message.contains("not supported")
        || message.contains("does not support")
        || message.contains("unsupported")
        || message.contains("不支持");
    FAILURES
        .iter()
        .find(|(http, failure)| {
            if *http != status {
                return false;
            }
            let capability = failure.code.strip_prefix("unsupported_").unwrap();
            code == failure.code
                || unsupported && (parameter.contains(capability) || message.contains(capability))
        })
        .map(|(_, failure)| *failure)
}

pub(super) fn from_public_message(message: &str) -> Option<ProviderHttpFailure> {
    FAILURES
        .iter()
        .find(|(_, failure)| failure.message == message)
        .map(|(_, failure)| *failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_upstream_tool_rejections_are_classified_without_echoing_secrets() {
        for capability in ["web_search", "web_fetch"] {
            let body = serde_json::json!({"error":{"code":"invalid_request_error","message":format!("{capability} is not supported by this model; private-key")}}).to_string();
            let failure = from_response(400, body.as_bytes()).unwrap();
            assert_eq!(failure.code, format!("unsupported_{capability}"));
            assert!(!failure.retryable);
            assert!(!failure.message.contains("private-key"));
            assert_eq!(from_public_message(failure.message), Some(failure));
            assert!(from_public_message(&format!("{} private-key", failure.message)).is_none());
            assert!(from_response(401, body.as_bytes()).is_none());
        }
        for message in [
            "web_search quota exceeded",
            "unsupported temperature",
            "model not found",
        ] {
            let body = serde_json::json!({"error":{"message":message}}).to_string();
            assert!(from_response(400, body.as_bytes()).is_none());
        }
    }
}
