use super::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetUpdateRequest,
    ApiError, Arc, Depot, Deserialize, Json, Request, Router, StatusCode, app_state, handler,
    invalid_request, join_error, path_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtensionStateRequest {
    enabled: bool,
}

pub(super) fn extension_router() -> Router {
    Router::with_path("extensions")
        .get(extension_inventory)
        .post(install_extension)
        .push(
            Router::with_path("publishers")
                .post(trust_extension_publisher)
                .push(Router::with_path("{key_id}/revoke").post(revoke_extension_publisher)),
        )
        .push(
            Router::with_path("{package_id}/{version}")
                .put(set_extension_state)
                .delete(uninstall_extension)
                .push(Router::with_path("revoke").post(revoke_extension)),
        )
}

#[handler]
pub(super) async fn list_agent_presets(depot: &mut Depot) -> Json<AgentPresetRoster> {
    Json(
        app_state(depot)
            .shared
            .application
            .agent_preset_roster()
            .await,
    )
}

#[handler]
pub(super) async fn get_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetDocument>, ApiError> {
    let preset_id = path_parameter(request, "preset_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .agent_preset_view(&preset_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn copy_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<AgentPresetDocument>), ApiError> {
    let copy = request
        .parse_json::<AgentPresetCopyRequest>()
        .await
        .map_err(invalid_request)?;
    let preset = app_state(depot)
        .shared
        .application
        .copy_agent_preset(copy)
        .await?;
    Ok((StatusCode::CREATED, Json(preset)))
}

#[handler]
pub(super) async fn update_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetDocument>, ApiError> {
    let preset_id = path_parameter(request, "preset_id")?;
    let update = request
        .parse_json::<AgentPresetUpdateRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .update_agent_preset(&preset_id, update)
            .await?,
    ))
}

#[handler]
pub(super) async fn delete_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let preset_id = path_parameter(request, "preset_id")?;
    app_state(depot)
        .shared
        .application
        .remove_agent_preset(&preset_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn set_default_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetRoster>, ApiError> {
    let preset_id = path_parameter(request, "preset_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .set_default_agent_preset(&preset_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn extension_inventory(
    depot: &mut Depot,
) -> Result<Json<ternilo_extension::ExtensionInventory>, ApiError> {
    let application = Arc::clone(&app_state(depot).shared.application);
    let inventory = tokio::task::spawn_blocking(move || application.extension_inventory())
        .await
        .map_err(|error| join_error(&error))??;
    Ok(Json(inventory))
}

#[handler]
pub(super) async fn trust_extension_publisher(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ternilo_extension::TrustedPublisher>), ApiError> {
    let publisher = request
        .parse_json::<ternilo_extension::PublisherTrust>()
        .await
        .map_err(invalid_request)?;
    let application = Arc::clone(&app_state(depot).shared.application);
    let trusted =
        tokio::task::spawn_blocking(move || application.trust_extension_publisher(publisher))
            .await
            .map_err(|error| join_error(&error))??;
    Ok((StatusCode::CREATED, Json(trusted)))
}

#[handler]
pub(super) async fn revoke_extension_publisher(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let key_id = path_parameter(request, "key_id")?;
    app_state(depot)
        .shared
        .application
        .revoke_extension_publisher(&key_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn install_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ternilo_extension::InstalledExtension>), ApiError> {
    let install = request
        .parse_json::<ternilo_extension::ExtensionInstallRequest>()
        .await
        .map_err(invalid_request)?;
    let application = Arc::clone(&app_state(depot).shared.application);
    let installed = tokio::task::spawn_blocking(move || application.install_extension(install))
        .await
        .map_err(|error| join_error(&error))??;
    Ok((StatusCode::CREATED, Json(installed)))
}

#[handler]
pub(super) async fn set_extension_state(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_extension::InstalledExtension>, ApiError> {
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    let state = request
        .parse_json::<ExtensionStateRequest>()
        .await
        .map_err(invalid_request)?;
    let installed = app_state(depot)
        .shared
        .application
        .set_extension_enabled_for_web(&package_id, &version, state.enabled)
        .await?;
    Ok(Json(installed))
}

#[handler]
pub(super) async fn revoke_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    app_state(depot)
        .shared
        .application
        .revoke_extension(&package_id, &version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn uninstall_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    app_state(depot)
        .shared
        .application
        .uninstall_extension(&package_id, &version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
