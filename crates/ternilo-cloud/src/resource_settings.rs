use sqlx::Row;
use ternilo_control::{ControlStore, ResourceAction, ResourceKind, resource_access_in};
use ternilo_protocol::{
    AgentPresetDocument, AgentPresetRoster, CredentialInventory, CredentialRecordInfo,
    CredentialReferenceInfo, CredentialSource, DEFAULT_AGENT_PRESET_ID, DefaultModelSelection,
    HarnessError, ProviderProfile, RunModelBinding, RunModelSnapshot, TenantId, UserId,
    system_agent_preset, system_agent_presets, validate_agent_preset_id,
};
use ternilo_storage::{Json, Transaction, database_error, set_tenant_scope, set_user_scope};

use crate::CloudStore;

impl CloudStore {
    pub async fn resource_account_models(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<(Vec<ProviderProfile>, CredentialInventory), HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let personal = ControlStore::account_provider_space_in(&mut transaction, &owner_id).await?;
        let approved = if owner_id != *actor_id && kind == ResourceKind::Session {
            sqlx::query_scalar::<_, Option<Json<RunModelSnapshot>>>(
                "SELECT model_snapshot FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2",
            )
            .bind(tenant_id.as_str())
            .bind(resource_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?
            .map(|snapshot| snapshot.0.binding)
        } else {
            None
        };
        set_tenant_scope(&mut transaction, &personal).await?;
        let profiles = sqlx::query_scalar::<_, Json<ProviderProfile>>(
            "SELECT provider_json FROM control_user_provider_profiles WHERE tenant_id=$1 AND user_id=$2 ORDER BY provider_id",
        ).bind(personal.as_str()).bind(owner_id.as_str()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let configured: std::collections::BTreeSet<String> = sqlx::query_scalar(
            "SELECT name FROM control_user_credentials WHERE tenant_id=$1 AND user_id=$2 AND length(ciphertext)>16",
        ).bind(personal.as_str()).bind(owner_id.as_str()).fetch_all(&mut *transaction).await.map_err(database_error)?.into_iter().collect();
        let mut references = std::collections::BTreeSet::new();
        let providers = profiles
            .into_iter()
            .filter_map(|profile| {
                let mut profile = profile.0;
                if owner_id != *actor_id {
                    let Some(RunModelBinding::UserProvider {
                        tenant_id,
                        owner_user_id,
                        provider_id,
                        model,
                    }) = &approved
                    else {
                        return None;
                    };
                    if tenant_id != &personal
                        || owner_user_id != &owner_id
                        || provider_id != &profile.id
                    {
                        return None;
                    }
                    profile.models.retain(|entry| &entry.id == model);
                    if profile.models.is_empty() {
                        return None;
                    }
                }
                if let Some(reference) = &profile.api_key_ref {
                    references.insert(reference.clone());
                }
                if owner_id != *actor_id {
                    profile.base_url.clear();
                }
                Some(profile)
            })
            .collect();
        transaction.commit().await.map_err(database_error)?;
        Ok((
            providers,
            CredentialInventory {
                references: references
                    .into_iter()
                    .map(|reference| CredentialReferenceInfo {
                        configured: configured.contains(&reference),
                        reference,
                        source: Some(CredentialSource::Managed),
                        writable: false,
                    })
                    .collect(),
                records: Vec::new(),
            },
        ))
    }

    pub async fn resource_agent_preset_roster(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<AgentPresetRoster, HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let documents = sqlx::query_scalar::<_, Json<AgentPresetDocument>>(
            "SELECT document_json FROM control_user_agent_presets WHERE tenant_id=$1 AND user_id=$2 ORDER BY preset_id",
        ).bind(tenant_id.as_str()).bind(owner_id.as_str()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let default_id = sqlx::query_scalar::<_, String>(
            "SELECT default_agent_preset FROM control_user_preferences WHERE tenant_id=$1 AND user_id=$2",
        ).bind(tenant_id.as_str()).bind(owner_id.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?
            .unwrap_or_else(|| DEFAULT_AGENT_PRESET_ID.to_owned());
        transaction.commit().await.map_err(database_error)?;
        let mut presets = system_agent_presets()
            .into_iter()
            .map(|preset| preset.summary)
            .collect::<Vec<_>>();
        presets.extend(documents.into_iter().map(|document| document.0.summary));
        Ok(AgentPresetRoster {
            presets,
            default_id,
            authorable: owner_id == *actor_id,
        })
    }

    async fn resource_settings_transaction(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<(Transaction, UserId), HarnessError> {
        let mut transaction = self.tenant_transaction(tenant_id).await?;
        let access =
            resource_access_in(&mut transaction, actor_id, tenant_id, kind, resource_id).await?;
        access.require(ResourceAction::View)?;
        set_user_scope(&mut transaction, &access.owner_user_id).await?;
        Ok((transaction, access.owner_user_id))
    }

    /// Server-side resolution retains the owner's endpoint. Browser model lists
    /// use `resource_model_providers`, which removes private endpoints for guests.
    pub async fn resource_provider_profile(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
        provider_id: &str,
    ) -> Result<Option<ProviderProfile>, HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let provider = sqlx::query_scalar::<_, Json<ProviderProfile>>(
            "SELECT provider_json FROM control_user_provider_profiles
             WHERE tenant_id=$1 AND user_id=$2 AND provider_id=$3",
        )
        .bind(tenant_id.as_str())
        .bind(owner_id.as_str())
        .bind(provider_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(provider.map(|provider| provider.0))
    }

    pub async fn resource_model_providers(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<Vec<ProviderProfile>, HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let providers = sqlx::query_scalar::<_, Json<ProviderProfile>>(
            "SELECT provider_json FROM control_user_provider_profiles WHERE tenant_id=$1 AND user_id=$2 ORDER BY provider_id",
        ).bind(tenant_id.as_str()).bind(owner_id.as_str()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(providers
            .into_iter()
            .map(|provider| {
                let mut provider = provider.0;
                if owner_id != *actor_id {
                    // Endpoints may contain deployment-private hosts or query credentials.
                    provider.base_url.clear();
                }
                provider
            })
            .collect())
    }

    pub async fn resource_credential_inventory(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<CredentialInventory, HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let names = sqlx::query_scalar::<_, String>(
            "SELECT name FROM control_user_credentials WHERE tenant_id=$1 AND user_id=$2 ORDER BY name",
        ).bind(tenant_id.as_str()).bind(owner_id.as_str()).fetch_all(&mut *transaction).await.map_err(database_error)?;
        let records = sqlx::query(
            "SELECT record_key,kind,updated_at_ms FROM control_user_credential_records
             WHERE tenant_id=$1 AND user_id=$2 ORDER BY record_key",
        )
        .bind(tenant_id.as_str())
        .bind(owner_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        let records = records
            .into_iter()
            .map(|row| {
                Ok(CredentialRecordInfo {
                    key: row.try_get("record_key").map_err(database_error)?,
                    kind: row.try_get("kind").map_err(database_error)?,
                    updated_at_ms: crate::store::from_i64(
                        row.try_get("updated_at_ms").map_err(database_error)?,
                        "credential metadata timestamp",
                    )?,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        transaction.commit().await.map_err(database_error)?;
        Ok(CredentialInventory {
            references: names
                .into_iter()
                .map(|reference| CredentialReferenceInfo {
                    reference,
                    configured: true,
                    source: Some(CredentialSource::Managed),
                    writable: owner_id == *actor_id,
                })
                .collect(),
            records,
        })
    }

    pub async fn resource_default_model(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
    ) -> Result<DefaultModelSelection, HarnessError> {
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let selection = sqlx::query_scalar::<_, Json<DefaultModelSelection>>(
            "SELECT default_model FROM control_user_preferences WHERE tenant_id=$1 AND user_id=$2 AND default_model IS NOT NULL",
        ).bind(tenant_id.as_str()).bind(owner_id.as_str()).fetch_optional(&mut *transaction).await.map_err(database_error)?
            .map_or_else(DefaultModelSelection::default, |selection| selection.0);
        transaction.commit().await.map_err(database_error)?;
        selection.validate()?;
        Ok(selection)
    }

    pub async fn resource_agent_preset(
        &self,
        tenant_id: &TenantId,
        actor_id: &UserId,
        kind: ResourceKind,
        resource_id: &str,
        preset_id: &str,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(preset_id)?;
        let (mut transaction, owner_id) = self
            .resource_settings_transaction(tenant_id, actor_id, kind, resource_id)
            .await?;
        let document = if let Some(preset) = system_agent_preset(preset_id) {
            preset
        } else {
            sqlx::query_scalar::<_, Json<AgentPresetDocument>>(
                "SELECT document_json FROM control_user_agent_presets WHERE tenant_id=$1 AND user_id=$2 AND preset_id=$3",
            ).bind(tenant_id.as_str()).bind(owner_id.as_str()).bind(preset_id).fetch_optional(&mut *transaction).await.map_err(database_error)?
                .map(|document| document.0).ok_or_else(|| HarnessError::invalid(format!("unknown agent preset {preset_id:?}")))?
        };
        transaction.commit().await.map_err(database_error)?;
        Ok(document)
    }
}
