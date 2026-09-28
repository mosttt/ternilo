use super::{
    ApiError, Depot, Deserialize, Duration, ErrorCode, ExecutorId, Json, Request, StatusCode,
    Value, actor, app_state, handler, invalid_request, json, now_ms, path_parameter,
    tenant_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEnrollmentRequest {
    executor_id: String,
    project_id: Option<String>,
    ttl_seconds: Option<u64>,
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
    let records = state.store.list_executors(actor(depot), &tenant_id).await?;
    let mut executors = Vec::with_capacity(records.len());
    for record in records {
        let connected = record.state != "revoked"
            && state
                .edge
                .is_connected(&tenant_id, &record.executor_id)
                .await;
        executors.push(json!({
            "executor_id": record.executor_id,
            "project_id": record.project_id,
            "state": record.state,
            "connected": connected,
            "enrolled_at_ms": record.enrolled_at_ms,
            "last_seen_at_ms": record.last_seen_at_ms,
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
    let records = state
        .store
        .list_owned_executors(actor(depot), &tenant_id)
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
            "project_id": record.project_id,
            "state": record.state,
            "connected": connected,
            "enrolled_at_ms": record.enrolled_at_ms,
            "last_seen_at_ms": record.last_seen_at_ms,
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
        .create_enrollment(
            actor(depot),
            &tenant_id,
            body.project_id.as_deref(),
            ExecutorId::new(body.executor_id),
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
        .create_owned_enrollment(
            actor(depot),
            &tenant_id,
            body.project_id.as_deref(),
            ExecutorId::new(body.executor_id),
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
