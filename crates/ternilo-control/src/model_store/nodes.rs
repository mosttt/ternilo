use super::{
    ControlStore, ModelAccessError, ModelRequestInput, ModelRequestPermit, ModelRequestState,
    ModelServiceAttempt, ModelServiceRequest, grants, json_text, ledger, requests, scope,
    workloads,
};
use ternilo_protocol::{HarnessError, RunId, RunModelSnapshot, SessionId, TenantId, UserId};
use ternilo_storage::{Transaction, database_error};

/// Constructed by the Server from authenticated Node and session authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeModelPrincipal {
    pub credential_id: String,
    pub tenant_id: TenantId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub actor_user_id: UserId,
    pub resource_owner_user_id: UserId,
    pub snapshot: RunModelSnapshot,
}

impl ControlStore {
    pub async fn reserve_node_model_request_in(
        &self,
        tx: &mut Transaction,
        principal: &NodeModelPrincipal,
        input: &ModelRequestInput,
        now: u64,
    ) -> Result<ModelRequestPermit, ModelAccessError> {
        scope(tx).await?;
        requests::validate_request_input(input)?;
        principal.snapshot.validate()?;
        let binding = &principal.snapshot.binding;
        if input.model_id != binding.model_id() || input.protocol != principal.snapshot.protocol {
            return Err(
                HarnessError::policy("Node model does not match its accepted binding").into(),
            );
        }
        workloads::lock_binding(tx, binding, now).await?;
        let (route, grant) = self
            .resolve_workload_route_in(
                tx,
                &principal.actor_user_id,
                binding.beneficiary_user_id(),
                binding,
                now,
                true,
            )
            .await?;
        let caller_scope = json_text(&(
            &principal.credential_id,
            &principal.session_id,
            &principal.run_id,
        ))?;
        let reserved = requests::with_provider_budget(input, &route)?;
        let input = &reserved;
        if let Some(request) = requests::duplicate_in(tx, &caller_scope, input).await? {
            require_principal(&request, principal)?;
            return Ok(ModelRequestPermit {
                request,
                route,
                newly_accepted: false,
            });
        }
        if let Some(grant) = &grant {
            requests::check_quota(&grant.quota, input.reserved_tokens, "model grant", true)?;
        }
        let request = requests::insert_request_in(
            tx,
            requests::Admission {
                caller: requests::AdmissionCaller::Node(principal),
                scope: &caller_scope,
                grant: grant.as_ref(),
                input,
                max_attempts: route.provider.max_attempts,
                route: &route,
                budget_period_start: None,
                now,
            },
        )
        .await?;
        Ok(ModelRequestPermit {
            request,
            route,
            newly_accepted: true,
        })
    }

    pub async fn begin_node_model_attempt_in(
        &self,
        tx: &mut Transaction,
        principal: &NodeModelPrincipal,
        id: &str,
        attempt: u32,
        now: u64,
    ) -> Result<ModelServiceAttempt, ModelAccessError> {
        scope(tx).await?;
        workloads::lock_binding(tx, &principal.snapshot.binding, now).await?;
        self.check_node_model_request_in(tx, principal, id, now)
            .await?;
        let row = ledger::request_in(tx, id).await?;
        ledger::lock_request(tx, &row.request).await?;
        let row = ledger::request_in(tx, id).await?;
        if attempt == 0 || attempt > row.max_attempts {
            return Err(
                HarnessError::policy("model attempt exceeds its accepted retry limit").into(),
            );
        }
        let additional = if attempt > 1 {
            let last = row
                .request
                .attempts
                .last()
                .ok_or_else(|| HarnessError::execution("model request has no first attempt"))?;
            if last.attempt + 1 != attempt || last.state != ModelRequestState::Failed {
                return Err(
                    HarnessError::conflict("model retry must follow one failed attempt").into(),
                );
            }
            last.reserved_tokens
        } else {
            0
        };
        let (_, grant) = self
            .resolve_workload_route_in(
                tx,
                &principal.actor_user_id,
                principal.snapshot.binding.beneficiary_user_id(),
                &principal.snapshot.binding,
                now,
                false,
            )
            .await?;
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
        Ok(ledger::mark_attempt_in(tx, id, attempt).await?)
    }

    pub async fn check_node_model_request_in(
        &self,
        tx: &mut Transaction,
        principal: &NodeModelPrincipal,
        id: &str,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        scope(tx).await?;
        let row = ledger::request_in(tx, id).await?;
        require_principal(&row.request, principal)?;
        requests::require_pending(&row.request, now)?;
        requests::require_request_source(tx, &row.request).await?;
        let binding = &principal.snapshot.binding;
        self.resolve_workload_route_in(
            tx,
            &principal.actor_user_id,
            binding.beneficiary_user_id(),
            binding,
            now,
            false,
        )
        .await?;
        requests::renew_request_in(tx, id, now).await?;
        Ok(())
    }

    pub async fn finish_node_model_request(
        &self,
        id: &str,
        state: ModelRequestState,
        error_code: Option<&str>,
        now: u64,
    ) -> Result<ModelServiceRequest, HarnessError> {
        let mut tx = self.model_transaction().await?;
        let row = ledger::request_in(&mut tx, id).await?;
        require_node_request(&row.request)?;
        ledger::lock_request(&mut tx, &row.request).await?;
        ledger::finish_request_in(&mut tx, id, state, error_code, now).await?;
        let result = ledger::request_in(&mut tx, id).await?.request;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}

fn require_node_request(request: &ModelServiceRequest) -> Result<(), HarnessError> {
    if request.origin != super::ModelRequestOrigin::ClientDevice
        || request.resource_owner_user_id.is_none()
    {
        return Err(HarnessError::policy(
            "request does not originate from a Node model",
        ));
    }
    Ok(())
}

fn require_principal(
    request: &ModelServiceRequest,
    principal: &NodeModelPrincipal,
) -> Result<(), HarnessError> {
    require_node_request(request)?;
    if request.key_id.as_deref() != Some(principal.credential_id.as_str())
        || request.actor_user_id != principal.actor_user_id
        || request.resource_owner_user_id.as_ref() != Some(&principal.resource_owner_user_id)
        || &request.model_beneficiary_user_id != principal.snapshot.binding.beneficiary_user_id()
        || request.model_id != principal.snapshot.binding.model_id()
        || request.grant_id.as_deref()
            != match &principal.snapshot.binding {
                ternilo_protocol::RunModelBinding::Platform { grant_id, .. } => {
                    Some(grant_id.as_str())
                }
                ternilo_protocol::RunModelBinding::UserProvider { .. }
                | ternilo_protocol::RunModelBinding::ComputerProvider { .. } => None,
            }
    {
        return Err(HarnessError::policy("Node model request identity changed"));
    }
    Ok(())
}
