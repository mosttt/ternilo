use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, handler},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use ternilo_control::{ControlUser, ResourceAction, ResourceKind};
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetUpdateRequest,
    AuthorizationBeginRequest, AuthorizationCredentialKey, AuthorizationPromptAnswer,
    AuthorizationSnapshot, CredentialInventory, CredentialRecordInfo,
    ExtensionProviderMaterializeRequest, HarnessError, ProviderModel,
    ProviderModelDiscoveryRequest, ProviderProfile, SessionId, SidebarOrdering, TenantId,
    WorkspaceId,
};
use ternilo_transport::{ApplicationOperation, ExecutorId};

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

use super::placement::{PlacementResolver, SessionTarget, SettingsTarget, WorkspaceTarget};

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
    payload: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "Field names match the shared HTTP query contract."
)]
struct AuthorizationQuery {
    surface_id: String,
    #[serde(default)]
    session_id: Option<SessionId>,
    #[serde(default)]
    workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    executor_id: Option<ExecutorId>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "Field names match the shared HTTP query contract."
)]
struct SettingsQuery {
    #[serde(default)]
    session_id: Option<SessionId>,
    #[serde(default)]
    workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    executor_id: Option<ExecutorId>,
}

struct SharedSettingsRead {
    target: SettingsTarget,
    kind: ResourceKind,
    resource_id: String,
}

async fn shared_settings_read(
    state: &crate::platform::state::AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    query: &SettingsQuery,
) -> Result<Option<SharedSettingsRead>, HarnessError> {
    validate_settings_target(
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )?;
    let (kind, resource_id) = if let Some(id) = &query.session_id {
        (ResourceKind::Session, id.as_str())
    } else if let Some(id) = &query.workspace_id {
        (ResourceKind::Workspace, id.as_str())
    } else {
        return Ok(None);
    };
    let access = state
        .store
        .resource_access(user, tenant_id, kind, resource_id)
        .await?;
    if access.is_owner {
        return Ok(None);
    }
    access.require(ResourceAction::View)?;
    let resolver = PlacementResolver::new(state, user, tenant_id);
    let target = match kind {
        ResourceKind::Session => match resolver.session(&SessionId::new(resource_id)).await? {
            SessionTarget::Cloud(_) => SettingsTarget::Cloud,
            SessionTarget::Edge(session) => SettingsTarget::Edge(session.executor_id),
        },
        ResourceKind::Workspace => {
            match resolver.workspace(&WorkspaceId::new(resource_id)).await? {
                WorkspaceTarget::Cloud(_) => SettingsTarget::Cloud,
                WorkspaceTarget::Edge { executor_id, .. } => SettingsTarget::Edge(executor_id),
            }
        }
    };
    Ok(Some(SharedSettingsRead {
        target,
        kind,
        resource_id: resource_id.to_owned(),
    }))
}

async fn resolve_settings_target(
    state: &crate::platform::state::AppState,
    user: &ternilo_control::ControlUser,
    tenant_id: &TenantId,
    session_id: Option<&SessionId>,
    workspace_id: Option<&WorkspaceId>,
    executor_id: Option<&ExecutorId>,
) -> Result<SettingsTarget, HarnessError> {
    validate_settings_target(session_id, workspace_id, executor_id)?;
    if let Some(executor) = executor_id {
        let record = state
            .store
            .owned_executor(user, tenant_id, executor)
            .await?;
        if record.state == "revoked" {
            return Err(HarnessError::policy("computer access was revoked"));
        }
        return Ok(SettingsTarget::Edge(executor.clone()));
    }
    PlacementResolver::new(state, user, tenant_id)
        .settings(session_id, workspace_id)
        .await
}

fn validate_settings_target(
    session_id: Option<&SessionId>,
    workspace_id: Option<&WorkspaceId>,
    executor_id: Option<&ExecutorId>,
) -> Result<(), HarnessError> {
    if executor_id.is_some() && (session_id.is_some() || workspace_id.is_some()) {
        return Err(HarnessError::invalid(
            "choose either a computer or a workspace/session settings target",
        ));
    }
    Ok(())
}

async fn call_edge(
    state: &crate::platform::state::AppState,
    tenant_id: &TenantId,
    target: &SettingsTarget,
    operation: ApplicationOperation,
) -> Result<Option<Value>, HarnessError> {
    target.read(state, tenant_id, operation).await
}

async fn call_edge_mutation(
    state: &crate::platform::state::AppState,
    user: &ternilo_control::ControlUser,
    tenant_id: &TenantId,
    target: &SettingsTarget,
    operation: ApplicationOperation,
) -> Result<Option<Value>, HarnessError> {
    target.mutate(state, user, tenant_id, operation).await
}

fn decode<T: DeserializeOwned>(value: Value, label: &str) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::execution(format!("decode {label}: {error}")))
}

#[handler]
pub(super) async fn get_sidebar_ordering(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SidebarOrdering>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    Ok(Json(
        app_state(depot)
            .store
            .user_sidebar_ordering(actor(depot), &tenant_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn set_sidebar_ordering(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SidebarOrdering>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let ordering = request
        .parse_json::<SidebarOrdering>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .set_user_sidebar_ordering(actor(depot), &tenant_id, ordering, now_ms()?)
            .await?,
    ))
}

#[derive(Serialize)]
struct WorkbenchProviderProfile {
    #[serde(flatten)]
    profile: ProviderProfile,
    source: ProviderSource,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProviderSource {
    User,
}

async fn shared_provider_inventory(
    state: &crate::platform::state::AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    shared: &SharedSettingsRead,
) -> Result<Vec<WorkbenchProviderProfile>, HarnessError> {
    let mut providers = if let Some(value) = call_edge(
        state,
        tenant_id,
        &shared.target,
        ApplicationOperation::ProviderList,
    )
    .await?
    {
        decode::<Vec<ProviderProfile>>(value, "Node Provider inventory")?
            .into_iter()
            .map(|profile| WorkbenchProviderProfile {
                profile,
                source: ProviderSource::User,
            })
            .collect::<Vec<_>>()
    } else {
        state
            .cloud
            .resource_model_providers(tenant_id, &user.user_id, shared.kind, &shared.resource_id)
            .await?
            .into_iter()
            .map(|profile| WorkbenchProviderProfile {
                profile,
                source: ProviderSource::User,
            })
            .collect::<Vec<_>>()
    };
    for provider in &mut providers {
        provider.profile.base_url.clear();
    }
    Ok(providers)
}

#[handler]
pub(super) async fn list_providers(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<WorkbenchProviderProfile>>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    if let Some(shared) = shared_settings_read(state, actor(depot), &tenant_id, &query).await? {
        return Ok(Json(
            shared_provider_inventory(state, actor(depot), &tenant_id, &shared).await?,
        ));
    }
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::ProviderList,
    )
    .await?
    {
        return Ok(Json(
            decode::<Vec<ProviderProfile>>(value, "Node Provider inventory")?
                .into_iter()
                .map(|profile| WorkbenchProviderProfile {
                    profile,
                    source: ProviderSource::User,
                })
                .collect(),
        ));
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Ok(Json(
        state
            .store
            .user_provider_profiles(actor(depot), &tenant_id)
            .await?
            .into_iter()
            .map(|profile| WorkbenchProviderProfile {
                profile,
                source: ProviderSource::User,
            })
            .collect(),
    ))
}

#[handler]
pub(super) async fn upsert_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ProviderProfile>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let provider = request
        .parse_json::<ProviderProfile>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::ProviderUpsert {
            provider: provider.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node Provider update response")?));
    }

    Ok(Json(
        state
            .store
            .upsert_user_provider_profile(actor(depot), &tenant_id, provider, now_ms()?)
            .await?,
    ))
}

#[handler]
pub(super) async fn materialize_extension_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ProviderProfile>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let materialize = request
        .parse_json::<ExtensionProviderMaterializeRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::ProviderMaterialize {
            request: materialize.clone(),
        },
    )
    .await?
    {
        return Ok((
            StatusCode::CREATED,
            Json(decode(
                value,
                "Node extension Provider materialization response",
            )?),
        ));
    }

    let inventory = state
        .store
        .extension_inventory(actor(depot), &tenant_id)
        .await?;
    let provider =
        ternilo_extension::materialize_provider_from_inventory(&inventory, &materialize)?;
    Ok((
        StatusCode::CREATED,
        Json(
            state
                .store
                .create_user_provider_profile(actor(depot), &tenant_id, provider, now_ms()?)
                .await?,
        ),
    ))
}

#[handler]
pub(super) async fn delete_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let provider_id = path_parameter(request, "provider_id")?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::ProviderDelete {
            id: provider_id.clone(),
        },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }

    state
        .store
        .delete_user_provider_profile(actor(depot), &tenant_id, &provider_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn discover_provider_models(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ProviderModel>>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let discovery = request
        .parse_json::<ProviderModelDiscoveryRequest>()
        .await
        .map_err(invalid_request)?;
    discovery.validate()?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::ProviderDiscover {
            request: discovery.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node Provider discovery response")?));
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    let provider = match discovery.provider_id.as_deref() {
        Some(provider_id) => Some(
            state
                .store
                .user_provider_profile(actor(depot), &tenant_id, provider_id)
                .await?
                .ok_or_else(|| {
                    HarnessError::invalid(format!("unknown user Provider {provider_id:?}"))
                })?,
        ),
        None => None,
    };
    let base_url = match discovery.base_url.as_deref() {
        Some(base_url) => base_url.trim().trim_end_matches('/').to_owned(),
        None => provider
            .as_ref()
            .map(|provider| provider.base_url.trim_end_matches('/').to_owned())
            .ok_or_else(|| HarnessError::invalid("provider discovery requires base_url"))?,
    };
    let timeout_ms = discovery
        .timeout_ms
        .or_else(|| provider.as_ref().map(|provider| provider.timeout_ms))
        .unwrap_or(120_000);
    let protocol = discovery
        .protocol
        .or_else(|| provider.as_ref().map(|provider| provider.protocol))
        .unwrap_or_default();
    let api_key = match discovery.api_key.as_deref().map(str::trim) {
        Some(api_key) if !api_key.is_empty() => Some(api_key.to_owned()),
        _ => match provider
            .as_ref()
            .and_then(|provider| provider.api_key_ref.as_deref())
        {
            Some(reference) => Some(
                state
                    .store
                    .resolve_user_credential(actor(depot), &tenant_id, reference)
                    .await?
                    .ok_or_else(|| {
                        HarnessError::policy(format!(
                            "provider credential reference {reference:?} is not configured"
                        ))
                    })?
                    .to_string(),
            ),
            None => None,
        },
    };
    let mut client = reqwest::Client::builder();
    if timeout_ms > 0 {
        client = client.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let client = client.build().map_err(|error| {
        HarnessError::execution(format!("build provider discovery client: {error}"))
    })?;
    Ok(Json(
        ternilo_builtins::discover_provider_models(
            &client,
            &base_url,
            protocol,
            api_key.as_deref(),
        )
        .await?,
    ))
}

#[handler]
pub(super) async fn list_agent_presets(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetRoster>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    if let Some(shared) = shared_settings_read(state, actor(depot), &tenant_id, &query).await? {
        let mut roster: AgentPresetRoster = if let Some(value) = call_edge(
            state,
            &tenant_id,
            &shared.target,
            ApplicationOperation::AgentPresetList,
        )
        .await?
        {
            decode(value, "Node Agent preset roster")?
        } else {
            state
                .cloud
                .resource_agent_preset_roster(
                    &tenant_id,
                    &actor(depot).user_id,
                    shared.kind,
                    &shared.resource_id,
                )
                .await?
        };
        roster.authorable = false;
        return Ok(Json(roster));
    }
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetList,
    )
    .await?
    {
        return Ok(Json(decode(value, "Node Agent preset roster")?));
    }
    Ok(Json(
        state
            .store
            .user_agent_preset_roster(actor(depot), &tenant_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn get_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetDocument>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let preset_id = path_parameter(request, "preset_id")?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetGet {
            preset_id: preset_id.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node Agent preset")?));
    }
    let mut document = state
        .store
        .user_agent_preset(actor(depot), &tenant_id, &preset_id)
        .await?;
    document.base_profile = Some(ternilo_cloud::cloud_profile(None));
    Ok(Json(document))
}

#[handler]
pub(super) async fn copy_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<AgentPresetDocument>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let copy = request
        .parse_json::<AgentPresetCopyRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetCopy {
            request: copy.clone(),
        },
    )
    .await?
    {
        return Ok((
            StatusCode::CREATED,
            Json(decode(value, "Node Agent preset copy response")?),
        ));
    }
    let document = state
        .store
        .copy_user_agent_preset(actor(depot), &tenant_id, copy, now_ms()?)
        .await?;
    Ok((StatusCode::CREATED, Json(document)))
}

#[handler]
pub(super) async fn update_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetDocument>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let preset_id = path_parameter(request, "preset_id")?;
    let update = request
        .parse_json::<AgentPresetUpdateRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if matches!(&target, SettingsTarget::Cloud)
        && update
            .profile
            .plugins
            .iter()
            .any(|entry| entry.enabled && entry.kind == ternilo_extension::EXTENSION_PACKAGE_KIND)
    {
        state
            .store
            .resolve_extensions(
                actor(depot),
                &tenant_id,
                &update.profile,
                &state.worker_policy.extension_host_policy,
            )
            .await?;
    }
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetUpdate {
            preset_id: preset_id.clone(),
            request: update.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node Agent preset update response")?));
    }
    Ok(Json(
        state
            .store
            .update_user_agent_preset(actor(depot), &tenant_id, &preset_id, update, now_ms()?)
            .await?,
    ))
}

#[handler]
pub(super) async fn delete_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let preset_id = path_parameter(request, "preset_id")?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetDelete {
            preset_id: preset_id.clone(),
        },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .delete_user_agent_preset(actor(depot), &tenant_id, &preset_id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn set_default_agent_preset(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentPresetRoster>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let preset_id = path_parameter(request, "preset_id")?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AgentPresetSetDefault {
            preset_id: preset_id.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node default Agent preset response")?));
    }
    Ok(Json(
        state
            .store
            .set_default_user_agent_preset(actor(depot), &tenant_id, &preset_id, now_ms()?)
            .await?,
    ))
}

#[handler]
pub(super) async fn list_credentials(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<CredentialInventory>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    if let Some(shared) = shared_settings_read(state, actor(depot), &tenant_id, &query).await? {
        let mut inventory: CredentialInventory = if let Some(value) = call_edge(
            state,
            &tenant_id,
            &shared.target,
            ApplicationOperation::CredentialList,
        )
        .await?
        {
            decode(value, "Node credential inventory")?
        } else {
            state
                .cloud
                .resource_credential_inventory(
                    &tenant_id,
                    &actor(depot).user_id,
                    shared.kind,
                    &shared.resource_id,
                )
                .await?
        };
        for reference in &mut inventory.references {
            reference.writable = false;
        }
        return Ok(Json(inventory));
    }
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::CredentialList,
    )
    .await?
    {
        return Ok(Json(decode(value, "Node credential inventory")?));
    }
    Ok(Json(
        state
            .store
            .user_credential_inventory(actor(depot), &tenant_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn set_credential(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let credential = request
        .parse_json::<CredentialRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::CredentialSet {
            name: credential.name.clone(),
            value: credential.value.clone(),
        },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .put_user_credential(
            actor(depot),
            &tenant_id,
            &credential.name,
            &credential.value,
            now_ms()?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn delete_credential(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let name = path_parameter(request, "name")?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::CredentialRemove { name: name.clone() },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .delete_user_credential(actor(depot), &tenant_id, &name)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn set_credential_record(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<CredentialRecordInfo>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let record = request
        .parse_json::<CredentialRecordRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::CredentialRecordSet {
            key: record.key.clone(),
            kind: record.kind.clone(),
            payload: record.payload.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(
            value,
            "Node credential record update response",
        )?));
    }
    Ok(Json(
        state
            .store
            .put_user_credential_record(
                actor(depot),
                &tenant_id,
                &record.key,
                &record.kind,
                &record.payload,
                now_ms()?,
            )
            .await?,
    ))
}

#[handler]
pub(super) async fn delete_credential_record(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let scope = path_parameter(request, "scope")?;
    let record_id = path_parameter(request, "record_id")?;
    let key = format!("{scope}/{record_id}");
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::CredentialRecordDelete { key: key.clone() },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .delete_user_credential_record(actor(depot), &tenant_id, &key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn authorization_snapshot(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AuthorizationSnapshot>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<AuthorizationQuery>()
        .map_err(invalid_request)?;
    validate_surface(&query.surface_id)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge(
        state,
        &tenant_id,
        &target,
        ApplicationOperation::AuthorizationSnapshot {
            surface_id: query.surface_id.clone(),
        },
    )
    .await?
    {
        return Ok(Json(decode(value, "Node authorization snapshot")?));
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Ok(Json(AuthorizationSnapshot {
        entries: Vec::new(),
        attempts: Vec::new(),
        notices: Vec::new(),
        prompts: Vec::new(),
    }))
}

#[handler]
pub(super) async fn begin_authorization(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let begin = request
        .parse_json::<AuthorizationBeginRequest>()
        .await
        .map_err(invalid_request)?;
    begin.key.validate()?;
    validate_surface(&begin.surface_id)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if let Some(value) = call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AuthorizationBegin {
            request: begin.clone(),
        },
    )
    .await?
    {
        return Ok(Json(value));
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Err(authorization_unavailable())
}

#[handler]
pub(super) async fn answer_authorization_prompt(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let answer = request
        .parse_json::<AuthorizationPromptAnswer>()
        .await
        .map_err(invalid_request)?;
    validate_surface(&answer.surface_id)?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AuthorizationAnswer {
            answer: answer.clone(),
        },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Err(authorization_unavailable())
}

#[handler]
pub(super) async fn cancel_authorization(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<SettingsQuery>()
        .map_err(invalid_request)?;
    let key = request
        .parse_json::<AuthorizationCredentialKey>()
        .await
        .map_err(invalid_request)?;
    key.validate()?;
    let state = app_state(depot);
    let target = resolve_settings_target(
        state,
        actor(depot),
        &tenant_id,
        query.session_id.as_ref(),
        query.workspace_id.as_ref(),
        query.executor_id.as_ref(),
    )
    .await?;
    if call_edge_mutation(
        state,
        actor(depot),
        &tenant_id,
        &target,
        ApplicationOperation::AuthorizationCancel { key: key.clone() },
    )
    .await?
    .is_some()
    {
        return Ok(StatusCode::NO_CONTENT);
    }
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    Err(authorization_unavailable())
}

fn validate_surface(surface_id: &str) -> Result<(), HarnessError> {
    if surface_id.is_empty()
        || surface_id.len() > 128
        || !surface_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Err(HarnessError::invalid(
            "authorization surface_id must contain 1 to 128 safe ASCII characters",
        ))
    } else {
        Ok(())
    }
}

fn authorization_unavailable() -> ApiError {
    HarnessError::composition(
        "interactive authorization is unavailable because this Control deployment has no registered authorization flow",
    )
    .into()
}
