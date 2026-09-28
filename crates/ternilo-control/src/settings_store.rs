use serde_json::Value;
use sqlx::Row;
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetSummary,
    AgentPresetTrust, AgentPresetUpdateRequest, CredentialInventory, CredentialRecordInfo,
    CredentialReferenceInfo, CredentialSource, DEFAULT_AGENT_PRESET_ID, DefaultModelSelection,
    HarnessError, ProviderProfile, SidebarOrdering, TenantId, is_system_agent_preset,
    system_agent_preset, system_agent_presets, validate_agent_preset_id,
};
use ternilo_storage::Json;
use zeroize::Zeroizing;

use crate::{ControlStore, ControlUser, crypto::EncryptedSecret};

impl ControlStore {
    pub async fn user_sidebar_ordering(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<SidebarOrdering, HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let ordering = sqlx::query_scalar::<_, Json<SidebarOrdering>>(
            "SELECT sidebar_ordering FROM control_user_preferences
             WHERE tenant_id = $1 AND user_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .map_or_else(SidebarOrdering::default, |ordering| ordering.0);
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        ordering.validate()?;
        Ok(ordering)
    }

    pub async fn set_user_sidebar_ordering(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        ordering: SidebarOrdering,
        now_ms: u64,
    ) -> Result<SidebarOrdering, HarnessError> {
        ordering.validate()?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_user_preferences
                (tenant_id, user_id, default_agent_preset, sidebar_ordering, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, user_id) DO UPDATE
             SET sidebar_ordering = EXCLUDED.sidebar_ordering,
                 updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(DEFAULT_AGENT_PRESET_ID)
        .bind(Json(&ordering))
        .bind(timestamp(now_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(ordering)
    }

    pub async fn user_default_model(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<DefaultModelSelection, HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let selection = sqlx::query_scalar::<_, Json<DefaultModelSelection>>(
            "SELECT default_model FROM control_user_preferences
             WHERE tenant_id = $1 AND user_id = $2 AND default_model IS NOT NULL",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .map_or_else(DefaultModelSelection::default, |selection| selection.0);
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        selection.validate()?;
        Ok(selection)
    }

    pub async fn set_user_default_model(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        selection: DefaultModelSelection,
        now_ms: u64,
    ) -> Result<DefaultModelSelection, HarnessError> {
        selection.validate()?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_user_preferences
                (tenant_id, user_id, default_agent_preset, default_model, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, user_id) DO UPDATE
             SET default_model = EXCLUDED.default_model,
                 updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(DEFAULT_AGENT_PRESET_ID)
        .bind(Json(&selection))
        .bind(timestamp(now_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(selection)
    }

    pub async fn user_provider_profiles(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<ProviderProfile>, HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let providers = sqlx::query_scalar::<_, Json<ProviderProfile>>(
            "SELECT provider_json FROM control_user_provider_profiles
             WHERE tenant_id = $1 AND user_id = $2 ORDER BY provider_id",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .into_iter()
        .map(|provider| provider.0)
        .collect();
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(providers)
    }

    pub async fn user_provider_profile(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        provider_id: &str,
    ) -> Result<Option<ProviderProfile>, HarnessError> {
        validate_provider_id(provider_id)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let provider = sqlx::query_scalar::<_, Json<ProviderProfile>>(
            "SELECT provider_json FROM control_user_provider_profiles
             WHERE tenant_id = $1 AND user_id = $2 AND provider_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(provider_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .map(|provider| provider.0);
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(provider)
    }

    pub async fn upsert_user_provider_profile(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        provider: ProviderProfile,
        now_ms: u64,
    ) -> Result<ProviderProfile, HarnessError> {
        provider.validate()?;
        let now = timestamp(now_ms)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_user_provider_profiles
                (tenant_id, user_id, provider_id, provider_json, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $5)
             ON CONFLICT (tenant_id, user_id, provider_id) DO UPDATE
             SET provider_json = EXCLUDED.provider_json,
                 updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(&provider.id)
        .bind(Json(&provider))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(provider)
    }

    pub async fn create_user_provider_profile(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        provider: ProviderProfile,
        now_ms: u64,
    ) -> Result<ProviderProfile, HarnessError> {
        provider.validate()?;
        let now = timestamp(now_ms)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "INSERT INTO control_user_provider_profiles
                (tenant_id, user_id, provider_id, provider_json, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $5)
             ON CONFLICT (tenant_id, user_id, provider_id) DO NOTHING",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(&provider.id)
        .bind(Json(&provider))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::conflict(format!(
                "provider profile {:?} already exists",
                provider.id
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(provider)
    }

    pub async fn delete_user_provider_profile(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        provider_id: &str,
    ) -> Result<(), HarnessError> {
        validate_provider_id(provider_id)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "DELETE FROM control_user_provider_profiles
             WHERE tenant_id = $1 AND user_id = $2 AND provider_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(format!(
                "unknown provider profile {provider_id:?}"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    pub async fn user_credential_inventory(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<CredentialInventory, HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let references = sqlx::query(
            "SELECT name FROM control_user_credentials
             WHERE tenant_id = $1 AND user_id = $2 ORDER BY name",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .into_iter()
        .map(|row| {
            Ok(CredentialReferenceInfo {
                reference: row
                    .try_get("name")
                    .map_err(|error| database_error(&error))?,
                configured: true,
                source: Some(CredentialSource::Managed),
                writable: true,
            })
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
        let records = sqlx::query(
            "SELECT record_key, kind, updated_at_ms FROM control_user_credential_records
             WHERE tenant_id = $1 AND user_id = $2 ORDER BY record_key",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .into_iter()
        .map(|row| {
            Ok(CredentialRecordInfo {
                key: row
                    .try_get("record_key")
                    .map_err(|error| database_error(&error))?,
                kind: row
                    .try_get("kind")
                    .map_err(|error| database_error(&error))?,
                updated_at_ms: from_timestamp(
                    row.try_get("updated_at_ms")
                        .map_err(|error| database_error(&error))?,
                )?,
            })
        })
        .collect::<Result<Vec<_>, HarnessError>>()?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(CredentialInventory {
            references,
            records,
        })
    }

    pub async fn put_user_credential(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        name: &str,
        value: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_credential_name(name)?;
        if value.is_empty() || value.len() > 64 * 1024 {
            return Err(HarnessError::invalid(
                "credential value must contain 1 to 65536 bytes",
            ));
        }
        let now = timestamp(now_ms)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let version = next_credential_version(&mut transaction, actor, tenant_id, name).await?;
        let encrypted = self.cipher.encrypt(
            tenant_id.as_str(),
            Some(actor.user_id.as_str()),
            name,
            u64::try_from(version)
                .map_err(|_| HarnessError::execution("credential version overflow"))?,
            value.as_bytes(),
        )?;
        sqlx::query(
            "INSERT INTO control_user_credentials
                (tenant_id, user_id, name, version, nonce, ciphertext, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
             ON CONFLICT (tenant_id, user_id, name) DO UPDATE
             SET version = EXCLUDED.version, nonce = EXCLUDED.nonce,
                 ciphertext = EXCLUDED.ciphertext, updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(name)
        .bind(version)
        .bind(encrypted.nonce.to_vec())
        .bind(encrypted.ciphertext)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    pub async fn delete_user_credential(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        name: &str,
    ) -> Result<(), HarnessError> {
        validate_credential_name(name)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "DELETE FROM control_user_credentials
             WHERE tenant_id = $1 AND user_id = $2 AND name = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(name)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(format!(
                "unknown credential {name:?}"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    pub async fn resolve_user_credential(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        name: &str,
    ) -> Result<Option<Zeroizing<String>>, HarnessError> {
        validate_credential_name(name)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let row = sqlx::query(
            "SELECT version, nonce, ciphertext FROM control_user_credentials
             WHERE tenant_id = $1 AND user_id = $2 AND name = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(name)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        row.map(|row| {
            let version = u64::try_from(
                row.try_get::<i64, _>("version")
                    .map_err(|error| database_error(&error))?,
            )
            .map_err(|_| HarnessError::execution("credential version is invalid"))?;
            let encrypted = encrypted_from_row(&row)?;
            let plaintext = self.cipher.decrypt(
                tenant_id.as_str(),
                Some(actor.user_id.as_str()),
                name,
                version,
                &encrypted,
            )?;
            String::from_utf8(plaintext.to_vec())
                .map(Zeroizing::new)
                .map_err(|_| HarnessError::execution("credential plaintext is not UTF-8"))
        })
        .transpose()
    }

    pub async fn put_user_credential_record(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        key: &str,
        kind: &str,
        payload: &Value,
        now_ms: u64,
    ) -> Result<CredentialRecordInfo, HarnessError> {
        validate_record(key, kind, payload)?;
        let bytes = serde_json::to_vec(payload).map_err(|error| {
            HarnessError::invalid(format!("serialize credential payload: {error}"))
        })?;
        let now = timestamp(now_ms)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let version =
            next_credential_record_version(&mut transaction, actor, tenant_id, key).await?;
        let encrypted = self.cipher.encrypt(
            tenant_id.as_str(),
            Some(actor.user_id.as_str()),
            key,
            u64::try_from(version)
                .map_err(|_| HarnessError::execution("credential record version overflow"))?,
            &bytes,
        )?;
        sqlx::query(
            "INSERT INTO control_user_credential_records
                (tenant_id, user_id, record_key, kind, version, nonce, ciphertext,
                 created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
             ON CONFLICT (tenant_id, user_id, record_key) DO UPDATE
             SET kind = EXCLUDED.kind, version = EXCLUDED.version, nonce = EXCLUDED.nonce,
                 ciphertext = EXCLUDED.ciphertext, updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(key)
        .bind(kind)
        .bind(version)
        .bind(encrypted.nonce.to_vec())
        .bind(encrypted.ciphertext)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(CredentialRecordInfo {
            key: key.to_owned(),
            kind: kind.to_owned(),
            updated_at_ms: now_ms,
        })
    }

    pub async fn delete_user_credential_record(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        key: &str,
    ) -> Result<(), HarnessError> {
        validate_record_key(key)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "DELETE FROM control_user_credential_records
             WHERE tenant_id = $1 AND user_id = $2 AND record_key = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(key)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(format!(
                "unknown credential record {key:?}"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    pub async fn user_agent_preset_roster(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<AgentPresetRoster, HarnessError> {
        let documents = self.user_agent_preset_documents(actor, tenant_id).await?;
        let mut presets = system_agent_presets()
            .into_iter()
            .map(|preset| preset.summary)
            .collect::<Vec<_>>();
        presets.extend(documents.into_iter().map(|document| document.summary));
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let default_id = sqlx::query_scalar::<_, String>(
            "SELECT default_agent_preset FROM control_user_preferences
             WHERE tenant_id = $1 AND user_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .unwrap_or_else(|| DEFAULT_AGENT_PRESET_ID.to_owned());
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(AgentPresetRoster {
            presets,
            default_id,
            authorable: true,
        })
    }

    pub async fn user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        preset_id: &str,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(preset_id)?;
        if let Some(preset) = system_agent_preset(preset_id) {
            return Ok(preset);
        }
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let document = sqlx::query_scalar::<_, Json<AgentPresetDocument>>(
            "SELECT document_json FROM control_user_agent_presets
             WHERE tenant_id = $1 AND user_id = $2 AND preset_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(preset_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .map(|document| document.0)
        .ok_or_else(|| HarnessError::invalid(format!("unknown agent preset {preset_id:?}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(document)
    }

    pub async fn copy_user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        request: AgentPresetCopyRequest,
        now_ms: u64,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(&request.from)?;
        validate_agent_preset_id(&request.id)?;
        if is_system_agent_preset(&request.id) {
            return Err(HarnessError::policy("system agent presets are read-only"));
        }
        let source = self
            .user_agent_preset(actor, tenant_id, &request.from)
            .await?;
        let display_name = request.display_name.unwrap_or_else(|| request.id.clone());
        validate_preset_metadata(&display_name, &source.summary.description)?;
        let document = AgentPresetDocument {
            base_profile: None,
            summary: AgentPresetSummary {
                id: request.id,
                display_name: display_name.trim().to_owned(),
                description: source.summary.description,
                trust: AgentPresetTrust::User,
            },
            profile: source.profile,
        };
        self.insert_user_agent_preset(actor, tenant_id, &document, now_ms)
            .await?;
        Ok(document)
    }

    pub async fn update_user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        preset_id: &str,
        update: AgentPresetUpdateRequest,
        now_ms: u64,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(preset_id)?;
        if is_system_agent_preset(preset_id) {
            return Err(HarnessError::policy("system agent presets are read-only"));
        }
        validate_preset_metadata(&update.display_name, &update.description)?;
        let document = AgentPresetDocument {
            base_profile: None,
            summary: AgentPresetSummary {
                id: preset_id.to_owned(),
                display_name: update.display_name.trim().to_owned(),
                description: update.description.trim().to_owned(),
                trust: AgentPresetTrust::User,
            },
            profile: update.profile,
        };
        let now = timestamp(now_ms)?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "UPDATE control_user_agent_presets SET document_json = $4, updated_at_ms = $5
             WHERE tenant_id = $1 AND user_id = $2 AND preset_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(preset_id)
        .bind(Json(&document))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(format!(
                "unknown agent preset {preset_id:?}"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(document)
    }

    pub async fn delete_user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        preset_id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        validate_agent_preset_id(preset_id)?;
        if is_system_agent_preset(preset_id) {
            return Err(HarnessError::policy("system agent presets are read-only"));
        }
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let changed = sqlx::query(
            "DELETE FROM control_user_agent_presets
             WHERE tenant_id = $1 AND user_id = $2 AND preset_id = $3",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(preset_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .rows_affected();
        if changed != 1 {
            return Err(HarnessError::invalid(format!(
                "unknown agent preset {preset_id:?}"
            )));
        }
        sqlx::query(
            "UPDATE control_user_preferences
             SET default_agent_preset = $3, updated_at_ms = $4
             WHERE tenant_id = $1 AND user_id = $2 AND default_agent_preset = $5",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(DEFAULT_AGENT_PRESET_ID)
        .bind(timestamp(now_ms)?)
        .bind(preset_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    pub async fn set_default_user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        preset_id: &str,
        now_ms: u64,
    ) -> Result<AgentPresetRoster, HarnessError> {
        self.user_agent_preset(actor, tenant_id, preset_id).await?;
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_user_preferences
                (tenant_id, user_id, default_agent_preset, updated_at_ms)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, user_id) DO UPDATE
             SET default_agent_preset = EXCLUDED.default_agent_preset,
                 updated_at_ms = EXCLUDED.updated_at_ms",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(preset_id)
        .bind(timestamp(now_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        self.user_agent_preset_roster(actor, tenant_id).await
    }

    async fn user_agent_preset_documents(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<Vec<AgentPresetDocument>, HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        let documents = sqlx::query_scalar::<_, Json<AgentPresetDocument>>(
            "SELECT document_json FROM control_user_agent_presets
             WHERE tenant_id = $1 AND user_id = $2 ORDER BY preset_id",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| database_error(&error))?
        .into_iter()
        .map(|document| document.0)
        .collect();
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))?;
        Ok(documents)
    }

    async fn insert_user_agent_preset(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
        document: &AgentPresetDocument,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut transaction = self.user_settings_transaction(actor, tenant_id).await?;
        sqlx::query(
            "INSERT INTO control_user_agent_presets
                (tenant_id, user_id, preset_id, document_json, created_at_ms, updated_at_ms)
             VALUES ($1, $2, $3, $4, $5, $5)",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .bind(&document.summary.id)
        .bind(Json(document))
        .bind(timestamp(now_ms)?)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                HarnessError::policy(format!(
                    "agent preset {:?} already exists",
                    document.summary.id
                ))
            } else {
                database_error(&error)
            }
        })?;
        transaction
            .commit()
            .await
            .map_err(|error| database_error(&error))
    }

    async fn user_settings_transaction(
        &self,
        actor: &ControlUser,
        tenant_id: &TenantId,
    ) -> Result<ternilo_storage::Transaction, HarnessError> {
        tenant_id.validate()?;
        actor.user_id.validate()?;
        let mut transaction = self.database.tenant_transaction(tenant_id).await?;
        let member = sqlx::query_scalar::<_, i64>(
            "SELECT CAST(EXISTS(
                SELECT 1 FROM control_memberships WHERE tenant_id = $1 AND user_id = $2
             ) AS INTEGER)",
        )
        .bind(tenant_id.as_str())
        .bind(actor.user_id.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map(|value| value != 0)
        .map_err(|error| database_error(&error))?;
        if !member {
            return Err(HarnessError::policy("user is not a member of this tenant"));
        }
        Ok(transaction)
    }
}

async fn next_credential_version(
    transaction: &mut ternilo_storage::Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
    key: &str,
) -> Result<i64, HarnessError> {
    let current = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
        transaction,
        "SELECT version FROM control_user_credentials
         WHERE tenant_id = $1 AND user_id = $2 AND name = $3",
        "SELECT version FROM control_user_credentials
         WHERE tenant_id = $1 AND user_id = $2 AND name = $3 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(actor.user_id.as_str())
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| database_error(&error))?
    .unwrap_or(0);
    current
        .checked_add(1)
        .ok_or_else(|| HarnessError::execution("credential version overflow"))
}

async fn next_credential_record_version(
    transaction: &mut ternilo_storage::Transaction,
    actor: &ControlUser,
    tenant_id: &TenantId,
    key: &str,
) -> Result<i64, HarnessError> {
    let current = sqlx::query_scalar::<_, i64>(ternilo_storage::for_update(
        transaction,
        "SELECT version FROM control_user_credential_records
         WHERE tenant_id = $1 AND user_id = $2 AND record_key = $3",
        "SELECT version FROM control_user_credential_records
         WHERE tenant_id = $1 AND user_id = $2 AND record_key = $3 FOR UPDATE",
    ))
    .bind(tenant_id.as_str())
    .bind(actor.user_id.as_str())
    .bind(key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| database_error(&error))?
    .unwrap_or(0);
    current
        .checked_add(1)
        .ok_or_else(|| HarnessError::execution("credential record version overflow"))
}

fn encrypted_from_row(row: &sqlx::any::AnyRow) -> Result<EncryptedSecret, HarnessError> {
    let nonce = row
        .try_get::<Vec<u8>, _>("nonce")
        .map_err(|error| database_error(&error))?
        .try_into()
        .map_err(|_| HarnessError::execution("credential nonce has an invalid length"))?;
    Ok(EncryptedSecret {
        nonce,
        ciphertext: row
            .try_get("ciphertext")
            .map_err(|error| database_error(&error))?,
    })
}

fn validate_credential_name(name: &str) -> Result<(), HarnessError> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        Err(HarnessError::invalid(
            "credential name must match [A-Za-z_][A-Za-z0-9_]*",
        ))
    } else {
        Ok(())
    }
}

fn validate_provider_id(id: &str) -> Result<(), HarnessError> {
    let mut bytes = id.bytes();
    if id.len() > 64
        || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        Err(HarnessError::invalid(
            "provider id must start with a lowercase letter and use lowercase letters, digits, dash, or underscore",
        ))
    } else {
        Ok(())
    }
}

fn validate_record_key(key: &str) -> Result<(), HarnessError> {
    let Some((scope, id)) = key.split_once('/') else {
        return Err(HarnessError::invalid(
            "credential record key must be <plugin-scope>/<id>",
        ));
    };
    if key.len() > 200
        || scope.is_empty()
        || id.is_empty()
        || id.contains('/')
        || !scope
            .bytes()
            .chain(id.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Err(HarnessError::invalid(
            "credential record key segments must use ASCII letters, digits, dot, dash, or underscore",
        ))
    } else {
        Ok(())
    }
}

fn validate_record(key: &str, kind: &str, payload: &Value) -> Result<(), HarnessError> {
    validate_record_key(key)?;
    if kind.is_empty()
        || kind.len() > 64
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HarnessError::invalid(
            "credential record kind has an invalid format",
        ));
    }
    if serde_json::to_vec(payload)
        .map_err(|error| HarnessError::invalid(format!("serialize credential payload: {error}")))?
        .len()
        > 64 * 1024
    {
        return Err(HarnessError::invalid(
            "credential record payload may not exceed 64 KiB",
        ));
    }
    Ok(())
}

fn validate_preset_metadata(display_name: &str, description: &str) -> Result<(), HarnessError> {
    if display_name.trim().is_empty() || display_name.chars().count() > 120 {
        return Err(HarnessError::invalid(
            "agent preset display name must contain 1 to 120 characters",
        ));
    }
    if description.chars().count() > 2_000 {
        return Err(HarnessError::invalid(
            "agent preset description must not exceed 2000 characters",
        ));
    }
    Ok(())
}

fn timestamp(value: u64) -> Result<i64, HarnessError> {
    i64::try_from(value).map_err(|_| HarnessError::execution("timestamp exceeds PostgreSQL BIGINT"))
}

fn from_timestamp(value: i64) -> Result<u64, HarnessError> {
    u64::try_from(value).map_err(|_| HarnessError::execution("database timestamp is negative"))
}

fn database_error(error: &sqlx::Error) -> HarnessError {
    HarnessError::execution(format!(
        "control settings database operation failed: {error}"
    ))
}
