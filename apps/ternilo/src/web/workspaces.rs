use super::{
    ApiError, Attachment, Depot, Deserialize, DirectoryListing, Json, ReferenceCandidateRequest,
    ReferenceCandidateSnapshot, Request, Serialize, StatusCode, Workspace, WorkspaceId, app_state,
    handler, invalid_request, path_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveAttachmentRequest {
    attachment: Attachment,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorkspaceRequest {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameWorkspaceRequest {
    title: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDirectoryRequest {
    parent: String,
    name: String,
}

#[derive(Serialize)]
struct CreatedDirectory {
    path: String,
}

#[handler]
pub(super) async fn reference_candidates(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ReferenceCandidateSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let query = request
        .parse_queries::<ReferenceCandidateRequest>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .reference_candidates(&session_id, query)
            .await?,
    ))
}

#[handler]
pub(super) async fn workspace_browser_info(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .workspace_browser(&session_id, ternilo_protocol::WorkspaceRequest::Info)
            .await?,
    ))
}

#[handler]
pub(super) async fn workspace_browser(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<serde_json::Value>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let operation = request
        .parse_json::<ternilo_protocol::WorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .workspace_browser(&session_id, operation)
            .await?,
    ))
}

#[handler]
pub(super) async fn resolve_attachment(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Attachment>, ApiError> {
    let request = request
        .parse_json::<ResolveAttachmentRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .resolve_attachment(request.attachment)
            .await?,
    ))
}

#[handler]
pub(super) async fn list_workspaces(depot: &mut Depot) -> Json<Vec<Workspace>> {
    let state = app_state(depot);
    Json(state.shared.application.snapshot().await.workspaces)
}

#[handler]
pub(super) async fn create_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let request = request
        .parse_json::<CreateWorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let workspace = state
        .shared
        .application
        .add_workspace(&request.path)
        .await?;
    Ok((StatusCode::CREATED, Json(workspace)))
}

#[handler]
pub(super) async fn rename_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Workspace>, ApiError> {
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let body = request
        .parse_json::<RenameWorkspaceRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .rename_workspace(workspace_id, body.title)
            .await?,
    ))
}

#[handler]
pub(super) async fn unregister_workspace(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let workspace_id = WorkspaceId::new(path_parameter(request, "workspace_id")?);
    let state = app_state(depot);
    state
        .shared
        .application
        .unregister_workspace(workspace_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn directory_listing(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<DirectoryListing>, ApiError> {
    let query = request
        .parse_queries::<DirectoryQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .list_directory(query.path.as_deref())
            .await?,
    ))
}

#[handler]
pub(super) async fn make_directory(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<CreatedDirectory>), ApiError> {
    let request = request
        .parse_json::<CreateDirectoryRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let path = state
        .shared
        .application
        .create_directory(&request.parent, &request.name)
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedDirectory { path })))
}

#[handler]
pub(super) async fn file_inventory(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionFilePage>, ApiError> {
    let query = request
        .parse_queries::<ternilo_protocol::SessionFileQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot).shared.application.files(query).await?,
    ))
}

#[handler]
pub(super) async fn session_file_content(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionFileContent>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let file_id = path_parameter(request, "file_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .session_file_content(&session_id, &file_id)
            .await?,
    ))
}
