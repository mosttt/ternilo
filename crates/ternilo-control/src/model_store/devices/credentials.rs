use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{
    HarnessError, ModelDeviceGrant, ModelDeviceIdentity, ModelDevicePage, ModelDeviceSession,
    UserId,
};
use ternilo_storage::{Transaction, database_error, lock};

use super::super::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, ModelCredentialKind, ModelKeyRecord,
    PublicModel, from_json, grants, number, read_number, read_optional_number, require_account,
};
use crate::{
    ControlUser, PageQuery,
    account_store::append_platform_audit,
    crypto::{hex, token_hash},
};

impl ControlStore {
    pub async fn model_device_session(
        &self,
        token: &str,
        cursor: Option<String>,
        now: u64,
    ) -> Result<ModelDeviceSession, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let identity = authenticate_in(&mut tx, token, now).await?;
        let session = session_in(&mut tx, identity, cursor, now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(session)
    }

    pub async fn list_device_models(
        &self,
        token: &str,
        grant: &str,
        now: u64,
    ) -> Result<Vec<PublicModel>, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let identity = authenticate_in(&mut tx, token, now).await?;
        let key = grant_key_in(&mut tx, &identity, grant, now).await?;
        let models = grants::grant_models_in(&mut tx, grant)
            .await?
            .into_iter()
            .filter(|model| key.model_ids.contains(&model.model_id))
            .collect();
        tx.commit().await.map_err(database_error)?;
        Ok(models)
    }

    pub async fn list_model_devices(
        &self,
        actor: &ControlUser,
        query: &PageQuery,
    ) -> Result<ModelDevicePage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        let rows = sqlx::query("SELECT d.*,u.username FROM control_model_devices d JOIN control_users u ON u.user_id=d.user_id WHERE d.user_id=$1 AND (CAST($2 AS TEXT) IS NULL OR LOWER(d.device_name) LIKE $2 ESCAPE '!') AND (CAST($3 AS TEXT) IS NULL OR d.device_id>$3) ORDER BY d.device_id LIMIT $4")
            .bind(actor.user_id.as_str()).bind(pattern).bind(cursor).bind(i64::from(query.limit)+1).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut devices = rows
            .iter()
            .map(device_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = query.finish(&mut devices, |device| device.device_id.clone());
        tx.commit().await.map_err(database_error)?;
        Ok(ModelDevicePage {
            devices,
            next_cursor,
        })
    }

    pub async fn revoke_model_device(
        &self,
        actor: &ControlUser,
        id: &str,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        lock(&mut tx, &format!("ternilo:model-key:{id}")).await?;
        let device = device_in(&mut tx, id).await?;
        if device.user_id != actor.user_id {
            return Err(HarnessError::policy(
                "model device belongs to another account",
            ));
        }
        sqlx::query("UPDATE control_model_devices SET revoked_at_ms=COALESCE(revoked_at_ms,$2) WHERE device_id=$1")
            .bind(id).bind(number(now)?).execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.device.revoke",
            id,
            json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }

    pub async fn disconnect_model_device(
        &self,
        token: &str,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let device = authenticate_in(&mut tx, token, now).await?;
        tx.commit().await.map_err(database_error)?;
        self.revoke_model_device(
            &ControlUser {
                user_id: device.user_id,
                username: device.username,
            },
            &device.device_id,
            now,
        )
        .await?;
        Ok(())
    }
}

pub(in crate::model_store) async fn authenticate_in(
    tx: &mut Transaction,
    token: &str,
    now: u64,
) -> Result<ModelDeviceIdentity, ModelAccessError> {
    if !token.starts_with("kmd_") || token.len() > 256 {
        return Err(unauthorized());
    }
    let row = sqlx::query("SELECT d.*,u.username FROM control_model_devices d JOIN control_users u ON u.user_id=d.user_id WHERE d.token_hash=$1")
        .bind(hex(&token_hash(token))).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(unauthorized)?;
    let identity = device_from_row(&row)?;
    validate_device(tx, &identity, now).await?;
    Ok(identity)
}

pub(in crate::model_store) async fn device_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<ModelDeviceIdentity, HarnessError> {
    let row = sqlx::query("SELECT d.*,u.username FROM control_model_devices d JOIN control_users u ON u.user_id=d.user_id WHERE d.device_id=$1")
        .bind(id).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(|| HarnessError::policy("model device does not exist"))?;
    device_from_row(&row)
}

fn device_from_row(row: &AnyRow) -> Result<ModelDeviceIdentity, HarnessError> {
    Ok(ModelDeviceIdentity {
        device_id: row.try_get("device_id").map_err(database_error)?,
        device_name: row.try_get("device_name").map_err(database_error)?,
        user_id: UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        ),
        username: row.try_get("username").map_err(database_error)?,
        scope: from_json(
            &row.try_get::<String, _>("scope_json")
                .map_err(database_error)?,
        )?,
        limits: from_json(
            &row.try_get::<String, _>("limits_json")
                .map_err(database_error)?,
        )?,
        revoked_at_ms: read_optional_number(row, "revoked_at_ms")?,
        created_at_ms: read_number(row, "created_at_ms")?,
        last_used_at_ms: read_optional_number(row, "last_used_at_ms")?,
    })
}

pub(super) async fn validate_device(
    tx: &mut Transaction,
    device: &ModelDeviceIdentity,
    now: u64,
) -> Result<(), ModelAccessError> {
    if device.revoked_at_ms.is_some()
        || device
            .limits
            .expires_at_ms
            .is_some_and(|expiry| expiry <= now)
    {
        return Err(unauthorized());
    }
    require_account(tx, &device.user_id).await?;
    Ok(())
}

pub(in crate::model_store) async fn grant_key_in(
    tx: &mut Transaction,
    device: &ModelDeviceIdentity,
    grant_id: &str,
    now: u64,
) -> Result<ModelKeyRecord, ModelAccessError> {
    validate_device(tx, device, now).await?;
    let grant = grants::grant_in(tx, grant_id, now).await?;
    grants::require_grant(tx, &device.user_id, &grant, now).await?;
    let model_ids: Vec<_> = grant
        .model_ids
        .iter()
        .filter(|id| super::scope::permits(&device.scope, grant_id, id))
        .cloned()
        .collect();
    if model_ids.is_empty() {
        return Err(HarnessError::policy("model grant is outside this device scope").into());
    }
    // A request-local projection reuses the shared admission and ledger rules; no API key is issued.
    Ok(ModelKeyRecord {
        key_id: device.device_id.clone(),
        kind: ModelCredentialKind::ClientDevice,
        user_id: device.user_id.clone(),
        name: device.device_name.clone(),
        token_prefix: String::new(),
        grant_id: grant_id.to_owned(),
        grant_name: grant.name,
        model_ids,
        monthly_tokens: None,
        max_concurrent_requests: None,
        expires_at_ms: device.limits.expires_at_ms,
        revoked_at_ms: device.revoked_at_ms,
        created_at_ms: device.created_at_ms,
        last_used_at_ms: device.last_used_at_ms,
    })
}

pub(super) async fn session_in(
    tx: &mut Transaction,
    identity: ModelDeviceIdentity,
    cursor: Option<String>,
    now: u64,
) -> Result<ModelDeviceSession, HarnessError> {
    let providers = if cursor.is_none() {
        super::account::catalog_in(tx, &identity.user_id, Some(&identity.scope)).await?
    } else {
        Vec::new()
    };
    super::super::scope(tx).await?;
    let page = grants::entitlements_for_user_in(
        tx,
        &identity.user_id,
        None,
        &PageQuery {
            cursor,
            limit: 50,
            query: None,
        },
        now,
    )
    .await?;
    let grants = page
        .entitlements
        .into_iter()
        .filter_map(|entry| {
            let models: Vec<_> = entry
                .models
                .into_iter()
                .filter(|model| {
                    super::scope::permits(&identity.scope, &entry.grant.grant_id, &model.model_id)
                })
                .collect();
            (!models.is_empty()).then_some(ModelDeviceGrant {
                grant_id: entry.grant.grant_id,
                grant_name: entry.grant.name,
                models,
            })
        })
        .collect();
    Ok(ModelDeviceSession {
        identity,
        grants,
        providers,
        next_cursor: page.next_cursor,
    })
}

fn unauthorized() -> ModelAccessError {
    ModelAccessError {
        kind: ModelAccessErrorKind::Unauthorized,
        error: HarnessError::policy("model device is invalid, expired, or revoked"),
    }
}
