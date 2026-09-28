use salvo_core::prelude::{Depot, Json, Request, Response, Router, StatusCode, handler};
use ternilo_control::{GroupInput, GroupPage, GroupRecord, MemberPage, PageQuery};
use ternilo_protocol::UserId;

use super::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

#[cfg(test)]
mod tests;

pub(crate) fn router() -> Router {
    Router::with_path("groups")
        .get(list_groups)
        .post(create_group)
        .push(
            Router::with_path("{group_id}")
                .get(get_group)
                .patch(update_group)
                .delete(delete_group)
                .push(
                    Router::with_path("members").get(list_members).push(
                        Router::with_path("{user_id}")
                            .put(add_member)
                            .delete(remove_member),
                    ),
                ),
        )
}

#[handler]
async fn list_groups(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<GroupPage>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_permission_groups(actor(depot), &tenant, &query)
            .await?,
    ))
}

#[handler]
async fn create_group(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<GroupRecord>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let input = request
        .parse_json::<GroupInput>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let group = state
        .store
        .create_permission_group(actor(depot), &tenant, &input, now_ms()?)
        .await?;
    state.edge.notify_resource_change(&tenant);
    response.status_code(StatusCode::CREATED);
    Ok(Json(group))
}

#[handler]
async fn get_group(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<GroupRecord>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let group_id = path_parameter(request, "group_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .permission_group(actor(depot), &tenant, &group_id)
            .await?,
    ))
}

#[handler]
async fn update_group(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<GroupRecord>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let group_id = path_parameter(request, "group_id")?;
    let input = request
        .parse_json::<GroupInput>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let group = state
        .store
        .update_permission_group(actor(depot), &tenant, &group_id, &input, now_ms()?)
        .await?;
    state.edge.notify_resource_change(&tenant);
    Ok(Json(group))
}

#[handler]
async fn delete_group(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let tenant = tenant_parameter(request)?;
    let group_id = path_parameter(request, "group_id")?;
    let state = app_state(depot);
    state
        .store
        .delete_permission_group(actor(depot), &tenant, &group_id, now_ms()?)
        .await?;
    state.edge.notify_resource_change(&tenant);
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn list_members(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<MemberPage>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let group_id = path_parameter(request, "group_id")?;
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_permission_group_members(actor(depot), &tenant, &group_id, &query)
            .await?,
    ))
}

#[handler]
async fn add_member(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    set_member(request, depot, true).await
}

#[handler]
async fn remove_member(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    set_member(request, depot, false).await
}

async fn set_member(
    request: &Request,
    depot: &Depot,
    present: bool,
) -> Result<StatusCode, ApiError> {
    let tenant = tenant_parameter(request)?;
    let group_id = path_parameter(request, "group_id")?;
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    let state = app_state(depot);
    state
        .store
        .set_permission_group_member(
            actor(depot),
            &tenant,
            &group_id,
            &user_id,
            present,
            now_ms()?,
        )
        .await?;
    state.edge.notify_resource_change(&tenant);
    Ok(StatusCode::NO_CONTENT)
}
