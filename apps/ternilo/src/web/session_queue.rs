use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use ternilo_protocol::{QueueEditRequest, SessionSubmissionRequest, SubmissionId};

use super::{ApiError, app_state, invalid_request, path_parameter};

pub(super) fn router() -> Router {
    Router::with_path("queue").get(inbox).post(submit).push(
        Router::with_path("{submission_id}")
            .patch(edit)
            .delete(remove)
            .push(Router::with_path("steer").post(steer)),
    )
}

#[handler]
async fn inbox(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionInboxSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .session_inbox(&session_id)
            .await?,
    ))
}

#[handler]
async fn submit(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ternilo_protocol::SessionSubmission>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let payload = request
        .parse_json::<SessionSubmissionRequest>()
        .await
        .map_err(invalid_request)?;
    let application = &app_state(depot).shared.application;
    Ok((
        StatusCode::CREATED,
        Json(application.submit_session(&session_id, payload).await?),
    ))
}

#[handler]
async fn edit(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionSubmission>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    let payload = request
        .parse_json::<QueueEditRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .edit_session_queue_item(&session_id, submission_id, payload)
            .await?,
    ))
}

#[handler]
async fn remove(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionSubmission>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .remove_session_queue_item(&session_id, submission_id)
            .await?,
    ))
}

#[handler]
async fn steer(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionSubmission>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let submission_id = SubmissionId::new(path_parameter(request, "submission_id")?);
    let application = &app_state(depot).shared.application;
    Ok(Json(
        application
            .steer_queued_session_item(&session_id, submission_id)
            .await?,
    ))
}
