//! Browser-approved, model-only device identities independent of individual grants.

use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{
    HarnessError, ModelDeviceAuthorization, ModelDeviceLimits, ModelDeviceReview, ModelDeviceScope,
};
use ternilo_storage::{database_error, lock};

use super::{ControlStore, json_text, number, read_number, require_account, validate_text};
use crate::{
    ControlUser,
    account_store::append_platform_audit,
    crypto::{hex, random_token, token_hash},
};

pub(super) mod account;
pub(super) mod credentials;
mod exchange;
pub(super) mod limits;
mod scope;
#[cfg(test)]
mod tests;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const LIFETIME_MS: u64 = 600_000;
const INTERVAL_MS: u64 = 5_000;

impl ControlStore {
    pub async fn begin_model_device_authorization(
        &self,
        name: &str,
        now: u64,
    ) -> Result<ModelDeviceAuthorization, HarnessError> {
        validate_text(name, "device name", 120)?;
        let device_code = random_token("ter_c");
        let raw: String = rand::random::<[u8; 8]>()
            .into_iter()
            .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
            .collect();
        let user_code = format!("{}-{}", &raw[..4], &raw[4..]);
        let mut tx = self.model_transaction().await?;
        lock(&mut tx, "ternilo:model-device:admission").await?;
        sqlx::query("DELETE FROM control_model_device_authorizations WHERE expires_at_ms<=$1")
            .bind(number(now)?)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        let pending: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM control_model_device_authorizations")
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
        if pending >= 10_000 {
            return Err(HarnessError::conflict(
                "too many pending device authorizations; try again later",
            ));
        }
        sqlx::query("INSERT INTO control_model_device_authorizations(device_hash,user_code_hash,device_name,state,expires_at_ms,next_poll_at_ms,interval_ms,created_at_ms) VALUES($1,$2,$3,'pending',$4,$5,$6,$7)")
            .bind(hex(&token_hash(&device_code))).bind(user_code_hash(&user_code)?).bind(name.trim())
            .bind(number(now + LIFETIME_MS)?).bind(number(now + INTERVAL_MS)?).bind(number(INTERVAL_MS)?).bind(number(now)?)
            .execute(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelDeviceAuthorization {
            device_code,
            user_code,
            expires_in: LIFETIME_MS / 1000,
            interval: INTERVAL_MS / 1000,
        })
    }

    pub async fn review_model_device_authorization(
        &self,
        actor: &ControlUser,
        code: &str,
        now: u64,
    ) -> Result<ModelDeviceReview, HarnessError> {
        let mut tx = self.model_transaction().await?;
        require_account(&mut tx, &actor.user_id).await?;
        let row = sqlx::query("SELECT device_name,expires_at_ms FROM control_model_device_authorizations WHERE user_code_hash=$1 AND state='pending' AND expires_at_ms>$2")
            .bind(user_code_hash(code)?).bind(number(now)?).fetch_optional(&mut *tx).await.map_err(database_error)?
            .ok_or_else(|| HarnessError::invalid("device code is invalid, expired, or already decided"))?;
        let review = ModelDeviceReview {
            providers: account::catalog_in(&mut tx, &actor.user_id, None).await?,
            device_name: row.try_get("device_name").map_err(database_error)?,
            user_code: code.trim().to_ascii_uppercase(),
            expires_at_ms: read_number(&row, "expires_at_ms")?,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(review)
    }

    pub async fn decide_model_device_authorization(
        &self,
        actor: &ControlUser,
        code: &str,
        scope: Option<&ModelDeviceScope>,
        limits: &ModelDeviceLimits,
        now: u64,
    ) -> Result<(), HarnessError> {
        let mut tx = self.model_transaction().await?;
        let code_hash = user_code_hash(code)?;
        let device_hash: String = sqlx::query_scalar(
            "SELECT device_hash FROM control_model_device_authorizations WHERE user_code_hash=$1",
        )
        .bind(&code_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("device code is invalid or expired"))?;
        lock(&mut tx, &format!("ternilo:model-device:{device_hash}")).await?;
        lock(&mut tx, &format!("ternilo:account-role:{}", actor.user_id)).await?;
        require_account(&mut tx, &actor.user_id).await?;
        if let Some(scope) = scope {
            limits::validate(limits, now)?;
            scope::validate_scope(&mut tx, actor, scope, now).await?;
        }
        let updated = sqlx::query("UPDATE control_model_device_authorizations SET state=$2,user_id=$3,scope_json=$4,limits_json=$6 WHERE device_hash=$1 AND state='pending' AND expires_at_ms>$5")
            .bind(&device_hash).bind(if scope.is_some() {"approved"} else {"denied"}).bind(actor.user_id.as_str()).bind(scope.map(json_text).transpose()?).bind(number(now)?)
            .bind(if scope.is_some() { json_text(limits)? } else { json_text(&ModelDeviceLimits::default())? })
            .execute(&mut *tx).await.map_err(database_error)?.rows_affected();
        if updated != 1 {
            return Err(HarnessError::conflict(
                "device code has expired or was already decided",
            ));
        }
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            if scope.is_some() {
                "model.device.approve"
            } else {
                "model.device.deny"
            },
            &device_hash,
            json!({"scope":scope,"limits":scope.map(|_| limits)}),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)
    }
}

fn user_code_hash(code: &str) -> Result<String, HarnessError> {
    let normalized = code.trim().replace('-', "").to_ascii_uppercase();
    if normalized.len() != 8
        || !normalized
            .bytes()
            .all(|value| value.is_ascii_alphanumeric())
    {
        return Err(HarnessError::invalid(
            "device code must contain eight letters or digits",
        ));
    }
    Ok(hex(&token_hash(&normalized)))
}
