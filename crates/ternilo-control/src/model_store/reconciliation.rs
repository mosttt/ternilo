use serde::{Deserialize, Serialize};
use serde_json::json;
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Transaction, database_error, lock};

use super::{
    ControlStore, ModelRequestSettlement, ModelRequestState, ModelServiceAttempt, PlatformAction,
    ServiceModelUsage, from_json, ledger, validate_text,
};
use crate::{ControlUser, account_store::append_platform_audit, authorize_platform_in};

const ACTION: &str = "model.usage.reconcile";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageReconciliationInput {
    pub expected_settled_at_ms: u64,
    pub usage: ServiceModelUsage,
    pub reference: String,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageReconciliation {
    pub attempt: u32,
    pub actor_user_id: UserId,
    pub reconciled_at_ms: u64,
    pub input: ModelUsageReconciliationInput,
    pub previous_usage: Option<ServiceModelUsage>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelUsageReconciliationResult {
    pub attempt: ModelServiceAttempt,
    pub reconciliation: ModelUsageReconciliation,
}

impl ControlStore {
    pub async fn model_usage_reconciliations(
        &self,
        actor: &ControlUser,
        request_id: &str,
    ) -> Result<Vec<ModelUsageReconciliation>, HarnessError> {
        let mut tx = self
            .model_admin_transaction(actor, PlatformAction::ModelsRead)
            .await?;
        ledger::request_in(&mut tx, request_id).await?;
        let records = records_in(&mut tx, request_id).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(records)
    }

    pub async fn reconcile_model_usage(
        &self,
        actor: &ControlUser,
        request_id: &str,
        attempt: u32,
        input: &ModelUsageReconciliationInput,
        now: u64,
    ) -> Result<ModelUsageReconciliationResult, HarnessError> {
        validate_input(input)?;
        let mut tx = self.model_transaction().await?;
        lock(&mut tx, &format!("ternilo:account-role:{}", actor.user_id)).await?;
        authorize_platform_in(&mut tx, &actor.user_id, PlatformAction::ModelGrantsManage).await?;
        let row = ledger::request_in(&mut tx, request_id).await?;
        ledger::lock_request(&mut tx, &row.request).await?;
        let row = ledger::request_in(&mut tx, request_id).await?;
        let previous = row
            .request
            .attempts
            .iter()
            .find(|value| value.attempt == attempt)
            .ok_or_else(|| HarnessError::invalid("accepted model attempt does not exist"))?;
        if row.request.state == ModelRequestState::Pending
            || previous.state == ModelRequestState::Pending
            || !previous.attempted
        {
            return Err(HarnessError::conflict(
                "only terminal attempted model requests can be reconciled",
            ));
        }
        if let Some(record) = records_in(&mut tx, request_id)
            .await?
            .into_iter()
            .find(|record| record.attempt == attempt)
        {
            if record.input != *input {
                return Err(HarnessError::conflict(
                    "model attempt already has a different reconciliation",
                ));
            }
            let result = ModelUsageReconciliationResult {
                attempt: previous.clone(),
                reconciliation: record,
            };
            tx.commit().await.map_err(database_error)?;
            return Ok(result);
        }
        if previous.accounted_tokens.is_some() {
            return Err(HarnessError::conflict(
                "known model usage cannot be overwritten",
            ));
        }
        if previous.settled_at_ms != Some(input.expected_settled_at_ms) {
            return Err(HarnessError::conflict(
                "model attempt changed; reload it before reconciliation",
            ));
        }
        let record = ModelUsageReconciliation {
            attempt,
            actor_user_id: actor.user_id.clone(),
            reconciled_at_ms: now,
            input: input.clone(),
            previous_usage: previous.usage.clone(),
        };
        let mut usage = input.usage.clone();
        usage.raw_usage = previous
            .usage
            .as_ref()
            .and_then(|value| value.raw_usage.clone());
        let settled = ledger::settle_attempt_in(
            &mut tx,
            request_id,
            attempt,
            &ModelRequestSettlement {
                state: previous.state,
                usage: Some(usage),
                upstream_request_id: previous.upstream_request_id.clone(),
                error_code: previous.error_code.clone(),
            },
            now,
        )
        .await?;
        ledger::refresh_workload_budget(&mut tx, &row).await?;
        append_platform_audit(
            &mut tx,
            &actor.user_id,
            ACTION,
            request_id,
            json!(record),
            now,
        )
        .await?;
        tx.commit().await.map_err(database_error)?;
        Ok(ModelUsageReconciliationResult {
            attempt: settled,
            reconciliation: record,
        })
    }
}

fn validate_input(input: &ModelUsageReconciliationInput) -> Result<(), HarnessError> {
    validate_text(&input.reference, "usage evidence reference", 512)?;
    validate_text(&input.note, "usage reconciliation note", 1024)?;
    if input.usage.input_tokens.is_none()
        || input.usage.output_tokens.is_none()
        || input.usage.raw_usage.is_some()
    {
        return Err(HarnessError::invalid(
            "reconciliation requires input and output tokens and does not accept raw upstream data",
        ));
    }
    Ok(())
}

async fn records_in(
    tx: &mut Transaction,
    request_id: &str,
) -> Result<Vec<ModelUsageReconciliation>, HarnessError> {
    // Reconciliation is rare and read on demand; normal gateway accounting does not scan audit history.
    let records: Vec<String> = sqlx::query_scalar("SELECT metadata FROM control_platform_audit WHERE action=$1 AND resource_id=$2 ORDER BY sequence")
        .bind(ACTION).bind(request_id).fetch_all(&mut **tx).await.map_err(database_error)?;
    records.iter().map(|record| from_json(record)).collect()
}
