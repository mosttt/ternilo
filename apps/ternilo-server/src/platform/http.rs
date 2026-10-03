use std::time::{SystemTime, UNIX_EPOCH};

use salvo_core::{
    http::{StatusCode, header},
    prelude::{Json, Request, Response, Scribe},
};
use serde_json::json;
use ternilo_protocol::{ErrorCode, HarnessError, TenantId};

#[derive(Debug)]
pub(crate) struct ApiError {
    error: HarnessError,
    status: Option<StatusCode>,
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    pub(crate) fn rate_limited(message: &str) -> Self {
        Self {
            retry_after_seconds: None,
            error: HarnessError::policy(message),
            status: Some(StatusCode::TOO_MANY_REQUESTS),
        }
    }
    pub(crate) fn unauthorized(error: HarnessError) -> Self {
        Self {
            retry_after_seconds: None,
            error,
            status: Some(StatusCode::UNAUTHORIZED),
        }
    }

    pub(crate) fn unavailable(error: HarnessError) -> Self {
        Self {
            retry_after_seconds: None,
            error,
            status: Some(StatusCode::SERVICE_UNAVAILABLE),
        }
    }
}

impl From<HarnessError> for ApiError {
    fn from(error: HarnessError) -> Self {
        Self {
            retry_after_seconds: None,
            error,
            status: None,
        }
    }
}

impl From<ternilo_control::ModelAccessError> for ApiError {
    fn from(value: ternilo_control::ModelAccessError) -> Self {
        use ternilo_control::ModelAccessErrorKind;
        let mut result = Self::from(value.error);
        match value.kind {
            ModelAccessErrorKind::RateLimited {
                retry_after_seconds,
            } => {
                result.status = Some(StatusCode::TOO_MANY_REQUESTS);
                result.retry_after_seconds = Some(retry_after_seconds);
            }
            ModelAccessErrorKind::QuotaExceeded => {
                result.status = Some(StatusCode::TOO_MANY_REQUESTS);
            }
            ModelAccessErrorKind::Unauthorized => result.status = Some(StatusCode::UNAUTHORIZED),
            _ => {}
        }
        result
    }
}

impl Scribe for ApiError {
    fn render(self, response: &mut Response) {
        let status = self.status.unwrap_or(match self.error.code {
            ErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
            ErrorCode::Composition => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::PolicyDenied => StatusCode::FORBIDDEN,
            ErrorCode::Execution => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Cancelled | ErrorCode::Conflict => StatusCode::CONFLICT,
        });
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                "Bearer".parse().expect("valid authentication challenge"),
            );
        }
        if let Some(seconds) = self.retry_after_seconds {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                seconds.to_string().parse().expect("integer Retry-After"),
            );
        }
        response.status_code(status);
        if self.error.code == ErrorCode::Execution {
            eprintln!("{}", self.error);
            response.render(Json(json!({
                "error": HarnessError::execution("control-plane request failed")
            })));
        } else {
            response.render(Json(json!({ "error": self.error })));
        }
    }
}

pub(crate) fn path_parameter(request: &Request, name: &str) -> Result<String, ApiError> {
    request.try_param(name).map_err(invalid_request)
}

pub(crate) fn bearer_token(request: &Request) -> Result<&str, ApiError> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized(HarnessError::policy("bearer token is required")))?;
    let token = value.strip_prefix("Bearer ").unwrap_or_default();
    if token.is_empty() || token.chars().any(char::is_whitespace) {
        Err(ApiError::unauthorized(HarnessError::policy(
            "authorization must contain one Bearer token",
        )))
    } else {
        Ok(token)
    }
}

pub(crate) fn tenant_parameter(request: &Request) -> Result<TenantId, ApiError> {
    let value = request
        .param::<String>("tenant_id")
        .or_else(|| {
            request
                .headers()
                .get("x-ternilo-tenant")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        })
        .ok_or_else(|| HarnessError::invalid("tenant context is required"))?;
    let tenant_id = TenantId::new(value);
    tenant_id.validate()?;
    Ok(tenant_id)
}

pub(crate) fn invalid_request(error: impl std::fmt::Display) -> ApiError {
    HarnessError::invalid(format!("invalid HTTP request: {error}")).into()
}

pub(crate) fn now_ms() -> Result<u64, HarnessError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            HarnessError::execution(format!("system clock is before Unix epoch: {error}"))
        })?
        .as_millis();
    milliseconds
        .try_into()
        .map_err(|_| HarnessError::execution("system timestamp exceeds u64 milliseconds"))
}
