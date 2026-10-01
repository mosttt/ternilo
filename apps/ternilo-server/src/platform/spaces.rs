use super::execution::require_managed_execution;
use super::{
    ApiError, ApplicationOperation, Depot, Deserialize, ExecutorId, HarnessError, Json, Request,
    StatusCode, TenantQuota, TenantRole, UserId, Value, WorkspaceId, WorkspacePlacement, actor,
    app_state, handler, invalid_request, json, now_ms, path_parameter, tenant_parameter, workbench,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTenantRequest {
    slug: String,
    display_name: String,
    quota: Option<TenantQuota>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectRequest {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorkspaceRequest {
    project_id: String,
    name: String,
    placement: WorkspacePlacement,
    executor_id: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct NodeWorkspaceRegistration {
    workspace_id: WorkspaceId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetMembershipRequest {
    role: TenantRole,
}

#[handler]
pub(super) async fn list_tenants(depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let tenants = app_state(depot).store.list_tenants(actor(depot)).await?;
    Ok(Json(json!({ "tenants": tenants })))
}

#[handler]
pub(super) async fn create_tenant(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = request
        .parse_json::<CreateTenantRequest>()
        .await
        .map_err(invalid_request)?;
    let tenant = app_state(depot)
        .store
        .create_tenant(
            actor(depot),
            &body.slug,
            &body.display_name,
            body.quota.unwrap_or_default(),
            now_ms()?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "tenant": tenant }))))
}

#[handler]
pub(super) async fn list_projects(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let projects = app_state(depot)
        .store
        .list_projects(actor(depot), &tenant_id)
        .await?;
    Ok(Json(json!({ "projects": projects })))
}

#[handler]
pub(super) async fn create_project(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<CreateProjectRequest>()
        .await
        .map_err(invalid_request)?;
    let project = app_state(depot)
        .store
        .create_project(actor(depot), &tenant_id, &body.name, now_ms()?)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "project": project }))))
}

#[handler]
pub(super) async fn rename_project(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let project_id = path_parameter(request, "project_id")?;
    let body = request
        .parse_json::<CreateProjectRequest>()
        .await
        .map_err(invalid_request)?;
    let project = app_state(depot)
        .store
        .rename_project(actor(depot), &tenant_id, &project_id, &body.name, now_ms()?)
        .await?;
    Ok(Json(json!({ "project": project })))
}

#[handler]
pub(super) async fn delete_project(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let project_id = path_parameter(request, "project_id")?;
    app_state(depot)
        .store
        .delete_project(actor(depot), &tenant_id, &project_id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn list_workspaces(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let workspaces = app_state(depot)
        .store
        .list_workspaces(actor(depot), &tenant_id)
        .await?;
    Ok(Json(json!({ "workspaces": workspaces })))
}

#[handler]
pub(super) async fn create_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<CreateWorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    let workspace = match body.placement {
        WorkspacePlacement::Cloud => {
            require_managed_execution(app_state(depot))?;
            if body.executor_id.is_some() || body.path.is_some() {
                return Err(HarnessError::invalid(
                    "cloud workspace must not contain a Node executor binding",
                )
                .into());
            }
            app_state(depot)
                .store
                .create_cloud_workspace(
                    actor(depot),
                    &tenant_id,
                    &body.project_id,
                    &body.name,
                    now_ms()?,
                )
                .await?
        }
        WorkspacePlacement::LocalNode => {
            let executor_id =
                ExecutorId::new(body.executor_id.ok_or_else(|| {
                    HarnessError::invalid("local workspace requires executor_id")
                })?);
            let path = body
                .path
                .ok_or_else(|| HarnessError::invalid("local workspace requires path"))?;
            let state = app_state(depot);
            workbench::authorize_edge_mutation(state, actor(depot), &tenant_id).await?;
            let executor = state
                .store
                .owned_executor(actor(depot), &tenant_id, &executor_id)
                .await?;
            if executor.state == "revoked"
                || executor
                    .project_id
                    .as_deref()
                    .is_some_and(|project| project != body.project_id)
            {
                return Err(
                    HarnessError::policy("Node is revoked or bound to another project").into(),
                );
            }
            let _resources = state.edge.lock_resources(&tenant_id, &executor_id).await;
            let value = state
                .edge
                .call(
                    &tenant_id,
                    &executor_id,
                    ApplicationOperation::WorkspaceCreate { path },
                )
                .await?;
            let node_workspace: NodeWorkspaceRegistration =
                serde_json::from_value(value).map_err(|error| {
                    HarnessError::execution(format!(
                        "Node Workspace create response is invalid: {error}"
                    ))
                })?;
            node_workspace.workspace_id.validate()?;
            // Node registration can return an existing directory. A server-side
            // validation failure must never unregister that existing resource.
            state
                .store
                .create_local_workspace(
                    actor(depot),
                    &tenant_id,
                    &body.project_id,
                    &body.name,
                    (&executor_id, &node_workspace.workspace_id),
                    now_ms()?,
                )
                .await?
        }
    };
    Ok((StatusCode::CREATED, Json(json!({ "workspace": workspace }))))
}

#[handler]
pub(super) async fn get_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let workspace = app_state(depot)
        .store
        .get_workspace(actor(depot), &tenant_id, &workspace_id)
        .await?;
    let access = app_state(depot)
        .store
        .resource_access(
            actor(depot),
            &tenant_id,
            ternilo_control::ResourceKind::Workspace,
            workspace_id.as_str(),
        )
        .await?;
    let mut workspace = json!(workspace);
    workspace["storage_user_id"] = workspace["owner_user_id"].clone();
    workspace["owner_user_id"] = json!(access.owner_user_id);
    workspace["access"] = json!(access);
    Ok(Json(json!({ "workspace": workspace })))
}

#[handler]
pub(super) async fn set_membership(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    let body = request
        .parse_json::<SetMembershipRequest>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .set_membership(actor(depot), &tenant_id, &user_id, body.role, now_ms()?)
        .await?;
    app_state(depot).edge.notify_resource_change(&tenant_id);
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn list_memberships(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_control::MemberPage>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<ternilo_control::PageQuery>()
        .map_err(invalid_request)?;
    let memberships = app_state(depot)
        .store
        .list_memberships(actor(depot), &tenant_id, &query)
        .await?;
    Ok(Json(memberships))
}

#[handler]
pub(super) async fn remove_membership(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let user_id = UserId::new(path_parameter(request, "user_id")?);
    app_state(depot)
        .store
        .remove_membership(actor(depot), &tenant_id, &user_id, now_ms()?)
        .await?;
    app_state(depot).edge.notify_resource_change(&tenant_id);
    Ok(StatusCode::NO_CONTENT)
}
