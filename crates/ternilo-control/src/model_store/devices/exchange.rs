use serde_json::json;
use sqlx::Row;
use ternilo_protocol::{
    HarnessError, ModelDeviceLimits, ModelDevicePoll, ModelDeviceScope, UserId,
};
use ternilo_storage::{database_error, lock};

use super::super::{from_json, require_account};
use super::{ControlStore, INTERVAL_MS, json_text, number, read_number};
use crate::{
    account_store::append_platform_audit,
    crypto::{hex, random_identifier, random_token, token_hash},
};

impl ControlStore {
    pub async fn poll_model_device_authorization(
        &self,
        code: &str,
        now: u64,
    ) -> Result<ModelDevicePoll, HarnessError> {
        if !code.starts_with("ter_c_") || code.len() > 256 {
            return Ok(ModelDevicePoll::Expired);
        }
        let hash = hex(&token_hash(code));
        let mut tx = self.model_transaction().await?;
        lock(&mut tx, &format!("ternilo:model-device:{hash}")).await?;
        let Some(row) =
            sqlx::query("SELECT * FROM control_model_device_authorizations WHERE device_hash=$1")
                .bind(&hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(database_error)?
        else {
            return Ok(ModelDevicePoll::Expired);
        };
        let state: String = row.try_get("state").map_err(database_error)?;
        if read_number(&row, "expires_at_ms")? <= now || state == "consumed" {
            return Ok(ModelDevicePoll::Expired);
        }
        if state == "denied" {
            return Ok(ModelDevicePoll::Denied);
        }
        let limits: ModelDeviceLimits = from_json(
            &row.try_get::<String, _>("limits_json")
                .map_err(database_error)?,
        )?;
        if limits.expires_at_ms.is_some_and(|expiry| expiry <= now) {
            return Ok(ModelDevicePoll::Expired);
        }
        let mut interval = read_number(&row, "interval_ms")?;
        let early = now < read_number(&row, "next_poll_at_ms")?;
        if early {
            interval = interval.saturating_add(INTERVAL_MS).min(60_000);
        }
        if early || state == "pending" {
            sqlx::query("UPDATE control_model_device_authorizations SET next_poll_at_ms=$2,interval_ms=$3 WHERE device_hash=$1")
            .bind(&hash).bind(number(now + interval)?).bind(number(interval)?).execute(&mut *tx).await.map_err(database_error)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(if early {
                ModelDevicePoll::SlowDown {
                    interval: interval / 1000,
                }
            } else {
                ModelDevicePoll::Pending {
                    interval: interval / 1000,
                }
            });
        }
        let user = UserId::new(
            row.try_get::<String, _>("user_id")
                .map_err(database_error)?,
        );
        lock(&mut tx, &format!("ternilo:account-role:{user}")).await?;
        let current: String = sqlx::query_scalar(
            "SELECT state FROM control_model_device_authorizations WHERE device_hash=$1",
        )
        .bind(&hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
        if current != "approved" {
            return Ok(ModelDevicePoll::Denied);
        }
        require_account(&mut tx, &user).await?;
        let scope: ModelDeviceScope = from_json(
            &row.try_get::<String, _>("scope_json")
                .map_err(database_error)?,
        )?;
        let key_id = random_identifier("mdv");
        let token = random_token("ter_d");
        sqlx::query("INSERT INTO control_model_devices(device_id,token_hash,user_id,device_name,scope_json,created_at_ms,limits_json) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&key_id).bind(hex(&token_hash(&token))).bind(user.as_str()).bind(row.try_get::<String, _>("device_name").map_err(database_error)?).bind(json_text(&scope)?).bind(number(now)?)
            .bind(json_text(&limits)?)
            .execute(&mut *tx).await.map_err(database_error)?;
        sqlx::query(
            "UPDATE control_model_device_authorizations SET state='consumed' WHERE device_hash=$1",
        )
        .bind(&hash)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        append_platform_audit(
            &mut tx,
            &user,
            "model.device.connect",
            &key_id,
            json!({"scope":scope,"limits":limits}),
            now,
        )
        .await?;
        let identity = super::credentials::device_in(&mut tx, &key_id).await?;
        let session = super::credentials::session_in(&mut tx, identity, None, now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelDevicePoll::Authorized {
            token,
            session: Box::new(session),
        })
    }
}
