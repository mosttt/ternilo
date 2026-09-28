use super::{
    ApiError, Attachment, Depot, Deserialize, FeedbackRating, Json, LocalSession,
    LocalSessionUpdate, ModelSelection, PendingQuestion, PermissionPreset, PluginEntry, Profile,
    Request, Router, SessionEvent, SessionExport, SessionMode, SessionProjectionSnapshot,
    SessionSearchHit, SessionSearchRequest, SessionStats, SessionTelemetrySharingStatus,
    StatusCode, SubagentId, SubagentSnapshot, TurnResponse, UserAnswer, WorkspaceId, agent_team,
    app_state, handler, invalid_request, path_parameter, reference_candidates, resolve_attachment,
    session_file_content, session_queue, workspace_browser, workspace_browser_info,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSessionRequest {
    #[serde(rename = "workspace_id")]
    workspace: WorkspaceId,
    #[serde(rename = "session_id")]
    session: Option<String>,
    #[serde(rename = "agent_id")]
    agent: Option<String>,
    agent_preset: Option<String>,
    permissions: Option<PermissionPreset>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkSessionRequest {
    at_seq: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnRequest {
    input: String,
    run_id: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSessionRequest {
    title: Option<String>,
    permissions: Option<PermissionPreset>,
    model: Option<ModelSelection>,
    agent_preset: Option<String>,
    profile_plugins: Option<Vec<PluginEntry>>,
    mode: Option<SessionMode>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackRequest {
    target_seq: u64,
    expected_revision: u64,
    rating: Option<FeedbackRating>,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandFeedbackRequest {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubagentFollowupRequest {
    message: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionQuery {
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSearchQuery {
    query: String,
    session_id: Option<ternilo_protocol::SessionId>,
    workspace_id: Option<WorkspaceId>,
    run_id: Option<ternilo_protocol::RunId>,
    category: Option<ternilo_protocol::SessionEventCategory>,
    occurred_after_ms: Option<u64>,
    occurred_before_ms: Option<u64>,
    #[serde(default = "default_session_search_limit")]
    limit: u32,
}

const fn default_session_search_limit() -> u32 {
    20
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerQuestionRequest {
    selected: Vec<String>,
    custom: Option<String>,
}

pub(super) fn session_router() -> Router {
    Router::with_path("sessions")
        .get(list_sessions)
        .post(create_session)
        .push(Router::with_path("archived").get(list_archived_sessions))
        .push(
            Router::with_path("{session_id}")
                .delete(delete_session)
                .patch(update_session)
                .push(Router::with_path("fork").post(fork_session))
                .push(Router::with_path("archive").post(archive_session))
                .push(Router::with_path("restore").post(restore_session))
                .push(Router::with_path("events").get(session_events))
                .push(Router::with_path("archive-events").get(archived_session_events))
                .push(Router::with_path("files/{file_id}/content").get(session_file_content))
                .push(Router::with_path("plugins").get(session_plugins))
                .push(Router::with_path("commands").get(session_commands))
                .push(Router::with_path("services").get(session_services))
                .push(Router::with_path("services/{service_id}/start").post(start_session_service))
                .push(Router::with_path("services/{service_id}/stop").post(stop_session_service))
                .push(Router::with_path("references").get(reference_candidates))
                .push(
                    Router::with_path("workspace")
                        .get(workspace_browser_info)
                        .post(workspace_browser),
                )
                .push(Router::with_path("attachments/resolve").post(resolve_attachment))
                .push(Router::with_path("projection").get(session_projection))
                .push(Router::with_path("telemetry").get(session_telemetry))
                .push(Router::with_path("stats").get(session_stats))
                .push(Router::with_path("export").get(export_session))
                .push(Router::with_path("feedback").post(record_feedback))
                .push(Router::with_path("commands/feedback").post(record_command_feedback))
                .push(
                    Router::with_path("subagents/{subagent_id}")
                        .push(Router::with_path("followup").post(followup_subagent))
                        .push(Router::with_path("interrupt").post(interrupt_subagent)),
                )
                .push(agent_team::router())
                .push(session_queue::router())
                .push(
                    Router::with_path("skills")
                        .get(session_skills)
                        .push(Router::with_path("{skill_name}/turns").post(run_skill_turn)),
                )
                .push(
                    Router::with_path("turns")
                        .post(run_turn)
                        .push(Router::with_path("{run_id}").delete(cancel_turn)),
                ),
        )
}

#[handler]
pub(super) async fn list_sessions(depot: &mut Depot) -> Json<Vec<LocalSession>> {
    let state = app_state(depot);
    Json(state.shared.application.snapshot().await.sessions)
}

#[handler]
pub(super) async fn list_archived_sessions(depot: &mut Depot) -> Json<Vec<LocalSession>> {
    Json(
        app_state(depot)
            .shared
            .application
            .snapshot()
            .await
            .sessions
            .into_iter()
            .filter(|session| session.archived_at_ms.is_some())
            .collect(),
    )
}

#[handler]
pub(super) async fn search_sessions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<SessionSearchHit>>, ApiError> {
    let query = request
        .parse_queries::<SessionSearchQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .search_sessions(SessionSearchRequest {
                query: query.query,
                session_id: query.session_id,
                workspace_id: query.workspace_id,
                filters: ternilo_protocol::SessionSearchFilters {
                    run_id: query.run_id,
                    category: query.category,
                    occurred_after_ms: query.occurred_after_ms,
                    occurred_before_ms: query.occurred_before_ms,
                },
                limit: query.limit,
            })
            .await?,
    ))
}

#[handler]
pub(super) async fn create_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<LocalSession>), ApiError> {
    let request = request
        .parse_json::<CreateSessionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let session = state
        .shared
        .application
        .create_session_with_options(
            request.workspace,
            request.session,
            request.agent,
            request.agent_preset,
            request.permissions,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(session)))
}

#[handler]
pub(super) async fn fork_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<LocalSession>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let payload = request.payload().await.map_err(invalid_request)?;
    let fork = if payload.iter().all(u8::is_ascii_whitespace) {
        ForkSessionRequest::default()
    } else {
        serde_json::from_slice(payload).map_err(invalid_request)?
    };
    let state = app_state(depot);
    let child = state
        .shared
        .application
        .fork_session_at(&session_id, fork.at_seq, None, None)
        .await?;
    Ok((StatusCode::CREATED, Json(child)))
}

#[handler]
pub(super) async fn archive_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<LocalSession>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    let session = state
        .shared
        .application
        .archive_session(&session_id)
        .await?;
    Ok((StatusCode::OK, Json(session)))
}

#[handler]
pub(super) async fn restore_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<LocalSession>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .restore_session(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn run_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<TurnResponse, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let request = request
        .parse_json::<TurnRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    match state
        .shared
        .application
        .run_turn_with_attachments(
            &session_id,
            request.run_id,
            request.input,
            request.attachments,
        )
        .await
    {
        Ok(outcome) => Ok(TurnResponse::Completed(outcome)),
        Err(error) if error.is_cancelled() => Ok(TurnResponse::Cancelled),
        Err(error) => Err(error.into()),
    }
}

#[handler]
pub(super) async fn session_skills(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SkillCatalogSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .skill_catalog(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn run_skill_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<TurnResponse, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let skill_name = path_parameter(request, "skill_name")?;
    let request = request
        .parse_json::<TurnRequest>()
        .await
        .map_err(invalid_request)?;
    match app_state(depot)
        .shared
        .application
        .run_skill_turn(
            &session_id,
            request.run_id,
            skill_name,
            request.input,
            request.attachments,
        )
        .await
    {
        Ok(outcome) => Ok(TurnResponse::Completed(outcome)),
        Err(error) if error.is_cancelled() => Ok(TurnResponse::Cancelled),
        Err(error) => Err(error.into()),
    }
}

#[handler]
pub(super) async fn cancel_turn(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let run_id = path_parameter(request, "run_id")?;
    let state = app_state(depot);
    state
        .shared
        .application
        .cancel_turn(&session_id, &run_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn update_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<LocalSession>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let request = request
        .parse_json::<UpdateSessionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .update_session(
                &session_id,
                LocalSessionUpdate {
                    title: request.title,
                    permissions: request.permissions,
                    model: request.model,
                    server_model: None,
                    agent_preset: request.agent_preset,
                    profile_plugins: request.profile_plugins,
                    mode: request.mode,
                },
            )
            .await?,
    ))
}

#[handler]
pub(super) async fn session_stats(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionStats>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(state.shared.application.stats(&session_id).await?))
}

#[handler]
pub(super) async fn session_plugins(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Profile>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .effective_session_profile(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn session_commands(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionCommandCatalog>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .session_command_catalog(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn session_services(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ternilo_protocol::SessionServiceSnapshot>>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .session_services(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn start_session_service(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionServiceSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let service_id = path_parameter(request, "service_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .start_session_service(&session_id, service_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn stop_session_service(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_protocol::SessionServiceSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let service_id = path_parameter(request, "service_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .stop_session_service(&session_id, service_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn session_projection(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionProjectionSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .session_projection(ternilo_protocol::SessionId::new(session_id))
            .await?,
    ))
}

#[handler]
pub(super) async fn session_telemetry(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionTelemetrySharingStatus>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .session_telemetry_sharing(&session_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn export_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SessionExport>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(
        state.shared.application.export_session(&session_id).await?,
    ))
}

#[handler]
pub(super) async fn record_feedback(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<SessionEvent>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let request = request
        .parse_json::<FeedbackRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let event = state
        .shared
        .application
        .record_feedback(
            &session_id,
            request.target_seq,
            request.expected_revision,
            request.rating,
            request.note,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(event)))
}

#[handler]
pub(super) async fn record_command_feedback(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<ternilo_protocol::SessionCommandReceipt>), ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let payload = request
        .parse_json::<CommandFeedbackRequest>()
        .await
        .map_err(invalid_request)?;
    Ok((
        StatusCode::CREATED,
        Json(
            app_state(depot)
                .shared
                .application
                .record_command_feedback(&session_id, payload.text)
                .await?,
        ),
    ))
}

#[handler]
pub(super) async fn followup_subagent(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SubagentSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let subagent_id = SubagentId::new(path_parameter(request, "subagent_id")?);
    let payload = request
        .parse_json::<SubagentFollowupRequest>()
        .await
        .map_err(invalid_request)?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .followup_subagent(&session_id, subagent_id, payload.message)
            .await?,
    ))
}

#[handler]
pub(super) async fn interrupt_subagent(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<SubagentSnapshot>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let subagent_id = SubagentId::new(path_parameter(request, "subagent_id")?);
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .interrupt_subagent(&session_id, subagent_id)
            .await?,
    ))
}

#[handler]
pub(super) async fn pending_questions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<PendingQuestion>>, ApiError> {
    let query = request
        .parse_queries::<QuestionQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    Ok(Json(
        state
            .shared
            .application
            .pending_questions(query.session_id.as_deref())
            .await,
    ))
}

#[handler]
pub(super) async fn answer_question(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let question_id = path_parameter(request, "question_id")?;
    let request = request
        .parse_json::<AnswerQuestionRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .shared
        .application
        .answer_question(UserAnswer {
            question_id,
            selected: request.selected,
            custom: request.custom,
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
pub(super) async fn session_events(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<SessionEvent>>, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    Ok(Json(state.shared.application.events(&session_id).await?))
}

#[handler]
pub(super) async fn delete_session(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<StatusCode, ApiError> {
    let session_id = path_parameter(request, "session_id")?;
    let state = app_state(depot);
    state.shared.application.delete_session(&session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[handler]
async fn archived_session_events(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut salvo_core::prelude::Response,
) -> Result<Json<Vec<SessionEvent>>, ApiError> {
    response.headers_mut().insert(
        salvo_core::http::header::CACHE_CONTROL,
        "no-store".parse().unwrap(),
    );
    let session_id = path_parameter(request, "session_id")?;
    Ok(Json(
        app_state(depot)
            .shared
            .application
            .archived_events(&session_id)
            .await?,
    ))
}
