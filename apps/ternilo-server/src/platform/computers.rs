use super::{
    ApiError, Depot, Deserialize, Duration, ErrorCode, ExecutorId, Json, Request, StatusCode,
    Value, actor, app_state, handler, invalid_request, json, now_ms, path_parameter,
    tenant_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnrollmentRequest {
    name: String,
    project_id: Option<String>,
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ComputerListQuery {
    include_removed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumeEnrollmentRequest {
    token: String,
}

#[handler]
pub(super) async fn list_executors(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let query = request
        .parse_queries::<ComputerListQuery>()
        .map_err(invalid_request)?;
    let records = state
        .store
        .list_computers(actor(depot), &tenant_id, false, query.include_removed)
        .await?;
    let mut executors = Vec::with_capacity(records.len());
    for record in records {
        let connected = record.state != "revoked"
            && state
                .edge
                .is_connected(&tenant_id, &record.executor_id)
                .await;
        executors.push(json!({
            "executor_id": record.executor_id,
            "name": record.management.name,
            "project_id": record.project_id,
            "state": record.state,
            "connected": connected,
            "enrolled_at_ms": record.enrolled_at_ms,
            "last_seen_at_ms": record.last_seen_at_ms,
            "management": record.management,
        }));
    }
    Ok(Json(json!({ "executors": executors })))
}

#[handler]
pub(super) async fn list_owned_executors(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let state = app_state(depot);
    let query = request
        .parse_queries::<ComputerListQuery>()
        .map_err(invalid_request)?;
    let records = state
        .store
        .list_computers(actor(depot), &tenant_id, true, query.include_removed)
        .await?;
    let mut executors = Vec::with_capacity(records.len());
    for record in records {
        let connected = record.state != "revoked"
            && state
                .edge
                .is_connected(&tenant_id, &record.executor_id)
                .await;
        executors.push(json!({
            "executor_id": record.executor_id,
            "name": record.management.name,
            "project_id": record.project_id,
            "state": record.state,
            "connected": connected,
            "enrolled_at_ms": record.enrolled_at_ms,
            "last_seen_at_ms": record.last_seen_at_ms,
            "management": record.management,
        }));
    }
    Ok(Json(json!({ "executors": executors })))
}

#[handler]
pub(super) async fn revoke_executor(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let executor_id = ExecutorId::new(path_parameter(request, "executor_id")?);
    let state = app_state(depot);
    state
        .store
        .revoke_executor(actor(depot), &tenant_id, &executor_id, now_ms()?)
        .await?;
    state
        .edge
        .disconnect(&tenant_id, &executor_id, "this computer was revoked")
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn revoke_owned_executor(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let executor_id = ExecutorId::new(path_parameter(request, "executor_id")?);
    let state = app_state(depot);
    state
        .store
        .revoke_owned_executor(actor(depot), &tenant_id, &executor_id, now_ms()?)
        .await?;
    state
        .edge
        .disconnect(
            &tenant_id,
            &executor_id,
            "this computer was revoked by its owner",
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn create_enrollment(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<CreateEnrollmentRequest>()
        .await
        .map_err(invalid_request)?;
    let enrollment = app_state(depot)
        .store
        .create_computer_enrollment(
            actor(depot),
            &tenant_id,
            body.project_id.as_deref(),
            &body.name,
            false,
            Duration::from_secs(body.ttl_seconds.unwrap_or(600)),
            now_ms()?,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "enrollment": enrollment })),
    ))
}

#[handler]
pub(super) async fn create_owned_enrollment(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<CreateEnrollmentRequest>()
        .await
        .map_err(invalid_request)?;
    let enrollment = app_state(depot)
        .store
        .create_computer_enrollment(
            actor(depot),
            &tenant_id,
            body.project_id.as_deref(),
            &body.name,
            true,
            Duration::from_secs(body.ttl_seconds.unwrap_or(600)),
            now_ms()?,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "enrollment": enrollment })),
    ))
}

#[handler]
pub(super) async fn consume_enrollment(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let body = request
        .parse_json::<ConsumeEnrollmentRequest>()
        .await
        .map_err(invalid_request)?;
    let credential = app_state(depot)
        .store
        .consume_enrollment(&body.token, now_ms()?)
        .await
        .map_err(|error| {
            if error.code == ErrorCode::PolicyDenied {
                ApiError::unauthorized(error)
            } else {
                ApiError::from(error)
            }
        })?;
    Ok(Json(json!({ "credential": credential })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerSuspensionRequest {
    suspended: bool,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerRemovalRequest {
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ComputerRecoveryRequest {
    name: String,
    expected_revision: u64,
}

async fn computer_recovery(
    request: &mut Request,
    depot: &mut Depot,
    owned: bool,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body = request
        .parse_json::<ComputerRecoveryRequest>()
        .await
        .map_err(invalid_request)?;
    let enrollment = app_state(depot)
        .store
        .recover_computer_enrollment(
            actor(depot),
            &tenant,
            &executor,
            &body.name,
            owned,
            body.expected_revision,
            Duration::from_secs(600),
            now_ms()?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"enrollment":enrollment}))))
}

#[handler]
pub(super) async fn recover_owned_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    computer_recovery(request, depot, true).await
}

#[handler]
pub(super) async fn recover_managed_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    computer_recovery(request, depot, false).await
}

async fn computer_details(
    request: &mut Request,
    depot: &mut Depot,
    owned: bool,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let state = app_state(depot);
    let details = state
        .store
        .computer_details(actor(depot), &tenant, &executor, owned)
        .await?;
    let connected = state.edge.is_connected(&tenant, &executor).await;
    Ok(Json(json!({"details":details,"connected":connected})))
}

async fn computer_update(
    request: &mut Request,
    depot: &mut Depot,
    owned: bool,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body = request
        .parse_json::<ternilo_control::ComputerUpdate>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let management = state
        .store
        .update_computer(actor(depot), &tenant, &executor, owned, &body, now_ms()?)
        .await?;
    state.edge.notify_computer_changed(&tenant, &executor);
    Ok(Json(json!({"management":management})))
}

async fn computer_suspension(
    request: &mut Request,
    depot: &mut Depot,
    owned: bool,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body = request
        .parse_json::<ComputerSuspensionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let management = state
        .store
        .set_computer_suspended(
            actor(depot),
            &tenant,
            &executor,
            owned,
            body.suspended,
            body.expected_revision,
            now_ms()?,
        )
        .await?;
    if body.suspended {
        state
            .edge
            .disconnect(
                &tenant,
                &executor,
                "this computer's Server access is suspended",
            )
            .await;
    }
    state.edge.notify_computer_changed(&tenant, &executor);
    Ok(Json(json!({"management":management})))
}

async fn computer_removal(
    request: &mut Request,
    depot: &mut Depot,
    owned: bool,
) -> Result<StatusCode, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(path_parameter(request, "executor_id")?);
    let body = request
        .parse_json::<ComputerRemovalRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .remove_computer_registration(
            actor(depot),
            &tenant,
            &executor,
            owned,
            body.expected_revision,
            now_ms()?,
        )
        .await?;
    state
        .edge
        .disconnect(&tenant, &executor, "this computer registration was removed")
        .await;
    state.edge.notify_computer_changed(&tenant, &executor);
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn get_managed_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_details(request, depot, false).await
}
#[handler]
pub(super) async fn get_owned_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_details(request, depot, true).await
}
#[handler]
pub(super) async fn update_managed_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_update(request, depot, false).await
}
#[handler]
pub(super) async fn update_owned_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_update(request, depot, true).await
}
#[handler]
pub(super) async fn suspend_managed_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_suspension(request, depot, false).await
}
#[handler]
pub(super) async fn suspend_owned_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    computer_suspension(request, depot, true).await
}
#[handler]
pub(super) async fn remove_managed_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    computer_removal(request, depot, false).await
}
#[handler]
pub(super) async fn remove_owned_computer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    computer_removal(request, depot, true).await
}
