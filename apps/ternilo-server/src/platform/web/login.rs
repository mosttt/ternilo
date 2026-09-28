use salvo_core::{
    http::{HeaderValue, header},
    prelude::{Depot, Json, Request, Response, handler},
};
use ternilo_control::{OidcSessionGrant, OidcSessionIdentity};
use ternilo_protocol::HarnessError;

use super::{
    ApiError, BrowserTokenResponse, CodeExchangeRequest, ProviderTokenResponse, RefreshRequest,
    app_state, exchange, valid_pkce_verifier,
};
use crate::platform::{auth::authentication_error, http::now_ms};

fn browser_tokens(
    grant: OidcSessionGrant,
    scope: Option<String>,
    now: u64,
) -> BrowserTokenResponse {
    BrowserTokenResponse {
        access_token: grant.access_token,
        expires_in: grant.expires_at_ms.saturating_sub(now) / 1000,
        refresh_token: grant.refresh_token,
        scope,
    }
}

fn expiry(
    token: &ProviderTokenResponse,
    id_expiry: Option<u64>,
    now: u64,
) -> Result<u64, HarnessError> {
    let expiry = now
        .saturating_add(token.expires_in.unwrap_or(300).min(3600) * 1000)
        .min(id_expiry.map_or(u64::MAX, |seconds| seconds.saturating_mul(1000)));
    if expiry <= now.saturating_add(5_000) {
        return Err(HarnessError::policy("OIDC token expires too soon"));
    }
    Ok(expiry)
}

#[handler]
pub(super) async fn exchange_code(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<BrowserTokenResponse>, ApiError> {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let body = request
        .parse_json::<CodeExchangeRequest>()
        .await
        .map_err(crate::platform::invalid_request)?;
    if body.code.is_empty()
        || body.code.len() > 16 * 1024
        || !valid_pkce_verifier(&body.code_verifier)
        || !(16..=256).contains(&body.nonce.len())
        || !body
            .nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HarnessError::invalid("invalid OIDC code, PKCE verifier or nonce").into());
    }
    let state = app_state(depot);
    let runtime = state.security.current(&state.store).await?;
    let web = runtime
        .web_auth
        .as_ref()
        .ok_or_else(|| HarnessError::policy("OIDC is not enabled on this server"))?;
    let validator = runtime
        .auth
        .as_ref()
        .ok_or_else(|| HarnessError::policy("OIDC is not enabled on this server"))?;
    let token = exchange(
        web,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &web.client_id),
            ("redirect_uri", &web.redirect_uri),
            ("code", &body.code),
            ("code_verifier", &body.code_verifier),
        ],
    )
    .await?;
    let now = now_ms()?;
    let id_token = token.id_token.as_deref().ok_or_else(|| {
        authentication_error(HarnessError::policy("OIDC token response has no ID Token"))
    })?;
    let identity = validator
        .authenticate_id_token(
            id_token,
            &web.client_id,
            &body.nonce,
            &token.access_token,
            false,
            now / 1000,
        )
        .await
        .map_err(authentication_error)?;
    let principal = validator
        .userinfo(&token.access_token, identity.principal)
        .await
        .map_err(authentication_error)?;
    let expires_at_ms = expiry(&token, Some(identity.expires_at), now)?;
    let grant = state
        .store
        .create_oidc_session(
            &OidcSessionIdentity {
                principal,
                nonce: body.nonce,
                upstream_refresh_token: token.refresh_token,
            },
            &web.binding,
            expires_at_ms,
            now,
        )
        .await
        .map_err(authentication_error)?;
    Ok(Json(browser_tokens(grant, token.scope, now)))
}

#[handler]
pub(super) async fn refresh_token(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<BrowserTokenResponse>, ApiError> {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let body = request
        .parse_json::<RefreshRequest>()
        .await
        .map_err(crate::platform::invalid_request)?;
    let state = app_state(depot);
    let runtime = state.security.current(&state.store).await?;
    let web = runtime
        .web_auth
        .as_ref()
        .ok_or_else(|| HarnessError::policy("OIDC is not enabled on this server"))?;
    let validator = runtime
        .auth
        .as_ref()
        .ok_or_else(|| HarnessError::policy("OIDC is not enabled on this server"))?;
    let previous = state
        .store
        .oidc_refresh_session(&body.refresh_token, &web.binding, now_ms()?)
        .await
        .map_err(authentication_error)?;
    let upstream_refresh = previous
        .identity
        .upstream_refresh_token
        .as_deref()
        .ok_or_else(|| {
            authentication_error(HarnessError::policy("OIDC session cannot be refreshed"))
        })?;
    let token = exchange(
        web,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &web.client_id),
            ("refresh_token", upstream_refresh),
        ],
    )
    .await?;
    let now = now_ms()?;
    let mut id_expiry = None;
    let principal = if let Some(id_token) = &token.id_token {
        let verified = validator
            .authenticate_id_token(
                id_token,
                &web.client_id,
                &previous.identity.nonce,
                &token.access_token,
                true,
                now / 1000,
            )
            .await
            .map_err(authentication_error)?;
        if verified.principal.subject != previous.identity.principal.subject
            || verified.principal.issuer != previous.identity.principal.issuer
        {
            return Err(authentication_error(HarnessError::policy(
                "OIDC refreshed identity does not match this session",
            )));
        }
        id_expiry = Some(verified.expires_at);
        verified.principal
    } else {
        previous.identity.principal
    };
    let principal = validator
        .userinfo(&token.access_token, principal)
        .await
        .map_err(authentication_error)?;
    let expires_at_ms = expiry(&token, id_expiry, now)?.min(previous.expires_at_ms);
    let grant = state
        .store
        .replace_oidc_session(
            &OidcSessionIdentity {
                principal,
                nonce: previous.identity.nonce,
                upstream_refresh_token: token
                    .refresh_token
                    .or(previous.identity.upstream_refresh_token),
            },
            &web.binding,
            expires_at_ms,
            (&body.refresh_token, previous.expires_at_ms),
            now,
        )
        .await
        .map_err(authentication_error)?;
    Ok(Json(browser_tokens(grant, token.scope, now)))
}
