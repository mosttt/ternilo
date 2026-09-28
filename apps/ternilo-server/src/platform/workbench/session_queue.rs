use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use serde_json::Value;
use ternilo_protocol::{QueueEditRequest, SessionSubmissionRequest, SubmissionId};

use crate::platform::{
    http::{ApiError, invalid_request, path_parameter},
    state::{actor, app_state},
};

use super::{
    cloud_adapter::CloudAdapter,
    edge_adapter::EdgeAdapter,
    placement::SessionTarget,
    sessions::{resolve, scope, to_value},
};

pub(super) fn router() -> Router {
    Router::with_path("queue").get(inbox).post(submit).push(
        Router::with_path("{submission_id}")
            .patch(edit)
            .delete(remove)
            .push(Router::with_path("steer").post(steer)),
    )
}

#[handler]
async fn inbox(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .inbox(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .inbox(&session)
                .await?,
        )?,
    };
    Ok(Json(value))
}

#[handler]
async fn submit(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let payload = request
        .parse_json::<SessionSubmissionRequest>()
        .await
        .map_err(invalid_request)?;
    payload.validate()?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .submit_inbox(&session, payload)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .submit_inbox(&session, payload)
                .await?,
        )?,
    };
    Ok((StatusCode::CREATED, Json(value)))
}

#[handler]
async fn edit(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    submission_id.validate()?;
    let payload = request
        .parse_json::<QueueEditRequest>()
        .await
        .map_err(invalid_request)?;
    payload.validate()?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .edit_inbox(&session, submission_id, payload)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .edit_inbox(&session, submission_id, payload)
                .await?,
        )?,
    };
    Ok(Json(value))
}

#[handler]
async fn remove(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    submission_id.validate()?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .remove_inbox(&session, submission_id)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .remove_inbox(&session, submission_id)
                .await?,
        )?,
    };
    Ok(Json(value))
}

#[handler]
async fn steer(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    submission_id.validate()?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .steer_inbox(&session, submission_id)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .steer_inbox(&session, submission_id)
                .await?,
        )?,
    };
    Ok(Json(value))
}
