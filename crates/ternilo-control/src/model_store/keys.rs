use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Transaction, database_error, lock};

use super::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, ModelCredentialKind, ModelGrantRecord,
    ModelGrantSubject, ModelKeyCreation, ModelKeyInput, ModelKeyPage, ModelKeyPrincipal,
    ModelKeyRecord, PublicModel, config, from_json, grants, json_text, number, read_number,
    read_optional_number, require_account, validate_ids, validate_text,
};
use crate::{
    ControlUser, PageQuery,
    account_store::append_platform_audit,
    crypto::{hex, random_identifier, random_token, token_hash},
};

impl ControlStore {
    pub async fn create_model_key(
        &self,
        actor: &ControlUser,
        input: &ModelKeyInput,
        now_ms: u64,
    ) -> Result<ModelKeyCreation, HarnessError> {
        validate_text(&input.name, "model key name", 120)?;
        validate_ids(&input.model_ids)?;
        if input.monthly_tokens == Some(0)
            || input
                .max_concurrent_requests
                .is_some_and(|max| max == 0 || max > 10_000)
        {
            return Err(HarnessError::invalid(
                "model key limits must be positive and concurrency at most 10000",
            ));
        }
        if input.expires_at_ms.is_some_and(|expires| expires <= now_ms) {
            return Err(HarnessError::invalid(
                "model key expiration must be in the future",
            ));
        }
        let mut tx = self.model_transaction().await?;
        lock(&mut tx, &format!("ternilo:account-role:{}", actor.user_id)).await?;
        lock_grant(&mut tx, &input.grant_id, now_ms).await?;
        let grant = grants::grant_in(&mut tx, &input.grant_id, now_ms).await?;
        grants::require_grant(&mut tx, &actor.user_id, &grant, now_ms).await?;
        if input
            .model_ids
            .iter()
            .any(|model| !grant.model_ids.contains(model))
        {
            return Err(HarnessError::policy("model key scope exceeds its grant"));
        }
        if input
            .monthly_tokens
            .is_some_and(|tokens| tokens > grant.quota.limit_tokens)
            || input
                .max_concurrent_requests
                .is_some_and(|max| max > grant.quota.max_concurrent_requests)
        {
            return Err(HarnessError::invalid(
                "model key limits cannot exceed its grant limits",
            ));
        }
        let key_id = random_identifier("mky");
        let token = random_token("ter_m");
        let prefix = &token[..12];
        sqlx::query("INSERT INTO control_model_keys(key_id,token_hash,token_prefix,user_id,grant_id,name,model_ids_json,monthly_tokens,max_concurrent_requests,expires_at_ms,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(&key_id).bind(hex(&token_hash(&token))).bind(prefix).bind(actor.user_id.as_str()).bind(&input.grant_id).bind(input.name.trim()).bind(json_text(&input.model_ids)?).bind(input.monthly_tokens.map(number).transpose()?).bind(input.max_concurrent_requests.map(i64::from)).bind(input.expires_at_ms.map(number).transpose()?).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.key.create",
            &key_id,
            json!({"grant_id":input.grant_id,"model_ids":input.model_ids}),
            now_ms,
        )
        .await?;
        let key = key_in(&mut tx, &key_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelKeyCreation { key, token })
    }

    pub async fn list_model_keys(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelKeyPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        let rows=sqlx::query("SELECT k.*,g.name AS grant_name FROM control_model_keys k JOIN control_model_grants g ON g.grant_id=k.grant_id WHERE k.user_id=$1 AND (CAST($2 AS TEXT) IS NULL OR LOWER(k.name) LIKE $2 ESCAPE '!' OR LOWER(k.token_prefix) LIKE $2 ESCAPE '!' OR LOWER(g.name) LIKE $2 ESCAPE '!') AND (CAST($3 AS TEXT) IS NULL OR k.key_id>$3) ORDER BY k.key_id LIMIT $4")
            .bind(actor.user_id.as_str()).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut keys = rows
            .iter()
            .map(key_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = query.finish(&mut keys, |key| key.key_id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelKeyPage { keys, next_cursor })
    }

    pub async fn revoke_model_key(
        &self,
        actor: &ControlUser,
        id: &str,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        lock(&mut tx, &format!("ternilo:model-key:{id}")).await?;
        let key = key_in(&mut tx, id).await?;
        if key.user_id != actor.user_id {
            return Err(HarnessError::policy("model key belongs to another account"));
        }
        sqlx::query("UPDATE control_model_keys SET revoked_at_ms=COALESCE(revoked_at_ms,$2) WHERE key_id=$1").bind(id).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.key.revoke",
            id,
            json!({}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn authenticate_model_key(
        &self,
        raw_key: &str,
        now_ms: u64,
    ) -> Result<ModelKeyPrincipal, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let key = authenticate_in(&mut tx, raw_key, now_ms).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelKeyPrincipal {
            key_id: key.key_id,
            user_id: key.user_id,
            grant_id: key.grant_id,
        })
    }

    pub async fn list_key_models(
        &self,
        raw_key: &str,
        now_ms: u64,
    ) -> Result<Vec<PublicModel>, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let key = authenticate_in(&mut tx, raw_key, now_ms).await?;
        let models = grants::grant_models_in(&mut tx, &key.grant_id)
            .await?
            .into_iter()
            .filter(|model| key.model_ids.contains(&model.model_id))
            .collect();
        tx.commit().await.map_err(database_error)?;
        Ok(models)
    }
}

pub(super) async fn key_in(tx: &mut Transaction, id: &str) -> Result<ModelKeyRecord, HarnessError> {
    let row = sqlx::query("SELECT k.*,g.name AS grant_name FROM control_model_keys k JOIN control_model_grants g ON g.grant_id=k.grant_id WHERE k.key_id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::policy("model key does not exist"))?;
    key_from_row(&row)
}

pub(super) fn key_from_row(row: &AnyRow) -> Result<ModelKeyRecord, HarnessError> {
    Ok(ModelKeyRecord {
        key_id: row.try_get("key_id").map_err(database_error)?,
        kind: ModelCredentialKind::ApiKey,
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        name: row.try_get("name").map_err(database_error)?,
        token_prefix: row.try_get("token_prefix").map_err(database_error)?,
        grant_id: row.try_get("grant_id").map_err(database_error)?,
        grant_name: row.try_get("grant_name").map_err(database_error)?,
        model_ids: from_json(
            &row.try_get::<String, _>("model_ids_json")
                .map_err(database_error)?,
        )?,
        monthly_tokens: read_optional_number(row, "monthly_tokens")?,
        max_concurrent_requests: read_optional_number(row, "max_concurrent_requests")?
            .map(|value| {
                u32::try_from(value)
                    .map_err(|_| HarnessError::execution("stored model concurrency exceeds u32"))
            })
            .transpose()?,
        expires_at_ms: read_optional_number(row, "expires_at_ms")?,
        revoked_at_ms: read_optional_number(row, "revoked_at_ms")?,
        created_at_ms: read_number(row, "created_at_ms")?,
        last_used_at_ms: read_optional_number(row, "last_used_at_ms")?,
    })
}

pub(super) async fn authenticate_in(
    tx: &mut Transaction,
    raw_key: &str,
    now_ms: u64,
) -> Result<ModelKeyRecord, ModelAccessError> {
    if !raw_key.starts_with("ter_m_") || raw_key.len() > 256 {
        return Err(unauthorized());
    }
    let row = sqlx::query("SELECT k.*,g.name AS grant_name FROM control_model_keys k JOIN control_model_grants g ON g.grant_id=k.grant_id WHERE k.token_hash=$1")
        .bind(hex(&token_hash(raw_key)))
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(unauthorized)?;
    let key = key_from_row(&row)?;
    validate_key(tx, &key, None, now_ms).await?;
    Ok(key)
}

pub(super) async fn validate_key(
    tx: &mut Transaction,
    key: &ModelKeyRecord,
    model_id: Option<&str>,
    now_ms: u64,
) -> Result<ModelGrantRecord, ModelAccessError> {
    if key.revoked_at_ms.is_some() || key.expires_at_ms.is_some_and(|expires| expires <= now_ms) {
        return Err(unauthorized());
    }
    let grant = grants::grant_in(tx, &key.grant_id, now_ms).await?;
    grants::require_grant(tx, &key.user_id, &grant, now_ms).await?;
    if let Some(model) = model_id {
        if !key.model_ids.iter().any(|value| value == model)
            || !grant.model_ids.iter().any(|value| value == model)
        {
            return Err(HarnessError::policy(
                "model is outside the current key and grant intersection",
            )
            .into());
        }
        let publication = config::publication_in(tx, model).await?;
        if !publication.enabled || !publication.provider_enabled {
            return Err(HarnessError::policy("public model is disabled").into());
        }
    }
    Ok(grant)
}

pub(super) async fn lock_grant(
    tx: &mut Transaction,
    id: &str,
    now_ms: u64,
) -> Result<(), HarnessError> {
    let grant = grants::grant_in(tx, id, now_ms).await?;
    if let ModelGrantSubject::Group { id } = grant.subject {
        lock(tx, &format!("ternilo:model-group:{id}")).await?;
    }
    lock(tx, &format!("ternilo:model-grant:{id}")).await
}

fn unauthorized() -> ModelAccessError {
    ModelAccessError {
        kind: ModelAccessErrorKind::Unauthorized,
        error: HarnessError::policy("model key is invalid, revoked, or expired"),
    }
}
