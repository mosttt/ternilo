use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};
use salvo_core::prelude::{Depot, Json, Request, handler};
use ternilo_control::{
    ModelUsageReconciliation, ModelUsageReconciliationInput, ModelUsageReconciliationResult,
};

#[handler]
pub(super) async fn records(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ModelUsageReconciliation>>, ApiError> {
    let id = path_parameter(request, "request_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .model_usage_reconciliations(actor(depot), &id)
            .await?,
    ))
}

#[handler]
pub(super) async fn reconcile(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelUsageReconciliationResult>, ApiError> {
    let id = path_parameter(request, "request_id")?;
    let attempt = path_parameter(request, "attempt")?
        .parse::<u32>()
        .map_err(invalid_request)?;
    let input = request
        .parse_json::<ModelUsageReconciliationInput>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .reconcile_model_usage(actor(depot), &id, attempt, &input, now_ms()?)
            .await?,
    ))
}
