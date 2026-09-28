use salvo_core::prelude::{Depot, Json, Request, Router, StatusCode, handler};
use ternilo_control::{
    ModelProviderInput, ModelProviderPage, ModelProviderRecord, ModelPublicationInput,
    ModelPublicationPage, ModelPublicationRecord, PageQuery,
};
use ternilo_protocol::HarnessError;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

pub(super) fn providers_router() -> Router {
    Router::with_path("providers")
        .get(list_providers)
        .post(save_provider)
        .push(Router::with_path("discover").post(super::discovery::discover))
        .push(
            Router::with_path("{provider_id}")
                .get(get_provider)
                .put(save_provider)
                .delete(disable_provider)
                .push(super::rotation::router()),
        )
}

pub(super) fn publications_router() -> Router {
    Router::with_path("publications")
        .get(list_publications)
        .post(save_publication)
        .push(
            Router::with_path("{model_id}")
                .get(get_publication)
                .put(save_publication)
                .delete(disable_publication),
        )
}

#[handler]
async fn list_providers(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelProviderPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_providers(actor(depot), &query)
            .await?,
    ))
}

#[handler]
async fn get_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelProviderRecord>, ApiError> {
    let id = path_parameter(request, "provider_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .get_model_provider(actor(depot), &id)
            .await?,
    ))
}

#[handler]
async fn save_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelProviderRecord>, ApiError> {
    let input = request
        .parse_json::<ModelProviderInput>()
        .await
        .map_err(invalid_request)?;
    if request
        .param::<String>("provider_id")
        .is_some_and(|id| id != input.profile.id)
    {
        return Err(HarnessError::invalid("provider ID must match the URL").into());
    }
    Ok(Json(
        app_state(depot)
            .store
            .save_model_provider(actor(depot), &input, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn disable_provider(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "provider_id")?;
    app_state(depot)
        .store
        .disable_model_provider(actor(depot), &id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn list_publications(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelPublicationPage>, ApiError> {
    let query = request
        .parse_queries::<PageQuery>()
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .store
            .list_model_publications(actor(depot), &query)
            .await?,
    ))
}

#[handler]
async fn get_publication(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelPublicationRecord>, ApiError> {
    let id = path_parameter(request, "model_id")?;
    Ok(Json(
        app_state(depot)
            .store
            .get_model_publication(actor(depot), &id)
            .await?,
    ))
}

#[handler]
async fn save_publication(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ModelPublicationRecord>, ApiError> {
    let input = request
        .parse_json::<ModelPublicationInput>()
        .await
        .map_err(invalid_request)?;
    if request
        .param::<String>("model_id")
        .is_some_and(|id| id != input.model_id)
    {
        return Err(HarnessError::invalid("model ID must match the URL").into());
    }
    Ok(Json(
        app_state(depot)
            .store
            .save_model_publication(actor(depot), &input, now_ms()?)
            .await?,
    ))
}

#[handler]
async fn disable_publication(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let id = path_parameter(request, "model_id")?;
    app_state(depot)
        .store
        .disable_model_publication(actor(depot), &id, now_ms()?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
