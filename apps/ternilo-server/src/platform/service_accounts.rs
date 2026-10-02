use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use serde_json::{Value, json};
use ternilo_control::{ServiceAccountCreate, ServiceAccountUpdate, ServiceCredentialCreate};
use ternilo_protocol::UserId;

use super::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

pub(super) fn router() -> Router {
    Router::with_path("service-accounts")
        .hoop(super::identity::no_store)
        .get(list)
        .post(create)
        .push(
            Router::with_path("{service_account_id}")
                .patch(update)
                .push(
                    Router::with_path("credentials")
                        .get(credentials)
                        .post(issue)
                        .push(Router::with_path("{credential_id}").delete(revoke)),
                ),
        )
}

#[handler]
async fn list(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    Ok(Json(
        json!({"service_accounts":app_state(depot).store.list_service_accounts(actor(depot),&tenant).await?}),
    ))
}

#[handler]
async fn create(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant = tenant_parameter(request)?;
    let draft = request
        .parse_json::<ServiceAccountCreate>()
        .await
        .map_err(invalid_request)?;
    let account = app_state(depot)
        .store
        .create_service_account(actor(depot), &tenant, &draft, now_ms()?)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"service_account":account})),
    ))
}

#[handler]
async fn update(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let id = UserId::new(path_parameter(request, "service_account_id")?);
    let draft = request
        .parse_json::<ServiceAccountUpdate>()
        .await
        .map_err(invalid_request)?;
    let account = app_state(depot)
        .store
        .update_service_account(actor(depot), &tenant, &id, &draft, now_ms()?)
        .await?;
    Ok(Json(json!({"service_account":account})))
}

#[handler]
async fn credentials(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let id = UserId::new(path_parameter(request, "service_account_id")?);
    Ok(Json(
        json!({"credentials":app_state(depot).store.list_service_credentials(actor(depot),&tenant,&id).await?}),
    ))
}

#[handler]
async fn issue(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant = tenant_parameter(request)?;
    let id = UserId::new(path_parameter(request, "service_account_id")?);
    let draft = request
        .parse_json::<ServiceCredentialCreate>()
        .await
        .map_err(invalid_request)?;
    let grant = app_state(depot)
        .store
        .create_service_credential(actor(depot), &tenant, &id, &draft, now_ms()?)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(grant).map_err(invalid_request)?),
    ))
}

#[handler]
async fn revoke(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let tenant = tenant_parameter(request)?;
    let id = UserId::new(path_parameter(request, "service_account_id")?);
    let credential = path_parameter(request, "credential_id")?;
    app_state(depot)
        .store
        .revoke_service_credential(actor(depot), &tenant, &id, &credential, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;
