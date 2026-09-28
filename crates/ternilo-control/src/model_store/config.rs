use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{
    HarnessError, ProviderModelCatalog, ProviderModelDefaults, ProviderProfile,
};
use ternilo_storage::{Transaction, database_error, lock};
use zeroize::Zeroizing;

use super::{
    ControlStore, ModelProviderInput, ModelProviderPage, ModelProviderRecord,
    ModelPublicationInput, ModelPublicationPage, ModelPublicationRecord, PlatformAction,
    PublicModel, ResolvedModelRoute, from_json, json_text, number, read_number, scope,
    validate_text,
};
use crate::{
    ControlUser, PageQuery, SecretCipher, account_store::append_platform_audit,
    crypto::EncryptedSecret,
};

pub(super) const CREDENTIAL_SCOPE: &str = "platform-model-provider-v1";

impl ControlStore {
    pub async fn resolve_model_provider_secret(
        &self,
        actor: &ControlUser,
        provider_id: &str,
    ) -> Result<Option<Zeroizing<String>>, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        let row = sqlx::query("SELECT * FROM control_model_providers WHERE provider_id=$1")
            .bind(provider_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(database_error)?
            .ok_or_else(|| HarnessError::invalid("model Provider does not exist"))?;
        let plaintext = encrypted_provider(&row)?
            .map(|encrypted| {
                let bytes = self.cipher.decrypt(
                    CREDENTIAL_SCOPE,
                    Some(provider_id),
                    "api-key",
                    read_number(&row, "credential_version")?,
                    &encrypted,
                )?;
                String::from_utf8(bytes.to_vec())
                    .map(Zeroizing::new)
                    .map_err(|_| HarnessError::execution("stored upstream credential is not UTF-8"))
            })
            .transpose()?;
        tx.commit().await.map_err(database_error)?;
        Ok(plaintext)
    }

    pub async fn list_model_providers(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelProviderPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let rows = sqlx::query("SELECT * FROM control_model_providers WHERE (CAST($1 AS TEXT) IS NULL OR LOWER(provider_id) LIKE $1 ESCAPE '!' OR LOWER(profile_json) LIKE $1 ESCAPE '!') AND (CAST($2 AS TEXT) IS NULL OR provider_id>$2) ORDER BY provider_id LIMIT $3")
            .bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut providers = rows
            .iter()
            .map(provider_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = query.finish(&mut providers, |value| value.profile.id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelProviderPage {
            providers,
            next_cursor,
        })
    }

    pub async fn get_model_provider(
        &self,
        actor: &ControlUser,
        provider_id: &str,
    ) -> Result<ModelProviderRecord, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let record = provider_in(&mut tx, provider_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(record)
    }

    pub async fn save_model_provider(
        &self,
        actor: &ControlUser,
        input: &ModelProviderInput,
        now_ms: u64,
    ) -> Result<ModelProviderRecord, HarnessError> {
        input.profile.validate()?;
        if input.profile.api_key_ref.is_some() {
            return Err(HarnessError::invalid(
                "platform Providers use their own saved credential, not api_key_ref",
            ));
        }
        if input.clear_api_key && input.api_key.is_some() {
            return Err(HarnessError::invalid(
                "set or clear the upstream credential, not both",
            ));
        }
        if input
            .api_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.len() > 16_384)
        {
            return Err(HarnessError::invalid(
                "upstream credential must contain 1 to 16384 bytes",
            ));
        }
        let id = &input.profile.id;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-provider:{id}")).await?;
        if input.api_key.is_some() || input.clear_api_key {
            let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_providers WHERE provider_id=$1 AND key_rotation_id IS NOT NULL")
                .bind(id).fetch_one(&mut *tx).await.map_err(database_error)?;
            if pending != 0 {
                return Err(HarnessError::conflict(
                    "finish or roll back the pending Provider key rotation before replacing its credential",
                ));
            }
        }
        let publications: Vec<String> = sqlx::query_scalar(
            "SELECT upstream_model FROM control_model_publications WHERE provider_id=$1",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(database_error)?;
        for model in publications {
            input.profile.resolved_model(&model)?;
        }
        let previous = sqlx::query("SELECT credential_version,credential_nonce,credential_ciphertext FROM control_model_providers WHERE provider_id=$1").bind(id).fetch_optional(&mut *tx).await.map_err(database_error)?;
        let mut version = previous
            .as_ref()
            .map(|row| read_number(row, "credential_version"))
            .transpose()?
            .unwrap_or(0);
        let mut nonce: Option<String> = previous
            .as_ref()
            .map(|row| row.try_get("credential_nonce").map_err(database_error))
            .transpose()?
            .flatten();
        let mut ciphertext: Option<String> = previous
            .as_ref()
            .map(|row| row.try_get("credential_ciphertext").map_err(database_error))
            .transpose()?
            .flatten();
        if let Some(key) = &input.api_key {
            version = version
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("upstream credential version overflow"))?;
            let encrypted = self.cipher.encrypt(
                CREDENTIAL_SCOPE,
                Some(id),
                "api-key",
                version,
                key.as_bytes(),
            )?;
            nonce = Some(STANDARD.encode(encrypted.nonce));
            ciphertext = Some(STANDARD.encode(encrypted.ciphertext));
        } else if input.clear_api_key {
            nonce = None;
            ciphertext = None;
        }
        sqlx::query("INSERT INTO control_model_providers(provider_id,profile_json,enabled,credential_version,credential_nonce,credential_ciphertext,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$7) ON CONFLICT(provider_id) DO UPDATE SET profile_json=EXCLUDED.profile_json,enabled=EXCLUDED.enabled,credential_version=EXCLUDED.credential_version,credential_nonce=EXCLUDED.credential_nonce,credential_ciphertext=EXCLUDED.credential_ciphertext,updated_at_ms=EXCLUDED.updated_at_ms")
            .bind(id).bind(json_text(&input.profile)?).bind(i64::from(input.enabled)).bind(number(version)?).bind(nonce).bind(ciphertext).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(&mut tx,&actor.user_id,"model.provider.save",id,json!({"enabled":input.enabled,"credential_changed":input.api_key.is_some()||input.clear_api_key}),now_ms).await?;
        let record = provider_in(&mut tx, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(record)
    }

    pub async fn disable_model_provider(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-provider:{id}")).await?;
        provider_in(&mut tx, id).await?;
        sqlx::query(
            "UPDATE control_model_providers SET enabled=0,updated_at_ms=$2 WHERE provider_id=$1",
        )
        .bind(id)
        .bind(number(now_ms)?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.provider.disable",
            id,
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn list_model_publications(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelPublicationPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let rows=sqlx::query("SELECT p.*,r.profile_json,r.enabled AS provider_enabled FROM control_model_publications p JOIN control_model_providers r ON r.provider_id=p.provider_id WHERE (CAST($1 AS TEXT) IS NULL OR LOWER(p.model_id) LIKE $1 ESCAPE '!' OR LOWER(p.display_name) LIKE $1 ESCAPE '!') AND (CAST($2 AS TEXT) IS NULL OR p.model_id>$2) ORDER BY p.model_id LIMIT $3")
            .bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut models = rows
            .iter()
            .map(publication_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = query.finish(&mut models, |value| value.model.model_id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelPublicationPage {
            models,
            next_cursor,
        })
    }

    pub async fn get_model_publication(
        &self,
        actor: &ControlUser,
        id: &str,
    ) -> Result<ModelPublicationRecord, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        let value = publication_in(&mut tx, id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }

    pub async fn save_model_publication(
        &self,
        actor: &ControlUser,
        input: &ModelPublicationInput,
        now_ms: u64,
    ) -> Result<ModelPublicationRecord, HarnessError> {
        validate_text(&input.model_id, "public model ID", 128)?;
        validate_text(&input.display_name, "model display name", 120)?;
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(
            &mut tx,
            &format!("ternilo:model-publication:{}", input.model_id),
        )
        .await?;
        lock(
            &mut tx,
            &format!("ternilo:model-provider:{}", input.provider_id),
        )
        .await?;
        let provider = provider_in(&mut tx, &input.provider_id).await?;
        provider.profile.resolved_model(&input.upstream_model)?;
        sqlx::query("INSERT INTO control_model_publications(model_id,display_name,provider_id,upstream_model,enabled,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$6) ON CONFLICT(model_id) DO UPDATE SET display_name=EXCLUDED.display_name,provider_id=EXCLUDED.provider_id,upstream_model=EXCLUDED.upstream_model,enabled=EXCLUDED.enabled,updated_at_ms=EXCLUDED.updated_at_ms")
            .bind(&input.model_id).bind(input.display_name.trim()).bind(&input.provider_id).bind(&input.upstream_model).bind(i64::from(input.enabled)).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.publication.save",
            &input.model_id,
            json!({"provider_id":input.provider_id,"enabled":input.enabled}),
            now_ms,
        )
        .await?;
        let value = publication_in(&mut tx, &input.model_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(value)
    }

    pub async fn disable_model_publication(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-publication:{id}")).await?;
        publication_in(&mut tx, id).await?;
        sqlx::query(
            "UPDATE control_model_publications SET enabled=0,updated_at_ms=$2 WHERE model_id=$1",
        )
        .bind(id)
        .bind(number(now_ms)?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.publication.disable",
            id,
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}

pub(super) async fn provider_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<ModelProviderRecord, HarnessError> {
    let row = sqlx::query("SELECT * FROM control_model_providers WHERE provider_id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("model Provider does not exist"))?;
    provider_from_row(&row)
}

fn provider_from_row(row: &AnyRow) -> Result<ModelProviderRecord, HarnessError> {
    Ok(ModelProviderRecord {
        profile: from_json(
            &row.try_get::<String, _>("profile_json")
                .map_err(database_error)?,
        )?,
        enabled: row.try_get::<i64, _>("enabled").map_err(database_error)? != 0,
        has_api_key: row
            .try_get::<Option<String>, _>("credential_ciphertext")
            .map_err(database_error)?
            .is_some(),
        created_at_ms: read_number(row, "created_at_ms")?,
        updated_at_ms: read_number(row, "updated_at_ms")?,
    })
}

pub(super) async fn publication_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<ModelPublicationRecord, HarnessError> {
    let row=sqlx::query("SELECT p.*,r.profile_json,r.enabled AS provider_enabled FROM control_model_publications p JOIN control_model_providers r ON r.provider_id=p.provider_id WHERE p.model_id=$1")
        .bind(id).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(||HarnessError::policy("public model is unavailable"))?;
    publication_from_row(&row)
}

pub(super) fn publication_from_row(row: &AnyRow) -> Result<ModelPublicationRecord, HarnessError> {
    let profile: ProviderProfile = from_json(
        &row.try_get::<String, _>("profile_json")
            .map_err(database_error)?,
    )?;
    let upstream_model: String = row.try_get("upstream_model").map_err(database_error)?;
    let resolved = profile.resolved_model(&upstream_model)?;
    Ok(ModelPublicationRecord {
        model: PublicModel {
            model_id: row.try_get("model_id").map_err(database_error)?,
            display_name: row.try_get("display_name").map_err(database_error)?,
            protocol: profile.protocol,
            defaults: ProviderModelDefaults {
                context_window: resolved.context_window,
                max_output_tokens: resolved.max_output_tokens,
                reasoning: resolved.reasoning,
            },
        },
        provider_id: profile.id,
        upstream_model,
        enabled: row.try_get::<i64, _>("enabled").map_err(database_error)? != 0,
        provider_enabled: row
            .try_get::<i64, _>("provider_enabled")
            .map_err(database_error)?
            != 0,
        created_at_ms: read_number(row, "created_at_ms")?,
        updated_at_ms: read_number(row, "updated_at_ms")?,
    })
}

pub(super) async fn resolve_route(
    tx: &mut Transaction,
    cipher: &SecretCipher,
    id: &str,
) -> Result<ResolvedModelRoute, HarnessError> {
    let published = publication_in(tx, id).await?;
    if !published.enabled || !published.provider_enabled {
        return Err(HarnessError::policy("public model is disabled"));
    }
    let row = sqlx::query("SELECT * FROM control_model_providers WHERE provider_id=$1")
        .bind(&published.provider_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    let provider = provider_from_row(&row)?.profile;
    let api_key = encrypted_provider(&row)?
        .map(|encrypted| {
            let plaintext = cipher.decrypt(
                CREDENTIAL_SCOPE,
                Some(&provider.id),
                "api-key",
                read_number(&row, "credential_version")?,
                &encrypted,
            )?;
            String::from_utf8(plaintext.to_vec())
                .map(Zeroizing::new)
                .map_err(|_| HarnessError::execution("stored upstream credential is not UTF-8"))
        })
        .transpose()?;
    Ok(ResolvedModelRoute {
        provider,
        model: published.model,
        upstream_model: published.upstream_model,
        api_key,
    })
}

fn encrypted_provider(row: &AnyRow) -> Result<Option<EncryptedSecret>, HarnessError> {
    let Some(ciphertext) = row
        .try_get::<Option<String>, _>("credential_ciphertext")
        .map_err(database_error)?
    else {
        return Ok(None);
    };
    let nonce: String = row.try_get("credential_nonce").map_err(database_error)?;
    let invalid = || HarnessError::execution("stored upstream credential encoding is invalid");
    Ok(Some(EncryptedSecret {
        nonce: STANDARD
            .decode(nonce)
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?,
        ciphertext: STANDARD.decode(ciphertext).map_err(|_| invalid())?,
    }))
}

pub(crate) async fn rotate_model_credentials(
    tx: &mut Transaction,
    current: &SecretCipher,
    next: &SecretCipher,
) -> Result<u64, HarnessError> {
    scope(tx).await?;
    let mut count = 0;
    for previous in [false, true] {
        let mut cursor = None::<String>;
        let select = if previous {
            "SELECT provider_id,previous_credential_version AS credential_version,previous_credential_nonce AS credential_nonce,
             previous_credential_ciphertext AS credential_ciphertext FROM control_model_providers
             WHERE previous_credential_ciphertext IS NOT NULL AND (CAST($1 AS TEXT) IS NULL OR provider_id>$1) ORDER BY provider_id LIMIT 128"
        } else {
            "SELECT provider_id,credential_version,credential_nonce,credential_ciphertext FROM control_model_providers
             WHERE credential_ciphertext IS NOT NULL AND (CAST($1 AS TEXT) IS NULL OR provider_id>$1) ORDER BY provider_id LIMIT 128"
        };
        loop {
            let rows = sqlx::query(select)
                .bind(&cursor)
                .fetch_all(&mut **tx)
                .await
                .map_err(database_error)?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let id: String = row.try_get("provider_id").map_err(database_error)?;
                let version = read_number(&row, "credential_version")?;
                let encrypted = encrypted_provider(&row)?.ok_or_else(|| {
                    HarnessError::execution("upstream credential disappeared during rotation")
                })?;
                let plaintext =
                    current.decrypt(CREDENTIAL_SCOPE, Some(&id), "api-key", version, &encrypted)?;
                let replacement =
                    next.encrypt(CREDENTIAL_SCOPE, Some(&id), "api-key", version, &plaintext)?;
                let update = if previous {
                    "UPDATE control_model_providers SET previous_credential_nonce=$2,previous_credential_ciphertext=$3 WHERE provider_id=$1"
                } else {
                    "UPDATE control_model_providers SET credential_nonce=$2,credential_ciphertext=$3 WHERE provider_id=$1"
                };
                sqlx::query(update)
                    .bind(&id)
                    .bind(STANDARD.encode(replacement.nonce))
                    .bind(STANDARD.encode(replacement.ciphertext))
                    .execute(&mut **tx)
                    .await
                    .map_err(database_error)?;
                count += 1;
                cursor = Some(id);
            }
        }
    }
    Ok(count)
}
