use super::{ApiError, app_state, invalid_request, path_parameter};
use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use serde::Deserialize;
use ternilo_local::{ConnectionAuthorization, ConnectionPoll, ModelConnection};

pub(super) fn router() -> Router {
    Router::with_path("model-connections")
        .get(list)
        .push(
            Router::with_path("authorize")
                .post(begin)
                .push(Router::with_path("{attempt_id}").post(poll).delete(cancel)),
        )
        .push(
            Router::with_path("{connection_id}")
                .delete(remove)
                .push(Router::with_path("refresh").post(refresh)),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    server_url: String,
    name: String,
}

#[handler]
async fn list(depot: &mut Depot) -> Json<Vec<ModelConnection>> {
    Json(
        app_state(depot)
            .shared
            .application
            .model_connections()
            .await,
    )
}
#[handler]
async fn begin(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ConnectionAuthorization>, ApiError> {
    let input = request
        .parse_json::<Begin>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .begin_model_connection(&input.server_url, &input.name)
            .await?,
    ))
}
#[handler]
async fn poll(request: &mut Request, depot: &mut Depot) -> Result<Json<ConnectionPoll>, ApiError> {
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .poll_model_connection(&path_parameter(request, "attempt_id")?)
            .await?,
    ))
}
#[handler]
async fn cancel(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    app_state(depot)
        .shared
        .application
        .cancel_model_connection(&path_parameter(request, "attempt_id")?)
        .await;
    Ok(StatusCode::NO_CONTENT)
}
#[handler]
async fn refresh(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelConnection>, ApiError> {
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .refresh_model_connection(&path_parameter(request, "connection_id")?)
            .await?,
    ))
}
#[handler]
async fn remove(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let revoke = request.query::<bool>("revoke").unwrap_or(true);
    app_state(depot)
        .shared
        .application
        .remove_model_connection(&path_parameter(request, "connection_id")?, revoke)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
