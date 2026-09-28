use serde::{Deserialize, Serialize};
use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, ProviderProfile, ProviderProtocol, UserId};
use ternilo_storage::{Transaction, database_error, lock};

use super::{
    ModelRequestOrigin, ModelRequestSettlement, ModelRequestSource, ModelRequestState,
    ModelServiceAttempt, ModelServiceRequest, PublicModel, ResolvedModelRoute, ServiceModelUsage,
    WorkloadModelPrincipal, from_json, json_text, number, read_number, read_optional_number,
};

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct RouteSnapshot {
    pub provider: ProviderProfile,
    pub model: PublicModel,
    pub upstream_model: String,
}

impl From<&ResolvedModelRoute> for RouteSnapshot {
    fn from(route: &ResolvedModelRoute) -> Self {
        Self {
            provider: route.provider.clone(),
            model: route.model.clone(),
            upstream_model: route.upstream_model.clone(),
        }
    }
}

pub(super) struct RequestRow {
    pub request: ModelServiceRequest,
    pub max_attempts: u32,
    pub budget_period_start: Option<String>,
}

pub(super) async fn request_in(tx: &mut Transaction, id: &str) -> Result<RequestRow, HarnessError> {
    let row = sqlx::query("SELECT * FROM control_model_requests WHERE request_id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(database_error)?
        .ok_or_else(|| HarnessError::invalid("accepted model request does not exist"))?;
    request_from_row(tx, &row).await
}

pub(super) async fn request_from_row(
    tx: &mut Transaction,
    row: &AnyRow,
) -> Result<RequestRow, HarnessError> {
    let id: String = row.try_get("request_id").map_err(database_error)?;
    let attempts = attempts_in(tx, &id).await?;
    let state = state_from_str(&row.try_get::<String, _>("state").map_err(database_error)?)?;
    let accounted_tokens = if state == ModelRequestState::Pending {
        None
    } else {
        sum_complete(attempts.iter().map(|attempt| attempt.accounted_tokens))?
    };
    let reserved_tokens = checked_sum(attempts.iter().map(|attempt| attempt.reserved_tokens))?;
    let last = attempts.last();
    let usage = aggregate_usage(&attempts)?;
    let workload: Option<WorkloadModelPrincipal> = row
        .try_get::<Option<String>, _>("workload_json")
        .map_err(database_error)?
        .map(|value| from_json(&value))
        .transpose()?;
    let origin = match row
        .try_get::<String, _>("origin")
        .map_err(database_error)?
        .as_str()
    {
        "api_key" => ModelRequestOrigin::ApiKey,
        "client_device" => ModelRequestOrigin::ClientDevice,
        "workload" => ModelRequestOrigin::Workload,
        _ => {
            return Err(HarnessError::execution(
                "stored model request origin is invalid",
            ));
        }
    };
    let source = match row
        .try_get::<String, _>("source")
        .map_err(database_error)?
        .as_str()
    {
        "platform_grant" => ModelRequestSource::PlatformGrant,
        "user_provider" => ModelRequestSource::UserProvider,
        _ => return Err(HarnessError::execution("stored model source is invalid")),
    };
    Ok(RequestRow {
        request: ModelServiceRequest {
            request_id: id,
            origin,
            source,
            key_id: row.try_get("key_id").map_err(database_error)?,
            actor_user_id: UserId::new(
                row.try_get::<String, _>("actor_user_id")
                    .map_err(database_error)?,
            ),
            resource_owner_user_id: row
                .try_get::<Option<String>, _>("resource_owner_user_id")
                .map_err(database_error)?
                .map(UserId::new),
            model_beneficiary_user_id: UserId::new(
                row.try_get::<String, _>("model_beneficiary_user_id")
                    .map_err(database_error)?,
            ),
            grant_id: row.try_get("grant_id").map_err(database_error)?,
            grant_name: row.try_get("grant_name").map_err(database_error)?,
            workload,
            model_id: row.try_get("model_id").map_err(database_error)?,
            provider_id: row.try_get("provider_id").map_err(database_error)?,
            upstream_model: row.try_get("upstream_model").map_err(database_error)?,
            protocol: protocol_from_str(
                &row.try_get::<String, _>("protocol")
                    .map_err(database_error)?,
            )?,
            state,
            attempted: attempts.iter().any(|attempt| attempt.attempted),
            reserved_tokens,
            accounted_tokens,
            usage,
            upstream_request_id: last.and_then(|attempt| attempt.upstream_request_id.clone()),
            error_code: row.try_get("error_code").map_err(database_error)?,
            month: row.try_get("month").map_err(database_error)?,
            created_at_ms: read_number(row, "created_at_ms")?,
            expires_at_ms: read_number(row, "expires_at_ms")?,
            settled_at_ms: read_optional_number(row, "settled_at_ms")?,
            attempts,
        },
        max_attempts: u32::try_from(read_number(row, "max_attempts")?)
            .map_err(|_| HarnessError::execution("stored model attempt limit is invalid"))?,
        budget_period_start: row.try_get("budget_period_start").map_err(database_error)?,
    })
}

pub(super) async fn attempts_in(
    tx: &mut Transaction,
    id: &str,
) -> Result<Vec<ModelServiceAttempt>, HarnessError> {
    let rows =
        sqlx::query("SELECT * FROM control_model_attempts WHERE request_id=$1 ORDER BY attempt")
            .bind(id)
            .fetch_all(&mut **tx)
            .await
            .map_err(database_error)?;
    rows.iter().map(attempt_from_row).collect()
}

fn attempt_from_row(row: &AnyRow) -> Result<ModelServiceAttempt, HarnessError> {
    Ok(ModelServiceAttempt {
        attempt: u32::try_from(read_number(row, "attempt")?)
            .map_err(|_| HarnessError::execution("stored model attempt is invalid"))?,
        state: state_from_str(&row.try_get::<String, _>("state").map_err(database_error)?)?,
        attempted: row.try_get::<i64, _>("attempted").map_err(database_error)? != 0,
        reserved_tokens: read_number(row, "reserved_tokens")?,
        accounted_tokens: read_optional_number(row, "accounted_tokens")?,
        usage: row
            .try_get::<Option<String>, _>("usage_json")
            .map_err(database_error)?
            .map(|value| from_json(&value))
            .transpose()?,
        upstream_request_id: row.try_get("upstream_request_id").map_err(database_error)?,
        error_code: row.try_get("error_code").map_err(database_error)?,
        created_at_ms: read_number(row, "created_at_ms")?,
        settled_at_ms: read_optional_number(row, "settled_at_ms")?,
    })
}

pub(super) async fn insert_attempt_in(
    tx: &mut Transaction,
    id: &str,
    attempt: u32,
    reserved: u64,
    now: u64,
) -> Result<(), HarnessError> {
    sqlx::query("INSERT INTO control_model_attempts(request_id,attempt,state,reserved_tokens,created_at_ms) VALUES($1,$2,'pending',$3,$4)")
        .bind(id).bind(i64::from(attempt)).bind(number(reserved)?).bind(number(now)?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

pub(super) async fn mark_attempt_in(
    tx: &mut Transaction,
    id: &str,
    attempt: u32,
) -> Result<ModelServiceAttempt, HarnessError> {
    let changed = sqlx::query("UPDATE control_model_attempts SET attempted=1 WHERE request_id=$1 AND attempt=$2 AND state='pending' AND attempted=0")
        .bind(id).bind(i64::from(attempt)).execute(&mut **tx).await.map_err(database_error)?.rows_affected();
    if changed != 1 {
        return Err(HarnessError::conflict(
            "model request already started this upstream attempt",
        ));
    }
    attempts_in(tx, id)
        .await?
        .into_iter()
        .find(|value| value.attempt == attempt)
        .ok_or_else(|| HarnessError::execution("started model attempt disappeared"))
}

pub(super) async fn settle_attempt_in(
    tx: &mut Transaction,
    id: &str,
    attempt: u32,
    input: &ModelRequestSettlement,
    now: u64,
) -> Result<ModelServiceAttempt, HarnessError> {
    validate_settlement(input)?;
    let previous = attempts_in(tx, id)
        .await?
        .into_iter()
        .find(|value| value.attempt == attempt)
        .ok_or_else(|| HarnessError::invalid("accepted model attempt does not exist"))?;
    let accounted = accounted_tokens(input.usage.as_ref())?;
    if !previous.attempted && accounted.is_some_and(|value| value > 0) {
        return Err(HarnessError::invalid(
            "unattempted model request cannot report consumed tokens",
        ));
    }
    if previous.state != ModelRequestState::Pending {
        if previous.state == input.state
            && previous.usage == input.usage
            && previous.upstream_request_id == input.upstream_request_id
            && previous.error_code == input.error_code
        {
            return Ok(previous);
        }
        if previous.accounted_tokens.is_some() || accounted.is_none() {
            return Err(HarnessError::conflict(
                "model attempt was already settled with a different result",
            ));
        }
    }
    let accounted = if previous.attempted {
        accounted
    } else {
        Some(0)
    };
    let usage = input.usage.as_ref();
    sqlx::query("UPDATE control_model_attempts SET state=$3,accounted_tokens=$4,usage_json=$5,upstream_request_id=$6,error_code=$7,settled_at_ms=$8,input_tokens=$9,output_tokens=$10,cached_input_tokens=$11,cache_write_tokens=$12,reasoning_tokens=$13 WHERE request_id=$1 AND attempt=$2")
        .bind(id).bind(i64::from(attempt)).bind(state_str(input.state)).bind(accounted.map(number).transpose()?).bind(input.usage.as_ref().map(json_text).transpose()?).bind(&input.upstream_request_id).bind(&input.error_code).bind(number(now)?)
        .bind(usage.and_then(|value| value.input_tokens).map(number).transpose()?).bind(usage.and_then(|value| value.output_tokens).map(number).transpose()?).bind(usage.and_then(|value| value.cached_input_tokens).map(number).transpose()?).bind(usage.and_then(|value| value.cache_write_tokens).map(number).transpose()?).bind(usage.and_then(|value| value.reasoning_tokens).map(number).transpose()?)
        .execute(&mut **tx).await.map_err(database_error)?;
    attempts_in(tx, id)
        .await?
        .into_iter()
        .find(|value| value.attempt == attempt)
        .ok_or_else(|| HarnessError::execution("settled model attempt disappeared"))
}

pub(super) async fn finish_request_in(
    tx: &mut Transaction,
    id: &str,
    state: ModelRequestState,
    error_code: Option<&str>,
    now: u64,
) -> Result<(), HarnessError> {
    if state == ModelRequestState::Pending {
        return Err(HarnessError::invalid(
            "model request completion must be terminal",
        ));
    }
    let current = request_in(tx, id).await?;
    if current.request.state != ModelRequestState::Pending {
        return Ok(());
    }
    for attempt in current
        .request
        .attempts
        .iter()
        .filter(|attempt| attempt.state == ModelRequestState::Pending)
    {
        settle_attempt_in(
            tx,
            id,
            attempt.attempt,
            &ModelRequestSettlement {
                state,
                usage: None,
                upstream_request_id: None,
                error_code: error_code.map(str::to_owned),
            },
            now,
        )
        .await?;
    }
    sqlx::query("UPDATE control_model_requests SET state=$2,error_code=$3,settled_at_ms=$4 WHERE request_id=$1")
        .bind(id).bind(state_str(state)).bind(error_code).bind(number(now)?).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

pub(super) async fn lock_request(
    tx: &mut Transaction,
    request: &ModelServiceRequest,
) -> Result<(), HarnessError> {
    if let Some(workload) = &request.workload {
        super::budgets::lock_budget(tx, &workload.tenant_id).await?;
    }
    if let Some(grant) = &request.grant_id {
        lock(tx, &format!("ternilo:model-grant:{grant}")).await?;
    }
    if let Some(key) = &request.key_id {
        lock(tx, &format!("ternilo:model-key:{key}")).await?;
    }
    lock(tx, &format!("ternilo:model-request:{}", request.request_id)).await
}

pub(super) async fn refresh_workload_budget(
    tx: &mut Transaction,
    row: &RequestRow,
) -> Result<(), HarnessError> {
    if let Some(workload) = &row.request.workload
        && super::budgets::refresh_reservation_in(
            tx,
            &workload.tenant_id,
            &workload.execution_reservation_id,
        )
        .await?
        .is_none()
    {
        let period = row
            .budget_period_start
            .as_deref()
            .ok_or_else(|| HarnessError::execution("accepted workload lost its budget period"))?;
        super::budgets::refresh_period_in(tx, &workload.tenant_id, period).await?;
    }
    Ok(())
}

fn validate_settlement(input: &ModelRequestSettlement) -> Result<(), HarnessError> {
    if input.state == ModelRequestState::Pending {
        return Err(HarnessError::invalid("model settlement must be terminal"));
    }
    if let Some(value) = &input.upstream_request_id {
        super::validate_text(value, "upstream request ID", 256)?;
    }
    if let Some(value) = &input.error_code {
        super::validate_text(value, "model error code", 128)?;
    }
    Ok(())
}

fn accounted_tokens(usage: Option<&ServiceModelUsage>) -> Result<Option<u64>, HarnessError> {
    let Some(usage) = usage else {
        return Ok(None);
    };
    for value in [
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_write_tokens,
        usage.reasoning_tokens,
    ]
    .into_iter()
    .flatten()
    {
        number(value)?;
    }
    match (usage.input_tokens, usage.output_tokens) {
        (Some(input), Some(output)) => Ok(Some(checked_sum([input, output])?)),
        _ => Ok(None),
    }
}

fn checked_sum(values: impl IntoIterator<Item = u64>) -> Result<u64, HarnessError> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or_else(|| HarnessError::execution("model token aggregate overflow"))
    })
}

fn sum_complete(
    values: impl IntoIterator<Item = Option<u64>>,
) -> Result<Option<u64>, HarnessError> {
    let mut total = 0_u64;
    for value in values {
        let Some(value) = value else {
            return Ok(None);
        };
        total = checked_sum([total, value])?;
    }
    Ok(Some(total))
}

fn aggregate_usage(
    attempts: &[ModelServiceAttempt],
) -> Result<Option<ServiceModelUsage>, HarnessError> {
    if attempts.len() == 1 {
        return Ok(attempts[0].usage.clone());
    }
    if attempts.iter().all(|attempt| attempt.usage.is_none()) {
        return Ok(None);
    }
    Ok(Some(ServiceModelUsage {
        input_tokens: sum_complete(
            attempts
                .iter()
                .map(|attempt| attempt.usage.as_ref().and_then(|usage| usage.input_tokens)),
        )?,
        output_tokens: sum_complete(
            attempts
                .iter()
                .map(|attempt| attempt.usage.as_ref().and_then(|usage| usage.output_tokens)),
        )?,
        cached_input_tokens: sum_complete(attempts.iter().map(|attempt| {
            attempt
                .usage
                .as_ref()
                .and_then(|usage| usage.cached_input_tokens)
        }))?,
        cache_write_tokens: sum_complete(attempts.iter().map(|attempt| {
            attempt
                .usage
                .as_ref()
                .and_then(|usage| usage.cache_write_tokens)
        }))?,
        reasoning_tokens: sum_complete(attempts.iter().map(|attempt| {
            attempt
                .usage
                .as_ref()
                .and_then(|usage| usage.reasoning_tokens)
        }))?,
        raw_usage: None,
    }))
}

pub(super) fn protocol_from_str(value: &str) -> Result<ProviderProtocol, HarnessError> {
    match value {
        "openai-chat-completions" => Ok(ProviderProtocol::OpenAiChatCompletions),
        "openai-responses" => Ok(ProviderProtocol::OpenAiResponses),
        "deepseek-responses" => Ok(ProviderProtocol::DeepSeekResponses),
        "google-gemini" => Ok(ProviderProtocol::GoogleGemini),
        "anthropic-messages" => Ok(ProviderProtocol::AnthropicMessages),
        _ => Err(HarnessError::execution("stored model protocol is invalid")),
    }
}

pub(super) fn state_from_str(value: &str) -> Result<ModelRequestState, HarnessError> {
    match value {
        "pending" => Ok(ModelRequestState::Pending),
        "completed" => Ok(ModelRequestState::Completed),
        "failed" => Ok(ModelRequestState::Failed),
        "cancelled" => Ok(ModelRequestState::Cancelled),
        _ => Err(HarnessError::execution(
            "stored model request state is invalid",
        )),
    }
}

pub(super) const fn state_str(value: ModelRequestState) -> &'static str {
    match value {
        ModelRequestState::Pending => "pending",
        ModelRequestState::Completed => "completed",
        ModelRequestState::Failed => "failed",
        ModelRequestState::Cancelled => "cancelled",
    }
}
