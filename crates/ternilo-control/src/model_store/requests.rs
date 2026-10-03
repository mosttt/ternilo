use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, UserId};
use ternilo_storage::{Transaction, database_error};

use super::{
    ControlStore, ModelAccessError, ModelAccessErrorKind, ModelCredentialKind, ModelGrantRecord,
    ModelKeyRecord, ModelQuotaSnapshot, ModelRequestInput, ModelRequestOrigin, ModelRequestPage,
    ModelRequestPermit, ModelRequestSettlement, ModelRequestSource, ModelRequestState,
    ModelServiceAttempt, ModelServiceRequest, ModelServiceUsageReport, PlatformAction,
    ResolvedModelRoute, WorkloadModelPrincipal, authorize_platform_in, config, grants, json_text,
    keys, ledger, month_at, number, read_number, require_account, validate_text,
};
use crate::{ControlUser, PageQuery, crypto::random_identifier};

mod client_admission;

pub(super) fn with_provider_budget(
    input: &ModelRequestInput,
    route: &ResolvedModelRoute,
) -> Result<ModelRequestInput, HarnessError> {
    let mut input = input.clone();
    if let Some(hosted) = &route.provider.hosted_tools {
        // Provider-side searches can introduce input and model iterations absent
        // from the client payload. Reserve their configured upper envelope.
        let calls = u64::from(hosted.max_uses)
            * (u64::from(hosted.web_search) + u64::from(hosted.web_fetch))
            + 1;
        let ceiling = route
            .model
            .defaults
            .context_window
            .checked_add(route.model.defaults.max_output_tokens)
            .and_then(|tokens| tokens.checked_mul(calls))
            .ok_or_else(|| HarnessError::invalid("hosted tool token reservation overflow"))?;
        input.reserved_tokens = input.reserved_tokens.max(ceiling);
    }
    Ok(input)
}

impl ControlStore {
    pub async fn mark_model_request_attempted(
        &self,
        id: &str,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let initial = ledger::request_in(&mut tx, id).await?;
        require_api_request(&initial.request)?;
        if let Some(grant_id) = initial.request.grant_id.as_deref() {
            keys::lock_grant(&mut tx, grant_id, now).await?;
        }
        ledger::lock_request(&mut tx, &initial.request).await?;
        let row = ledger::request_in(&mut tx, id).await?;
        self.check_api_request_in(&mut tx, &row.request, now)
            .await?;
        ledger::mark_attempt_in(&mut tx, id, 1).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }

    pub async fn check_model_request_authorized(
        &self,
        id: &str,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        let mut tx = self.model_transaction().await?;
        let row = ledger::request_in(&mut tx, id).await?;
        self.check_api_request_in(&mut tx, &row.request, now)
            .await?;
        renew_request_in(&mut tx, id, now).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(())
    }

    async fn check_api_request_in(
        &self,
        tx: &mut Transaction,
        request: &ModelServiceRequest,
        now: u64,
    ) -> Result<(), ModelAccessError> {
        require_api_request(request)?;
        require_pending(request, now)?;
        require_request_source(tx, request).await?;
        if request.source == ModelRequestSource::UserProvider {
            return self.check_device_account_request_in(tx, request, now).await;
        }
        let key_id = request
            .key_id
            .as_deref()
            .ok_or_else(|| HarnessError::execution("API model request has no key"))?;
        let key = if request.origin == ModelRequestOrigin::ClientDevice {
            let device = super::devices::credentials::device_in(tx, key_id).await?;
            let grant = request
                .grant_id
                .as_deref()
                .ok_or_else(|| HarnessError::execution("device request has no model grant"))?;
            super::devices::credentials::grant_key_in(tx, &device, grant, now).await?
        } else {
            keys::key_in(tx, key_id).await?
        };
        keys::validate_key(tx, &key, Some(&request.model_id), now).await?;
        Ok(())
    }

    /// Settle the single attempt of a public API request and release its concurrency slot.
    pub async fn settle_model_request(
        &self,
        id: &str,
        input: &ModelRequestSettlement,
        now: u64,
    ) -> Result<ModelServiceRequest, HarnessError> {
        let mut tx = self.model_transaction().await?;
        let row = ledger::request_in(&mut tx, id).await?;
        require_api_request(&row.request)?;
        ledger::lock_request(&mut tx, &row.request).await?;
        ledger::settle_attempt_in(&mut tx, id, 1, input, now).await?;
        ledger::finish_request_in(&mut tx, id, input.state, input.error_code.as_deref(), now)
            .await?;
        let result = ledger::request_in(&mut tx, id).await?.request;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    /// Usage is accepted for an existing attempt even after its originating workload loses its lease.
    pub async fn settle_model_attempt(
        &self,
        id: &str,
        attempt: u32,
        input: &ModelRequestSettlement,
        now: u64,
    ) -> Result<ModelServiceAttempt, HarnessError> {
        let mut tx = self.model_transaction().await?;
        let row = ledger::request_in(&mut tx, id).await?;
        ledger::lock_request(&mut tx, &row.request).await?;
        let result = ledger::settle_attempt_in(&mut tx, id, attempt, input, now).await?;
        ledger::refresh_workload_budget(&mut tx, &row).await?;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    pub async fn finish_workload_model_request(
        &self,
        id: &str,
        state: ModelRequestState,
        error_code: Option<&str>,
        now: u64,
    ) -> Result<ModelServiceRequest, HarnessError> {
        let mut tx = self.model_transaction().await?;
        let row = ledger::request_in(&mut tx, id).await?;
        if row.request.origin != ModelRequestOrigin::Workload {
            return Err(HarnessError::invalid(
                "model request does not originate from a workload",
            ));
        }
        ledger::lock_request(&mut tx, &row.request).await?;
        ledger::finish_request_in(&mut tx, id, state, error_code, now).await?;
        ledger::refresh_workload_budget(&mut tx, &row).await?;
        let result = ledger::request_in(&mut tx, id).await?.request;
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }

    /// Expired logical requests retain unknown attempt reservations and release only concurrency.
    pub async fn expire_model_requests(&self, now: u64, limit: u32) -> Result<u64, HarnessError> {
        if !(1..=1000).contains(&limit) {
            return Err(HarnessError::invalid(
                "model recovery limit must be between 1 and 1000",
            ));
        }
        let mut tx = self.model_transaction().await?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT request_id FROM control_model_requests WHERE state='pending' AND expires_at_ms<=$1 ORDER BY expires_at_ms,request_id LIMIT $2")
            .bind(number(now)?).bind(i64::from(limit)).fetch_all(&mut *tx).await.map_err(database_error)?;
        tx.commit().await.map_err(database_error)?;
        let mut recovered = 0;
        for id in ids {
            let mut tx = self.model_transaction().await?;
            let row = ledger::request_in(&mut tx, &id).await?;
            ledger::lock_request(&mut tx, &row.request).await?;
            let current = ledger::request_in(&mut tx, &id).await?;
            if current.request.state == ModelRequestState::Pending
                && current.request.expires_at_ms <= now
            {
                ledger::finish_request_in(
                    &mut tx,
                    &id,
                    ModelRequestState::Failed,
                    Some("request_expired"),
                    now,
                )
                .await?;
                ledger::refresh_workload_budget(&mut tx, &current).await?;
                recovered += 1;
            }
            tx.commit().await.map_err(database_error)?;
        }
        Ok(recovered)
    }

    pub async fn list_model_service_requests(
        &self,
        actor: &ControlUser,
        user_id: Option<&UserId>,
        query: &PageQuery,
    ) -> Result<ModelRequestPage, HarnessError> {
        self.list_model_service_requests_by_source(actor, user_id, query, None)
            .await
    }

    pub async fn list_model_service_requests_by_source(
        &self,
        actor: &ControlUser,
        user_id: Option<&UserId>,
        query: &PageQuery,
        source: Option<ModelRequestSource>,
    ) -> Result<ModelRequestPage, HarnessError> {
        let (pattern, cursor) = query.parameters()?;
        let cursor = cursor.as_deref().map(request_cursor).transpose()?;
        let mut tx = self.model_transaction().await?;
        authorize_usage(&mut tx, actor, user_id).await?;
        let rows=sqlx::query("SELECT * FROM control_model_requests WHERE (CAST($1 AS TEXT) IS NULL OR actor_user_id=$1 OR model_beneficiary_user_id=$1) AND (CAST($2 AS TEXT) IS NULL OR LOWER(request_id) LIKE $2 ESCAPE '!' OR LOWER(model_id) LIKE $2 ESCAPE '!' OR LOWER(COALESCE(grant_name,'')) LIKE $2 ESCAPE '!') AND (CAST($3 AS BIGINT) IS NULL OR created_at_ms<$3 OR (created_at_ms=$3 AND request_id<$4)) AND (CAST($6 AS TEXT) IS NULL OR source=$6) ORDER BY created_at_ms DESC,request_id DESC LIMIT $5")
            .bind(user_id.map(UserId::as_str)).bind(pattern).bind(cursor.as_ref().map(|value| value.0)).bind(cursor.as_ref().map(|value| value.1)).bind(i64::from(query.limit)+1).bind(source_name(source)).fetch_all(&mut *tx).await.map_err(database_error)?;
        let mut requests = Vec::with_capacity(rows.len());
        for row in rows {
            requests.push(ledger::request_from_row(&mut tx, &row).await?.request);
        }
        let next_cursor = query.finish(&mut requests, |request| {
            format!("{}:{}", request.created_at_ms, request.request_id)
        });
        tx.commit().await.map_err(database_error)?;
        Ok(ModelRequestPage {
            requests,
            next_cursor,
        })
    }

    pub async fn model_service_usage(
        &self,
        actor: &ControlUser,
        user_id: Option<&UserId>,
        now: u64,
    ) -> Result<ModelServiceUsageReport, HarnessError> {
        self.model_service_usage_by_source(actor, user_id, now, None)
            .await
    }

    pub async fn model_service_usage_by_source(
        &self,
        actor: &ControlUser,
        user_id: Option<&UserId>,
        now: u64,
        source: Option<ModelRequestSource>,
    ) -> Result<ModelServiceUsageReport, HarnessError> {
        let mut tx = self.model_transaction().await?;
        authorize_usage(&mut tx, actor, user_id).await?;
        let month = month_at(now)?;
        let row = sqlx::query("SELECT COUNT(*) AS request_count,CAST(COALESCE(SUM(CASE WHEN state='pending' AND expires_at_ms>$3 THEN 1 ELSE 0 END),0) AS BIGINT) AS active_requests,CAST(COALESCE(SUM(CASE WHEN (state<>'pending' OR expires_at_ms<=$3) AND EXISTS(SELECT 1 FROM control_model_attempts a WHERE a.request_id=r.request_id AND a.accounted_tokens IS NULL) THEN 1 ELSE 0 END),0) AS BIGINT) AS unknown_requests FROM control_model_requests r WHERE (CAST($1 AS TEXT) IS NULL OR actor_user_id=$1 OR model_beneficiary_user_id=$1) AND month=$2 AND (CAST($4 AS TEXT) IS NULL OR source=$4)")
            .bind(user_id.map(UserId::as_str)).bind(&month).bind(number(now)?).bind(source_name(source)).fetch_one(&mut *tx).await.map_err(database_error)?;
        let totals = sqlx::query("SELECT CAST(COALESCE(SUM(a.accounted_tokens),0) AS BIGINT) AS used_tokens,CAST(COALESCE(SUM(CASE WHEN a.accounted_tokens IS NULL THEN a.reserved_tokens ELSE 0 END),0) AS BIGINT) AS reserved_tokens,CAST(COALESCE(SUM(a.input_tokens),0) AS BIGINT) AS input_tokens,CAST(COALESCE(SUM(a.output_tokens),0) AS BIGINT) AS output_tokens,CAST(COALESCE(SUM(a.cached_input_tokens),0) AS BIGINT) AS cached_input_tokens,CAST(COALESCE(SUM(a.cache_write_tokens),0) AS BIGINT) AS cache_write_tokens,CAST(COALESCE(SUM(a.reasoning_tokens),0) AS BIGINT) AS reasoning_tokens FROM control_model_attempts a JOIN control_model_requests r ON r.request_id=a.request_id WHERE (CAST($1 AS TEXT) IS NULL OR r.actor_user_id=$1 OR r.model_beneficiary_user_id=$1) AND r.month=$2 AND (CAST($3 AS TEXT) IS NULL OR r.source=$3)")
            .bind(user_id.map(UserId::as_str)).bind(&month).bind(source_name(source)).fetch_one(&mut *tx).await.map_err(database_error)?;
        let result = ModelServiceUsageReport {
            month,
            request_count: read_number(&row, "request_count")?,
            active_requests: read_number(&row, "active_requests")?,
            unknown_requests: read_number(&row, "unknown_requests")?,
            used_tokens: read_number(&totals, "used_tokens")?,
            reserved_tokens: read_number(&totals, "reserved_tokens")?,
            input_tokens: read_number(&totals, "input_tokens")?,
            output_tokens: read_number(&totals, "output_tokens")?,
            cached_input_tokens: read_number(&totals, "cached_input_tokens")?,
            cache_write_tokens: read_number(&totals, "cache_write_tokens")?,
            reasoning_tokens: read_number(&totals, "reasoning_tokens")?,
        };
        tx.commit().await.map_err(database_error)?;
        Ok(result)
    }
}

fn source_name(source: Option<ModelRequestSource>) -> Option<&'static str> {
    source.map(|value| match value {
        ModelRequestSource::PlatformGrant => "platform_grant",
        ModelRequestSource::UserProvider => "user_provider",
    })
}

#[derive(Clone, Copy)]
pub(super) enum AdmissionCaller<'a> {
    ApiKey(&'a ModelKeyRecord),
    Workload(&'a WorkloadModelPrincipal),
    Node(&'a super::NodeModelPrincipal),
    DeviceAccount(&'a ternilo_protocol::ModelDeviceIdentity),
}

pub(super) struct Admission<'a> {
    pub caller: AdmissionCaller<'a>,
    pub scope: &'a str,
    pub grant: Option<&'a ModelGrantRecord>,
    pub route: &'a ResolvedModelRoute,
    pub input: &'a ModelRequestInput,
    pub max_attempts: u32,
    pub budget_period_start: Option<&'a str>,
    pub now: u64,
}

pub(super) async fn insert_request_in(
    tx: &mut Transaction,
    admission: Admission<'_>,
) -> Result<ModelServiceRequest, HarnessError> {
    let (origin, key_id, actor, owner, beneficiary, workload) = match admission.caller {
        AdmissionCaller::DeviceAccount(device) => (
            "client_device",
            Some(device.device_id.as_str()),
            &device.user_id,
            None,
            &device.user_id,
            None,
        ),
        AdmissionCaller::ApiKey(key) => (
            if key.kind == ModelCredentialKind::ClientDevice {
                "client_device"
            } else {
                "api_key"
            },
            Some(key.key_id.as_str()),
            &key.user_id,
            None,
            &key.user_id,
            None,
        ),
        AdmissionCaller::Workload(principal) => (
            "workload",
            None,
            &principal.actor_user_id,
            Some(&principal.resource_owner_user_id),
            principal.model.beneficiary_user_id(),
            Some(principal),
        ),
        AdmissionCaller::Node(principal) => (
            "client_device",
            Some(principal.credential_id.as_str()),
            &principal.actor_user_id,
            Some(&principal.resource_owner_user_id),
            principal.snapshot.binding.beneficiary_user_id(),
            None,
        ),
    };
    let node = match admission.caller {
        AdmissionCaller::Node(principal) => Some(principal),
        _ => None,
    };
    let id = random_identifier("mrq");
    let month = month_at(admission.now)?;
    let expires = request_deadline(&admission)?;
    sqlx::query("INSERT INTO control_model_requests(request_id,origin,source,caller_scope,key_id,request_key,payload_hash,actor_user_id,resource_owner_user_id,model_beneficiary_user_id,grant_id,grant_name,workload_json,tenant_id,project_id,session_id,run_id,execution_reservation_id,budget_period_start,model_id,provider_id,upstream_model,protocol,route_snapshot_json,max_attempts,state,month,created_at_ms,expires_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,'pending',$26,$27,$28)")
        .bind(&id).bind(origin).bind(if admission.grant.is_some() {"platform_grant"} else {"user_provider"}).bind(admission.scope).bind(key_id).bind(&admission.input.request_key).bind(&admission.input.payload_hash).bind(actor.as_str()).bind(owner.map(UserId::as_str)).bind(beneficiary.as_str()).bind(admission.grant.map(|grant| grant.grant_id.as_str())).bind(admission.grant.map(|grant| grant.name.as_str())).bind(workload.map(json_text).transpose()?)
        .bind(workload.map(|value| value.tenant_id.as_str()).or_else(|| node.map(|value| value.tenant_id.as_str()))).bind(workload.map(|value| value.project_id.as_str())).bind(workload.map(|value| value.session_id.as_str()).or_else(|| node.map(|value| value.session_id.as_str()))).bind(workload.map(|value| value.run_id.as_str()).or_else(|| node.map(|value| value.run_id.as_str()))).bind(workload.map(|value| value.execution_reservation_id.as_str())).bind(admission.budget_period_start)
        .bind(&admission.input.model_id).bind(&admission.route.provider.id).bind(&admission.route.upstream_model).bind(admission.input.protocol.as_str()).bind(json_text(&ledger::RouteSnapshot::from(admission.route))?).bind(i64::from(admission.max_attempts)).bind(&month).bind(number(admission.now)?).bind(number(expires)?).execute(&mut **tx).await.map_err(database_error)?;
    ledger::insert_attempt_in(tx, &id, 1, admission.input.reserved_tokens, admission.now).await?;
    Ok(ledger::request_in(tx, &id).await?.request)
}

pub(super) async fn duplicate_in(
    tx: &mut Transaction,
    scope: &str,
    input: &ModelRequestInput,
) -> Result<Option<ModelServiceRequest>, HarnessError> {
    let row = sqlx::query(
        "SELECT * FROM control_model_requests WHERE caller_scope=$1 AND request_key=$2",
    )
    .bind(scope)
    .bind(&input.request_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some(row) = row else {
        return Ok(None);
    };
    validate_duplicate(&row, input)?;
    Ok(Some(ledger::request_from_row(tx, &row).await?.request))
}

fn validate_duplicate(row: &AnyRow, input: &ModelRequestInput) -> Result<(), HarnessError> {
    if row
        .try_get::<String, _>("payload_hash")
        .map_err(database_error)?
        != input.payload_hash
        || row
            .try_get::<String, _>("model_id")
            .map_err(database_error)?
            != input.model_id
        || row
            .try_get::<String, _>("protocol")
            .map_err(database_error)?
            != input.protocol.as_str()
    {
        return Err(HarnessError::conflict(
            "model request key was already used for a different payload",
        ));
    }
    Ok(())
}

pub(super) async fn require_request_source(
    tx: &mut Transaction,
    request: &ModelServiceRequest,
) -> Result<(), HarnessError> {
    if request.source == super::ModelRequestSource::PlatformGrant {
        let enabled: Option<i64> =
            sqlx::query_scalar("SELECT enabled FROM control_model_providers WHERE provider_id=$1")
                .bind(&request.provider_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(database_error)?;
        if enabled != Some(1) {
            return Err(HarnessError::policy(
                "accepted model request upstream is disabled",
            ));
        }
    }
    Ok(())
}

pub(super) fn require_pending(request: &ModelServiceRequest, now: u64) -> Result<(), HarnessError> {
    if request.state != ModelRequestState::Pending || request.expires_at_ms <= now {
        return Err(HarnessError::policy("model request is no longer active"));
    }
    Ok(())
}

fn require_api_request(request: &ModelServiceRequest) -> Result<(), HarnessError> {
    if matches!(
        request.origin,
        ModelRequestOrigin::ApiKey | ModelRequestOrigin::ClientDevice
    ) && request.resource_owner_user_id.is_none()
    {
        Ok(())
    } else {
        Err(HarnessError::invalid(
            "this operation requires a public API model request",
        ))
    }
}

async fn authorize_usage(
    tx: &mut Transaction,
    actor: &ControlUser,
    user_id: Option<&UserId>,
) -> Result<(), HarnessError> {
    if user_id == Some(&actor.user_id) {
        require_account(tx, &actor.user_id).await
    } else {
        authorize_platform_in(tx, &actor.user_id, PlatformAction::ModelsRead).await
    }
}

pub(super) fn check_quota(
    quota: &ModelQuotaSnapshot,
    reserved: u64,
    source: &str,
    new_request: bool,
) -> Result<(), ModelAccessError> {
    let total = quota
        .used_tokens
        .checked_add(quota.reserved_tokens)
        .and_then(|value| value.checked_add(reserved));
    let reason = if total.is_none_or(|value| value > quota.limit_tokens) {
        Some("monthly token limit would be exceeded")
    } else if new_request && quota.active_requests >= u64::from(quota.max_concurrent_requests) {
        Some("concurrent request limit is reached")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(ModelAccessError {
            kind: ModelAccessErrorKind::QuotaExceeded,
            error: HarnessError::policy(format!("{source} {reason}")),
        });
    }
    Ok(())
}

pub(super) fn validate_request_input(input: &ModelRequestInput) -> Result<(), HarnessError> {
    validate_text(&input.request_key, "model request key", 200)?;
    if input.payload_hash.len() != 64
        || !input
            .payload_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(HarnessError::invalid(
            "model payload hash must be a SHA-256 hexadecimal digest",
        ));
    }
    if input.reserved_tokens == 0 {
        return Err(HarnessError::invalid("model reservation must be positive"));
    }
    number(input.reserved_tokens)?;
    Ok(())
}

fn request_cursor(value: &str) -> Result<(i64, &str), HarnessError> {
    let invalid = || HarnessError::invalid("model request cursor is invalid");
    let (timestamp, id) = value.split_once(':').ok_or_else(invalid)?;
    let timestamp = timestamp.parse::<u64>().map_err(|_| invalid())?;
    validate_text(id, "model request cursor ID", 128)?;
    Ok((number(timestamp)?, id))
}

/// A live request renews its recovery lease independently from the HTTP timeout.
pub(super) async fn renew_request_in(
    tx: &mut Transaction,
    id: &str,
    now: u64,
) -> Result<(), HarnessError> {
    let expires = number(
        now.checked_add(60_000)
            .ok_or_else(|| HarnessError::invalid("model lease overflow"))?,
    )?;
    sqlx::query("UPDATE control_model_requests SET expires_at_ms=CASE WHEN expires_at_ms<$2 THEN $2 ELSE expires_at_ms END WHERE request_id=$1 AND state='pending'")
        .bind(id).bind(expires).execute(&mut **tx).await.map_err(database_error)?;
    Ok(())
}

fn request_deadline(admission: &Admission<'_>) -> Result<u64, HarnessError> {
    let mut lifetime = admission
        .route
        .provider
        .timeout_ms
        .checked_mul(u64::from(admission.max_attempts))
        .ok_or_else(|| HarnessError::invalid("model request lifetime overflow"))?;
    for retry in 1..admission.max_attempts {
        lifetime = lifetime
            .checked_add(
                admission
                    .route
                    .provider
                    .retry_base_delay_ms
                    .checked_mul(1_u64 << retry.saturating_sub(1).min(6))
                    .ok_or_else(|| HarnessError::invalid("model retry lifetime overflow"))?,
            )
            .ok_or_else(|| HarnessError::invalid("model request lifetime overflow"))?;
    }
    admission
        .now
        .checked_add(lifetime)
        .and_then(|value| value.checked_add(60_000))
        .ok_or_else(|| HarnessError::invalid("model request deadline overflow"))
}
