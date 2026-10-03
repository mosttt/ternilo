use salvo_core::{
    http::{StatusCode, header},
    prelude::{Json, Response, Scribe},
};
use serde_json::{Value, json};
use ternilo_control::{ModelAccessError, ModelAccessErrorKind};
use ternilo_protocol::{ErrorCode, HarnessError, ProviderProtocol};

#[derive(Debug)]
pub(super) struct GatewayError {
    pub(super) status: StatusCode,
    pub(super) code: &'static str,
    pub(super) message: String,
    retry_after_seconds: Option<u64>,
}

impl GatewayError {
    pub(super) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after_seconds: None,
        }
    }

    pub(super) fn upstream(code: &'static str, message: &str) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, code, message)
    }

    pub(super) fn cancelled(message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, "request_cancelled", message)
    }

    pub(super) fn value(&self) -> Value {
        json!({"error": {
            "message": self.message,
            "type": if self.status.is_server_error() { "server_error" } else { "invalid_request_error" },
            "param": null,
            "code": self.code,
        }})
    }

    pub(super) fn stream_value(&self, protocol: ProviderProtocol) -> Value {
        match protocol {
            ProviderProtocol::OpenAiChatCompletions | ProviderProtocol::GoogleGemini => {
                self.value()
            }
            ProviderProtocol::AnthropicMessages => {
                json!({"type": "error", "error": {"type": self.code, "message": self.message}})
            }
            ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => json!({
                "type": "error", "code": self.code, "message": self.message, "param": null,
            }),
        }
    }
}

impl From<ModelAccessError> for GatewayError {
    fn from(error: ModelAccessError) -> Self {
        let (status, code) = match error.kind {
            ModelAccessErrorKind::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid_api_key"),
            ModelAccessErrorKind::Forbidden => (StatusCode::FORBIDDEN, "model_access_denied"),
            ModelAccessErrorKind::InvalidInput => (StatusCode::BAD_REQUEST, "invalid_request"),
            ModelAccessErrorKind::Conflict => (StatusCode::CONFLICT, "request_conflict"),
            ModelAccessErrorKind::RateLimited { .. } => {
                (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
            }
            ModelAccessErrorKind::QuotaExceeded => {
                (StatusCode::TOO_MANY_REQUESTS, "quota_exceeded")
            }
            ModelAccessErrorKind::Internal => {
                (StatusCode::INTERNAL_SERVER_ERROR, "model_service_error")
            }
        };
        let message = if error.kind == ModelAccessErrorKind::Internal {
            eprintln!("model service: {}", error.error);
            "model service could not complete the request".to_owned()
        } else {
            error.error.message
        };
        let mut result = Self::new(status, code, message);
        if let ModelAccessErrorKind::RateLimited {
            retry_after_seconds,
        } = error.kind
        {
            result.retry_after_seconds = Some(retry_after_seconds);
        }
        result
    }
}

impl From<HarnessError> for GatewayError {
    fn from(error: HarnessError) -> Self {
        if error.code == ErrorCode::InvalidInput {
            Self::new(StatusCode::BAD_REQUEST, "invalid_request", error.message)
        } else {
            eprintln!("model service: {error}");
            Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "model_service_error",
                "model service could not complete the request",
            )
        }
    }
}

impl Scribe for GatewayError {
    fn render(self, response: &mut Response) {
        if let Some(seconds) = self.retry_after_seconds {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                seconds.to_string().parse().expect("integer Retry-After"),
            );
        }
        response.status_code(self.status);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            "no-store".parse().expect("static cache policy"),
        );
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                "Bearer".parse().expect("static challenge"),
            );
        }
        response.render(Json(self.value()));
    }
}
