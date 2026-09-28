use salvo_core::prelude::{Depot, Json, Request, Router, handler};
use ternilo_control::{BrowserSessionAuthentication, BrowserSessionRevocation, BrowserSessions};

use crate::platform::{
    auth::authentication_error,
    http::{ApiError, bearer_token, now_ms, path_parameter},
    state::{AppState, actor, app_state},
};

pub(super) fn router() -> Router {
    Router::with_path("auth/sessions")
        .get(list)
        .push(Router::with_path("revoke-others").post(revoke_others))
        .push(Router::with_path("{session_id}").delete(revoke))
}

async fn authentication(
    request: &Request,
    state: &AppState,
) -> Result<BrowserSessionAuthentication, ApiError> {
    let token = bearer_token(request)?;
    if token.starts_with("kns_") {
        return Ok(BrowserSessionAuthentication::NativeToken(token.to_owned()));
    }
    let (principal, _) = crate::platform::auth::authenticate_oidc_identity(state, token)
        .await
        .map_err(authentication_error)?;
    Ok(BrowserSessionAuthentication::VerifiedOidc(principal))
}

#[handler]
async fn list(request: &mut Request, depot: &mut Depot) -> Result<Json<BrowserSessions>, ApiError> {
    let state = app_state(depot);
    let authentication = authentication(request, state).await?;
    Ok(Json(
        state
            .store
            .list_browser_sessions(actor(depot), &authentication, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn revoke(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserSessionRevocation>, ApiError> {
    let state = app_state(depot);
    let authentication = authentication(request, state).await?;
    let session_id = path_parameter(request, "session_id")?;
    let result = state
        .store
        .revoke_browser_session(actor(depot), &authentication, &session_id, now_ms()?)
        .await?;
    if result.revoked_count > 0 {
        state
            .cloud_events
            .reauthenticate_user(&actor(depot).user_id);
    }
    Ok(Json(result))
}

#[handler]
async fn revoke_others(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserSessionRevocation>, ApiError> {
    let state = app_state(depot);
    let authentication = authentication(request, state).await?;
    let result = state
        .store
        .revoke_other_browser_sessions(actor(depot), &authentication, now_ms()?)
        .await?;
    if result.revoked_count > 0 {
        state
            .cloud_events
            .reauthenticate_user(&actor(depot).user_id);
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests;
