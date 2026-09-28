use super::capabilities::workbench_execution_target;
use super::{
    ApiError, ApplicationOperation, Depot, Deserialize, Json, Request, StatusCode, Value, actor,
    app_state, encode_node, handler, invalid_request, now_ms, path_parameter, tenant_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtensionStateRequest {
    enabled: bool,
}

#[handler]
pub(super) async fn list_extensions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_extension::ExtensionInventory>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let inventory = app_state(depot)
        .store
        .extension_inventory(actor(depot), &tenant_id)
        .await?;
    Ok(Json(inventory))
}

#[handler]
pub(super) async fn trust_extension_publisher(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let trust = request
        .parse_json::<ternilo_extension::PublisherTrust>()
        .await
        .map_err(invalid_request)?;
    if let Some(value) = target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::PublisherTrust {
                publisher: encode_node(&trust, "extension publisher trust")?,
            },
        )
        .await?
    {
        return Ok((StatusCode::CREATED, Json(value)));
    }
    let publisher = state
        .store
        .trust_extension_publisher(actor(depot), &tenant_id, trust, now_ms()?)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(encode_node(
            &publisher,
            "Control extension publisher response",
        )?),
    ))
}

#[handler]
pub(super) async fn revoke_extension_publisher(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let key_id = path_parameter(request, "key_id")?;
    if target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::PublisherRevoke {
                key_id: key_id.clone(),
            },
        )
        .await?
        .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .revoke_extension_publisher(actor(depot), &tenant_id, &key_id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn install_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let install = request
        .parse_json::<ternilo_extension::ExtensionInstallRequest>()
        .await
        .map_err(invalid_request)?;
    if let Some(value) = target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::ExtensionInstall {
                request: encode_node(&install, "extension install request")?,
            },
        )
        .await?
    {
        return Ok((StatusCode::CREATED, Json(value)));
    }
    let installed = state
        .store
        .install_extension(
            actor(depot),
            &tenant_id,
            install,
            &state.worker_policy.extension_host_policy,
            now_ms()?,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(encode_node(
            &installed,
            "Control extension install response",
        )?),
    ))
}

#[handler]
pub(super) async fn set_extension_state(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    let body = request
        .parse_json::<ExtensionStateRequest>()
        .await
        .map_err(invalid_request)?;
    if let Some(value) = target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::ExtensionSetEnabled {
                package_id: package_id.clone(),
                version: version.clone(),
                enabled: body.enabled,
            },
        )
        .await?
    {
        return Ok(Json(value));
    }
    let installed = state
        .store
        .set_extension_enabled(
            actor(depot),
            &tenant_id,
            &package_id,
            &version,
            body.enabled,
            now_ms()?,
        )
        .await?;
    Ok(Json(encode_node(
        &installed,
        "Control extension state response",
    )?))
}

#[handler]
pub(super) async fn revoke_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    if target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::ExtensionRevoke {
                package_id: package_id.clone(),
                version: version.clone(),
            },
        )
        .await?
        .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .revoke_extension(actor(depot), &tenant_id, &package_id, &version, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn uninstall_extension(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let target = workbench_execution_target(request, depot, &tenant_id).await?;
    let package_id = path_parameter(request, "package_id")?;
    let version = path_parameter(request, "version")?;
    if target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::ExtensionUninstall {
                package_id: package_id.clone(),
                version: version.clone(),
            },
        )
        .await?
        .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .uninstall_extension(actor(depot), &tenant_id, &package_id, &version, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
