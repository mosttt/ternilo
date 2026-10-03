use ternilo_protocol::{
    HarnessError, ModelDeviceIdentity, ModelDeviceProvider, ModelDeviceScope, ProviderModelCatalog,
    ProviderModelDefaults, ProviderProfile, UserId,
};
use ternilo_storage::{Transaction, database_error, lock, set_tenant_scope, set_user_scope};

use super::super::{
    ControlStore, ModelAccessError, ModelRequestInput, ModelRequestPermit, ModelServiceRequest,
    PublicModel, from_json, number, requests, scope, workloads,
};
use super::{credentials, scope::permits_provider};

pub(super) async fn catalog_in(
    tx: &mut Transaction,
    owner: &UserId,
    allowed: Option<&ModelDeviceScope>,
) -> Result<Vec<ModelDeviceProvider>, HarnessError> {
    if matches!(
        allowed,
        Some(ModelDeviceScope::Account {
            include_account_providers: false
        })
    ) || matches!(allowed, Some(ModelDeviceScope::Selected { providers, .. }) if providers.is_empty())
    {
        return Ok(Vec::new());
    }
    let tenant = ControlStore::account_provider_space_in(tx, owner).await?;
    set_tenant_scope(tx, &tenant).await?;
    set_user_scope(tx, owner).await?;
    let rows: Vec<String> = sqlx::query_scalar("SELECT provider_json FROM control_user_provider_profiles WHERE tenant_id=$1 AND user_id=$2 ORDER BY provider_id")
        .bind(tenant.as_str()).bind(owner.as_str()).fetch_all(&mut **tx).await.map_err(database_error)?;
    let mut catalog = Vec::new();
    for row in rows {
        let provider: ProviderProfile = from_json(&row)?;
        if let Err(error) =
            workloads::require_byok_credential_in(tx, &tenant, owner, &provider).await
        {
            if error.code == ternilo_protocol::ErrorCode::PolicyDenied {
                continue;
            }
            return Err(error);
        }
        let mut models = Vec::new();
        for model in &provider.models {
            if allowed.is_some_and(|scope| !permits_provider(scope, &provider.id, &model.id)) {
                continue;
            }
            let resolved = provider.resolved_model(&model.id)?;
            models.push(PublicModel {
                model_id: model.id.clone(),
                display_name: model
                    .display_name
                    .clone()
                    .unwrap_or_else(|| model.id.clone()),
                protocol: provider.protocol,
                defaults: ProviderModelDefaults {
                    context_window: resolved.context_window,
                    max_output_tokens: resolved.max_output_tokens,
                    reasoning: resolved.reasoning,
                },
            });
        }
        if !models.is_empty() {
            catalog.push(ModelDeviceProvider {
                provider_id: provider.id,
                provider_name: provider.display_name,
                models,
            });
        }
    }
    Ok(catalog)
}

impl ControlStore {
    pub async fn list_device_account_models(
        &self,
        token: &str,
        provider: &str,
        now: u64,
    ) -> Result<Vec<PublicModel>, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let device = credentials::authenticate_in(&mut tx, token, now).await?;
        let catalog = catalog_in(&mut tx, &device.user_id, Some(&device.scope)).await?;
        let models = catalog
            .into_iter()
            .find(|entry| entry.provider_id == provider)
            .ok_or_else(|| {
                HarnessError::policy("account Provider is unavailable or outside this device scope")
            })?
            .models;
        tx.commit().await.map_err(database_error)?;
        Ok(models)
    }

    pub async fn reserve_device_account_request(
        &self,
        token: &str,
        provider: &str,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        requests::validate_request_input(input)?;
        let mut tx = self.model_transaction().await?;
        let initial = credentials::authenticate_in(&mut tx, token, now).await?;
        lock(&mut tx, &format!("ternilo:model-key:{}", initial.device_id)).await?;
        let device = credentials::authenticate_in(&mut tx, token, now).await?;
        require_scope(&device, provider, &input.model_id)?;
        let tenant = Self::account_provider_space_in(&mut tx, &device.user_id).await?;
        let route = self
            .resolve_workload_byok_in(
                &mut tx,
                &tenant,
                &device.user_id,
                provider,
                &input.model_id,
                true,
            )
            .await?;
        if route.model.protocol != input.protocol {
            return Err(HarnessError::invalid(
                "account model does not support the requested API protocol",
            )
            .into());
        }
        scope(&mut tx).await?;
        let caller_scope = format!("device_account:{}:{provider}", device.device_id);
        let reserved = requests::with_provider_budget(input, &route)?;
        let input = &reserved;
        if let Some(request) = requests::duplicate_in(&mut tx, &caller_scope, input).await? {
            tx.commit().await.map_err(database_error)?;
            return Ok(ModelRequestPermit {
                request,
                route,
                newly_accepted: false,
            });
        }
        super::limits::check_admission_in(&mut tx, &device, input.reserved_tokens, now).await?;
        let request = requests::insert_request_in(
            &mut tx,
            requests::Admission {
                caller: requests::AdmissionCaller::DeviceAccount(&device),
                scope: &caller_scope,
                grant: None,
                route: &route,
                input,
                max_attempts: 1,
                budget_period_start: None,
                now,
            },
        )
        .await?;
        sqlx::query("UPDATE control_model_devices SET last_used_at_ms=$2 WHERE device_id=$1")
            .bind(&device.device_id)
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

    pub(in crate::model_store) async fn check_device_account_request_in(
        &self,
        tx: &mut Transaction,
        request: &ModelServiceRequest,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        let device = credentials::device_in(
            tx,
            request
                .key_id
                .as_deref()
                .ok_or_else(|| HarnessError::policy("device model request has no device"))?,
        )
        .await?;
        credentials::validate_device(tx, &device, now).await?;
        if device.user_id != request.actor_user_id
            || device.user_id != request.model_beneficiary_user_id
            || request.resource_owner_user_id.is_some()
            || request.grant_id.is_some()
        {
            return Err(HarnessError::policy(
                "account model request does not belong to this device",
            )
            .into());
        }
        require_scope(&device, &request.provider_id, &request.model_id)?;
        let tenant = Self::account_provider_space_in(tx, &device.user_id).await?;
        let route = self
            .resolve_workload_byok_in(
                tx,
                &tenant,
                &device.user_id,
                &request.provider_id,
                &request.model_id,
                false,
            )
            .await?;
        if route.model.protocol != request.protocol {
            return Err(HarnessError::policy("account model protocol changed").into());
        }
        scope(tx).await?;
        Ok(())
    }
}

fn require_scope(
    device: &ModelDeviceIdentity,
    provider: &str,
    model: &str,
) -> Result<(), HarnessError> {
    if !permits_provider(&device.scope, provider, model) {
        return Err(HarnessError::policy(
            "account model is outside this device scope",
        ));
    }
    Ok(())
}
