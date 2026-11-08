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
    Ok(Json(
        read_workspace(
            app_state(depot),
            actor(depot),
            &tenant_parameter(request)?,
            &SessionId::new(path_parameter(request, "session_id")?),
            operation,
        )
        .await?,
    ))
}

async fn read_workspace(
    state: &crate::platform::state::AppState,
    user: &ternilo_control::ControlUser,
    tenant: &ternilo_protocol::TenantId,
    session_id: &SessionId,
    operation: WorkspaceRequest,
) -> Result<Value, HarnessError> {
    operation.validate()?;
    let target = PlacementResolver::new(state, user, tenant)
        .session(session_id)
        .await?;
    let workspace_id = match &target {
        SessionTarget::Cloud(session) => session.workspace_id.clone(),
        SessionTarget::Edge(session) => session.workspace_id.clone(),
    };
    let access = state
        .store
        .resource_access(user, tenant, ResourceKind::Workspace, workspace_id.as_str())
        .await?;
    if !access.permissions.view {
        if matches!(operation, WorkspaceRequest::Info) {
            return Ok(json!({"root":"", "can_browse":false, "applications":[]}));
        }
        return Err(HarnessError::policy(
            "workspace browsing requires a workspace share, not only a session share",
        ));
    }
    if matches!(operation, WorkspaceRequest::Open { .. }) {
        return Err(HarnessError::policy(
            "native applications can only be opened from the local Ternilo interface",
        ));
    }
    let is_info = matches!(operation, WorkspaceRequest::Info);
    let mut value = match target {
        SessionTarget::Edge(mapping) => {
            EdgeAdapter::new(state, user, tenant)
                .call_session(&mapping, |session_id| {
                    ApplicationOperation::SessionWorkspace {
                        session_id,
                        request: operation,
                    }
                })
                .await?
        }
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, user, tenant)
                .workspace_browser(&session, operation)
                .await?
        }
    };
    if is_info {
        value["applications"] = json!([]);
    }
    state
        .store
        .resource_access(user, tenant, ResourceKind::Workspace, workspace_id.as_str())
        .await?
        .require(ResourceAction::View)?;
    Ok(value)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceDownloadRequest {
    path: String,
}

#[handler]
pub(crate) async fn download(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut salvo_core::Response,
) -> Result<(), ApiError> {
    let body = request
        .parse_json::<WorkspaceDownloadRequest>()
        .await
        .map_err(invalid_request)?;
    let session = SessionId::new(path_parameter(request, "session_id")?);
    let tenant = tenant_parameter(request)?;
    let state = app_state(depot).clone();
    let user = actor(depot).clone();
    let read = move |offset, version| {
        let (state, user, tenant, session, path) = (
            state.clone(),
            user.clone(),
            tenant.clone(),
            session.clone(),
            body.path.clone(),
        );
        async move {
            let value = read_workspace(
                &state,
                &user,
                &tenant,
                &session,
                WorkspaceRequest::Download {
                    path,
                    offset,
                    version,
                },
            )
            .await?;
            serde_json::from_value::<ternilo_protocol::FileDownloadChunk>(value).map_err(|error| {
                HarnessError::execution(format!("decode workspace download: {error}"))
            })
        }
    };
    let first = read(0, None).await?;
    for (name, value) in [
        ("content-type", "application/octet-stream".to_owned()),
        ("content-length", first.version.bytes.to_string()),
        ("cache-control", "no-store".to_owned()),
        ("content-disposition", "attachment".to_owned()),
    ] {
        response
            .add_header(name, value, true)
            .map_err(|_| HarnessError::execution("set download headers"))?;
    }
    response.stream(ternilo_local::workspace_download_stream(
        first,
        move |offset, version| read(offset, Some(version)),
    ));
    Ok(())
}
