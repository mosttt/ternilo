use salvo_core::prelude::{Depot, Json, Request, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::{ControlUser, PageQuery, PublicModel, ResourceAction, ResourceKind};
use ternilo_protocol::{
    CredentialInventory, DefaultModelSelection, HarnessError, ProviderModelReasoning,
    ProviderProfile, RunModelBinding, RunModelSnapshot, SessionId, TenantId, UserId, WorkspaceId,
};

use super::placement::{PlacementResolver, SessionTarget, WorkspaceTarget};
use crate::platform::{
    http::{ApiError, invalid_request, now_ms, tenant_parameter},
    state::{AppState, actor, app_state},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionsQuery {
    session_id: Option<SessionId>,
    workspace_id: Option<WorkspaceId>,
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[derive(Serialize)]
struct ModelOptions {
    current: Option<CurrentModel>,
    options: Vec<ModelOption>,
    next_cursor: Option<String>,
    providers: Vec<ProviderProfile>,
    credentials: CredentialInventory,
}

#[derive(Serialize)]
struct ModelOption {
    grant_id: String,
    grant_name: String,
    model: PublicModel,
}

#[derive(Serialize)]
struct CurrentModel {
    selection: DefaultModelSelection,
    model: Option<PublicModel>,
    selectable_reasoning: Option<ProviderModelReasoning>,
    source_name: Option<String>,
    available: bool,
    unavailable_reason: Option<String>,
}

mod edge;
pub(crate) use edge::resolve_edge_selection;

pub(crate) async fn resolve_selection(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    owner: &UserId,
    selection: DefaultModelSelection,
) -> Result<Option<RunModelSnapshot>, HarnessError> {
    selection.validate()?;
    let (binding, effort) = match selection {
        DefaultModelSelection::ProfileDefault => return Ok(None),
        DefaultModelSelection::PlatformModel {
            grant_id,
            model_id,
            reasoning_effort,
        } => (
            RunModelBinding::Platform {
                grant_id,
                model_id,
                beneficiary_user_id: owner.clone(),
            },
            reasoning_effort,
        ),
        DefaultModelSelection::NamedProvider {
            provider_id,
            model,
            reasoning_effort,
        } => (
            RunModelBinding::UserProvider {
                tenant_id: state.store.account_provider_space(owner).await?,
                owner_user_id: owner.clone(),
                provider_id,
                model,
            },
            reasoning_effort,
        ),
        DefaultModelSelection::AccountProvider {
            owner_user_id,
            provider_id,
            model,
            reasoning_effort,
        } => {
            if &owner_user_id != owner {
                return Err(HarnessError::policy(
                    "account Provider owner does not match the authorized source",
                ));
            }
            (
                RunModelBinding::UserProvider {
                    tenant_id: state.store.account_provider_space(owner).await?,
                    owner_user_id,
                    provider_id,
                    model,
                },
                reasoning_effort,
            )
        }
        DefaultModelSelection::OpenAiCompatible { .. } => {
            return Err(HarnessError::policy(
                "Managed sessions require a platform model authorization or a saved user Provider",
            ));
        }
    };
    state
        .store
        .resolve_workload_model_snapshot(&user.user_id, owner, tenant, &binding, effort, now_ms()?)
        .await
        .map(Some)
        .map_err(|error| error.error)
}

fn public_snapshot(snapshot: &RunModelSnapshot) -> PublicModel {
    PublicModel {
        model_id: snapshot.binding.model_id().to_owned(),
        display_name: snapshot.display_name.clone(),
        protocol: snapshot.protocol,
        defaults: snapshot.defaults.clone(),
    }
}

#[handler]
pub(super) async fn model_options(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelOptions>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let query = request
        .parse_queries::<OptionsQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let user = actor(depot);
    let resolver = PlacementResolver::new(state, user, &tenant);
    let (resource, saved) = match (&query.session_id, &query.workspace_id) {
        (Some(id), None) => match resolver.session(id).await? {
            SessionTarget::Cloud(session) => {
                (Some((ResourceKind::Session, id.as_str())), session.model)
            }
            SessionTarget::Edge(session) => {
                return edge::edge_model_options(
                    state,
                    user,
                    &tenant,
                    session,
                    PageQuery {
                        query: query.query,
                        cursor: query.cursor,
                        limit: query.limit.unwrap_or(25),
                    },
                )
                .await
                .map(Json)
                .map_err(Into::into);
            }
        },
        (None, Some(id)) => match resolver.workspace(id).await? {
            WorkspaceTarget::Cloud(_) => (Some((ResourceKind::Workspace, id.as_str())), None),
            WorkspaceTarget::Edge { .. } => {
                return Ok(Json(ModelOptions {
                    current: None,
                    options: Vec::new(),
                    next_cursor: None,
                    providers: Vec::new(),
                    credentials: CredentialInventory {
                        references: Vec::new(),
                        records: Vec::new(),
                    },
                }));
            }
        },
        (None, None) => (None, None),
        _ => {
            return Err(
                HarnessError::invalid("Specify at most one session_id or workspace_id").into(),
            );
        }
    };
    let owner = if let Some((kind, id)) = resource {
        let access = state.store.resource_access(user, &tenant, kind, id).await?;
        access.require(ResourceAction::View)?;
        access.storage_user_id
    } else {
        state
            .store
            .authorize(user, &tenant, ternilo_control::ControlAction::TenantRead)
            .await?;
        user.user_id.clone()
    };
    let selection = match resource {
        Some((ResourceKind::Session, _)) => saved.as_ref().map_or(
            DefaultModelSelection::ProfileDefault,
            RunModelSnapshot::selection,
        ),
        Some((kind, id)) => {
            state
                .cloud
                .resource_default_model(&tenant, &user.user_id, kind, id)
                .await?
        }
        None => state.store.user_default_model(user, &tenant).await?,
    };
    let current = if matches!(selection, DefaultModelSelection::ProfileDefault) {
        None
    } else {
        let resolved = if let Some(snapshot) = &saved {
            state
                .store
                .resolve_workload_model_snapshot(
                    &user.user_id,
                    &owner,
                    &tenant,
                    &snapshot.binding,
                    snapshot.reasoning_effort,
                    now_ms()?,
                )
                .await
                .map(Some)
                .map_err(|error| error.error)
        } else {
            resolve_selection(state, user, &tenant, &owner, selection.clone()).await
        };
        match resolved {
            Ok(Some(model)) => Some(CurrentModel {
                selectable_reasoning: model.defaults.reasoning.clone(),
                selection, model: Some(public_snapshot(&model)), source_name: Some(model.source_name),
                available: true, unavailable_reason: None,
            }),
            Ok(None) => None,
            Err(error) if matches!(error.code, ternilo_protocol::ErrorCode::InvalidInput | ternilo_protocol::ErrorCode::PolicyDenied | ternilo_protocol::ErrorCode::Conflict) => Some(CurrentModel {
                selectable_reasoning: None,
                selection, model: saved.as_ref().map(public_snapshot), source_name: saved.map(|model| model.source_name),
                available: false, unavailable_reason: Some("The selected model or authorization is unavailable. Check model settings and resource access.".to_owned()),
            }),
            Err(error) => return Err(error.into()),
        }
    };
    let page_query = PageQuery {
        query: query.query,
        cursor: query.cursor,
        limit: query.limit.unwrap_or(25),
    };
    let (providers, credentials) = if let Some((kind, id)) = resource {
        state
            .cloud
            .resource_account_models(&tenant, &user.user_id, kind, id)
            .await?
    } else {
        let personal = state.store.account_provider_space(&user.user_id).await?;
        (
            state.store.user_provider_profiles(user, &personal).await?,
            state
                .store
                .user_credential_inventory(user, &personal)
                .await?,
        )
    };
    let page = match resource {
        Some((kind, id)) => {
            state
                .store
                .resource_model_entitlements(user, &tenant, kind, id, &page_query, now_ms()?)
                .await?
        }
        None => {
            state
                .store
                .list_model_entitlements(user, &page_query, now_ms()?)
                .await?
        }
    };
    Ok(Json(ModelOptions {
        current,
        providers,
        credentials,
        options: page
            .entitlements
            .into_iter()
            .flat_map(|entitlement| {
                entitlement
                    .models
                    .into_iter()
                    .map(move |model| ModelOption {
                        grant_id: entitlement.grant.grant_id.clone(),
                        grant_name: entitlement.grant.name.clone(),
                        model,
                    })
            })
            .collect(),
        next_cursor: page.next_cursor,
    }))
}
