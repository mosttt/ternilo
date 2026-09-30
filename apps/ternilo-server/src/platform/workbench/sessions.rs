use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, handler},
};
use serde::Serialize;
use serde_json::Value;
use ternilo_control::{ResourceAction, ResourceKind};
use ternilo_protocol::{
    DefaultModelSelection, HarnessError, ReferenceCandidateRequest, RunId, SessionId, SubagentId,
    UserAnswer,
};
use ternilo_transport::ApplicationOperation;

use crate::platform::{
    http::{ApiError, invalid_request, now_ms, path_parameter, tenant_parameter},
    state::{actor, app_state},
};

use super::{
    cloud_adapter::CloudAdapter,
    edge_adapter::EdgeAdapter,
    placement::{PlacementResolver, SessionTarget, WorkspaceTarget},
    types::{
        AnswerQuestionRequest, CommandFeedbackRequest, CreateSessionRequest,
        DefaultModelTargetQuery, FeedbackRequest, ForkSessionRequest, PendingQuestionQuery,
        ResolveAttachmentRequest, SubagentFollowupRequest, TurnRequest, UpdateSessionRequest,
    },
    workspace::load_state,
};

#[handler]
pub(crate) async fn get_default_model(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<DefaultModelTargetQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let resolver = PlacementResolver::new(state, actor(depot), &tenant_id);
    let value = if let Some(session_id) = &query.session_id {
        match resolver.session(session_id).await? {
            SessionTarget::Cloud(_) => to_value(
                state
                    .cloud
                    .resource_default_model(
                        &tenant_id,
                        &actor(depot).user_id,
                        ResourceKind::Session,
                        session_id.as_str(),
                    )
                    .await?,
            )?,
            SessionTarget::Edge(session) => {
                EdgeAdapter::new(state, actor(depot), &tenant_id)
                    .call_session(&session, |_| ApplicationOperation::DefaultModelGet)
                    .await?
            }
        }
    } else if let Some(workspace_id) = &query.workspace_id {
        match resolver.workspace(workspace_id).await? {
            WorkspaceTarget::Cloud(_) => to_value(
                state
                    .cloud
                    .resource_default_model(
                        &tenant_id,
                        &actor(depot).user_id,
                        ResourceKind::Workspace,
                        workspace_id.as_str(),
                    )
                    .await?,
            )?,
            WorkspaceTarget::Edge { executor_id, .. } => {
                state
                    .edge
                    .call(
                        &tenant_id,
                        &executor_id,
                        ApplicationOperation::DefaultModelGet,
                    )
                    .await?
            }
        }
    } else {
        to_value(
            state
                .store
                .user_default_model(actor(depot), &tenant_id)
                .await?,
        )?
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn set_default_model(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<DefaultModelTargetQuery>()
        .map_err(invalid_request)?;
    let selection = request
        .parse_json::<DefaultModelSelection>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let resource = query
        .session_id
        .as_ref()
        .map(|id| (ResourceKind::Session, id.as_str()))
        .or_else(|| {
            query
                .workspace_id
                .as_ref()
                .map(|id| (ResourceKind::Workspace, id.as_str()))
        });
    if let Some((kind, id)) = resource {
        state
            .store
            .resource_access(actor(depot), &tenant_id, kind, id)
            .await?
            .require(ResourceAction::ManageSharing)?;
    }
    let target = PlacementResolver::new(state, actor(depot), &tenant_id)
        .settings(query.session_id.as_ref(), query.workspace_id.as_ref())
        .await?;
    let value = if let Some(value) = target
        .mutate(
            state,
            actor(depot),
            &tenant_id,
            ApplicationOperation::DefaultModelSet {
                selection: selection.clone(),
            },
        )
        .await?
    {
        value
    } else {
        to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .set_default_model(selection)
                .await?,
        )?
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn list_sessions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    Ok(Json(to_value(
        load_state(app_state(depot), actor(depot), &tenant_id)
            .await?
            .sessions,
    )?))
}

#[handler]
pub(crate) async fn list_archived_sessions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    Ok(Json(to_value(
        super::workspace::load_archived_sessions(app_state(depot), actor(depot), &tenant_id)
            .await?,
    )?))
}

#[handler]
pub(crate) async fn reference_candidates(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let query = request
        .parse_queries::<ReferenceCandidateRequest>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .reference_candidates(&session, query)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .reference_candidates(&session, query)
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn pending_questions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<PendingQuestionQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &query.session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            state
                .cloud
                .pending_questions(&tenant_id, &actor(depot).user_id, &session.session_id)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .pending_questions(&session)
                .await?,
        )?,
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn answer_question(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<PendingQuestionQuery>()
        .map_err(invalid_request)?;
    let question_id = path_parameter(request, "question_id")?;
    let body = request
        .parse_json::<AnswerQuestionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    match resolve(state, actor(depot), &tenant_id, &query.session_id).await? {
        SessionTarget::Cloud(session) => {
            state
                .cloud
                .answer_question(
                    &tenant_id,
                    &actor(depot).user_id,
                    &session.session_id,
                    &UserAnswer {
                        question_id,
                        selected: body.selected,
                        custom: body.custom,
                    },
                    now_ms()?,
                )
                .await?;
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session_mutation(&session, |_| ApplicationOperation::AnswerQuestion {
                    answer: UserAnswer {
                        question_id,
                        selected: body.selected,
                        custom: body.custom,
                    },
                })
                .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(crate) async fn create_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<CreateSessionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let target = PlacementResolver::new(state, actor(depot), &tenant_id)
        .workspace(&body.workspace_id)
        .await?;
    let session = match target {
        WorkspaceTarget::Cloud(workspace) => {
            if let Some(requested) = body.session_id.as_deref() {
                PlacementResolver::new(state, actor(depot), &tenant_id)
                    .ensure_session_id_available(&SessionId::new(requested))
                    .await?;
            }
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .create(workspace, body)
                .await?
        }
        WorkspaceTarget::Edge { .. } => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .create(body)
                .await?
        }
    };
    Ok((StatusCode::CREATED, Json(to_value(session)?)))
}

#[handler]
pub(crate) async fn update_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let body = request
        .parse_json::<UpdateSessionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let session = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .update(session, body)
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .update(session, body)
                .await?
        }
    };
    Ok(Json(to_value(session)?))
}

#[handler]
pub(crate) async fn fork_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let payload = request.payload().await.map_err(invalid_request)?;
    let body = if payload.iter().all(u8::is_ascii_whitespace) {
        ForkSessionRequest::default()
    } else {
        serde_json::from_slice(payload).map_err(invalid_request)?
    };
    let state = app_state(depot);
    let child = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(_) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .present(
                    state
                        .cloud
                        .fork_session(
                            &tenant_id,
                            &actor(depot).user_id,
                            &session_id,
                            body.at_seq,
                            now_ms()?,
                        )
                        .await?,
                    cloud_workspace_path(),
                )
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .fork(session, body.at_seq)
                .await?
        }
    };
    Ok((StatusCode::CREATED, Json(to_value(child)?)))
}

#[handler]
pub(crate) async fn archive_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let archived = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(_) => {
            state
                .store
                .resource_access(
                    actor(depot),
                    &tenant_id,
                    ResourceKind::Session,
                    session_id.as_str(),
                )
                .await?
                .require(ResourceAction::Delete)?;
            let now = now_ms()?;
            let runs = state
                .cloud
                .active_session_runs(&tenant_id, &actor(depot).user_id, &session_id)
                .await?;
            if !runs.is_empty() {
                state
                    .cloud
                    .pause_session_inbox(&tenant_id, &actor(depot).user_id, &session_id, None, now)
                    .await?;
                for run_id in runs {
                    state
                        .cloud
                        .cancel_run_as(&tenant_id, &actor(depot).user_id, &session_id, &run_id, now)
                        .await?;
                }
            }
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .present(
                    state
                        .cloud
                        .archive_session(&tenant_id, &actor(depot).user_id, &session_id, now)
                        .await?,
                    cloud_workspace_path(),
                )
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .archive(session)
                .await?
        }
    };
    Ok(Json(to_value(archived)?))
}

#[handler]
pub(crate) async fn restore_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let target = PlacementResolver::new(state, actor(depot), &tenant_id)
        .session_for_deletion(&session_id)
        .await?
        .ok_or_else(|| HarnessError::invalid("session does not exist"))?;
    let restored = match target {
        SessionTarget::Cloud(_) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .present(
                    state
                        .cloud
                        .restore_session(&tenant_id, &actor(depot).user_id, &session_id, now_ms()?)
                        .await?,
                    cloud_workspace_path(),
                )
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .restore(session)
                .await?
        }
    };
    Ok(Json(to_value(restored)?))
}

#[handler]
pub(crate) async fn delete_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    match PlacementResolver::new(state, actor(depot), &tenant_id)
        .session_for_deletion(&session_id)
        .await?
    {
        Some(SessionTarget::Cloud(session)) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .delete(&session)
                .await?;
        }
        Some(SessionTarget::Edge(session)) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .delete(session)
                .await?;
        }
        None => {}
    }
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(crate) async fn session_events(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let events = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .events(&session.session_id)
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .events(&session)
                .await?
        }
    };
    Ok(Json(to_value(events)?))
}

#[handler]
pub(crate) async fn resolve_attachment(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let body = request
        .parse_json::<ResolveAttachmentRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let resolved = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .resolve_attachment(&session, body.attachment)
                .await?
        }
        SessionTarget::Cloud(session) => {
            state
                .cloud
                .resolve_session_attachment_as(
                    &tenant_id,
                    &actor(depot).user_id,
                    &session.session_id,
                    body.attachment,
                )
                .await?
        }
    };
    Ok(Json(to_value(resolved)?))
}

#[handler]
pub(crate) async fn archived_session_events(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut salvo_core::prelude::Response,
) -> Result<Json<Value>, ApiError> {
    response.headers_mut().insert(
        salvo_core::http::header::CACHE_CONTROL,
        "no-store".parse().unwrap(),
    );
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let resolver = PlacementResolver::new(state, actor(depot), &tenant_id);
    let target = resolver.archived_session(&session_id).await?;
    let events = match target {
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .events(&session.session_id)
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .events(&session)
                .await?
        }
    };
    resolver.archived_session(&session_id).await?;
    Ok(Json(to_value(events)?))
}

#[handler]
pub(crate) async fn session_plugins(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(CloudAdapter::profile(&session)?)?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| {
                    ApplicationOperation::SessionPlugins { session_id }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_commands(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .command_catalog(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => to_value(
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .command_catalog(&session)
                .await?,
        )?,
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_stats(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .stats(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| ApplicationOperation::SessionStats {
                    session_id,
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_services(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    state
        .store
        .resource_access(
            actor(depot),
            &tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?
        .require(ResourceAction::View)?;
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .services(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| {
                    ApplicationOperation::SessionServices { session_id }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn start_session_service(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    session_service_control(request, depot, true).await
}

#[handler]
pub(crate) async fn stop_session_service(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    session_service_control(request, depot, false).await
}

async fn session_service_control(
    request: &mut Request,
    depot: &mut Depot,
    start: bool,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let service_id = path_parameter(request, "service_id")?;
    let state = app_state(depot);
    state
        .store
        .resource_access(
            actor(depot),
            &tenant_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await?
        .require(if start {
            ResourceAction::Submit
        } else {
            ResourceAction::Stop
        })?;
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .control_service(&session, service_id, start)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| {
                    if start {
                        ApplicationOperation::SessionServiceStart {
                            session_id,
                            service_id,
                        }
                    } else {
                        ApplicationOperation::SessionServiceStop {
                            session_id,
                            service_id,
                        }
                    }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_projection(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .projection(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| {
                    ApplicationOperation::SessionProjection { session_id }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_telemetry(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .telemetry(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| {
                    ApplicationOperation::SessionTelemetry { session_id }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn session_skills(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .skill_catalog(&session)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session(&session, |session_id| ApplicationOperation::SessionSkills {
                    session_id,
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn export_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .export(session)
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .sanitized_export(&session)
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn record_feedback(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let body = request
        .parse_json::<FeedbackRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(_) => to_value(
            state
                .cloud
                .record_session_feedback(
                    &tenant_id,
                    &actor(depot).user_id,
                    &session_id,
                    body.target_seq,
                    body.expected_revision,
                    body.rating,
                    body.note,
                    now_ms()?,
                )
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session_mutation(&session, |session_id| {
                    ApplicationOperation::SessionFeedback {
                        session_id,
                        target_seq: body.target_seq,
                        expected_revision: body.expected_revision,
                        rating: body.rating,
                        note: body.note,
                    }
                })
                .await?
        }
    };
    Ok((StatusCode::CREATED, Json(value)))
}

#[handler]
pub(crate) async fn record_command_feedback(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let body = request
        .parse_json::<CommandFeedbackRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            state
                .cloud
                .record_command_feedback(
                    &tenant_id,
                    &actor(depot).user_id,
                    &session.session_id,
                    body.text,
                    now_ms()?,
                )
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session_mutation(&session, |session_id| {
                    ApplicationOperation::SessionCommandFeedback {
                        session_id,
                        text: body.text,
                    }
                })
                .await?
        }
    };
    Ok((StatusCode::CREATED, Json(value)))
}

#[handler]
pub(crate) async fn followup_subagent(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let subagent_id = SubagentId::new(path_parameter(request, "subagent_id")?);
    let body = request
        .parse_json::<SubagentFollowupRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .followup_subagent(&session, &subagent_id, body.message)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session_mutation(&session, |session_id| {
                    ApplicationOperation::SessionSubagentFollowup {
                        session_id,
                        subagent_id,
                        message: body.message,
                    }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn interrupt_subagent(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let subagent_id = SubagentId::new(path_parameter(request, "subagent_id")?);
    let state = app_state(depot);
    let value = match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => to_value(
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .interrupt_subagent(&session, &subagent_id)
                .await?,
        )?,
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .call_session_mutation(&session, |session_id| {
                    ApplicationOperation::SessionSubagentInterrupt {
                        session_id,
                        subagent_id,
                    }
                })
                .await?
        }
    };
    Ok(Json(value))
}

#[handler]
pub(crate) async fn run_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    Box::pin(run_turn_inner(request, depot, None)).await
}

#[handler]
pub(crate) async fn run_skill_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let skill = path_parameter(request, "skill_name")?;
    validate_skill(&skill)?;
    Box::pin(run_turn_inner(request, depot, Some(skill))).await
}

async fn run_turn_inner(
    request: &mut Request,
    depot: &mut Depot,
    skill: Option<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let body = request
        .parse_json::<TurnRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => Ok((
            StatusCode::ACCEPTED,
            Json(
                CloudAdapter::new(state, actor(depot), &tenant_id)
                    .turn(session, body, skill)
                    .await?,
            ),
        )),
        SessionTarget::Edge(session) => Ok((
            StatusCode::OK,
            Json(
                EdgeAdapter::new(state, actor(depot), &tenant_id)
                    .turn(&session, body, skill)
                    .await?,
            ),
        )),
    }
}

#[handler]
pub(crate) async fn cancel_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let run_id = RunId::new(path_parameter(request, "run_id")?);
    let state = app_state(depot);
    match resolve(state, actor(depot), &tenant_id, &session_id).await? {
        SessionTarget::Cloud(session) => {
            CloudAdapter::new(state, actor(depot), &tenant_id)
                .cancel(&session, &run_id)
                .await?;
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .cancel(&session, run_id)
                .await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn resolve(
    state: &crate::platform::state::AppState,
    user: &ternilo_control::ControlUser,
    tenant_id: &ternilo_protocol::TenantId,
    session_id: &SessionId,
) -> Result<SessionTarget, HarnessError> {
    PlacementResolver::new(state, user, tenant_id)
        .session(session_id)
        .await
}

pub(super) fn scope(
    request: &mut Request,
) -> Result<(ternilo_protocol::TenantId, SessionId), ApiError> {
    Ok((
        tenant_parameter(request)?,
        SessionId::new(path_parameter(request, "session_id")?),
    ))
}

pub(super) fn to_value(value: impl Serialize) -> Result<Value, HarnessError> {
    serde_json::to_value(value)
        .map_err(|error| HarnessError::execution(format!("encode workbench response: {error}")))
}

fn cloud_workspace_path() -> String {
    "云端 / 已绑定 Workspace".to_owned()
}

fn validate_skill(skill: &str) -> Result<(), HarnessError> {
    if skill.is_empty()
        || skill.len() > 128
        || skill.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        Err(HarnessError::invalid("invalid Skill name"))
    } else {
        Ok(())
    }
}

#[handler]
pub(crate) async fn session_history(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    history_page(request, depot, false).await
}

#[handler]
pub(crate) async fn archived_session_history(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut salvo_core::prelude::Response,
) -> Result<Json<Value>, ApiError> {
    response.headers_mut().insert(
        salvo_core::http::header::CACHE_CONTROL,
        "no-store".parse().unwrap(),
    );
    history_page(request, depot, true).await
}

async fn history_page(
    request: &mut Request,
    depot: &mut Depot,
    archived: bool,
) -> Result<Json<Value>, ApiError> {
    let (tenant_id, session_id) = scope(request)?;
    let query = request
        .parse_queries::<ternilo_protocol::SessionHistoryQuery>()
        .map_err(invalid_request)?;
    query.validate()?;
    let state = app_state(depot);
    let resolver = PlacementResolver::new(state, actor(depot), &tenant_id);
    let target = if archived {
        resolver.archived_session(&session_id).await?
    } else {
        resolver.session(&session_id).await?
    };
    let page = match target {
        SessionTarget::Cloud(session) => {
            state
                .cloud
                .session_history_as(
                    &tenant_id,
                    &actor(depot).user_id,
                    &session.session_id,
                    query,
                )
                .await?
        }
        SessionTarget::Edge(session) => {
            EdgeAdapter::new(state, actor(depot), &tenant_id)
                .history(&session, query)
                .await?
        }
    };
    if archived {
        resolver.archived_session(&session_id).await?;
    }
    Ok(Json(to_value(page)?))
}
