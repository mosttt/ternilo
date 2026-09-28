use super::{
    AgentId, ApiError, AppState, Attachment, CloudRunDraft, CloudRunRecord, CloudStore,
    CompiledRun, ControlStore, ControlUser, Depot, Deserialize, Duration, ErrorCode, HarnessError,
    Json, PermissionPreset, Request, SessionId, StatusCode, TenantId, Value, WorkspaceId,
    WorkspacePlacement, actor, app_state, handler, invalid_request, json, now_ms, path_parameter,
    tenant_parameter, workbench,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitCloudChatRequest {
    project_id: String,
    workspace_id: String,
    session_id: String,
    input: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
    permissions: Option<PermissionPreset>,
    model: ternilo_protocol::DefaultModelSelection,
    reserved_model_tokens: u64,
}

#[derive(Deserialize)]
struct CloudRunListQuery {
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct CloudEventQuery {
    after_seq: Option<u64>,
    limit: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CloudRunWorkspaceResolution {
    Registered,
    SessionBinding,
}

#[handler]
pub(super) async fn submit_cloud_run(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let draft = request
        .parse_json::<CloudRunDraft>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let submitted = submit_draft(state, actor(depot), tenant_id, draft).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "run": submitted }))))
}

#[handler]
pub(super) async fn submit_cloud_chat(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let body = request
        .parse_json::<SubmitCloudChatRequest>()
        .await
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let snapshot = workbench::model_options::resolve_selection(
        state,
        actor(depot),
        &tenant_id,
        &actor(depot).user_id,
        body.model,
    )
    .await?
    .ok_or_else(|| HarnessError::policy("choose an available model before submitting"))?;
    let draft = CloudRunDraft {
        project_id: body.project_id,
        workspace_id: WorkspaceId::new(body.workspace_id),
        agent_id: AgentId::new("cloud-web"),
        session_id: SessionId::new(body.session_id),
        run_id: None,
        limits: state.worker_policy.maximum_limits,
        permissions: body.permissions.unwrap_or(PermissionPreset::WorkspaceWrite),
        mode: ternilo_protocol::SessionMode::Execute,
        profile: ternilo_cloud::cloud_profile(Some(&snapshot)),
        input: body.input,
        references: Vec::new(),
        reference_contexts: Vec::new(),
        attachments: body.attachments,
        reserved_model_tokens: body.reserved_model_tokens,
    };
    let submitted = submit_draft(state, actor(depot), tenant_id, draft).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "run": submitted }))))
}

pub(crate) async fn submit_draft(
    state: &AppState,
    user: &ControlUser,
    tenant_id: TenantId,
    draft: CloudRunDraft,
) -> Result<CloudRunRecord, ApiError> {
    let compiled = compile_draft(state, user, tenant_id.clone(), draft).await?;
    let request = ternilo_protocol::SessionSubmissionRequest {
        delivery: ternilo_protocol::SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: ternilo_protocol::SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: compiled.spec.references.clone(),
        attachments: compiled.spec.attachments.clone(),
    };
    let now = now_ms()?;
    let mut transaction = state
        .store
        .database()
        .tenant_transaction(&tenant_id)
        .await?;
    let reservation = ControlStore::reserve_quota_in(
        &mut transaction,
        &user.user_id,
        &tenant_id,
        Some(compiled.spec.metadata.run_id.as_str()),
        compiled.reserved_model_tokens,
        Duration::from_hours(24),
        now,
    )
    .await?;
    let submitted = CloudStore::enqueue_session_submission_in(
        &mut transaction,
        &compiled,
        &reservation.reservation_id,
        &request,
        now,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(ternilo_storage::database_error)?;
    Ok(submitted.run)
}

pub(crate) async fn compile_draft(
    state: &AppState,
    user: &ControlUser,
    tenant_id: TenantId,
    draft: CloudRunDraft,
) -> Result<CompiledRun, ApiError> {
    require_managed_execution(state)?;
    let existing_session = state
        .cloud
        .find_owned_session(&tenant_id, &user.user_id, &draft.session_id)
        .await?;
    let workspace = match cloud_run_workspace_resolution(
        existing_session
            .as_ref()
            .map(|session| (&session.workspace_id, session.project_id.as_str())),
        &draft.workspace_id,
        &draft.project_id,
    )? {
        CloudRunWorkspaceResolution::Registered => {
            state
                .store
                .resolve_owned_workspace(user, &tenant_id, &draft.workspace_id)
                .await?
        }
        CloudRunWorkspaceResolution::SessionBinding => {
            state
                .store
                .resolve_owned_session_workspace(user, &tenant_id, &draft.workspace_id)
                .await?
        }
    };
    if workspace.placement != WorkspacePlacement::Cloud || workspace.project_id != draft.project_id
    {
        return Err(HarnessError::policy(
            "cloud run workspace must be cloud-placed and belong to the requested project",
        )
        .into());
    }
    state
        .store
        .authorize(user, &tenant_id, ternilo_control::ControlAction::RunReserve)
        .await?;
    let compiled = state.worker_policy.compile_run(
        draft,
        tenant_id.clone(),
        user.user_id.clone(),
        user.user_id.clone(),
        &state.catalog,
    )?;
    state
        .store
        .resolve_extensions(
            user,
            &tenant_id,
            &compiled.spec.profile,
            &state.worker_policy.extension_host_policy,
        )
        .await?;
    Ok(compiled)
}

pub(crate) fn require_managed_execution(state: &AppState) -> Result<(), HarnessError> {
    if state.managed_execution_enabled {
        Ok(())
    } else {
        Err(HarnessError::policy(
            "managed execution is not enabled on this server",
        ))
    }
}

pub(super) fn cloud_run_workspace_resolution(
    existing_session: Option<(&WorkspaceId, &str)>,
    requested_workspace_id: &WorkspaceId,
    requested_project_id: &str,
) -> Result<CloudRunWorkspaceResolution, HarnessError> {
    let Some((session_workspace_id, session_project_id)) = existing_session else {
        return Ok(CloudRunWorkspaceResolution::Registered);
    };
    if session_workspace_id != requested_workspace_id || session_project_id != requested_project_id
    {
        return Err(HarnessError::policy(
            "cloud run must use the existing Session Workspace binding",
        ));
    }
    Ok(CloudRunWorkspaceResolution::SessionBinding)
}

#[handler]
pub(super) async fn list_cloud_runs(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<CloudRunListQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    let candidates = state
        .cloud
        .list_runs(&tenant_id, query.limit.unwrap_or(100))
        .await?;
    let mut runs = Vec::with_capacity(candidates.len());
    for run in candidates {
        if can_read_cloud_run(state, actor(depot), &tenant_id, &run).await? {
            runs.push(run);
        }
    }
    Ok(Json(json!({ "runs": runs })))
}

pub(super) async fn can_read_cloud_run(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    run: &CloudRunRecord,
) -> Result<bool, HarnessError> {
    if run.user_id == user.user_id {
        return Ok(true);
    }
    match state
        .store
        .resource_access(
            user,
            tenant_id,
            ternilo_control::ResourceKind::Session,
            run.session_id.as_str(),
        )
        .await
    {
        Ok(access) => Ok(access.permissions.view),
        // Run records outlive deleted sessions; a deleted resource has no guest access.
        Err(error) if error.code == ErrorCode::InvalidInput => Ok(false),
        Err(error) => Err(error),
    }
}

#[handler]
pub(super) async fn get_cloud_run(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let run_id = ternilo_protocol::RunId::new(path_parameter(request, "run_id")?);
    let state = app_state(depot);
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    let run = state.cloud.get_run(&tenant_id, &run_id).await?;
    if !can_read_cloud_run(state, actor(depot), &tenant_id, &run).await? {
        return Err(HarnessError::policy("run has not been shared for viewing").into());
    }
    Ok(Json(json!({ "run": run })))
}

#[handler]
pub(super) async fn list_cloud_sessions(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let query = request
        .parse_queries::<CloudRunListQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::TenantRead,
        )
        .await?;
    let sessions = state
        .cloud
        .list_accessible_sessions(
            &tenant_id,
            &actor(depot).user_id,
            query.limit.unwrap_or(100),
        )
        .await?;
    Ok(Json(json!({ "sessions": sessions })))
}

#[handler]
pub(super) async fn cancel_cloud_run(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let run_id = ternilo_protocol::RunId::new(path_parameter(request, "run_id")?);
    let state = app_state(depot);
    state
        .store
        .authorize(
            actor(depot),
            &tenant_id,
            ternilo_control::ControlAction::RunReserve,
        )
        .await?;
    let run = state.cloud.get_run(&tenant_id, &run_id).await?;
    let status = if run.user_id == actor(depot).user_id && run.state.terminal() {
        run.state
    } else {
        state
            .cloud
            .cancel_run_as(
                &tenant_id,
                &actor(depot).user_id,
                &run.session_id,
                &run_id,
                now_ms()?,
            )
            .await?
    };
    Ok(Json(json!({ "state": status })))
}

#[handler]
pub(super) async fn cloud_session_events(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Value>, ApiError> {
    let tenant_id = tenant_parameter(request)?;
    let session_id = ternilo_protocol::SessionId::new(path_parameter(request, "session_id")?);
    let query = request
        .parse_queries::<CloudEventQuery>()
        .map_err(invalid_request)?;
    let state = app_state(depot);
    let events = state
        .cloud
        .session_events_as(
            &tenant_id,
            &actor(depot).user_id,
            &session_id,
            query.after_seq,
            query.limit.unwrap_or(200),
        )
        .await?;
    Ok(Json(json!({ "events": events })))
}
