use super::{
    ApiError, ApplicationOperation, Depot, Deserialize, Json, Request, SessionId, TenantId, Value,
    WorkspaceId, actor, app_state, decode_node, encode_node, handler, invalid_request,
    tenant_parameter, workbench,
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionTargetQuery {
    #[serde(default)]
    session_id: Option<SessionId>,
    #[serde(default)]
    workspace_id: Option<WorkspaceId>,
}

pub(super) async fn workbench_execution_target(
    request: &mut Request,
    depot: &Depot,
    tenant_id: &TenantId,
) -> Result<workbench::SettingsTarget, ApiError> {
    let query = request
        .parse_queries::<ExecutionTargetQuery>()
        .map_err(invalid_request)?;
    workbench::PlacementResolver::new(app_state(depot), actor(depot), tenant_id)
        .settings(query.session_id.as_ref(), query.workspace_id.as_ref())
        .await
        .map_err(Into::into)
}

#[handler]
pub(super) async fn workbench_catalog(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::ApplicationCatalog>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let query = request
        .parse_queries::<ExecutionTargetQuery>()
        .map_err(invalid_request)?;
    let executor = if let Some(session_id) = &query.session_id {
        match workbench::PlacementResolver::new(state, actor(depot), &tenant_id)
            .session(session_id)
            .await?
        {
            workbench::SessionTarget::Cloud(_) => None,
            workbench::SessionTarget::Edge(session) => Some(session.executor_id),
        }
    } else if let Some(workspace_id) = &query.workspace_id {
        state
            .store
            .resolve_accessible_workspace(actor(depot), &tenant_id, workspace_id)
            .await?
            .executor_id
    } else {
        None
    };
    if let Some(executor) = executor {
        let value = state
            .edge
            .call(&tenant_id, &executor, ApplicationOperation::Catalog)
            .await?;
        return Ok(Json(decode_node(value, "Node application catalog")?));
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    let mut catalog = state.catalog.describe();
    catalog.host_limits = Some(state.worker_policy.maximum_limits);
    Ok(Json(catalog))
}

#[handler]
pub(super) async fn workbench_plugins(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    if let Some(value) = target
        .read(state, &tenant_id, ApplicationOperation::ExtensionInventory)
        .await?
    {
        return Ok(Json(value));
    }
    Ok(Json(encode_node(
        &state
            .store
            .extension_inventory(actor(depot), &tenant_id)
            .await?,
        "Control extension inventory",
    )?))
}

#[handler]
pub(super) async fn workbench_questions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<Value>>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    app_state(depot)
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Ok(Json(Vec::new()))
}
