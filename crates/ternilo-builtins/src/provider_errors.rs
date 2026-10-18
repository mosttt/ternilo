//! Public, credential-free diagnostics for documented Provider HTTP failures.

use ternilo_protocol::ProviderProtocol;

mod openai;

/// Only fixed diagnostics may cross the Server's upstream-error redaction boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderHttpFailure {
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
}

// https://api-docs.deepseek.com/quick_start/error_codes
const DEEPSEEK_FAILURES: &[(u16, ProviderHttpFailure)] = &[
    (
        400,
        ProviderHttpFailure {
            code: "deepseek_invalid_request",
            retryable: false,
            message: "DeepSeek API error (HTTP 400): the request format is invalid. Check the Provider API protocol and request format; for a shared model, contact its provider.",
        },
    ),
    (
        401,
        ProviderHttpFailure {
            code: "deepseek_authentication_failed",
            retryable: false,
            message: "DeepSeek API error (HTTP 401): API key authentication failed. Check or replace this Provider's API key; for a shared model, contact its provider.",
        },
    ),
    (
        402,
        ProviderHttpFailure {
            code: "deepseek_insufficient_balance",
            retryable: false,
            message: "DeepSeek API error (HTTP 402): the account balance is insufficient. Top up the DeepSeek account that owns this API key at https://platform.deepseek.com/top_up, or choose another model. For a shared model, contact its provider.",
        },
    ),
    (
        422,
        ProviderHttpFailure {
            code: "deepseek_invalid_parameters",
            retryable: false,
            message: "DeepSeek API error (HTTP 422): request parameters are invalid. Check the model ID, reasoning setting and maximum output tokens; for a shared model, contact its provider.",
        },
    ),
    (
        429,
        ProviderHttpFailure {
            code: "deepseek_rate_limited",
            retryable: true,
            message: "DeepSeek API error (HTTP 429): the request rate or concurrency limit was reached. Reduce concurrent requests and try again later.",
        },
    ),
    (
        500,
        ProviderHttpFailure {
            code: "deepseek_server_error",
            retryable: true,
            message: "DeepSeek API error (HTTP 500): the service encountered an internal error. Try again later; if it persists, contact DeepSeek support.",
        },
    ),
    (
        503,
        ProviderHttpFailure {
            code: "deepseek_overloaded",
            retryable: true,
            message: "DeepSeek API error (HTTP 503): the service is overloaded. Try again later or choose another model.",
        },
    ),
];

impl ProviderHttpFailure {
    #[must_use]
    pub fn from_response(
        protocol: ProviderProtocol,
        endpoint: &str,
        status: u16,
        body: &[u8],
    ) -> Option<Self> {
        let deepseek = protocol == ProviderProtocol::DeepSeekResponses
            || reqwest::Url::parse(endpoint)
                .is_ok_and(|url| url.host_str() == Some("api.deepseek.com"));
        if deepseek {
            return DEEPSEEK_FAILURES
                .iter()
                .find(|(code, _)| *code == status)
                .map(|(_, failure)| *failure);
        }
        if matches!(
            protocol,
            ProviderProtocol::OpenAiChatCompletions | ProviderProtocol::OpenAiResponses
        ) {
            return openai::from_response(status, body);
        }
        None
    }

    /// Match the entire canonical message, never an upstream body or a substring.
    #[must_use]
    pub fn from_public_message(message: &str) -> Option<Self> {
        DEEPSEEK_FAILURES
            .iter()
            .find(|(_, failure)| failure.message == message)
            .map(|(_, failure)| *failure)
            .or_else(|| openai::from_public_message(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_errors_require_an_explicit_adapter_or_the_exact_official_host() {
        for status in [400, 401, 402, 422, 429, 500, 503] {
            let expected = ProviderHttpFailure::from_response(
                ProviderProtocol::DeepSeekResponses,
                "http://127.0.0.1/v1/responses",
                status,
                &[],
            )
            .unwrap();
            assert_eq!(
                ProviderHttpFailure::from_response(
                    ProviderProtocol::OpenAiChatCompletions,
                    "https://api.deepseek.com/chat/completions",
                    status,
                    &[]
                ),
                Some(expected)
            );
            assert_eq!(
                ProviderHttpFailure::from_public_message(expected.message),
                Some(expected)
            );
            assert!(
                ProviderHttpFailure::from_public_message(&format!(
                    "{} private-key",
                    expected.message
                ))
                .is_none()
            );
            for endpoint in [
                "https://example.test/deepseek",
                "https://api.deepseek.com.example.test/v1",
                "https://api.deepseek.com@example.test/v1",
            ] {
                assert!(
                    ProviderHttpFailure::from_response(
                        ProviderProtocol::OpenAiChatCompletions,
                        endpoint,
                        status,
                        &[]
                    )
                    .is_none_or(|failure| !failure.code.starts_with("deepseek_"))
                );
            }
        }
        assert!(
            ProviderHttpFailure::from_response(
                ProviderProtocol::DeepSeekResponses,
                "https://api.deepseek.com/v1",
                403,
                &[]
            )
            .is_none()
        );
    }
}
