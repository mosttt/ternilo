use sqlx::Row;
use ternilo_protocol::{
    HarnessError, ProviderModelCatalog, ProviderModelDefaults, ProviderProfile, ReasoningEffort,
    RunModelBinding, RunModelSnapshot, TenantId, UserId,
};
use ternilo_storage::{Transaction, database_error, set_tenant_scope, set_user_scope};
use zeroize::Zeroizing;

use super::{
    ControlStore, ModelAccessError, ModelEntitlementPage, ModelGrantRecord, ModelRequestInput,
    ModelRequestOrigin, ModelRequestPermit, ModelRequestState, ModelServiceAttempt, PublicModel,
    ResolvedModelRoute, WorkloadModelPrincipal, budgets, config, from_json, grants, json_text,
    keys, ledger, read_number, requests, scope,
};
use crate::{
    ControlUser, EncryptedSecret, PageQuery, ResourceAction, ResourceKind,
    crypto::{hex, token_hash},
    resource_access_in,
};

impl ControlStore {
    /// The caller must validate canonical Worker identity, lease, fence and resource authority in this transaction.
    pub async fn reserve_workload_model_request_in(
        &self,
        tx: &mut Transaction,
        principal: &WorkloadModelPrincipal,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        scope(tx).await?;
        validate_principal(principal)?;
        require_provider_space(tx, &principal.tenant_id, &principal.model).await?;
        requests::validate_request_input(input)?;
        if input.model_id != principal.model.model_id() {
            return Err(HarnessError::policy(
                "workload request does not match its immutable model binding",
            )
            .into());
        }
        budgets::lock_budget(tx, &principal.tenant_id).await?;
        lock_binding(tx, &principal.model, now).await?;
        let (route, grant) = self
            .resolve_workload_route_in(
                tx,
                &principal.actor_user_id,
                &principal.resource_owner_user_id,
                &principal.model,
                now,
                true,
            )
            .await?;
        set_tenant_scope(tx, &principal.tenant_id).await?;
        if route.model.protocol != input.protocol {
            return Err(HarnessError::invalid(
                "workload request protocol differs from its model binding",
            )
            .into());
        }
        let caller_scope = workload_scope(principal)?;
        if let Some(request) = requests::duplicate_in(tx, &caller_scope, input).await? {
            require_same_principal(&request, principal)?;
            return Ok(ModelRequestPermit {
                request,
                route,
                newly_accepted: false,
            });
        }
        let period = budgets::require_run_budget(tx, principal, input.reserved_tokens, now).await?;
        if let Some(grant) = &grant {
            requests::check_quota(&grant.quota, input.reserved_tokens, "model grant", true)?;
        }
        let request = requests::insert_request_in(
            tx,
            requests::Admission {
                caller: requests::AdmissionCaller::Workload(principal),
                scope: &caller_scope,
                grant: grant.as_ref(),
                route: &route,
                input,
                max_attempts: route.provider.max_attempts,
                budget_period_start: Some(&period),
                now,
            },
        )
        .await?;
        budgets::refresh_reservation_in(
            tx,
            &principal.tenant_id,
            &principal.execution_reservation_id,
        )
        .await?;
        Ok(ModelRequestPermit {
            request,
            route,
            newly_accepted: true,
        })
    }

    /// Start one real upstream attempt after the caller revalidates the canonical workload in the same transaction.
    pub async fn begin_workload_model_attempt_in(
        &self,
        tx: &mut Transaction,
        principal: &WorkloadModelPrincipal,
        id: &str,
        attempt: u32,
        now: u64,
    ) -> Result<ModelServiceAttempt, ModelAccessError> {
        scope(tx).await?;
        validate_principal(principal)?;
        require_provider_space(tx, &principal.tenant_id, &principal.model).await?;
        budgets::lock_budget(tx, &principal.tenant_id).await?;
        lock_binding(tx, &principal.model, now).await?;
        let initial = ledger::request_in(tx, id).await?;
        ledger::lock_request(tx, &initial.request).await?;
        let row = ledger::request_in(tx, id).await?;
        require_same_principal(&row.request, principal)?;
        requests::require_pending(&row.request, now)?;
        requests::require_request_source(tx, &row.request).await?;
        let (_, grant) = self
            .resolve_workload_route_in(
                tx,
                &principal.actor_user_id,
                &principal.resource_owner_user_id,
                &principal.model,
                now,
                false,
            )
            .await?;
        set_tenant_scope(tx, &principal.tenant_id).await?;
        if attempt == 0 || attempt > row.max_attempts {
            return Err(
                HarnessError::policy("model attempt exceeds its accepted retry limit").into(),
            );
        }
        let first = row.request.attempts.first().ok_or_else(|| {
            HarnessError::execution("accepted model request has no first attempt")
        })?;
        let additional = if attempt == 1 {
            0
        } else {
            let last =
                row.request.attempts.last().ok_or_else(|| {
                    HarnessError::execution("accepted model request has no attempts")
                })?;
            if last.attempt + 1 != attempt || last.state != ModelRequestState::Failed {
                return Err(HarnessError::conflict(
                    "model retry must follow exactly one completed failed attempt",
                )
                .into());
            }
            first.reserved_tokens
        };
        budgets::require_run_budget(tx, principal, additional, now).await?;
        if let Some(grant) = grant {
            let quota = grants::quota_for_month_in(
                tx,
                &grant.grant_id,
                None,
                grant.quota.limit_tokens,
                grant.quota.max_concurrent_requests,
                &row.request.month,
                now,
            )
            .await?;
            requests::check_quota(&quota, additional, "model grant", false)?;
        }
        if attempt > 1 {
            ledger::insert_attempt_in(tx, id, attempt, additional, now).await?;
        }
        let result = ledger::mark_attempt_in(tx, id, attempt).await?;
        budgets::refresh_reservation_in(
            tx,
            &principal.tenant_id,
            &principal.execution_reservation_id,
        )
        .await?;
        Ok(result)
    }

    pub async fn check_workload_model_request_in(
        &self,
        tx: &mut Transaction,
        principal: &WorkloadModelPrincipal,
        id: &str,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        scope(tx).await?;
        require_provider_space(tx, &principal.tenant_id, &principal.model).await?;
        let row = ledger::request_in(tx, id).await?;
        require_same_principal(&row.request, principal)?;
        requests::require_pending(&row.request, now)?;
        requests::require_request_source(tx, &row.request).await?;
        budgets::require_run_budget(tx, principal, 0, now).await?;
        self.resolve_workload_route_in(
            tx,
            &principal.actor_user_id,
            &principal.resource_owner_user_id,
            &principal.model,
            now,
            false,
        )
        .await?;
        set_tenant_scope(tx, &principal.tenant_id).await?;
        requests::renew_request_in(tx, id, now).await?;
        Ok(())
    }

    /// Resolve a model after the Server has authorized the actor's canonical resource.
    pub async fn resolve_workload_model_snapshot(
        &self,
        actor_id: &UserId,
        resource_owner_id: &UserId,
        tenant_id: &TenantId,
        binding: &RunModelBinding,
        reasoning_effort: Option<ReasoningEffort>,
        now: u64,
    ) -> Result<RunModelSnapshot, ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        require_provider_space(&mut tx, tenant_id, binding).await?;
        let (route, grant) = self
            .resolve_workload_route_in(&mut tx, actor_id, resource_owner_id, binding, now, false)
            .await?;
        let snapshot = RunModelSnapshot {
            binding: binding.clone(),
            protocol: route.model.protocol,
            defaults: route.model.defaults,
            reasoning_effort,
            display_name: route.model.display_name,
            source_name: grant.map_or(route.provider.display_name, |grant| grant.name),
        };
        snapshot.validate()?;
        tx.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    pub async fn resource_model_entitlements(
        &self,
        actor: &ControlUser,
        tenant: &TenantId,
        kind: ResourceKind,
        id: &str,
        query: &PageQuery,
        now: u64,
    ) -> Result<ModelEntitlementPage, HarnessError> {
        let mut tx = self.model_transaction().await?;
        let access = resource_access_in(&mut tx, &actor.user_id, tenant, kind, id).await?;
        access.require(ResourceAction::View)?;
        let result = grants::entitlements_for_user_in(
            &mut tx,
            &access.storage_user_id,
            Some(&actor.user_id),
            query,
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub(super) async fn resolve_workload_route_in(
        &self,
        tx: &mut Transaction,
        actor: &UserId,
        owner: &UserId,
        binding: &RunModelBinding,
        now: u64,
        with_secret: bool,
    ) -> Result<(ResolvedModelRoute, Option<ModelGrantRecord>), ModelAccessError> {
        binding.validate()?;
        require_existing_account(tx, actor).await?;
        require_existing_account(tx, owner).await?;
        if binding.beneficiary_user_id() != owner {
            return Err(HarnessError::policy(
                "workload model beneficiary must retain the resource owner",
            )
            .into());
        }
        match binding {
            RunModelBinding::ComputerProvider { .. } => Err(HarnessError::policy(
                "computer models are available only to Server-managed remote computer sessions",
            )
            .into()),
            RunModelBinding::Platform {
                grant_id,
                model_id,
                beneficiary_user_id,
            } => {
                let grant = grants::grant_in(tx, grant_id, now).await?;
                grants::require_grant_for_workload(tx, beneficiary_user_id, &grant, now).await?;
                if actor != owner && !grant.allow_resource_sharing {
                    return Err(HarnessError::policy(
                        "model grant does not allow use by resource collaborators",
                    )
                    .into());
                }
                if !grant.model_ids.contains(model_id) {
                    return Err(HarnessError::policy(
                        "model is outside the workload's current grant",
                    )
                    .into());
                }
                let route = if with_secret {
                    config::resolve_route(tx, &self.cipher, model_id).await?
                } else {
                    let publication = config::publication_in(tx, model_id).await?;
                    if !publication.enabled || !publication.provider_enabled {
                        return Err(HarnessError::policy("public model is disabled").into());
                    }
                    let provider = config::provider_in(tx, &publication.provider_id)
                        .await?
                        .profile;
                    ResolvedModelRoute {
                        provider,
                        model: publication.model,
                        upstream_model: publication.upstream_model,
                        api_key: None,
                    }
                };
                Ok((route, Some(grant)))
            }
            RunModelBinding::UserProvider {
                tenant_id,
                owner_user_id,
                provider_id,
                model,
            } => self
                .resolve_workload_byok_in(
                    tx,
                    tenant_id,
                    owner_user_id,
                    provider_id,
                    model,
                    with_secret,
                )
                .await
                .map(|route| (route, None)),
        }
    }

    pub(super) async fn resolve_workload_byok_in(
        &self,
        tx: &mut Transaction,
        tenant_id: &TenantId,
        owner_user_id: &UserId,
        provider_id: &str,
        model: &str,
        with_secret: bool,
    ) -> Result<ResolvedModelRoute, ModelAccessError> {
        set_tenant_scope(tx, tenant_id).await?;
        set_user_scope(tx, owner_user_id).await?;
        let member: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM control_memberships WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(tenant_id.as_str())
        .bind(owner_user_id.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
        if member != 1 {
            return Err(HarnessError::policy(
                "BYOK owner is no longer a member of its Provider space",
            )
            .into());
        }
        let value:Option<String>=sqlx::query_scalar("SELECT provider_json FROM control_user_provider_profiles WHERE tenant_id=$1 AND user_id=$2 AND provider_id=$3")
                    .bind(tenant_id.as_str()).bind(owner_user_id.as_str()).bind(provider_id).fetch_optional(&mut **tx).await.map_err(database_error)?;
        let provider: ProviderProfile = from_json(
            &value.ok_or_else(|| HarnessError::policy("workload BYOK Provider is unavailable"))?,
        )?;
        provider.validate()?;
        let resolved = provider.resolved_model(model)?;
        let api_key = if with_secret {
            self.byok_secret_in(tx, tenant_id, owner_user_id, &provider)
                .await?
        } else {
            require_byok_credential_in(tx, tenant_id, owner_user_id, &provider).await?;
            None
        };
        let model = PublicModel {
            model_id: resolved.id.clone(),
            display_name: resolved.display_name.unwrap_or_else(|| resolved.id.clone()),
            protocol: provider.protocol,
            defaults: ProviderModelDefaults {
                context_window: resolved.context_window,
                max_output_tokens: resolved.max_output_tokens,
                reasoning: resolved.reasoning,
            },
        };
        Ok(ResolvedModelRoute {
            provider,
            upstream_model: model.model_id.clone(),
            model,
            api_key,
        })
    }

    async fn byok_secret_in(
        &self,
        tx: &mut Transaction,
        tenant: &TenantId,
        owner: &UserId,
        provider: &ProviderProfile,
    ) -> Result<Option<Zeroizing<String>>, HarnessError> {
        let Some(reference) = provider.api_key_ref.as_deref() else {
            return Ok(None);
        };
        let row=sqlx::query("SELECT version,nonce,ciphertext FROM control_user_credentials WHERE tenant_id=$1 AND user_id=$2 AND name=$3")
            .bind(tenant.as_str()).bind(owner.as_str()).bind(reference).fetch_optional(&mut **tx).await.map_err(database_error)?.ok_or_else(||HarnessError::policy("workload BYOK credential is not configured"))?;
        let bytes: Vec<u8> = row.try_get("nonce").map_err(database_error)?;
        let encrypted = EncryptedSecret {
            nonce: bytes
                .try_into()
                .map_err(|_| HarnessError::execution("stored BYOK credential nonce is invalid"))?,
            ciphertext: row.try_get("ciphertext").map_err(database_error)?,
        };
        let value = self.cipher.decrypt(
            tenant.as_str(),
            Some(owner.as_str()),
            reference,
            read_number(&row, "version")?,
            &encrypted,
        )?;
        let value = String::from_utf8(value.to_vec())
            .map_err(|_| HarnessError::execution("stored BYOK credential is not UTF-8"))?;
        if value.trim().is_empty() {
            return Err(HarnessError::policy("workload BYOK credential is empty"));
        }
        Ok(Some(Zeroizing::new(value)))
    }
}

pub(super) async fn require_byok_credential_in(
    tx: &mut Transaction,
    tenant: &TenantId,
    owner: &UserId,
    provider: &ProviderProfile,
) -> Result<(), HarnessError> {
    let Some(reference) = provider.api_key_ref.as_deref() else {
        return Ok(());
    };
    let size: Option<i64> = sqlx::query_scalar(
        "SELECT CAST(length(ciphertext) AS BIGINT) FROM control_user_credentials WHERE tenant_id=$1 AND user_id=$2 AND name=$3",
    ).bind(tenant.as_str()).bind(owner.as_str()).bind(reference)
        .fetch_optional(&mut **tx).await.map_err(database_error)?;
    // XChaCha20Poly1305 appends a 16-byte tag; a configured credential also contains plaintext bytes.
    if size.is_none_or(|size| size <= 16) {
        return Err(HarnessError::policy(
            "workload BYOK credential is not configured",
        ));
    }
    Ok(())
}

pub(super) async fn require_existing_account(
    tx: &mut Transaction,
    user: &UserId,
) -> Result<(), HarnessError> {
    user.validate()?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM control_users WHERE user_id=$1")
            .bind(user.as_str())
            .fetch_optional(&mut **tx)
            .await
            .map_err(database_error)?;
    crate::AccountStatus::parse(
        &status.ok_or_else(|| HarnessError::policy("workload account does not exist"))?,
    )?
    .require_active()
}

pub(super) async fn lock_binding(
    tx: &mut Transaction,
    binding: &RunModelBinding,
    now: u64,
) -> Result<(), HarnessError> {
    if let RunModelBinding::Platform { grant_id, .. } = binding {
        keys::lock_grant(tx, grant_id, now).await?;
    }
    Ok(())
}

fn workload_scope(principal: &WorkloadModelPrincipal) -> Result<String, HarnessError> {
    let value = json_text(&(
        principal.tenant_id.as_str(),
        principal.run_id.as_str(),
        &principal.worker_id,
        principal.worker_generation,
        principal.lease_token,
        principal.writer_fencing_token,
    ))?;
    Ok(format!("workload:{}", hex(&token_hash(&value))))
}

async fn require_provider_space(
    tx: &mut Transaction,
    execution_space: &TenantId,
    binding: &RunModelBinding,
) -> Result<(), HarnessError> {
    if let RunModelBinding::UserProvider {
        tenant_id,
        owner_user_id,
        ..
    } = binding
        && tenant_id != execution_space
        && *tenant_id != ControlStore::account_provider_space_in(tx, owner_user_id).await?
    {
        return Err(HarnessError::policy(
            "model Provider must belong to its owner's account or the bound execution space",
        ));
    }
    Ok(())
}

fn validate_principal(principal: &WorkloadModelPrincipal) -> Result<(), HarnessError> {
    principal.tenant_id.validate()?;
    principal.workspace_id.validate()?;
    principal.session_id.validate()?;
    principal.authorization_session_id.validate()?;
    principal.run_id.validate()?;
    principal.actor_user_id.validate()?;
    principal.resource_owner_user_id.validate()?;
    principal.execution_owner_user_id.validate()?;
    principal.model.validate()?;
    super::validate_text(&principal.project_id, "workload project", 128)?;
    super::validate_text(&principal.worker_id, "workload Worker", 128)?;
    super::validate_text(
        &principal.execution_reservation_id,
        "workload execution reservation",
        128,
    )?;
    if principal.worker_generation == 0
        || principal.lease_token == 0
        || principal.writer_fencing_token == 0
        || principal.run_token_limit == 0
    {
        return Err(HarnessError::invalid(
            "workload identity and budget counters must be positive",
        ));
    }
    Ok(())
}

fn require_same_principal(
    request: &super::ModelServiceRequest,
    principal: &WorkloadModelPrincipal,
) -> Result<(), HarnessError> {
    if request.origin != ModelRequestOrigin::Workload {
        return Err(HarnessError::policy(
            "model request does not belong to a workload",
        ));
    }
    let mut original = request.workload.clone().ok_or_else(|| {
        HarnessError::execution("accepted workload model request lost its principal")
    })?;
    original.run_token_limit = principal.run_token_limit;
    if original != *principal {
        return Err(HarnessError::policy(
            "model request does not belong to this canonical workload",
        ));
    }
    Ok(())
}
