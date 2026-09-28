use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use serde::Deserialize;
use ternilo_protocol::{
    AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamSnapshot, AgentTeamTask,
    AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace,
};

use super::{ApiError, app_state, invalid_request, path_parameter};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteTaskQuery {
    expected_revision: u64,
}

pub(super) fn router() -> Router {
    Router::with_path("team")
        .get(snapshot)
        .push(
            Router::with_path("tasks").post(create_task).push(
                Router::with_path("{task_id}")
                    .put(replace_task)
                    .delete(delete_task),
            ),
        )
        .push(
            Router::with_path("messages")
                .post(send_message)
                .push(Router::with_path("{message_id}/read").put(mark_message_read)),
        )
}

#[handler]
async fn snapshot(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentTeamSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .agent_team_snapshot(&session_id)
            .await?,
    ))
}

#[handler]
async fn create_task(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<AgentTeamTask>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let payload = request
        .parse_json::<AgentTeamTaskCreate>()
        .await
        .map_err(invalid_request)?;
    Ok((
        StatusCode::CREATED,
        Json(
            app_state(depot)
                .shared
                .application
                .create_agent_team_task(&session_id, payload)
                .await?,
        ),
    ))
}

#[handler]
async fn replace_task(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentTeamTask>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let task_id = AgentTeamTaskId::new(path_parameter(request, "task_id")?);
    let payload = request
        .parse_json::<AgentTeamTaskReplace>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .replace_agent_team_task(&session_id, task_id, payload)
            .await?,
    ))
}

#[handler]
async fn delete_task(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let task_id = AgentTeamTaskId::new(path_parameter(request, "task_id")?);
    let query = request
        .parse_queries::<DeleteTaskQuery>()
        .map_err(invalid_request)?;
    app_state(depot)
        .shared
        .application
        .delete_agent_team_task(&session_id, task_id, query.expected_revision)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn send_message(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<AgentTeamMessage>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let payload = request
        .parse_json::<AgentTeamMessageSend>()
        .await
        .map_err(invalid_request)?;
    Ok((
        StatusCode::CREATED,
        Json(
            app_state(depot)
                .shared
                .application
                .send_agent_team_message(&session_id, payload)
                .await?,
        ),
    ))
}

#[handler]
async fn mark_message_read(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<AgentTeamMessage>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let message_id = AgentTeamMessageId::new(path_parameter(request, "message_id")?);
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .mark_agent_team_message_read(&session_id, message_id)
            .await?,
    ))
}
