use super::{
    ApiError, Depot, Deserialize, Duration, Json, ModelUsageReport, Request, StatusCode,
    TenantQuota, Value, actor, app_state, handler, invalid_request, json, now_ms, path_parameter,
    tenant_parameter,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReserveQuotaRequest {
    run_id: Option<String>,
    model_tokens: u64,
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutSecretRequest {
    project_id: Option<String>,
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct ModelUsageQuery {
    period: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct SecretQuery {
    project_id: Option<String>,
}

#[handler]
pub(super) async fn get_quota(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let quota = app_state(depot)
        .store
        .get_quota(actor(depot), &tenant_id)
        .await?;
    Ok(Json(json!({ "quota": quota })))
}

#[handler]
pub(super) async fn update_quota(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let quota = request
        .parse_json::<TenantQuota>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .update_quota(actor(depot), &tenant_id, quota, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn get_model_usage(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelUsageReport>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<ModelUsageQuery>()
        .map_err(invalid_request)?;
    let report = app_state(depot)
        .store
        .model_usage_report(
            actor(depot),
            &tenant_id,
            query.period.as_deref(),
            query.limit.unwrap_or(200),
            now_ms()?,
        )
        .await?;
    Ok(Json(report))
}

#[handler]
pub(super) async fn reserve_quota(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<ReserveQuotaRequest>()
        .await
        .map_err(invalid_request)?;
    let reservation = app_state(depot)
        .store
        .reserve_quota(
            actor(depot),
            &tenant_id,
            body.run_id.as_deref(),
            body.model_tokens,
            Duration::from_secs(body.ttl_seconds.unwrap_or(900)),
            now_ms()?,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "reservation": reservation })),
    ))
}

#[handler]
pub(super) async fn release_quota(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let reservation_id = path_parameter(request, "reservation_id")?;
    app_state(depot)
        .cloud
        .release_unused_quota_reservation(actor(depot), &tenant_id, &reservation_id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn list_secrets(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let secrets = app_state(depot)
        .store
        .list_secrets(actor(depot), &tenant_id)
        .await?;
    Ok(Json(json!({ "secrets": secrets })))
}

#[handler]
pub(super) async fn put_secret(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<PutSecretRequest>()
        .await
        .map_err(invalid_request)?;
    let secret = app_state(depot)
        .store
        .put_secret(
            actor(depot),
            &tenant_id,
            body.project_id.as_deref(),
            &body.name,
            body.value.as_bytes(),
            now_ms()?,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(json!({ "secret": secret }))))
}

#[handler]
pub(super) async fn delete_secret(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let name = path_parameter(request, "name")?;
    let query = request
        .parse_queries::<SecretQuery>()
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .delete_secret(
            actor(depot),
            &tenant_id,
            query.project_id.as_deref(),
            &name,
            now_ms()?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn list_audit(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<AuditQuery>()
        .map_err(invalid_request)?;
    let entries = app_state(depot)
        .store
        .list_audit(actor(depot), &tenant_id, query.limit.unwrap_or(200))
        .await?;
    Ok(Json(json!({ "entries": entries })))
}
