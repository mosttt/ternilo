use salvo_core::prelude::{Depot, Json, Request, Router, handler};
use salvo_extra::size_limiter::max_size;
use serde::Deserialize;
use ternilo_cloud::ExecutionMaintenance;
use ternilo_control::PlatformAction;

use super::{
    auth,
    http::{ApiError, invalid_request},
    state::{actor, app_state},
};

pub(crate) fn router() -> Router {
    Router::with_path("api/v1/admin/execution")
        .hoop(auth::user_auth)
        .hoop(super::identity::no_store)
        .hoop(max_size(4096))
        .get(get_execution)
        .patch(update_execution)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseExecution {
    claims_paused: bool,
}

#[handler]
async fn get_execution(depot: &mut Depot) -> Result<Json<ExecutionMaintenance>, ApiError> {
    app_state(depot)
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersRead)
        .await?;
    Ok(Json(
        app_state(depot)
            .cloud
            .execution_maintenance(&actor(depot).user_id)
            .await?,
    ))
}

#[handler]
async fn update_execution(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ExecutionMaintenance>, ApiError> {
    app_state(depot)
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersManage)
        .await?;
    let body = request
        .parse_json::<PauseExecution>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .cloud
            .set_execution_paused(&actor(depot).user_id, body.claims_paused)
            .await?,
    ))
}
