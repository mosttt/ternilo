use super::ProviderHttpFailure;
use serde_json::Value;

// https://developers.openai.com/api/docs/guides/error-codes
// Compatible endpoints use neutral Provider wording, not an assumed OpenAI identity.
const FAILURES: &[ProviderHttpFailure] = &[
    ProviderHttpFailure {
        code: "context_length_exceeded",
        retryable: false,
        message: "Model API error (HTTP 400; context_length_exceeded): the input exceeds the model context window. Compact the conversation, reduce input, or choose a model with a larger context window.",
    },
    ProviderHttpFailure {
        code: "invalid_request",
        retryable: false,
        message: "Model API error (HTTP 400; invalid_request): the request is invalid. Check the Provider protocol, model and request parameters; for a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "service_tier",
        retryable: false,
        message: "Model API error (HTTP 400; service_tier): the requested service tier is not allowed. Choose an allowed tier or ask the API project owner to update the project's allowed service tiers.",
    },
    ProviderHttpFailure {
        code: "authentication_failed",
        retryable: false,
        message: "Model API error (HTTP 401; authentication_failed): authentication failed. Check the API key, organization/project membership and IP allowlist; for a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "access_denied",
        retryable: false,
        message: "Model API error (HTTP 403; access_denied): access was denied. Check the API key's resource permissions and whether the request region is supported; for a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "not_found",
        retryable: false,
        message: "Model API error (HTTP 404; not_found): the model or endpoint was not found or is inaccessible. Check the Provider URL, model ID and access permissions.",
    },
    ProviderHttpFailure {
        code: "credit_balance_exhausted",
        retryable: false,
        message: "Model API error (HTTP 429; credit_balance_exhausted): prepaid API credits are exhausted. Add credits to the organization that owns this API key, or choose another model. For a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "organization_spend_limit_exceeded",
        retryable: false,
        message: "Model API error (HTTP 429; organization_spend_limit_exceeded): the API organization's spend limit was reached. Ask its owner to increase the spend limit, wait for the monthly reset, or choose another model.",
    },
    ProviderHttpFailure {
        code: "project_spend_limit_exceeded",
        retryable: false,
        message: "Model API error (HTTP 429; project_spend_limit_exceeded): the API project's spend limit was reached. Ask its owner to increase the project's spend limit, wait for the monthly reset, or choose another model.",
    },
    ProviderHttpFailure {
        code: "organization_usage_limit_exceeded",
        retryable: false,
        message: "Model API error (HTTP 429; organization_usage_limit_exceeded): the API organization reached its approved usage limit. Ask its owner to request a higher limit from the Provider, or choose another model.",
    },
    ProviderHttpFailure {
        code: "insufficient_quota",
        retryable: false,
        message: "Model API error (HTTP 429; insufficient_quota): API quota is insufficient. Check the API account's credits, billing and usage limits before retrying; for a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "rate_limit_exceeded",
        retryable: true,
        message: "Model API error (HTTP 429; rate_limit_exceeded): the request or token rate limit was reached. Reduce request frequency, concurrency or token volume and wait before retrying.",
    },
    ProviderHttpFailure {
        code: "slow_down",
        retryable: true,
        message: "Model API error (HTTP 429; slow_down): the request rate increased too quickly. Reduce traffic, wait before retrying, and increase traffic gradually.",
    },
    ProviderHttpFailure {
        code: "request_limited",
        retryable: true,
        message: "Model API error (HTTP 429; request_limited): the Provider restricted this request without a recognized reason code. Check API quota and rate limits; for a shared model, contact its provider.",
    },
    ProviderHttpFailure {
        code: "server_error",
        retryable: true,
        message: "Model API error (HTTP 500; server_error): the Provider encountered an internal error. Retry later and check its service status; contact its support if the problem persists.",
    },
    ProviderHttpFailure {
        code: "server_is_overloaded",
        retryable: true,
        message: "Model API error (HTTP 503; server_is_overloaded): the model service is overloaded. Wait before retrying or choose another model.",
    },
];

pub(super) fn from_response(status: u16, body: &[u8]) -> Option<ProviderHttpFailure> {
    let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let error = &body["error"];
    let code = error["code"].as_str();
    let kind = error["type"].as_str();
    let reason = match status {
        400 if code == Some("context_length_exceeded") => "context_length_exceeded",
        400 if error["param"] == "service_tier" => "service_tier",
        400 => "invalid_request",
        401 => "authentication_failed",
        403 => "access_denied",
        404 => "not_found",
        429 => match code {
            Some(
                code @ ("credit_balance_exhausted"
                | "organization_spend_limit_exceeded"
                | "project_spend_limit_exceeded"
                | "organization_usage_limit_exceeded"
                | "insufficient_quota"
                | "rate_limit_exceeded"
                | "slow_down"),
            ) => code,
            _ if kind == Some("insufficient_quota") => "insufficient_quota",
            _ if kind == Some("rate_limit_error") => "rate_limit_exceeded",
            _ => "request_limited",
        },
        500 => "server_error",
        503 => "server_is_overloaded",
        _ => return None,
    };
    FAILURES
        .iter()
        .find(|failure| failure.code == reason)
        .copied()
}

pub(super) fn from_public_message(message: &str) -> Option<ProviderHttpFailure> {
    FAILURES
        .iter()
        .find(|failure| failure.message == message)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn specific_billing_codes_take_precedence_over_the_broad_quota_type() {
        for code in [
            "credit_balance_exhausted",
            "organization_spend_limit_exceeded",
            "project_spend_limit_exceeded",
            "organization_usage_limit_exceeded",
            "insufficient_quota",
            "rate_limit_exceeded",
            "slow_down",
        ] {
            let body = json!({"error":{"code":code,"type":"insufficient_quota","message":"private-api-key"}}).to_string();
            let failure = from_response(429, body.as_bytes()).unwrap();
            assert_eq!(failure.code, code);
            assert_eq!(
                failure.retryable,
                matches!(code, "rate_limit_exceeded" | "slow_down")
            );
            assert!(!failure.message.contains("private-api-key"));
            assert_eq!(from_public_message(failure.message), Some(failure));
        }
        assert_eq!(
            from_response(429, br#"{"error":{"type":"insufficient_quota"}}"#)
                .unwrap()
                .code,
            "insufficient_quota"
        );
        assert_eq!(
            from_response(429, b"not json").unwrap().code,
            "request_limited"
        );
        assert_eq!(
            from_response(400, br#"{"error":{"param":"service_tier"}}"#)
                .unwrap()
                .code,
            "service_tier"
        );
        // Neither a message mentioning billing nor a code on another status is enough.
        assert_eq!(
            from_response(429, br#"{"error":{"message":"insufficient_quota"}}"#)
                .unwrap()
                .code,
            "request_limited"
        );
        assert_eq!(
            from_response(401, br#"{"error":{"code":"credit_balance_exhausted"}}"#)
                .unwrap()
                .code,
            "authentication_failed"
        );
        for failure in FAILURES {
            assert!(from_public_message(&format!("{} secret", failure.message)).is_none());
        }
    }
}
