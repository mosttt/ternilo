use super::{
    ApiError, AuthorizationAttempt, AuthorizationBeginRequest, AuthorizationCredentialKey,
    AuthorizationPromptAnswer, AuthorizationSnapshot, CredentialInventory, CredentialRecordInfo,
    Depot, Deserialize, ExtensionProviderMaterializeRequest, Json, ModelSelection,
    ProviderModelDiscoveryRequest, ProviderProfile, Request, StatusCode, app_state, handler,
    invalid_request, path_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRequest {
    name: String,
    value: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRecordRequest {
    key: String,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "Field names match the shared HTTP query contract."
)]
struct AuthorizationQuery {
    surface_id: String,
    #[serde(default, rename = "session_id")]
    _session_id: Option<String>,
    #[serde(default, rename = "workspace_id")]
    _workspace_id: Option<String>,
}

#[handler]
pub(super) async fn list_credentials(depot: &mut Depot) -> Json<CredentialInventory> {
    Json(
        app_state(depot)
            .shared
            .application
            .credential_inventory()
            .await,
    )
}

#[handler]
pub(super) async fn set_credential(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let request = request
        .parse_json::<CredentialRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .shared
        .application
        .set_credential(request.name, request.value)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn remove_credential(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let name = path_parameter(request, "name")?;
    let state = app_state(depot);
    state.shared.application.remove_credential(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn set_credential_record(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<CredentialRecordInfo>, ApiError> {
    let request = request
        .parse_json::<CredentialRecordRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .set_credential_record(request.key, request.kind, request.payload)
            .await?,
    ))
}

#[handler]
pub(super) async fn delete_credential_record(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let scope = path_parameter(request, "scope")?;
    let record_id = path_parameter(request, "record_id")?;
    app_state(depot)
        .shared
        .application
        .delete_credential_record(&format!("{scope}/{record_id}"))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn authorization_snapshot(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AuthorizationSnapshot>, ApiError> {
    let query = request
        .parse_queries::<AuthorizationQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .authorization_snapshot(&query.surface_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn begin_authorization(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AuthorizationAttempt>, ApiError> {
    let request = request
        .parse_json::<AuthorizationBeginRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .begin_authorization(request)
            .await?,
    ))
}

#[handler]
pub(super) async fn answer_authorization_prompt(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let answer = request
        .parse_json::<AuthorizationPromptAnswer>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .shared
        .application
        .answer_authorization_prompt(answer)?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn cancel_authorization(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let key = request
        .parse_json::<AuthorizationCredentialKey>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .shared
        .application
        .cancel_authorization(&key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn list_providers(depot: &mut Depot) -> Json<Vec<ProviderProfile>> {
    Json(
        app_state(depot)
            .shared
            .application
            .provider_profiles()
            .await,
    )
}

#[handler]
pub(super) async fn get_default_model(depot: &mut Depot) -> Result<Json<ModelSelection>, ApiError> {
    Ok(Json(
        app_state(depot).shared.application.default_model().await?,
    ))
}

#[handler]
pub(super) async fn set_default_model(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelSelection>, ApiError> {
    let selection = request
        .parse_json::<ModelSelection>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .set_default_model(selection)
            .await?,
    ))
}

#[handler]
pub(super) async fn upsert_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ProviderProfile>, ApiError> {
    let provider = request
        .parse_json::<ProviderProfile>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .upsert_provider_profile(provider)
            .await?,
    ))
}

#[handler]
pub(super) async fn materialize_extension_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ProviderProfile>), ApiError> {
    let request = request
        .parse_json::<ExtensionProviderMaterializeRequest>()
        .await
        .map_err(invalid_request)?;
    Ok((
        StatusCode::CREATED,
        Json(
            app_state(depot)
                .shared
                .application
                .materialize_extension_provider(request)
                .await?,
        ),
    ))
}

#[handler]
pub(super) async fn delete_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let provider_id = path_parameter(request, "provider_id")?;
    app_state(depot)
        .shared
        .application
        .delete_provider_profile(&provider_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn discover_provider_models(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ternilo_protocol::ProviderModel>>, ApiError> {
    let discovery = request
        .parse_json::<ProviderModelDiscoveryRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .discover_provider_models(discovery)
            .await?,
    ))
}
