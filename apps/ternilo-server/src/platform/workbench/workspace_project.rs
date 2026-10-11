use salvo_core::prelude::{Depot, Json, Request, handler};
use ternilo_control::{WorkspaceProject, WorkspaceProjectMove};
use ternilo_protocol::WorkspaceId;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

#[handler]
pub(crate) async fn get(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkspaceProject>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let workspace = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    Ok(Json(
        app_state(depot)
            .store
            .workspace_project(actor(depot), &tenant, &workspace)
            .await?,
    ))
}

#[handler]
pub(crate) async fn move_to(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkspaceProject>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let workspace = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let input = request
        .parse_json::<WorkspaceProjectMove>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .move_workspace_project(actor(depot), &tenant, &workspace, &input, now_ms()?)
            .await?,
    ))
}
