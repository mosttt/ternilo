use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::json;
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::HarnessError;
use ternilo_storage::{Transaction, database_error, lock};

use super::{ControlStore, PlatformAction, config::CREDENTIAL_SCOPE, number, read_number};
use crate::{ControlUser, account_store::append_platform_audit, crypto::random_identifier};

#[derive(Clone, Debug, Serialize)]
pub struct ModelProviderKeyRotation {
    pub rotation_id: String,
    pub created_at_ms: u64,
}

impl ControlStore {
    pub async fn model_provider_key_rotation(
        &self,
        actor: &ControlUser,
        provider_id: &str,
    ) -> Result<Option<ModelProviderKeyRotation>, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        let row = provider_row(&mut tx, provider_id).await?;
        let rotation = rotation_from_row(&row)?;
        tx.commit().await.map_err(database_error)?;
        Ok(rotation)
    }

    pub async fn begin_model_provider_key_rotation(
        &self,
        actor: &ControlUser,
        provider_id: &str,
        api_key: &str,
        now_ms: u64,
    ) -> Result<ModelProviderKeyRotation, HarnessError> {
        if api_key.trim().is_empty() || api_key.len() > 16_384 {
            return Err(HarnessError::invalid(
                "upstream credential must contain 1 to 16384 bytes",
            ));
        }
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-provider:{provider_id}")).await?;
        let row = provider_row(&mut tx, provider_id).await?;
        if rotation_from_row(&row)?.is_some() {
            return Err(HarnessError::conflict(
                "finish or roll back the pending Provider key rotation first",
            ));
        }
        let version = read_number(&row, "credential_version")?
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("upstream credential version overflow"))?;
        let encrypted = self.cipher.encrypt(
            CREDENTIAL_SCOPE,
            Some(provider_id),
            "api-key",
            version,
            api_key.as_bytes(),
        )?;
        let rotation = ModelProviderKeyRotation {
            rotation_id: random_identifier("mrot"),
            created_at_ms: now_ms,
        };
        sqlx::query("UPDATE control_model_providers SET previous_credential_version=credential_version,
            previous_credential_nonce=credential_nonce, previous_credential_ciphertext=credential_ciphertext,
            key_rotation_id=$2, key_rotation_started_at_ms=$3,
            credential_version=$4, credential_nonce=$5, credential_ciphertext=$6, updated_at_ms=$3
            WHERE provider_id=$1")
            .bind(provider_id).bind(&rotation.rotation_id).bind(number(now_ms)?).bind(number(version)?)
            .bind(STANDARD.encode(encrypted.nonce)).bind(STANDARD.encode(encrypted.ciphertext))
            .execute(&mut *tx).await.map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            "model.provider.key_rotation.begin",
            provider_id,
            json!({"rotation_id": rotation.rotation_id}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(rotation)
    }

    pub async fn finish_model_provider_key_rotation(
        &self,
        actor: &ControlUser,
        provider_id: &str,
        rotation_id: &str,
        rollback: bool,
        now_ms: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsManage)
            .await?;
        lock(&mut tx, &format!("ternilo:model-provider:{provider_id}")).await?;
        let row = provider_row(&mut tx, provider_id).await?;
        if rotation_from_row(&row)?.is_none_or(|current| current.rotation_id != rotation_id) {
            return Err(HarnessError::conflict(
                "Provider key rotation is no longer current",
            ));
        }
        if rollback {
            sqlx::query("UPDATE control_model_providers SET credential_version=previous_credential_version,
                credential_nonce=previous_credential_nonce, credential_ciphertext=previous_credential_ciphertext WHERE provider_id=$1")
                .bind(provider_id).execute(&mut *tx).await.map_err(database_error)?;
        }
        sqlx::query("UPDATE control_model_providers SET key_rotation_id=NULL, key_rotation_started_at_ms=NULL,
            previous_credential_version=NULL, previous_credential_nonce=NULL, previous_credential_ciphertext=NULL,
            updated_at_ms=$2 WHERE provider_id=$1")
            .bind(provider_id).bind(number(now_ms)?).execute(&mut *tx).await.map_err(database_error)?;
        let event = if rollback {
            "model.provider.key_rotation.rollback"
        } else {
            "model.provider.key_rotation.commit"
        };
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            event,
            provider_id,
            json!({"rotation_id": rotation_id}),
            now_ms,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}

async fn provider_row(tx: &mut Transaction, provider_id: &str) -> Result<AnyRow, HarnessError> {
    sqlx::query("SELECT * FROM control_model_providers WHERE provider_id=$1")
        .bind(provider_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("model Provider does not exist"))
}

fn rotation_from_row(row: &AnyRow) -> Result<Option<ModelProviderKeyRotation>, HarnessError> {
    row.try_get::<Option<String>, _>("key_rotation_id")
        .map_err(database_error)?
        .map(|rotation_id| {
            Ok(ModelProviderKeyRotation {
                rotation_id,
                created_at_ms: read_number(row, "key_rotation_started_at_ms")?,
            })
        })
        .transpose()
}
