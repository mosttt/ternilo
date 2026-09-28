use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use salvo_extra::size_limiter::max_size;
use serde::Deserialize;
use ternilo_control::{ModelAccessError, ModelAccessErrorKind, ModelDeviceUsage, PageQuery};
use ternilo_protocol::{
    ModelDeviceAuthorization, ModelDeviceIdentity, ModelDeviceLimits, ModelDevicePage,
    ModelDevicePoll, ModelDeviceReview, ModelDeviceScope, ModelDeviceSession,
};

use crate::platform::{
    http::{ApiError, bearer_token, invalid_request, now_ms, path_parameter},
    identity::no_store,
    state::{actor, app_state},
};

pub(crate) fn public_router() -> Router {
    Router::new()
        .hoop(no_store)
        .hoop(max_size(4096))
        .push(Router::with_path("api/v1/model-device/authorize").post(begin))
        .push(Router::with_path("api/v1/model-device/token").post(poll))
        .push(
            Router::with_path("v1/model-device")
                .get(session)
                .delete(disconnect),
        )
}

pub(super) fn review_router() -> Router {
    Router::with_path("device-authorization")
        .get(review)
        .post(decide)
}

pub(super) fn devices_router() -> Router {
    Router::with_path("devices").get(list).push(
        Router::with_path("{key_id}")
            .patch(update_limits)
            .delete(revoke)
            .push(Router::with_path("usage").get(usage)),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    device_name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Poll {
    device_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    user_code: String,
    scope: Option<ModelDeviceScope>,
    #[serde(default)]
    limits: ModelDeviceLimits,
}

#[handler]
async fn begin(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelDeviceAuthorization>, ApiError> {
    let input = request
        .parse_json::<Begin>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .begin_model_device_authorization(&input.device_name, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn poll(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelDevicePoll>, ApiError> {
    let input = request
        .parse_json::<Poll>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .poll_model_device_authorization(&input.device_code, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn review(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelDeviceReview>, ApiError> {
    let code = request
        .query::<String>("user_code")
        .ok_or_else(|| ternilo_protocol::HarnessError::invalid("user_code is required"))?;
    Ok(Json(
        app_state(depot)
            .store
            .review_model_device_authorization(actor(depot), &code, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn decide(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let input = request
        .parse_json::<Decision>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .decide_model_device_authorization(
            actor(depot),
            &input.user_code,
            input.scope.as_ref(),
            &input.limits,
            now_ms()?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[handler]
async fn list(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelDevicePage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_devices(actor(depot), &query)
            .await?,
    ))
}
#[handler]
async fn session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelDeviceSession>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .model_device_session(bearer_token(request)?, request.query("cursor"), now_ms()?)
            .await
            .map_err(device_access_error)?,
    ))
}
#[handler]
async fn disconnect(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    app_state(depot)
        .store
        .disconnect_model_device(bearer_token(request)?, now_ms()?)
        .await
        .map_err(device_access_error)?;
    Ok(StatusCode::NO_CONTENT)
}
fn device_access_error(error: ModelAccessError) -> ApiError {
    if error.kind == ModelAccessErrorKind::Unauthorized {
        ApiError::unauthorized(error.error)
    } else {
        error.error.into()
    }
}

#[handler]
async fn update_limits(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelDeviceIdentity>, ApiError> {
    let limits = request
        .parse_json::<ModelDeviceLimits>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .update_model_device_limits(
                actor(depot),
                &path_parameter(request, "key_id")?,
                &limits,
                now_ms()?,
            )
            .await?,
    ))
}

#[handler]
async fn usage(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelDeviceUsage>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .model_device_usage(actor(depot), &path_parameter(request, "key_id")?, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn revoke(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    app_state(depot)
        .store
        .revoke_model_device(actor(depot), &path_parameter(request, "key_id")?, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
