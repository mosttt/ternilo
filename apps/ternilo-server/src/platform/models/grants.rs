use salvo_core::prelude::{Depot, Json, Request, Response, Router, StatusCode, handler};
use ternilo_control::{ModelGrantInput, ModelGrantPage, ModelGrantRecord, PageQuery};

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

pub(super) fn router() -> Router {
    Router::with_path("grants").get(list).post(save).push(
        Router::with_path("{grant_id}")
            .get(get)
            .put(save)
            .delete(revoke),
    )
}

#[handler]
async fn list(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelGrantPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_grants(actor(depot), &query, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn get(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelGrantRecord>, ApiError> {
    let id = path_parameter(request, "grant_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .get_model_grant(actor(depot), &id, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn save(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<ModelGrantRecord>, ApiError> {
    let id = request.param::<String>("grant_id");
    let input = request
        .parse_json::<ModelGrantInput>()
        .await
        .map_err(invalid_request)?;
    let grant = app_state(depot)
        .store
        .save_model_grant(actor(depot), id.as_deref(), &input, now_ms()?)
        .await?;
    if id.is_none() {
        response.status_code(StatusCode::CREATED);
    }
    Ok(Json(grant))
}

#[handler]
async fn revoke(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "grant_id")?;
    app_state(depot)
        .store
        .revoke_model_grant(actor(depot), &id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
