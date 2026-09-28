use std::future::Future;

use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Router, handler},
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ternilo_cloud::CloudStore;
use ternilo_protocol::{
    AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamSnapshot, AgentTeamTask,
    AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace, HarnessError, SessionId, TenantId,
    UserId,
};
use ternilo_transport::ApplicationOperation;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter},
    state::{actor, app_state},
};

use super::{
    edge_adapter::EdgeAdapter,
    placement::SessionTarget,
    sessions::{resolve, scope},
};

#[derive(Clone, Copy)]
enum EdgeCallKind {
    Read,
    Mutation,
}

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
async fn snapshot(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    team_call::<AgentTeamSnapshot, _, _>(
        request,
        depot,
        EdgeCallKind::Read,
        |session_id| ApplicationOperation::SessionAgentTeamSnapshot { session_id },
        |cloud, tenant_id, user_id, session_id| async move {
            cloud
                .agent_team_snapshot(&tenant_id, &user_id, &session_id)
                .await
        },
    )
    .await
}

#[handler]
async fn create_task(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = request
        .parse_json::<AgentTeamTaskCreate>()
        .await
        .map_err(invalid_request)?;
    let edge_body = body.clone();
    Ok((
        StatusCode::CREATED,
        team_call::<AgentTeamTask, _, _>(
            request,
            depot,
            EdgeCallKind::Mutation,
            |session_id| ApplicationOperation::SessionAgentTeamTaskCreate {
                session_id,
                request: edge_body,
            },
            move |cloud, tenant_id, user_id, session_id| async move {
                cloud
                    .create_agent_team_task(&tenant_id, &user_id, &session_id, body, now_ms()?)
                    .await
            },
        )
        .await?,
    ))
}

#[handler]
async fn replace_task(request: &mut Request, depot: &mut Depot) -> Result<Json<Value>, ApiError> {
    let task_id = AgentTeamTaskId::new(path_parameter(request, "task_id")?);
    let body = request
        .parse_json::<AgentTeamTaskReplace>()
        .await
        .map_err(invalid_request)?;
    let edge_task_id = task_id.clone();
    let edge_body = body.clone();
    team_call::<AgentTeamTask, _, _>(
        request,
        depot,
        EdgeCallKind::Mutation,
        |session_id| ApplicationOperation::SessionAgentTeamTaskReplace {
            session_id,
            task_id: edge_task_id,
            request: edge_body,
        },
        move |cloud, tenant_id, user_id, session_id| async move {
            cloud
                .replace_agent_team_task(
                    &tenant_id,
                    &user_id,
                    &session_id,
                    &task_id,
                    body,
                    now_ms()?,
                )
                .await
        },
    )
    .await
}

#[handler]
async fn delete_task(request: &mut Request, depot: &mut Depot) -> Result<StatusCode, ApiError> {
    let task_id = AgentTeamTaskId::new(path_parameter(request, "task_id")?);
    let query = request
        .parse_queries::<DeleteTaskQuery>()
        .map_err(invalid_request)?;
    let edge_task_id = task_id.clone();
    team_call::<(), _, _>(
        request,
        depot,
        EdgeCallKind::Mutation,
        |session_id| ApplicationOperation::SessionAgentTeamTaskDelete {
            session_id,
            task_id: edge_task_id,
            expected_revision: query.expected_revision,
        },
        move |cloud, tenant_id, user_id, session_id| async move {
            cloud
                .delete_agent_team_task(
                    &tenant_id,
                    &user_id,
                    &session_id,
                    &task_id,
                    query.expected_revision,
                )
                .await
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn send_message(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = request
        .parse_json::<AgentTeamMessageSend>()
        .await
        .map_err(invalid_request)?;
    let edge_body = body.clone();
    Ok((
        StatusCode::CREATED,
        team_call::<AgentTeamMessage, _, _>(
            request,
            depot,
            EdgeCallKind::Mutation,
            |session_id| ApplicationOperation::SessionAgentTeamMessageSend {
                session_id,
                request: edge_body,
            },
            move |cloud, tenant_id, user_id, session_id| async move {
                cloud
                    .send_agent_team_message(&tenant_id, &user_id, &session_id, body, now_ms()?)
                    .await
            },
        )
        .await?,
    ))
}

#[handler]
async fn mark_message_read(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let message_id = AgentTeamMessageId::new(path_parameter(request, "message_id")?);
    let edge_message_id = message_id.clone();
    team_call::<AgentTeamMessage, _, _>(
        request,
        depot,
        EdgeCallKind::Mutation,
        |session_id| ApplicationOperation::SessionAgentTeamMessageRead {
            session_id,
            message_id: edge_message_id,
        },
        move |cloud, tenant_id, user_id, session_id| async move {
            cloud
                .mark_agent_team_message_read(
                    &tenant_id,
                    &user_id,
                    &session_id,
                    &message_id,
                    now_ms()?,
                )
                .await
        },
    )
    .await
}

async fn team_call<T, CloudCall, CloudFuture>(
    request: &mut Request,
    depot: &mut Depot,
    edge_call_kind: EdgeCallKind,
    operation: impl FnOnce(SessionId) -> ApplicationOperation,
    cloud_call: CloudCall,
) -> Result<Json<Value>, ApiError>
where
    T: Serialize,
    CloudCall: FnOnce(CloudStore, TenantId, UserId, SessionId) -> CloudFuture,
    CloudFuture: Future<Output = Result<T, HarnessError>>,
{
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Edge(session) => {
            let adapter = EdgeAdapter::new(state, actor(depot), &tenant_id);
            let value = match edge_call_kind {
                EdgeCallKind::Read => adapter.call_session(&session, operation).await?,
                EdgeCallKind::Mutation => {
                    adapter.call_session_mutation(&session, operation).await?
                }
            };
            Ok(Json(value))
        }
        SessionTarget::Cloud(_) => Ok(Json(
            serde_json::to_value(
                cloud_call(
                    state.cloud.clone(),
                    tenant_id,
                    actor(depot).user_id.clone(),
                    session_id,
                )
                .await?,
            )
            .map_err(|error| {
                HarnessError::execution(format!("encode cloud Agent Team response: {error}"))
            })?,
        )),
    }
}
