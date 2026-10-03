use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};
use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use serde::Deserialize;
use ternilo_control::{
    AccountModelTraffic, ModelTrafficLimits, ModelTrafficPolicy, ModelTrafficPolicyRecord,
    ModelTrafficTargetPage, PageQuery,
};
use ternilo_protocol::UserId;

pub(super) fn router() -> Router {
    Router::with_path("traffic")
        .get(policy)
        .put(update_policy)
        .push(
            Router::with_path("accounts").get(targets).push(
                Router::with_path("{user_id}")
                    .get(account)
                    .put(update_account),
            ),
        )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyInput {
    revision: u64,
    policy: ModelTrafficPolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountInput {
    revision: u64,
    limits: Option<ModelTrafficLimits>,
}

#[handler]
async fn policy(depot: &mut Depot) -> Result<Json<ModelTrafficPolicyRecord>, ApiError> {
    Ok(Json(
        app_state(depot)
            .store
            .model_traffic_policy(actor(depot))
            .await?,
    ))
}
#[handler]
async fn update_policy(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelTrafficPolicyRecord>, ApiError> {
    let input = request
        .parse_json::<PolicyInput>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .update_model_traffic_policy(actor(depot), input.revision, &input.policy, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn targets(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelTrafficTargetPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .model_traffic_targets(actor(depot), &query)
            .await?,
    ))
}
#[handler]
async fn account(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AccountModelTraffic>, ApiError> {
    let target = UserId::new(path_parameter(request, "user_id")?);
    Ok(Json(
        app_state(depot)
            .store
            .account_model_traffic(actor(depot), &target, now_ms()?)
            .await?,
    ))
}
#[handler]
async fn update_account(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let target = UserId::new(path_parameter(request, "user_id")?);
    let input = request
        .parse_json::<AccountInput>()
        .await
        .map_err(invalid_request)?;
    app_state(depot)
        .store
        .update_account_model_traffic(
            actor(depot),
            &target,
            input.revision,
            input.limits.as_ref(),
            now_ms()?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[handler]
pub(super) async fn own(depot: &mut Depot) -> Result<Json<AccountModelTraffic>, ApiError> {
    let user = actor(depot);
    Ok(Json(
        app_state(depot)
            .store
            .account_model_traffic(user, &user.user_id, now_ms()?)
            .await?,
    ))
}
