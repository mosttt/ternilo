use serde_json::json;
use ternilo_protocol::{HarnessError, ModelDeviceIdentity, ModelDeviceLimits};
use ternilo_storage::{Transaction, database_error, lock};

use super::super::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, ModelDeviceUsage, json_text, month_at,
    number, read_number, require_account, unsigned,
};
use super::credentials;
use crate::{ControlUser, account_store::append_platform_audit};

impl ControlStore {
    pub async fn update_model_device_limits(
        &self,
        actor: &ControlUser,
        id: &str,
        limits: &ModelDeviceLimits,
        now: u64,
    ) -> Result<ModelDeviceIdentity, HarnessError> {
        let mut transaction = self.model_transaction().await?;
        let mut device = owned_device_in(&mut transaction, actor, id).await?;
        if device.revoked_at_ms.is_some()
            || device
                .limits
                .expires_at_ms
                .is_some_and(|expiry| expiry <= now)
        {
            return Err(HarnessError::conflict(
                "expired or revoked model devices require a new authorization",
            ));
        }
        validate(limits, now)?;
        if device.limits != *limits {
            sqlx::query("UPDATE control_model_devices SET limits_json=$2 WHERE device_id=$1")
                .bind(id)
                .bind(json_text(limits)?)
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            append_platform_audit(
                &mut transaction,
                &actor.user_id,
                "model.device.limits.update",
                id,
                json!({"previous":device.limits,"limits":limits}),
                now,
            )
            .await?;
            device.limits = limits.clone();
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(device)
    }

    pub async fn model_device_usage(
        &self,
        actor: &ControlUser,
        id: &str,
        now: u64,
    ) -> Result<ModelDeviceUsage, HarnessError> {
        let mut transaction = self.model_transaction().await?;
        owned_device_in(&mut transaction, actor, id).await?;
        let usage = usage_in(&mut transaction, id, now).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(usage)
    }
}

async fn owned_device_in(
    transaction: &mut Transaction,
    actor: &ControlUser,
    id: &str,
) -> Result<ModelDeviceIdentity, HarnessError> {
    require_account(transaction, &actor.user_id).await?;
    lock(transaction, &format!("ternilo:model-key:{id}")).await?;
    let device = credentials::device_in(transaction, id).await?;
    if device.user_id != actor.user_id {
        return Err(HarnessError::policy(
            "model device belongs to another account",
        ));
    }
    Ok(device)
}

pub(super) fn validate(limits: &ModelDeviceLimits, now: u64) -> Result<(), HarnessError> {
    number(now)?;
    if limits.monthly_tokens == Some(0) {
        return Err(HarnessError::invalid(
            "model device limits must be positive",
        ));
    }
    if limits
        .max_concurrent_requests
        .is_some_and(|limit| !(1..=10_000).contains(&limit))
    {
        return Err(HarnessError::invalid(
            "model device concurrency must be between 1 and 10000",
        ));
    }
    if let Some(tokens) = limits.monthly_tokens {
        number(tokens)?;
        if tokens > 9_007_199_254_740_991 {
            return Err(HarnessError::invalid(
                "model device monthly tokens must not exceed 9007199254740991",
            ));
        }
    }
    if limits
        .requests_per_minute
        .is_some_and(|limit| !(1..=10_000).contains(&limit))
    {
        return Err(HarnessError::invalid(
            "model device requests per minute must be between 1 and 10000",
        ));
    }
    if let Some(expiry) = limits.expires_at_ms {
        number(expiry)?;
        if expiry > 253_402_300_799_999 {
            return Err(HarnessError::invalid(
                "model device expiry must not exceed 9999-12-31T23:59:59.999Z",
            ));
        }
        if expiry <= now {
            return Err(HarnessError::invalid(
                "model device expiry must be in the future",
            ));
        }
    }
    Ok(())
}

async fn usage_in(
    transaction: &mut Transaction,
    id: &str,
    now: u64,
) -> Result<ModelDeviceUsage, HarnessError> {
    let month = month_at(now)?;
    let row = sqlx::query("SELECT CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS used_tokens,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN a.reserved_tokens ELSE 0 END),0) AS BIGINT) AS reserved_tokens FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE r.key_id=$1 AND r.month=$2")
        .bind(id).bind(&month).fetch_one(&mut **transaction).await.map_err(database_error)?;
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_model_requests WHERE key_id=$1 AND state='pending' AND expires_at_ms>$2")
        .bind(id).bind(number(now)?).fetch_one(&mut **transaction).await.map_err(database_error)?;
    Ok(ModelDeviceUsage {
        month,
        used_tokens: read_number(&row, "used_tokens")?,
        reserved_tokens: read_number(&row, "reserved_tokens")?,
        active_requests: unsigned(active)?,
    })
}

pub(in crate::model_store) async fn check_admission_in(
    transaction: &mut Transaction,
    device: &ModelDeviceIdentity,
    reserved: u64,
    now: u64,
) -> Result<(), ModelAccessError> {
    let limits = &device.limits;
    if let Some(limit) = limits.requests_per_minute {
        let since = now.saturating_sub(59_999);
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT request_id FROM control_model_requests WHERE key_id=$1 AND month>=$2 AND created_at_ms>=$3 LIMIT $4) recent_requests")
            .bind(&device.device_id)
            .bind(month_at(since)?)
            .bind(number(since)?)
            .bind(i64::from(limit))
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
        if requests >= i64::from(limit) {
            return Err(ModelAccessError {
                kind: ModelAccessErrorKind::QuotaExceeded,
                error: HarnessError::policy(
                    "model device request rate limit is reached; wait for the rolling 60-second window",
                ),
            });
        }
    }
    if limits.monthly_tokens.is_none() && limits.max_concurrent_requests.is_none() {
        return Ok(());
    }
    let usage = usage_in(transaction, &device.device_id, now).await?;
    let total = usage
        .used_tokens
        .checked_add(usage.reserved_tokens)
        .and_then(|tokens| tokens.checked_add(reserved));
    let reason = if limits
        .monthly_tokens
        .is_some_and(|limit| total.is_none_or(|tokens| tokens > limit))
    {
        Some("model device monthly token limit would be exceeded")
    } else if limits
        .max_concurrent_requests
        .is_some_and(|limit| usage.active_requests >= u64::from(limit))
    {
        Some("model device concurrent request limit is reached")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(ModelAccessError {
            kind: ModelAccessErrorKind::QuotaExceeded,
            error: HarnessError::policy(reason),
        });
    }
    Ok(())
}
