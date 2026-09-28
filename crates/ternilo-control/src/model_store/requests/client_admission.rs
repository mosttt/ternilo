use super::super::devices::credentials as devices;
use super::{
    Admission, AdmissionCaller, ControlStore, ModelAccessError, ModelKeyRecord, ModelRequestInput,
    ModelRequestPermit, check_quota, config, duplicate_in, grants, insert_request_in, keys, number,
    validate_request_input,
};
use ternilo_protocol::HarnessError;
use ternilo_storage::{Transaction, database_error, lock};

impl ControlStore {
    pub async fn reserve_model_request(
        &self,
        raw_key: &str,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        self.reserve_client_request(raw_key, None, input, now).await
    }

    pub async fn reserve_device_model_request(
        &self,
        token: &str,
        grant_id: &str,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        self.reserve_client_request(token, Some(grant_id), input, now)
            .await
    }

    async fn reserve_client_request(
        &self,
        raw_key: &str,
        device_grant: Option<&str>,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        validate_request_input(input)?;
        let mut tx = self.model_transaction().await?;
        let key = authorize_client(&mut tx, raw_key, device_grant, now).await?;
        let grant = keys::validate_key(&mut tx, &key, Some(&input.model_id), now).await?;
        lock(
            &mut tx,
            &format!("ternilo:model-publication:{}", input.model_id),
        )
        .await?;
        let publication = config::publication_in(&mut tx, &input.model_id).await?;
        lock(
            &mut tx,
            &format!("ternilo:model-provider:{}", publication.provider_id),
        )
        .await?;
        let route = config::resolve_route(&mut tx, &self.cipher, &input.model_id).await?;
        if route.model.protocol != input.protocol {
            return Err(HarnessError::invalid(
                "public model does not support the requested API protocol",
            )
            .into());
        }
        let scope = if device_grant.is_some() {
            format!("client_device:{}:{}", key.key_id, key.grant_id)
        } else {
            format!("api_key:{}", key.key_id)
        };
        if let Some(request) = duplicate_in(&mut tx, &scope, input).await? {
            tx.commit().await.map_err(database_error)?;
            return Ok(ModelRequestPermit {
                request,
                route,
                newly_accepted: false,
            });
        }
        check_quota(&grant.quota, input.reserved_tokens, "model grant", true)?;
        let quota = grants::quota_in(
            &mut tx,
            &key.grant_id,
            Some(&key.key_id),
            key.monthly_tokens.unwrap_or(grant.quota.limit_tokens),
            key.max_concurrent_requests
                .unwrap_or(grant.quota.max_concurrent_requests),
            now,
        )
        .await?;
        check_quota(&quota, input.reserved_tokens, "model key", true)?;
        if device_grant.is_some() {
            let device = devices::device_in(&mut tx, &key.key_id).await?;
            super::super::devices::limits::check_admission_in(
                &mut tx,
                &device,
                input.reserved_tokens,
                now,
            )
            .await?;
        }
        let request = insert_request_in(
            &mut tx,
            Admission {
                caller: AdmissionCaller::ApiKey(&key),
                scope: &scope,
                grant: Some(&grant),
                route: &route,
                input,
                max_attempts: 1,
                budget_period_start: None,
                now,
            },
        )
        .await?;
        sqlx::query(if device_grant.is_some() {
            "UPDATE control_model_devices SET last_used_at_ms=$2 WHERE device_id=$1"
        } else {
            "UPDATE control_model_keys SET last_used_at_ms=$2 WHERE key_id=$1"
        })
        .bind(&key.key_id)
        .bind(number(now)?)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelRequestPermit {
            request,
            route,
            newly_accepted: true,
        })
    }
}

async fn authorize_client(
    tx: &mut Transaction,
    raw_key: &str,
    device_grant: Option<&str>,
    now: u64,
) -> Result<ModelKeyRecord, ModelAccessError> {
    let initial = if let Some(grant) = device_grant {
        let device = devices::authenticate_in(tx, raw_key, now).await?;
        devices::grant_key_in(tx, &device, grant, now).await?
    } else {
        keys::authenticate_in(tx, raw_key, now).await?
    };
    keys::lock_grant(tx, &initial.grant_id, now).await?;
    lock(tx, &format!("ternilo:model-key:{}", initial.key_id)).await?;
    let key = if device_grant.is_some() {
        let device = devices::device_in(tx, &initial.key_id).await?;
        devices::grant_key_in(tx, &device, &initial.grant_id, now).await?
    } else {
        keys::key_in(tx, &initial.key_id).await?
    };
    Ok(key)
}
