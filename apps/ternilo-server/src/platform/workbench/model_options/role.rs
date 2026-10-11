use super::{
    ApiError, CurrentModel, DefaultModelSelection, Depot, HarnessError, Json, PlacementResolver,
    Request, ResourceAction, ResourceKind, SessionTarget, actor, app_state, handler,
    invalid_request, managed, public_snapshot, resolve_edge_selection, tenant_parameter,
};
use serde::Deserialize;
use ternilo_protocol::{AgentRole, CoordinationConfig, Profile, SessionId};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    session_id: SessionId,
    role: AgentRole,
    selection: DefaultModelSelection,
}

#[handler]
pub(crate) async fn preview(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Option<CurrentModel>>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let input = request
        .parse_json::<Preview>()
        .await
        .map_err(invalid_request)?;
    input.selection.validate()?;
    let state = app_state(depot);
    let user = actor(depot);
    let access = state
        .store
        .resource_access(
            user,
            &tenant,
            ResourceKind::Session,
            input.session_id.as_str(),
        )
        .await?;
    access.require(ResourceAction::Configure)?;
    let snapshot = match PlacementResolver::new(state, user, &tenant)
        .session(&input.session_id)
        .await?
    {
        SessionTarget::Edge(mut session) => {
            session.metadata.server_model = session
                .metadata
                .role_models
                .get(&input.role)
                .cloned()
                .unwrap_or(session.metadata.server_model);
            resolve_edge_selection(
                state,
                user,
                &tenant,
                &session,
                &serde_json::to_value(&input.selection).map_err(invalid_request)?,
            )
            .await?
        }
        SessionTarget::Cloud(session) => {
            let roles = CoordinationConfig::from_profile(&Profile {
                plugins: session.profile_plugins,
            })?;
            let previous = roles
                .resolved_models
                .get(&input.role)
                .or(session.model.as_ref());
            managed::selected(
                state,
                user,
                &tenant,
                Some((ResourceKind::Session, input.session_id.as_str())),
                &access.storage_user_id,
                previous,
                input.selection.clone(),
            )
            .await?
        }
    };
    if snapshot.is_none() && !matches!(input.selection, DefaultModelSelection::ProfileDefault) {
        return Err(HarnessError::invalid("role model is not a Server model").into());
    }
    Ok(Json(snapshot.map(|snapshot| CurrentModel {
        selection: input.selection,
        model: Some(public_snapshot(&snapshot)),
        selectable_reasoning: snapshot.defaults.reasoning,
        source_name: Some(snapshot.source_name),
        owner_user_id: Some(snapshot.binding.beneficiary_user_id().clone()),
        available: true,
        unavailable_reason: None,
    })))
}
