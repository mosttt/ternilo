use super::{
    AppState, ControlUser, CurrentModel, DefaultModelSelection, HarnessError, ModelOption,
    ModelOptions, RunModelBinding, RunModelSnapshot, TenantId, public_snapshot, resolve_selection,
};

pub(super) async fn edge_model_options(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    session: ternilo_control::EdgeSessionRecord,
    query: ternilo_control::PageQuery,
) -> Result<ModelOptions, HarnessError> {
    let selection: DefaultModelSelection = serde_json::from_value(session.metadata.model)
        .map_err(|error| HarnessError::invalid(error.to_string()))?;
    let current = if matches!(
        &selection,
        DefaultModelSelection::AccountProvider { .. } | DefaultModelSelection::PlatformModel { .. }
    ) {
        let mut current = None;
        if let Some(saved) = &session.metadata.server_model {
            let owner = saved.binding.beneficiary_user_id();
            if state
                .store
                .require_edge_model_owner_access(owner, tenant, &session.session_id)
                .await
                .is_ok()
            {
                current = state
                    .store
                    .resolve_workload_model_snapshot(
                        &user.user_id,
                        owner,
                        tenant,
                        &saved.binding,
                        None,
                        crate::platform::http::now_ms()?,
                    )
                    .await
                    .ok();
            }
        }
        let mut snapshot = session.metadata.server_model;
        let available = snapshot
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(saved, current)| snapshot_still_available(saved, current));
        if let Some((saved, current)) = snapshot.as_mut().zip(current.as_ref()) {
            saved.defaults.context_window = saved
                .defaults
                .context_window
                .min(current.defaults.context_window);
            saved.defaults.max_output_tokens = saved
                .defaults
                .max_output_tokens
                .min(current.defaults.max_output_tokens);
            saved.display_name.clone_from(&current.display_name);
            saved.source_name.clone_from(&current.source_name);
        }
        Some(CurrentModel {
            selectable_reasoning: current.and_then(|value| value.defaults.reasoning),
            selection,
            model: snapshot.as_ref().map(public_snapshot),
            source_name: snapshot.map(|value| value.source_name),
            available,
            unavailable_reason: (!available).then(|| {
                "The Server model or its session authorization is unavailable.".to_owned()
            }),
        })
    } else {
        None
    };
    let page = state
        .store
        .list_model_entitlements(user, &query, crate::platform::http::now_ms()?)
        .await?;
    Ok(ModelOptions {
        current,
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
        providers: Vec::new(),
        credentials: ternilo_protocol::CredentialInventory {
            references: Vec::new(),
            records: Vec::new(),
        },
    })
}

fn snapshot_still_available(saved: &RunModelSnapshot, current: &RunModelSnapshot) -> bool {
    saved.binding == current.binding
        && saved.protocol == current.protocol
        && saved
            .resolved_model()
            .reasoning_value(saved.reasoning_effort)
            .is_ok_and(|wire| {
                wire.is_none_or(|wire| {
                    current
                        .defaults
                        .reasoning
                        .as_ref()
                        .is_some_and(|reasoning| {
                            reasoning
                                .efforts
                                .values()
                                .any(|value| value.as_deref() == Some(wire))
                        })
                })
            })
}

pub(crate) async fn resolve_edge_selection(
    state: &AppState,
    user: &ControlUser,
    tenant: &TenantId,
    mapping: &ternilo_control::EdgeSessionRecord,
    value: &serde_json::Value,
) -> Result<Option<RunModelSnapshot>, HarnessError> {
    let selection: DefaultModelSelection = serde_json::from_value(value.clone())
        .map_err(|error| HarnessError::invalid(error.to_string()))?;
    if let DefaultModelSelection::PlatformModel {
        grant_id, model_id, ..
    } = &selection
    {
        let owner = mapping
            .metadata
            .server_model
            .as_ref()
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
            .unwrap_or(&user.user_id);
        state
            .store
            .require_edge_model_owner_access(owner, tenant, &mapping.session_id)
            .await?;
        return resolve_selection(state, user, tenant, owner, selection.clone()).await;
    }
    let DefaultModelSelection::AccountProvider {
        owner_user_id,
        provider_id,
        model,
        ..
    } = &selection
    else {
        return Ok(None);
    };
    if owner_user_id != &user.user_id && !mapping.metadata.server_model.as_ref().is_some_and(|snapshot| matches!(&snapshot.binding,
        RunModelBinding::UserProvider { owner_user_id: owner, provider_id: provider, model: selected, .. }
        if owner == owner_user_id && provider == provider_id && selected == model)) {
        return Err(HarnessError::policy("only the Provider owner may authorize a new account model for this session"));
    }
    state
        .store
        .require_edge_model_owner_access(owner_user_id, tenant, &mapping.session_id)
        .await?;
    resolve_selection(
        state,
        user,
        &state.store.account_provider_space(owner_user_id).await?,
        owner_user_id,
        selection.clone(),
    )
    .await
}
