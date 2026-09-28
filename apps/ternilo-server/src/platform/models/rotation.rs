use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use serde::{Deserialize, Serialize};
use ternilo_control::ModelProviderKeyRotation;
use zeroize::Zeroizing;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

pub(super) fn router() -> Router {
    Router::with_path("key-rotation")
        .get(status)
        .post(begin)
        .push(
            Router::with_path("{rotation_id}")
                .push(Router::with_path("commit").post(commit))
                .push(Router::with_path("rollback").post(rollback)),
        )
}

#[derive(Serialize)]
struct RotationStatus {
    rotation: Option<ModelProviderKeyRotation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginRotation {
    api_key: String,
}

#[handler]
async fn status(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<RotationStatus>, ApiError> {
    let provider_id = path_parameter(request, "provider_id")?;
    Ok(Json(RotationStatus {
        rotation: app_state(depot)
            .store
            .model_provider_key_rotation(actor(depot), &provider_id)
            .await?,
    }))
}

#[handler]
async fn begin(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ModelProviderKeyRotation>), ApiError> {
    let provider_id = path_parameter(request, "provider_id")?;
    let input = request
        .parse_json::<BeginRotation>()
        .await
        .map_err(invalid_request)?;
    let key = Zeroizing::new(input.api_key);
    let rotation = app_state(depot)
        .store
        .begin_model_provider_key_rotation(actor(depot), &provider_id, &key, now_ms()?)
        .await?;
    Ok((StatusCode::CREATED, Json(rotation)))
}

async fn finish(
    request: &Request,
    depot: &Depot,
    restore_previous: bool,
) -> Result<StatusCode, ApiError> {
    app_state(depot)
        .store
        .finish_model_provider_key_rotation(
            actor(depot),
            &path_parameter(request, "provider_id")?,
            &path_parameter(request, "rotation_id")?,
            restore_previous,
            now_ms()?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn commit(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    finish(request, depot, false).await
}

#[handler]
async fn rollback(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    finish(request, depot, true).await
}
