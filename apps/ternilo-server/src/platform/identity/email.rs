use super::{ApiError, actor, app_state, invalid_request, now_ms, session_details};
use salvo_core::prelude::{Depot, Json, Request, handler};
use serde::Deserialize;
use ternilo_protocol::HarnessError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryRequest {
    email: String,
    turnstile_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetRequest {
    token: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationRequest {
    token: String,
}

#[handler]
pub(super) async fn status(
    depot: &mut Depot,
) -> Result<Json<ternilo_control::AccountEmailStatus>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .account_email_status(actor(depot))
            .await?,
    ))
}

#[handler]
pub(super) async fn send_verification(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = app_state(depot);
    let runtime = state.security.current(&state.store).await?;
    let mailer = runtime
        .mailer
        .as_ref()
        .ok_or_else(|| HarnessError::unavailable("account email delivery is not configured"))?;
    let now = now_ms()?;
    let client = session_details::request_ip(state, request)
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    if !state.store.admit_email_delivery(&client, now).await? {
        return Err(ApiError::rate_limited(
            "too many email requests; try again in one minute",
        ));
    }
    let _slot = mailer
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| HarnessError::unavailable("email delivery is busy; try again shortly"))?;
    if let Some(delivery) = state
        .store
        .request_email_verification(actor(depot), now)
        .await?
    {
        mailer.send(delivery, false).await?;
    }
    Ok(Json(serde_json::json!({"accepted":true})))
}

#[handler]
pub(super) async fn verify(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let body = request
        .parse_json::<VerificationRequest>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .verify_account_email(actor(depot), &body.token, now_ms()?)
        .await?;
    Ok(Json(serde_json::json!({"verified":true})))
}

#[handler]
pub(super) async fn request_recovery(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = app_state(depot);
    let runtime = state.security.current(&state.store).await?;
    let mailer = runtime
        .mailer
        .clone()
        .ok_or_else(|| HarnessError::unavailable("account email delivery is not configured"))?;
    let body = request
        .parse_json::<RecoveryRequest>()
        .await
        .map_err(invalid_request)?;
    runtime
        .verify_turnstile(body.turnstile_token.as_deref(), "password_recovery")
        .await?;
    let client = session_details::request_ip(state, request)
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    let now = now_ms()?;
    if !state.store.admit_email_delivery(&client, now).await? {
        return Err(ApiError::rate_limited(
            "too many email requests; try again in one minute",
        ));
    }
    // Complete account lookup and SMTP work after the same public response for every address.
    let slot = mailer
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| HarnessError::unavailable("email delivery is busy; try again shortly"))?;
    {
        let store = state.store.clone();
        tokio::spawn(async move {
            let _slot = slot;
            match store.request_password_recovery(&body.email, now).await {
                Ok(Some(delivery)) => {
                    if mailer.send(delivery, true).await.is_err() {
                        eprintln!("account recovery email delivery failed");
                    }
                }
                Ok(None) => {}
                Err(_) => eprintln!("account recovery email preparation failed"),
            }
        });
    }
    Ok(Json(serde_json::json!({"accepted":true})))
}

#[handler]
pub(super) async fn reset_password(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let state = app_state(depot);
    if state.security.current(&state.store).await?.mailer.is_none() {
        return Err(HarnessError::unavailable("account email recovery is disabled").into());
    }
    let client = session_details::request_ip(state, request)
        .map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    if !state.store.admit_email_delivery(&client, now_ms()?).await? {
        return Err(ApiError::rate_limited(
            "too many email requests; try again in one minute",
        ));
    }
    let body = request
        .parse_json::<ResetRequest>()
        .await
        .map_err(invalid_request)?;
    let result = state
        .store
        .recover_native_password(&body.token, &body.password, now_ms()?)
        .await?;
    state.cloud_events.reauthenticate_user(&result.user.user_id);
    Ok(Json(serde_json::json!({"completed":true})))
}
