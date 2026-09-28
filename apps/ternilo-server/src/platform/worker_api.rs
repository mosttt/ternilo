//! Worker credentials authorize only lease-scoped operations on canonical server records.
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Json, Request, Response, Router, handler},
};
use salvo_extra::size_limiter::max_size;
use serde::Deserialize;
use ternilo_cloud::{
    CloudStore, StartedRun, WorkerConfiguration, WorkerCredentialGrant, WorkerCredentialRecord,
    WorkerPolicy, WorkerRegisterRequest, WorkerRegistration, WorkerReply, WorkerRequest,
    WorkerRpcRequest, WorkerTeamRequest,
};
use ternilo_control::PlatformAction;
use ternilo_protocol::{HarnessError, Profile, SubmissionReference};
use ternilo_transport::ExecutorId;

use super::{
    auth,
    http::{ApiError, bearer_token, invalid_request, now_ms, path_parameter},
    state::{AppState, actor, app_state},
};

const LEASE_SECONDS: u64 = 30;
const LEASE: Duration = Duration::from_secs(LEASE_SECONDS);
const MAX_WORKER_BODY_BYTES: u64 = 96 * 1024 * 1024;

#[cfg(test)]
mod tests;

pub(crate) fn router() -> Router {
    Router::with_path("internal/worker/v1")
        .hoop(max_size(MAX_WORKER_BODY_BYTES))
        .push(Router::with_path("configuration").get(configuration))
        .push(Router::with_path("register").post(register))
        .push(Router::with_path("rpc").post(rpc))
}

pub(crate) fn management_router() -> Router {
    Router::with_path("api/v1/admin/workers")
        .hoop(auth::user_auth)
        .hoop(super::identity::no_store)
        .hoop(max_size(16 * 1024))
        .get(list_workers)
        .post(create_worker)
        .push(Router::with_path("{worker_id}").delete(revoke_worker))
}

#[handler]
async fn list_workers(depot: &mut Depot) -> Result<Json<Vec<WorkerCredentialRecord>>, ApiError> {
    app_state(depot)
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersRead)
        .await?;
    Ok(Json(app_state(depot).cloud.worker_credentials().await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWorker {
    worker_id: ExecutorId,
    storage_id: Option<String>,
}

#[handler]
async fn create_worker(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<Json<WorkerCredentialGrant>, ApiError> {
    app_state(depot)
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersManage)
        .await?;
    let input: CreateWorker = request.parse_json().await.map_err(invalid_request)?;
    let storage = input
        .storage_id
        .as_deref()
        .unwrap_or(input.worker_id.as_str());
    let grant = app_state(depot)
        .cloud
        .create_worker_credential(&input.worker_id, storage, now_ms()?)
        .await?;
    response.status_code(StatusCode::CREATED);
    Ok(Json(grant))
}

#[handler]
async fn revoke_worker(
    request: &mut Request,
    depot: &mut Depot,
    response: &mut Response,
) -> Result<(), ApiError> {
    app_state(depot)
        .store
        .require_platform_action(actor(depot), PlatformAction::WorkersManage)
        .await?;
    let id = ExecutorId::new(path_parameter(request, "worker_id")?);
    app_state(depot)
        .cloud
        .revoke_worker_credential(&id, now_ms()?)
        .await?;
    response.status_code(StatusCode::NO_CONTENT);
    Ok(())
}

#[handler]
async fn configuration(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkerConfiguration>, ApiError> {
    Ok(Json(
        app_state(depot)
            .cloud
            .worker_configuration(bearer_token(request)?)
            .await
            .map_err(ApiError::unauthorized)?,
    ))
}

#[handler]
async fn register(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<WorkerRegistration>, ApiError> {
    let token = bearer_token(request)?.to_owned();
    let input: WorkerRegisterRequest = request.parse_json().await.map_err(invalid_request)?;
    let state = app_state(depot);
    let identity = state
        .cloud
        .register_authenticated_worker(&token, &input, LEASE, now_ms()?)
        .await?;
    Ok(Json(WorkerRegistration {
        capacity: input.capacity,
        identity,
        storage_id: input.storage_id,
        policy: (*state.worker_policy).clone(),
        lease_seconds: LEASE_SECONDS,
    }))
}

#[handler]
async fn rpc(request: &mut Request, depot: &mut Depot) -> Result<Json<WorkerReply>, ApiError> {
    let token = bearer_token(request)?.to_owned();
    let body: WorkerRpcRequest = request.parse_json().await.map_err(invalid_request)?;
    let state = app_state(depot);
    let store = state
        .cloud
        .authenticated_worker(&token, &body.identity, now_ms()?)
        .await?;
    Ok(Json(
        dispatch(state, store, &body.identity, body.request).await?,
    ))
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep every typed Worker operation and its canonical authorization visible in one dispatcher."
)]
async fn dispatch(
    state: &AppState,
    store: CloudStore,
    identity: &ternilo_cloud::CloudWorkerIdentity,
    request: WorkerRequest,
) -> Result<WorkerReply, HarnessError> {
    let now = now_ms()?;
    let worker = identity.worker_id.as_str();
    match request {
        WorkerRequest::Heartbeat => {
            store.heartbeat_cloud_worker(identity, LEASE, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::ClaimRun => {
            if !state.managed_execution_enabled {
                return Ok(WorkerReply::Claim { claim: None });
            }
            store.reap_expired(now).await?;
            Ok(WorkerReply::Claim {
                claim: store.claim_run(worker, LEASE, now).await?,
            })
        }
        WorkerRequest::WorkspaceRecoveryCandidates { after } => {
            store.reap_expired(now).await?;
            Ok(WorkerReply::WorkspaceRecovery {
                page: store
                    .workspace_recovery_candidates(identity, after.as_ref(), now)
                    .await?,
            })
        }
        WorkerRequest::ConfirmWorkspaceRecovery { ticket } => {
            store
                .confirm_workspace_recovery(identity, &ticket, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::StartRun { run } => {
            let claim = store.authorized_claim(&run, now).await?;
            Ok(WorkerReply::Started {
                run: store.start_run(claim, worker, LEASE, now).await?,
            })
        }
        WorkerRequest::ReleaseClaim { run } => {
            let claim = store.authorized_claim(&run, now).await?;
            store.release_claim(&claim, worker, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::RenewRun { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            store.renew_run(&run, worker, LEASE, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::ParkRun {
            run,
            activity_revision,
            dependencies,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Parked {
                revision: store
                    .park_run(&run, worker, &dependencies, activity_revision, now)
                    .await?,
            })
        }
        WorkerRequest::ResumeRun {
            run,
            activity_revision,
            parked_revision,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Resumed {
                admission: store
                    .resume_run(&run, worker, activity_revision, parked_revision, now)
                    .await?,
            })
        }
        WorkerRequest::ReleaseResident {
            run,
            worker_generation,
        } => {
            // Cleanup can acknowledge an expired execution without restoring its run authority.
            store
                .release_resident(&run, identity, worker_generation, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::CancelRequested { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Flag {
                value: store.cancel_requested(&run, worker).await?,
            })
        }
        WorkerRequest::AppendEvent { run, event } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            store.append_event(&run, worker, &event, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::RecordQuestion { run, question } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            store.record_question(&run, worker, &question, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::QuestionAnswer { run, question_id } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Answer {
                answer: store
                    .question_answer_for_worker(&run, worker, &question_id, now)
                    .await?,
            })
        }
        WorkerRequest::FinishRun {
            run,
            terminal,
            outcome,
            error,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            store
                .finish_run(
                    &run,
                    worker,
                    terminal,
                    outcome.as_ref(),
                    error.as_ref(),
                    now,
                )
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::RunSubmission { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            let mut additional_inputs = store.run_batch_for_worker(worker, &run, now).await?;
            for input in &mut additional_inputs {
                input.reference_contexts =
                    submission_reference_contexts(&store, &run, &input.references).await?;
            }
            Ok(WorkerReply::Submission {
                submission: store.run_submission_for_worker(worker, &run, now).await?,
                additional_inputs,
            })
        }
        WorkerRequest::ReferenceContexts { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::References {
                contexts: reference_contexts(&store, &run).await?,
            })
        }
        WorkerRequest::Extensions { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Extensions {
                extensions: store
                    .extensions_for_run(
                        &run,
                        worker,
                        &state.worker_policy.extension_host_policy,
                        now,
                    )
                    .await?,
            })
        }
        WorkerRequest::ExtensionsActive { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Flag {
                value: store.extensions_active(&run, worker, now).await?,
            })
        }
        WorkerRequest::StoreAttachment {
            run,
            attachment,
            content_base64,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            let bytes = STANDARD
                .decode(content_base64)
                .map_err(|_| HarnessError::invalid("attachment content must be base64"))?;
            store
                .store_attachment_object(&run, worker, &attachment, &bytes, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::DownloadAttachment { run, attachment } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            let content = store
                .attachment_for_worker(&run, worker, &attachment, now)
                .await?;
            Ok(WorkerReply::AttachmentObject {
                content_base64: STANDARD.encode(content),
            })
        }
        WorkerRequest::ClaimCommands => {
            if !state.managed_execution_enabled {
                return Ok(WorkerReply::Commands {
                    commands: Vec::new(),
                });
            }
            store.reap_session_commands(now).await?;
            let registration = store
                .cloud_worker(&identity.worker_id)
                .await?
                .ok_or_else(|| HarnessError::policy("Worker is not registered"))?;
            Ok(WorkerReply::Commands {
                commands: store
                    .claim_session_commands(
                        identity,
                        &registration.hello.capabilities,
                        LEASE,
                        16,
                        now,
                    )
                    .await?,
            })
        }
        WorkerRequest::CompleteCommand { command, outcome } => {
            store
                .complete_authenticated_command(&command, outcome, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::DeferCommand { command } => {
            let (store, command) = store.authorized_command(&command, now).await?;
            store.defer_session_command(identity, &command, now).await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::Inspection { command } => {
            let (store, command) = store.authorized_command(&command, now).await?;
            let session = store
                .inspection_session_for_worker(identity, &command, now)
                .await?;
            if matches!(
                command.command.body,
                ternilo_transport::ExecutorCommandBody::Application {
                    request: ternilo_transport::ApplicationOperation::SessionReferenceCandidates { .. }
                        | ternilo_transport::ApplicationOperation::SessionWorkspace { .. }
                }
            ) {
                return Ok(WorkerReply::Inspection {
                    session,
                    profile: None,
                    extensions: Vec::new(),
                });
            }
            let profile = inspection_profile(&session, &state.worker_policy)?;
            let extensions = store
                .extensions_for_inspection_command(
                    identity,
                    &command,
                    &profile,
                    &state.worker_policy.extension_host_policy,
                    now,
                )
                .await?;
            Ok(WorkerReply::Inspection {
                session,
                profile: Some(ternilo_cloud::cloud_child_profile(profile)),
                extensions,
            })
        }
        WorkerRequest::SteeringSubmission { command } => {
            let (store, command) = store.authorized_command(&command, now).await?;
            let mut submission = store
                .steering_submission_for_worker(identity, &command, now)
                .await?;
            if let Some(submission) = &mut submission {
                let session = store
                    .find_owned_session(&command.tenant_id, &command.user_id, &command.session_id)
                    .await?
                    .ok_or_else(|| HarnessError::policy("steering session is unavailable"))?;
                for attachment in &mut submission.attachments {
                    *attachment = store
                        .resolve_attachment(
                            &command.tenant_id,
                            &session.workspace_id,
                            attachment.clone(),
                        )
                        .await?;
                }
            }
            Ok(WorkerReply::Submission {
                submission,
                additional_inputs: Vec::new(),
            })
        }
        WorkerRequest::CompleteSteering { command, accepted } => {
            let (store, command) = store.authorized_command(&command, now).await?;
            Ok(WorkerReply::Command {
                reply: store
                    .complete_steering_command(identity, &command, accepted, now)
                    .await?,
            })
        }
        WorkerRequest::RequeueSteering { run } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Count {
                count: store.requeue_steering_for_run(identity, &run, now).await?,
            })
        }
        WorkerRequest::CreateSubagent {
            run,
            child_session_id,
            metadata,
            label,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::SessionId {
                session_id: store
                    .create_subagent_for_worker(
                        worker,
                        &run,
                        &child_session_id,
                        &metadata,
                        &label,
                        now,
                    )
                    .await?,
            })
        }
        WorkerRequest::EnqueueSubagent {
            run,
            child_session_id,
            child_run_id,
            input,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            let mut spec = run.claim.spec.clone();
            spec.metadata.session_id = child_session_id.clone();
            spec.metadata.run_id = child_run_id;
            spec.input.clone_from(&input);
            spec.references.clear();
            spec.reference_contexts.clear();
            spec.attachments.clear();
            state.worker_policy.validate(&spec, &state.catalog)?;
            let run_id = store
                .enqueue_subagent_for_worker(
                    worker,
                    &run,
                    &child_session_id,
                    &spec,
                    &input,
                    state.worker_policy.max_run_attempts,
                    now,
                )
                .await?;
            Ok(WorkerReply::SubagentAccepted {
                run: ternilo_protocol::AcceptedSubagentRun {
                    session_id: child_session_id,
                    run_id,
                },
            })
        }
        WorkerRequest::ReadSubagent {
            run,
            child_session_id,
            child_run_id,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Subagent {
                run: store
                    .subagent_run_for_worker(worker, &run, &child_session_id, &child_run_id, now)
                    .await?,
            })
        }
        WorkerRequest::CancelSubagent {
            run,
            child_session_id,
            child_run_id,
        } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::RunState {
                state: store
                    .cancel_subagent_for_worker(worker, &run, &child_session_id, &child_run_id, now)
                    .await?,
            })
        }
        WorkerRequest::AgentTeam { run, request } => {
            let (store, run) = store.authorized_run(&run, now).await?;
            Ok(WorkerReply::Team {
                value: team(&store, &run, request, now).await?,
            })
        }
        WorkerRequest::ClaimTelemetry { lease_ms } => {
            if !(1..=3_600_000).contains(&lease_ms) {
                return Err(HarnessError::invalid(
                    "telemetry lease must be between 1 ms and 1 hour",
                ));
            }
            Ok(WorkerReply::Telemetry {
                occurrences: store
                    .claim_telemetry(identity, Duration::from_millis(lease_ms), 1, now)
                    .await?,
            })
        }
        WorkerRequest::AcknowledgeTelemetry { occurrence } => {
            let occurrence = store.authorized_telemetry(&occurrence).await?;
            store
                .acknowledge_telemetry(identity, &occurrence, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::FailTelemetry { occurrence, error } => {
            let occurrence = store.authorized_telemetry(&occurrence).await?;
            store
                .fail_telemetry(identity, &occurrence, &error, now)
                .await?;
            Ok(WorkerReply::Unit)
        }
        WorkerRequest::Drain => Ok(WorkerReply::Count {
            count: store.drain_cloud_worker(identity, now).await?,
        }),
    }
}

fn inspection_profile(
    session: &ternilo_cloud::CloudSessionRecord,
    policy: &WorkerPolicy,
) -> Result<Profile, HarnessError> {
    let profile = ternilo_kernel::compose_profiles([
        ternilo_cloud::cloud_profile(session.model.as_ref()),
        Profile {
            plugins: session.profile_plugins.clone(),
        },
    ]);
    if ternilo_cloud::profile_model_snapshot(&profile)?.as_ref() != session.model.as_ref() {
        return Err(HarnessError::policy(
            "inspection profile must retain its saved model binding",
        ));
    }
    policy.validate_profile_composition(&profile, &ternilo_cloud::catalog()?)?;
    Ok(profile)
}

async fn reference_contexts(
    store: &CloudStore,
    run: &StartedRun,
) -> Result<Vec<ternilo_protocol::ReferenceContext>, HarnessError> {
    submission_reference_contexts(store, run, &run.claim.spec.references).await
}

async fn submission_reference_contexts(
    store: &CloudStore,
    run: &StartedRun,
    references: &[SubmissionReference],
) -> Result<Vec<ternilo_protocol::ReferenceContext>, HarnessError> {
    let mut result = Vec::new();
    for reference in references {
        let SubmissionReference::Session { session_id, .. } = reference else {
            continue;
        };
        if session_id == &run.claim.session_id {
            return Err(HarnessError::invalid("a Session cannot reference itself"));
        }
        let source = store
            .find_owned_session(
                &run.claim.tenant_id,
                &run.claim.spec.metadata.user_id,
                session_id,
            )
            .await?
            .ok_or_else(|| HarnessError::invalid("referenced Session is unavailable"))?;
        let mut events = Vec::new();
        let mut cursor = None;
        loop {
            let page = store
                .session_events(&run.claim.tenant_id, session_id, cursor, 1000)
                .await?;
            let length = page.len();
            cursor = page.last().map(|event| event.seq);
            events.extend(page);
            if length < 1000 {
                break;
            }
        }
        result.push(ternilo_local::session_reference_context(
            reference,
            &source.title,
            &events,
        )?);
    }
    Ok(ternilo_local::fit_reference_contexts(result))
}

async fn team(
    store: &CloudStore,
    run: &StartedRun,
    request: WorkerTeamRequest,
    now: u64,
) -> Result<serde_json::Value, HarnessError> {
    let tenant = &run.claim.tenant_id;
    let user = &run.claim.spec.metadata.user_id;
    let session = &run.claim.session_id;
    let result = match request {
        WorkerTeamRequest::Snapshot => {
            serde_json::to_value(store.agent_team_snapshot(tenant, user, session).await?)
        }
        WorkerTeamRequest::CreateTask { request } => serde_json::to_value(
            store
                .create_agent_team_task(tenant, user, session, request, now)
                .await?,
        ),
        WorkerTeamRequest::ReplaceTask { task_id, request } => serde_json::to_value(
            store
                .replace_agent_team_task(tenant, user, session, &task_id, request, now)
                .await?,
        ),
        WorkerTeamRequest::DeleteTask {
            task_id,
            expected_revision,
        } => {
            store
                .delete_agent_team_task(tenant, user, session, &task_id, expected_revision)
                .await?;
            Ok(serde_json::Value::Null)
        }
        WorkerTeamRequest::SendMessage { request } => serde_json::to_value(
            store
                .send_agent_team_message(tenant, user, session, request, now)
                .await?,
        ),
        WorkerTeamRequest::ReadMessage { message_id } => serde_json::to_value(
            store
                .mark_agent_team_message_read(tenant, user, session, &message_id, now)
                .await?,
        ),
    };
    result.map_err(|error| HarnessError::execution(format!("encode Agent Team reply: {error}")))
}
