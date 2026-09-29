use salvo_core::prelude::{Depot, Json, Request, handler};
use serde_json::{Value, json};
use ternilo_control::{ResourceAction, ResourceKind};
use ternilo_protocol::{HarnessError, SessionId, WorkspaceRequest};
use ternilo_transport::ApplicationOperation;

use super::{
    cloud_adapter::CloudAdapter,
    edge_adapter::EdgeAdapter,
    placement::{PlacementResolver, SessionTarget},
};
use crate::platform::{
    http::{ApiError, invalid_request, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

#[handler]
pub(crate) async fn info(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    execute(request, depot, WorkspaceRequest::Info).await
}

#[handler]
pub(crate) async fn operate(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let operation = request
        .parse_json::<WorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    execute(request, depot, operation).await
}

async fn execute(
    request: &Request,
    depot: &Depot,
    operation: WorkspaceRequest,
) -> Result<Json<Value>, ApiError> {
    operation.validate()?;
    let tenant = tenant_parameter(request)?;
    let session_id = SessionId::new(path_parameter(request, "session_id")?);
    let state = app_state(depot);
    let user = actor(depot);
    let target = PlacementResolver::new(state, user, &tenant)
        .session(&session_id)
        .await?;
    let workspace_id = match &target {
        SessionTarget::Cloud(session) => session.workspace_id.clone(),
        SessionTarget::Edge(session) => session.workspace_id.clone(),
    };
    let access = state
        .store
        .resource_access(
            user,
            &tenant,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?;
    if !access.permissions.view {
        if matches!(operation, WorkspaceRequest::Info) {
            return Ok(Json(
                json!({"root":"", "can_browse":false, "applications":[]}),
            ));
        }
        return Err(HarnessError::policy(
            "workspace browsing requires a workspace share, not only a session share",
        )
        .into());
    }
    if matches!(operation, WorkspaceRequest::Open { .. }) {
        return Err(HarnessError::policy(
            "native applications can only be opened from the local Ternilo interface",
        )
        .into());
    }
    let is_info = matches!(operation, WorkspaceRequest::Info);
    let mut value = match target {
        SessionTarget::Edge(mapping) => {
            EdgeAdapter::new(state, user, &tenant)
                .call_session(&mapping, |session_id| {
                    ApplicationOperation::SessionWorkspace {
                        session_id,
                        request: operation,
                    }
                })
                .await?
        }
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, user, &tenant)
                .workspace_browser(&session, operation)
                .await?
        }
    };
    if is_info {
        value["applications"] = json!([]);
    }
    state
        .store
        .resource_access(
            user,
            &tenant,
            ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?
        .require(ResourceAction::View)?;
    Ok(Json(value))
}
