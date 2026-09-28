use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Response, Router, handler},
};
use salvo_extra::size_limiter::max_size;
use serde::Deserialize;
use ternilo_control::{
    AccountListQuery, AccountPage, AccountRecord, AccountStatusAction, InstanceMode,
    PlatformAction, PlatformRole, RegistrationDecision, RegistrationMode, RegistrationSettings,
    UserInvitationGrant, UserInvitationRequest,
};
use ternilo_protocol::UserId;

use super::{
    auth,
    http::{ApiError, invalid_request, now_ms, path_parameter},
    identity::{BrowserInstance, browser_instance, no_store},
    state::{actor, app_state},
};

#[cfg(test)]
mod tests;

pub(crate) fn router() -> Router {
    Router::with_path("api/v1/admin")
        .hoop(auth::user_auth)
        .hoop(no_store)
        .hoop(max_size(16 * 1024))
        .push(
            Router::with_path("accounts")
                .get(list_accounts)
                .push(Router::with_path("{user_id}/role").patch(set_account_role))
                .push(Router::with_path("{user_id}/review").post(review_account))
                .push(Router::with_path("{user_id}/status").post(set_account_status)),
        )
        .push(
            Router::with_path("instance")
                .get(get_instance)
                .patch(update_instance),
        )
        .push(
            Router::with_path("instance/authentication")
                .get(super::security::get_settings)
                .put(super::security::update_settings),
        )
        .push(Router::with_path("invitations").post(create_invitation))
        .push(
            Router::with_path("registration")
                .get(get_registration)
                .patch(update_registration),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRegistrationRequest {
    mode: RegistrationMode,
    require_approval: bool,
    revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewAccountRequest {
    decision: RegistrationDecision,
    status_revision: u64,
}

#[handler]
async fn get_registration(depot: &mut Depot) -> Result<Json<RegistrationSettings>, ApiError> {
    let state = app_state(depot);
    state
        .store
        .require_platform_action(actor(depot), PlatformAction::AccountsRead)
        .await?;
    Ok(Json(state.store.registration_settings().await?))
}

#[handler]
async fn update_registration(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<RegistrationSettings>, ApiError> {
    let body = request
        .parse_json::<UpdateRegistrationRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .set_registration_settings(
                actor(depot),
                body.mode,
                body.require_approval,
                body.revision,
                now_ms()?,
            )
            .await?,
    ))
}

#[handler]
async fn review_account(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountRecord>, ApiError> {
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    let body = request
        .parse_json::<ReviewAccountRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .review_account_registration(
                actor(depot),
                &user_id,
                body.decision,
                body.status_revision,
                now_ms()?,
            )
            .await?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAccountRoleRequest {
    role: PlatformRole,
    role_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateInstanceRequest {
    mode: InstanceMode,
    revision: u64,
}

#[handler]
async fn list_accounts(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountPage>, ApiError> {
    let query = request
        .parse_queries::<AccountListQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_accounts(actor(depot), &query)
            .await?,
    ))
}

#[handler]
async fn set_account_role(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountRecord>, ApiError> {
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    let body = request
        .parse_json::<SetAccountRoleRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .set_account_role(
                actor(depot),
                &user_id,
                body.role,
                body.role_revision,
                now_ms()?,
            )
            .await?,
    ))
}

#[handler]
async fn get_instance(depot: &mut Depot) -> Result<Json<BrowserInstance>, ApiError> {
    let state = app_state(depot);
    state
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersRead)
        .await?;
    let session = depot
        .get_typed::<ternilo_control::IdentitySession>()
        .expect("user authentication middleware must run first");
    Ok(Json(browser_instance(state, session.instance.clone())))
}

#[handler]
async fn update_instance(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<BrowserInstance>, ApiError> {
    let body = request
        .parse_json::<UpdateInstanceRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let instance = state
        .store
        .set_instance_mode(actor(depot), body.mode, body.revision, now_ms()?)
        .await?;
    Ok(Json(browser_instance(state, instance)))
}

#[handler]
async fn create_invitation(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<UserInvitationGrant>, ApiError> {
    let body = request
        .parse_json::<UserInvitationRequest>()
        .await
        .map_err(invalid_request)?;
    // Platform entry invitations and team invitations have distinct authorization.
    let invitation = app_state(depot)
        .store
        .create_user_invitation(actor(depot), &body, now_ms()?)
        .await?;
    response.status_code(StatusCode::CREATED);
    Ok(Json(invitation))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAccountStatusRequest {
    action: AccountStatusAction,
    status_revision: u64,
}

#[handler]
async fn set_account_status(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountRecord>, ApiError> {
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    let body = request
        .parse_json::<SetAccountStatusRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let account = state
        .store
        .set_account_status(
            actor(depot),
            &user_id,
            body.action,
            body.status_revision,
            now_ms()?,
        )
        .await?;
    if body.action != AccountStatusAction::Unban {
        state.edge.disconnect_account(&user_id).await;
        state.cloud_events.reauthenticate_user(&user_id);
    }
    Ok(Json(account))
}
