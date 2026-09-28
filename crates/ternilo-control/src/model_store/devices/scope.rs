use ternilo_protocol::{HarnessError, ModelDeviceScope};
use ternilo_storage::Transaction;

use super::super::{grants, keys, validate_ids};
use crate::ControlUser;

pub(super) async fn validate_scope(
    tx: &mut Transaction,
    actor: &ControlUser,
    scope: &ModelDeviceScope,
    now: u64,
) -> Result<(), HarnessError> {
    if let ModelDeviceScope::Selected {
        grants: selected,
        providers,
    } = scope
    {
        if selected.is_empty() && providers.is_empty() {
            return Err(HarnessError::invalid("select at least one model source"));
        }
        let ids: Vec<_> = selected
            .iter()
            .map(|entry| entry.grant_id.clone())
            .collect();
        if !ids.is_empty() {
            validate_ids(&ids)?;
        }
        for entry in selected {
            validate_ids(&entry.model_ids)?;
            keys::lock_grant(tx, &entry.grant_id, now).await?;
            let grant = grants::grant_in(tx, &entry.grant_id, now).await?;
            grants::require_grant(tx, &actor.user_id, &grant, now).await?;
            if entry
                .model_ids
                .iter()
                .any(|id| !grant.model_ids.contains(id))
            {
                return Err(HarnessError::policy(
                    "device scope exceeds the current model grant",
                ));
            }
        }
        let ids: Vec<_> = providers
            .iter()
            .map(|entry| entry.provider_id.clone())
            .collect();
        if !ids.is_empty() {
            validate_ids(&ids)?;
        }
        let catalog = super::account::catalog_in(tx, &actor.user_id, None).await?;
        for entry in providers {
            validate_ids(&entry.model_ids)?;
            if !catalog.iter().any(|provider| {
                provider.provider_id == entry.provider_id
                    && entry
                        .model_ids
                        .iter()
                        .all(|id| provider.models.iter().any(|model| &model.model_id == id))
            }) {
                return Err(HarnessError::policy(
                    "device scope exceeds the account Provider catalog",
                ));
            }
        }
        super::super::scope(tx).await?;
    }
    Ok(())
}

pub(super) fn permits(scope: &ModelDeviceScope, grant: &str, model: &str) -> bool {
    match scope {
        ModelDeviceScope::Account { .. } => true,
        ModelDeviceScope::Selected { grants, .. } => grants
            .iter()
            .any(|entry| entry.grant_id == grant && entry.model_ids.iter().any(|id| id == model)),
    }
}

pub(super) fn permits_provider(scope: &ModelDeviceScope, provider: &str, model: &str) -> bool {
    match scope {
        ModelDeviceScope::Account {
            include_account_providers,
        } => *include_account_providers,
        ModelDeviceScope::Selected { providers, .. } => providers.iter().any(|entry| {
            entry.provider_id == provider && entry.model_ids.iter().any(|id| id == model)
        }),
    }
}
