use super::{ApiError, actor, app_state, invalid_request, now_ms};
use salvo_core::prelude::{Depot, Json, Request, Router, handler};
use serde::Deserialize;

pub(super) fn router() -> Router {
    Router::with_path("auth/mfa")
        .get(status)
        .push(Router::with_path("setup").post(begin))
        .push(Router::with_path("enable").post(enable))
        .push(Router::with_path("disable").post(disable))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Password {
    current_password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enable {
    current_password: String,
    generation: String,
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Disable {
    current_password: String,
    code: String,
}

#[handler]
async fn status(depot: &mut Depot) -> Result<Json<ternilo_control::MfaStatus>, ApiError> {
    Ok(Json(app_state(depot).store.mfa_status(actor(depot)).await?))
}
#[handler]
async fn begin(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_control::MfaEnrollment>, ApiError> {
    let body = request
        .parse_json::<Password>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .begin_mfa_enrollment(actor(depot), &body.current_password, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn enable(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = request
        .parse_json::<Enable>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .activate_mfa(
            actor(depot),
            &body.current_password,
            &body.generation,
            &body.code,
            now_ms()?,
        )
        .await?;
    state
        .cloud_events
        .reauthenticate_user(&actor(depot).user_id);
    Ok(Json(serde_json::json!({"enabled":true})))
}
#[handler]
async fn disable(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = request
        .parse_json::<Disable>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .disable_mfa(actor(depot), &body.current_password, &body.code, now_ms()?)
        .await?;
    state
        .cloud_events
        .reauthenticate_user(&actor(depot).user_id);
    Ok(Json(serde_json::json!({"enabled":false})))
}
