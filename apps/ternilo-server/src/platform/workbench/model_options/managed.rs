use super::{
    AppState, ControlUser, DefaultModelSelection, HarnessError, ResourceKind, RunModelBinding,
    RunModelSnapshot, TenantId, UserId, now_ms, resolve_selection,
};

pub(crate) async fn selected(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    resource: Option<(ResourceKind, &str)>,
    resource_owner: &UserId,
    previous: Option<&RunModelSnapshot>,
    selection: DefaultModelSelection,
) -> Result<Option<RunModelSnapshot>, HarnessError> {
    let inherited_workspace = resource.is_some_and(|(kind, _)| kind == ResourceKind::Workspace);
    let owner = match &selection {
        DefaultModelSelection::AccountProvider {
            owner_user_id,
            provider_id,
            model,
            ..
        } => {
            let approved = previous.is_some_and(|snapshot| matches!(&snapshot.binding,
                RunModelBinding::UserProvider { owner_user_id: saved_owner, provider_id: saved_provider, model: saved_model, .. }
                if saved_owner == owner_user_id && saved_provider == provider_id && saved_model == model));
            if owner_user_id != &user.user_id
                && !approved
                && !(inherited_workspace && owner_user_id == resource_owner)
            {
                return Err(HarnessError::policy(
                    "only the model owner may authorize a new account model",
                ));
            }
            owner_user_id
        }
        DefaultModelSelection::PlatformModel {
            grant_id, model_id, ..
        } => previous
            .and_then(|snapshot| match &snapshot.binding {
                RunModelBinding::Platform {
                    grant_id: saved_grant,
                    model_id: saved_model,
                    beneficiary_user_id,
                } if saved_grant == grant_id && saved_model == model_id => {
                    Some(beneficiary_user_id)
                }
                _ => None,
            })
            .unwrap_or(if inherited_workspace {
                resource_owner
            } else {
                &user.user_id
            }),
        _ => {
            if inherited_workspace {
                resource_owner
            } else {
                &user.user_id
            }
        }
    };
    let resolved = resolve_selection(state, user, tenant, owner, selection.clone()).await?;
    if let (Some((kind, id)), Some(snapshot)) = (resource, &resolved) {
        state
            .cloud
            .require_model_owner(tenant, kind, id, &snapshot.binding)
            .await?;
    }
    Ok(resolved)
}

pub(crate) async fn current(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    kind: ResourceKind,
    id: &str,
    snapshot: &RunModelSnapshot,
) -> Result<RunModelSnapshot, HarnessError> {
    state
        .cloud
        .require_model_owner(tenant, kind, id, &snapshot.binding)
        .await?;
    state
        .store
        .resolve_workload_model_snapshot(
            &user.user_id,
            snapshot.binding.beneficiary_user_id(),
            tenant,
            &snapshot.binding,
            snapshot.reasoning_effort,
            now_ms()?,
        )
        .await
        .map_err(|error| error.error)
}
