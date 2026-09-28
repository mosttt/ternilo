use salvo_core::prelude::{Depot, Json, Request, Response, StatusCode, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::{
    ModelEntitlementPage, ModelKeyCreation, ModelKeyInput, ModelKeyPage, ModelRequestOrigin,
    ModelRequestPage, ModelRequestSource, ModelRequestState, ModelServiceAttempt,
    ModelServiceRequest, ModelServiceUsageReport, PageQuery, ServiceModelUsage,
    WorkloadModelPrincipal,
};
use ternilo_protocol::{ProviderProtocol, RunId, SessionId, UserId};

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

#[handler]
pub(super) async fn catalog(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelEntitlementPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_entitlements(actor(depot), &query, now_ms()?)
            .await?,
    ))
}

#[handler]
pub(super) async fn keys(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelKeyPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_keys(actor(depot), &query)
            .await?,
    ))
}

#[handler]
pub(super) async fn create_key(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<ModelKeyCreation>, ApiError> {
    let input = request
        .parse_json::<ModelKeyInput>()
        .await
        .map_err(invalid_request)?;
    let key = app_state(depot)
        .store
        .create_model_key(actor(depot), &input, now_ms()?)
        .await?;
    response.status_code(StatusCode::CREATED);
    Ok(Json(key))
}

#[handler]
pub(super) async fn revoke_key(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "key_id")?;
    app_state(depot)
        .store
        .revoke_model_key(actor(depot), &id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct UserRequestPage {
    requests: Vec<UserModelRequest>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct UserModelRequest {
    request_id: String,
    origin: ModelRequestOrigin,
    source: ModelRequestSource,
    key_id: Option<String>,
    actor_user_id: UserId,
    resource_owner_user_id: Option<UserId>,
    model_beneficiary_user_id: UserId,
    grant_id: Option<String>,
    grant_name: Option<String>,
    workload: Option<UserWorkload>,
    model_id: String,
    protocol: ProviderProtocol,
    state: ModelRequestState,
    attempted: bool,
    reserved_tokens: u64,
    accounted_tokens: Option<u64>,
    usage: Option<UserModelUsage>,
    error_code: Option<UserModelFailure>,
    month: String,
    created_at_ms: u64,
    expires_at_ms: u64,
    settled_at_ms: Option<u64>,
    attempts: Vec<UserModelAttempt>,
}

#[derive(Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "The safe workload projection identifies each entity without exposing its private record."
)]
struct UserWorkload {
    session_id: SessionId,
    run_id: RunId,
    actor_user_id: UserId,
    resource_owner_user_id: UserId,
}

impl From<WorkloadModelPrincipal> for UserWorkload {
    fn from(workload: WorkloadModelPrincipal) -> Self {
        Self {
            session_id: workload.session_id,
            run_id: workload.run_id,
            actor_user_id: workload.actor_user_id,
            resource_owner_user_id: workload.resource_owner_user_id,
        }
    }
}

#[derive(Serialize)]
struct UserModelAttempt {
    attempt: u32,
    state: ModelRequestState,
    attempted: bool,
    reserved_tokens: u64,
    accounted_tokens: Option<u64>,
    usage: Option<UserModelUsage>,
    error_code: Option<UserModelFailure>,
    created_at_ms: u64,
    settled_at_ms: Option<u64>,
}

impl From<ModelServiceAttempt> for UserModelAttempt {
    fn from(attempt: ModelServiceAttempt) -> Self {
        Self {
            attempt: attempt.attempt,
            state: attempt.state,
            attempted: attempt.attempted,
            reserved_tokens: attempt.reserved_tokens,
            accounted_tokens: attempt.accounted_tokens,
            usage: attempt.usage.map(UserModelUsage::from),
            error_code: public_failure(attempt.error_code.as_deref()),
            created_at_ms: attempt.created_at_ms,
            settled_at_ms: attempt.settled_at_ms,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum UserModelFailure {
    AccessDenied,
    QuotaExceeded,
    ModelBusy,
    Cancelled,
    RequestExpired,
    StreamInterrupted,
    ModelConfigurationChanged,
    InvalidRequest,
    RequestConflict,
    UpstreamFailed,
}

fn public_failure(code: Option<&str>) -> Option<UserModelFailure> {
    Some(match code? {
        "model_access_denied" | "invalid_api_key" | "policy_denied" => {
            UserModelFailure::AccessDenied
        }
        "quota_exceeded" => UserModelFailure::QuotaExceeded,
        "upstream_http_429" | "unavailable" => UserModelFailure::ModelBusy,
        "request_cancelled" | "cancelled" => UserModelFailure::Cancelled,
        "request_expired" => UserModelFailure::RequestExpired,
        "upstream_stream_error" | "upstream_stream_incomplete" => {
            UserModelFailure::StreamInterrupted
        }
        "model_configuration_changed" | "composition" => {
            UserModelFailure::ModelConfigurationChanged
        }
        "invalid_request" | "invalid_input" => UserModelFailure::InvalidRequest,
        "request_conflict" | "conflict" => UserModelFailure::RequestConflict,
        _ => UserModelFailure::UpstreamFailed,
    })
}

#[derive(Serialize)]
#[expect(
    clippy::struct_field_names,
    reason = "The public usage schema labels every count explicitly in tokens."
)]
struct UserModelUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cached_input_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}

impl From<ServiceModelUsage> for UserModelUsage {
    fn from(usage: ServiceModelUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }
}

impl From<ModelServiceRequest> for UserModelRequest {
    fn from(request: ModelServiceRequest) -> Self {
        Self {
            request_id: request.request_id,
            origin: request.origin,
            source: request.source,
            key_id: request.key_id,
            actor_user_id: request.actor_user_id,
            resource_owner_user_id: request.resource_owner_user_id,
            model_beneficiary_user_id: request.model_beneficiary_user_id,
            grant_id: request.grant_id,
            grant_name: request.grant_name,
            workload: request.workload.map(UserWorkload::from),
            model_id: request.model_id,
            protocol: request.protocol,
            state: request.state,
            attempted: request.attempted,
            reserved_tokens: request.reserved_tokens,
            accounted_tokens: request.accounted_tokens,
            usage: request.usage.map(UserModelUsage::from),
            error_code: public_failure(request.error_code.as_deref()),
            month: request.month,
            created_at_ms: request.created_at_ms,
            expires_at_ms: request.expires_at_ms,
            settled_at_ms: request.settled_at_ms,
            attempts: request
                .attempts
                .into_iter()
                .map(UserModelAttempt::from)
                .collect(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UsageQuery {
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    source: Option<ModelRequestSource>,
}

#[handler]
pub(super) async fn requests(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<UserRequestPage>, ApiError> {
    let query = request
        .parse_queries::<UsageQuery>()
        .map_err(invalid_request)?;
    let user = actor(depot);
    let page_query = PageQuery {
        query: query.query,
        cursor: query.cursor,
        limit: query.limit.unwrap_or_else(|| PageQuery::default().limit),
    };
    let page = app_state(depot)
        .store
        .list_model_service_requests_by_source(user, Some(&user.user_id), &page_query, query.source)
        .await?;
    Ok(Json(UserRequestPage {
        requests: page
            .requests
            .into_iter()
            .map(UserModelRequest::from)
            .collect(),
        next_cursor: page.next_cursor,
    }))
}

#[handler]
pub(super) async fn user_usage(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelServiceUsageReport>, ApiError> {
    let query = request
        .parse_queries::<UsageQuery>()
        .map_err(invalid_request)?;
    let user = actor(depot);
    Ok(Json(
        app_state(depot)
            .store
            .model_service_usage_by_source(user, Some(&user.user_id), now_ms()?, query.source)
            .await?,
    ))
}

#[handler]
pub(super) async fn admin_requests(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelRequestPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_service_requests(actor(depot), None, &query)
            .await?,
    ))
}

#[handler]
pub(super) async fn admin_usage(
    depot: &mut Depot,
) -> Result<Json<ModelServiceUsageReport>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .model_service_usage(actor(depot), None, now_ms()?)
            .await?,
    ))
}
