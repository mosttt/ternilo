use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use ternilo_transport::{NodeCleanupReceipt, NodeCleanupSnapshot};

use super::{
    app_state,
    auth::authentication_error,
    http::{ApiError, bearer_token, invalid_request, now_ms},
};

pub(super) fn router() -> Router {
    Router::with_path("api/v1/executors/cleanup")
        .hoop(super::identity::no_store)
        .hoop(salvo_extra::size_limiter::max_size(8 * 1024))
        .get(snapshot)
        .post(receipt)
        .push(Router::with_path("sync").post(synchronize))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SynchronizeRequest {
    storage_instance_id: String,
}

#[handler]
async fn synchronize(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<NodeCleanupSnapshot>, ApiError> {
    let body = request
        .parse_json::<SynchronizeRequest>()
        .await
        .map_err(invalid_request)?;
    let token = bearer_token(request)?;
    Ok(Json(
        app_state(depot)
            .store
            .synchronize_node_cleanup(token, &body.storage_instance_id, now_ms()?)
            .await
            .map_err(authentication_error)?,
    ))
}

#[handler]
async fn receipt(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let body = request
        .parse_json::<NodeCleanupReceipt>()
        .await
        .map_err(invalid_request)?;
    let token = bearer_token(request)?;
    app_state(depot)
        .store
        .record_node_cleanup_receipt(token, &body, now_ms()?)
        .await
        .map_err(authentication_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn snapshot(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<NodeCleanupSnapshot>, ApiError> {
    let token = bearer_token(request)?;
    Ok(Json(
        app_state(depot)
            .store
            .node_cleanup_snapshot(token)
            .await
            .map_err(authentication_error)?,
    ))
}
