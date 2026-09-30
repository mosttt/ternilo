use salvo_core::prelude::{Depot, FlowCtrl, Request, Response, Scribe, handler};
use ternilo_control::{ControlUser, IdentitySession, OidcPrincipal};
use ternilo_protocol::{ErrorCode, HarnessError};

use crate::platform::{
    http::{ApiError, bearer_token, now_ms},
    state::{AppState, app_state},
};

#[handler]
pub(crate) async fn user_auth(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
    control: &mut FlowCtrl,
) {
    match authenticate_user(request, app_state(depot)).await {
        Ok(session) => {
            depot.insert_typed(session.user.clone());
            depot.insert_typed(session);
            control.call_next(request, depot, response).await;
        }
        Err(error) => {
            error.render(response);
            control.skip_rest();
        }
    }
}

/// Verify the credential; callers must also apply the current instance policy.
pub(crate) async fn authenticate_token(
    state: &AppState,
    token: &str,
) -> Result<(ControlUser, Option<u64>), HarnessError> {
    if token.starts_with("ter_a_") {
        let (user, expiry) = state
            .store
            .authenticate_native_token(token, now_ms()?)
            .await?;
        return Ok((user, Some(expiry)));
    }
    let (principal, expiry) = authenticate_oidc_identity(state, token).await?;
    let user = state
        .store
        .authenticate_oidc_user(&principal, now_ms()?)
        .await?;
    Ok((user, expiry))
}

pub(crate) async fn authenticate_oidc_identity(
    state: &AppState,
    token: &str,
) -> Result<(OidcPrincipal, Option<u64>), HarnessError> {
    let runtime = state.security.current(&state.store).await?;
    if token.starts_with("ter_o_") {
        let web = runtime
            .web_auth
            .as_ref()
            .ok_or_else(|| HarnessError::policy("OIDC is not configured on this server"))?;
        let (principal, expiry) = state
            .store
            .authenticate_oidc_session(token, &web.binding, now_ms()?)
            .await?;
        return Ok((principal, Some(expiry)));
    }
    let auth = runtime
        .auth
        .as_ref()
        .ok_or_else(|| HarnessError::policy("browser session is invalid or expired"))?;
    Ok((auth.authenticate(token).await?, None))
}

async fn authenticate_user(
    request: &Request,
    state: &AppState,
) -> Result<IdentitySession, ApiError> {
    let token = bearer_token(request)?;
    let (user, expires_at_ms) = authenticate_token(state, token)
        .await
        .map_err(authentication_error)?;
    let mut session = state.store.identity_session(user).await?;
    session.expires_at_ms = expires_at_ms;
    super::identity::session_details::record(state, request, &session.user, token).await?;
    Ok(session)
}

pub(crate) fn authentication_error(error: HarnessError) -> ApiError {
    if error.code == ErrorCode::PolicyDenied
        && matches!(
            error.message.as_str(),
            "account registration is pending approval"
                | "account registration was rejected"
                | "account is banned"
                | "account was removed"
                | "registration requires an administrator invitation"
                | "choose a platform username to finish registration"
        )
    {
        return ApiError::from(error);
    }
    if matches!(
        error.code,
        ErrorCode::InvalidInput | ErrorCode::PolicyDenied
    ) {
        ApiError::unauthorized(error)
    } else {
        ApiError::from(error)
    }
}
