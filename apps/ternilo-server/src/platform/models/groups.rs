use salvo_core::prelude::{Depot, Json, Request, Response, Router, StatusCode, handler};
use ternilo_control::{
    GroupInput, ModelGroupMemberPage, ModelGroupPage, ModelGroupRecord, PageQuery,
};
use ternilo_protocol::UserId;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

pub(super) fn router() -> Router {
    Router::with_path("groups").get(list).post(save).push(
        Router::with_path("{group_id}")
            .get(get)
            .put(save)
            .delete(remove)
            .push(Router::with_path("candidates").get(candidates))
            .push(
                Router::with_path("members").get(members).push(
                    Router::with_path("{user_id}")
                        .put(add_member)
                        .delete(remove_member),
                ),
            ),
    )
}

#[handler]
async fn list(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelGroupPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_groups(actor(depot), &query)
            .await?,
    ))
}

#[handler]
async fn get(request: &mut Request, depot: &mut Depot) -> Result<Json<ModelGroupRecord>, ApiError> {
    let id = path_parameter(request, "group_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .get_model_group(actor(depot), &id)
            .await?,
    ))
}

#[handler]
async fn save(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<ModelGroupRecord>, ApiError> {
    let id = request.param::<String>("group_id");
    let input = request
        .parse_json::<GroupInput>()
        .await
        .map_err(invalid_request)?;
    let group = app_state(depot)
        .store
        .save_model_group(actor(depot), id.as_deref(), &input, now_ms()?)
        .await?;
    if id.is_none() {
        response.status_code(StatusCode::CREATED);
    }
    Ok(Json(group))
}

#[handler]
async fn remove(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "group_id")?;
    app_state(depot)
        .store
        .delete_model_group(actor(depot), &id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn members(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelGroupMemberPage>, ApiError> {
    let id = path_parameter(request, "group_id")?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_group_members(actor(depot), &id, &query)
            .await?,
    ))
}

#[handler]
async fn candidates(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelGroupMemberPage>, ApiError> {
    let id = path_parameter(request, "group_id")?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_group_candidates(actor(depot), &id, &query)
            .await?,
    ))
}

#[handler]
async fn add_member(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "group_id")?;
    let user = UserId::new(path_parameter(request, "user_id")?);
    app_state(depot)
        .store
        .set_model_group_member(actor(depot), &id, &user, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn remove_member(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "group_id")?;
    let user = UserId::new(path_parameter(request, "user_id")?);
    app_state(depot)
        .store
        .remove_model_group_member(actor(depot), &id, &user, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
