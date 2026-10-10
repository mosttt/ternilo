use std::{collections::BTreeMap, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ternilo_cloud::{
    CloudCommandDelivery, CloudRunDraft, CloudSessionCommandDraft, CloudSessionDraft,
    CloudSessionRecord, CloudSessionUpdate,
};
use ternilo_control::{ControlUser, ResourceAction, ResourceKind};
use ternilo_protocol::{
    AgentId, DefaultModelSelection, HarnessError, PermissionPreset, PreparedSkillInvocation,
    Profile, QueueEditRequest, ReferenceCandidate, ReferenceCandidateRequest,
    ReferenceCandidateSnapshot, RunId, RunModelSnapshot, SessionCommandCatalog, SessionEvent,
    SessionId, SessionInboxSnapshot, SessionMode, SessionProjectionSnapshot, SessionStats,
    SessionSubmission, SessionSubmissionRequest, SessionTelemetrySharingStatus,
    SkillCatalogSnapshot, SubagentId, SubagentSnapshot, SubagentStatus, SubmissionContent,
    SubmissionDelivery, SubmissionId, SubmissionPlacement, TenantId,
};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandOutcome, ExecutorCapability, ExecutorCommand,
    ExecutorCommandBody, ExecutorScope,
};

use crate::platform::{http::now_ms, state::AppState};

use super::types::{CreateSessionRequest, TurnRequest, UpdateSessionRequest, WorkbenchSession};

pub(crate) struct CloudAdapter<'a> {
    state: &'a AppState,
    actor: &'a ControlUser,
    tenant_id: &'a TenantId,
}

impl<'a> CloudAdapter<'a> {
    pub(crate) fn new(
        state: &'a AppState,
        actor: &'a ControlUser,
        tenant_id: &'a TenantId,
    ) -> Self {
        Self {
            state,
            actor,
            tenant_id,
        }
    }

    pub(crate) async fn create(
        &self,
        workspace: ternilo_control::WorkspaceRecord,
        request: CreateSessionRequest,
    ) -> Result<WorkbenchSession, HarnessError> {
        crate::platform::require_managed_execution(self.state)?;
        let default_model = self
            .state
            .cloud
            .resource_default_model(
                self.tenant_id,
                &self.actor.user_id,
                ResourceKind::Workspace,
                workspace.workspace_id.as_str(),
            )
            .await?;
        let model = self
            .model_update_value(
                default_model,
                Some((ResourceKind::Workspace, workspace.workspace_id.as_str())),
            )
            .await?;
        let preset_id = request
            .agent_preset
            .unwrap_or_else(|| "standard".to_owned());
        let preset = self
            .state
            .cloud
            .resource_agent_preset(
                self.tenant_id,
                &self.actor.user_id,
                ResourceKind::Workspace,
                workspace.workspace_id.as_str(),
                &preset_id,
            )
            .await?;
        let profile_plugins = preset.profile.plugins;
        self.validate_profile_parts(model.as_ref(), &profile_plugins)
            .await?;
        let session = self
            .state
            .cloud
            .create_session(
                CloudSessionDraft {
                    project_id: workspace.project_id,
                    workspace_id: workspace.workspace_id,
                    session_id: request.session_id.map(SessionId::new),
                    agent_id: AgentId::new(
                        request.agent_id.unwrap_or_else(|| "cloud-web".to_owned()),
                    ),
                    title: "New session".to_owned(),
                    permissions: request
                        .permissions
                        .unwrap_or(PermissionPreset::WorkspaceWrite),
                    model,
                    reserved_model_tokens: 32_768,
                    agent_preset: preset_id,
                    profile_plugins,
                    mode: SessionMode::Execute,
                },
                self.tenant_id,
                &self.actor.user_id,
                now_ms()?,
            )
            .await?;
        self.present(session, format!("云端 / {}", workspace.name))
            .await
    }

    pub(crate) async fn update(
        &self,
        mut candidate: CloudSessionRecord,
        mut update: UpdateSessionRequest,
    ) -> Result<WorkbenchSession, HarnessError> {
        if !update.has_changes() {
            return Err(HarnessError::invalid("session update has no fields"));
        }
        let model_update = self
            .model_update(
                update.model.as_ref(),
                Some((ResourceKind::Session, candidate.session_id.as_str())),
            )
            .await?;
        let model = model_update;
        if let Some(value) = update.title.as_ref() {
            candidate.title.clone_from(value);
        }
        if let Some(value) = update.permissions {
            candidate.permissions = value;
        }
        if let Some(value) = model.as_ref() {
            candidate.model.clone_from(value);
        }
        if let Some(value) = update.model_token_limit {
            if value == 0 || i64::try_from(value).is_err() {
                return Err(HarnessError::invalid(
                    "task token limit must be a positive integer within the supported range",
                ));
            }
            candidate.reserved_model_tokens = value;
        }
        if let Some(value) = update.agent_preset.as_ref() {
            let preset = self
                .state
                .cloud
                .resource_agent_preset(
                    self.tenant_id,
                    &self.actor.user_id,
                    ResourceKind::Session,
                    candidate.session_id.as_str(),
                    value,
                )
                .await?;
            if update.profile_plugins.is_none() {
                update.profile_plugins = Some(preset.profile.plugins);
            }
            candidate.agent_preset.clone_from(value);
        }
        if let Some(value) = update.profile_plugins.as_ref() {
            candidate.profile_plugins.clone_from(value);
        }
        if let Some(value) = update.mode {
            candidate.mode = value;
        }
        self.validate_profile(&candidate).await?;
        let session = self
            .state
            .cloud
            .update_session(
                self.tenant_id,
                &candidate.session_id,
                &self.actor.user_id,
                CloudSessionUpdate {
                    title: update.title,
                    permissions: update.permissions,
                    model,
                    reserved_model_tokens: update.model_token_limit,
                    agent_preset: update.agent_preset,
                    profile_plugins: update.profile_plugins,
                    mode: update.mode,
                },
                now_ms()?,
            )
            .await?;
        self.present(session, cloud_workspace_path()).await
    }

    pub(crate) async fn present(
        &self,
        session: CloudSessionRecord,
        workspace_path: String,
    ) -> Result<WorkbenchSession, HarnessError> {
        let access = self
            .state
            .store
            .resource_access(
                self.actor,
                self.tenant_id,
                ResourceKind::Session,
                session.session_id.as_str(),
            )
            .await?;
        access.require(ResourceAction::View)?;
        Ok(WorkbenchSession::cloud(session, workspace_path).with_access(access))
    }

    pub(crate) async fn set_default_model(
        &self,
        selection: DefaultModelSelection,
    ) -> Result<DefaultModelSelection, HarnessError> {
        if matches!(selection, DefaultModelSelection::ProfileDefault) {
            return Err(HarnessError::invalid(
                "choose an explicit model as the managed default",
            ));
        }
        self.model_update_value(selection.clone(), None).await?;
        self.state
            .store
            .set_user_default_model(self.actor, self.tenant_id, selection, now_ms()?)
            .await
    }

    pub(crate) async fn delete(&self, session: &CloudSessionRecord) -> Result<(), HarnessError> {
        self.state
            .cloud
            .delete_session(
                self.tenant_id,
                &session.session_id,
                &self.actor.user_id,
                now_ms()?,
            )
            .await
    }

    pub(crate) async fn events(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        let mut events = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .state
                .cloud
                .session_events_as(
                    self.tenant_id,
                    &self.actor.user_id,
                    session_id,
                    cursor,
                    1_000,
                )
                .await?;
            let page_len = page.len();
            cursor = page.last().map(|event| event.seq);
            events.extend(page);
            if page_len < 1_000 {
                return Ok(events);
            }
        }
    }

    pub(crate) async fn reference_candidates(
        &self,
        session: &CloudSessionRecord,
        request: ReferenceCandidateRequest,
    ) -> Result<ReferenceCandidateSnapshot, HarnessError> {
        let mut snapshot: ReferenceCandidateSnapshot = self
            .worker_session_operation(
                session,
                ApplicationOperation::SessionReferenceCandidates {
                    session_id: session.session_id.clone(),
                    request: request.clone(),
                },
                ExecutorCapability::WorkspaceFiles,
            )
            .await?;
        if !request.directory.is_empty() {
            return Ok(snapshot);
        }
        let needle = request.query.to_lowercase();
        let mut sessions = self
            .state
            .cloud
            .list_accessible_sessions(self.tenant_id, &self.actor.user_id, 500)
            .await?
            .into_iter()
            .filter(|candidate| {
                candidate.session_id != session.session_id
                    && (needle.is_empty()
                        || candidate.title.to_lowercase().contains(&needle)
                        || candidate
                            .session_id
                            .as_str()
                            .to_lowercase()
                            .contains(&needle))
            })
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            let left_workspace = left.workspace_id == session.workspace_id;
            let right_workspace = right.workspace_id == session.workspace_id;
            right_workspace
                .cmp(&left_workspace)
                .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
        });
        snapshot
            .candidates
            .extend(
                sessions
                    .into_iter()
                    .take(20)
                    .map(|candidate| ReferenceCandidate::Session {
                        session_id: candidate.session_id,
                        label: candidate.title,
                        workspace: candidate.workspace_id.as_str().to_owned(),
                        same_workspace: candidate.workspace_id == session.workspace_id,
                        updated_at_ms: candidate.updated_at_ms,
                    }),
            );
        Ok(snapshot)
    }

    pub(crate) fn profile(session: &CloudSessionRecord) -> Result<Profile, HarnessError> {
        compose_model_profile(session.model.as_ref(), &session.profile_plugins)
    }

    pub(crate) async fn command_catalog(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SessionCommandCatalog, HarnessError> {
        self.worker_session_operation(
            session,
            ApplicationOperation::SessionCommands {
                session_id: session.session_id.clone(),
            },
            ExecutorCapability::AddressedSessionCommands,
        )
        .await
    }

    pub(crate) async fn skill_catalog(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SkillCatalogSnapshot, HarnessError> {
        self.worker_session_operation(
            session,
            ApplicationOperation::SessionSkills {
                session_id: session.session_id.clone(),
            },
            ExecutorCapability::Skills,
        )
        .await
    }

    pub(crate) async fn services(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<Vec<ternilo_protocol::SessionServiceSnapshot>, HarnessError> {
        if self
            .state
            .cloud
            .session_runtime_target(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                ternilo_control::ResourceAction::View,
                now_ms()?,
            )
            .await?
            .is_none()
        {
            return Ok(Vec::new());
        }
        self.worker_session_operation(
            session,
            ApplicationOperation::SessionServices {
                session_id: session.session_id.clone(),
            },
            ExecutorCapability::AddressedSessionCommands,
        )
        .await
    }

    pub(crate) async fn control_service(
        &self,
        session: &CloudSessionRecord,
        service_id: String,
        start: bool,
    ) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError> {
        let request = if start {
            ApplicationOperation::SessionServiceStart {
                session_id: session.session_id.clone(),
                service_id,
            }
        } else {
            ApplicationOperation::SessionServiceStop {
                session_id: session.session_id.clone(),
                service_id,
            }
        };
        self.worker_session_operation(
            session,
            request,
            ExecutorCapability::AddressedSessionCommands,
        )
        .await
    }

    async fn prepare_skill(
        &self,
        session: &CloudSessionRecord,
        name: String,
        input: String,
    ) -> Result<PreparedSkillInvocation, HarnessError> {
        self.worker_session_operation(
            session,
            ApplicationOperation::SessionSkillResolve {
                session_id: session.session_id.clone(),
                name,
                input,
            },
            ExecutorCapability::Skills,
        )
        .await
    }

    pub(crate) async fn workspace_browser(
        &self,
        session: &CloudSessionRecord,
        request: ternilo_protocol::WorkspaceRequest,
    ) -> Result<Value, HarnessError> {
        self.worker_session_operation(
            session,
            ApplicationOperation::SessionWorkspace {
                session_id: session.session_id.clone(),
                request,
            },
            ExecutorCapability::WorkspaceFiles,
        )
        .await
    }

    async fn worker_session_operation<T: DeserializeOwned>(
        &self,
        session: &CloudSessionRecord,
        request: ApplicationOperation,
        required_capability: ExecutorCapability,
    ) -> Result<T, HarnessError> {
        crate::platform::require_managed_execution(self.state)?;
        let service_operation = matches!(
            request,
            ApplicationOperation::SessionServices { .. }
                | ApplicationOperation::SessionServiceStart { .. }
                | ApplicationOperation::SessionServiceStop { .. }
        );
        if !matches!(
            request,
            ApplicationOperation::SessionReferenceCandidates { .. }
                | ApplicationOperation::SessionWorkspace { .. }
                | ApplicationOperation::SessionServices { .. }
                | ApplicationOperation::SessionServiceStart { .. }
                | ApplicationOperation::SessionServiceStop { .. }
        ) {
            self.validate_profile(session).await?;
        }
        request.validate()?;
        let issued_at_ms = now_ms()?;
        let delivery = self
            .worker_command_delivery(session, &request, issued_at_ms)
            .await?;
        let expires_at_ms = issued_at_ms
            .checked_add(if service_operation { 30_000 } else { 15_000 })
            .ok_or_else(|| HarnessError::invalid("cloud inspection expiry exceeds u64"))?;
        let command_id = CommandId::new(format!(
            "inspect_{}",
            URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
        ));
        self.state
            .cloud
            .enqueue_session_command(
                self.tenant_id,
                &self.actor.user_id,
                &CloudSessionCommandDraft {
                    session_id: session.session_id.clone(),
                    command: ExecutorCommand {
                        input_authorization: None,
                        input_provenance: None,
                        command_id: command_id.clone(),
                        scope: ExecutorScope {
                            tenant_id: self.tenant_id.clone(),
                            user_id: session.user_id.clone(),
                        },
                        issued_at_ms,
                        expires_at_ms,
                        body: ExecutorCommandBody::Application { request },
                    },
                    required_capability,
                    required_catalog_revision: (!service_operation)
                        .then(|| self.state.catalog.revision().to_owned()),
                    delivery,
                },
                issued_at_ms,
            )
            .await?;
        let reply = self
            .state
            .cloud
            .wait_for_session_command_reply_as(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                &command_id,
                if service_operation {
                    Duration::from_secs(25)
                } else {
                    Duration::from_millis(expires_at_ms.saturating_sub(now_ms()?))
                },
                Duration::from_millis(40),
            )
            .await?
            .ok_or_else(|| {
                HarnessError::unavailable(format!(
                    "cloud Worker did not finish the {required_capability:?} request before its deadline; check its connection and load"
                ))
            })?;
        match reply.outcome {
            CommandOutcome::Ok { value } => serde_json::from_value(value).map_err(|error| {
                HarnessError::execution(format!("decode cloud inspection reply: {error}"))
            }),
            CommandOutcome::Error { error } => Err(error),
        }
    }

    async fn worker_command_delivery(
        &self,
        session: &CloudSessionRecord,
        request: &ApplicationOperation,
        now: u64,
    ) -> Result<CloudCommandDelivery, HarnessError> {
        let action = match request {
            ApplicationOperation::SessionServiceStart { .. } => {
                ternilo_control::ResourceAction::Submit
            }
            ApplicationOperation::SessionServiceStop { .. } => {
                ternilo_control::ResourceAction::Stop
            }
            _ => return Ok(CloudCommandDelivery::ReadOnly),
        };
        self.state
            .cloud
            .session_runtime_target(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                action,
                now,
            )
            .await?
            .ok_or_else(|| HarnessError::conflict("cloud session has no active runtime to control"))
    }

    pub(crate) async fn stats(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SessionStats, HarnessError> {
        self.state
            .cloud
            .session_stats_as(self.tenant_id, &self.actor.user_id, &session.session_id)
            .await
    }

    pub(crate) fn stats_from_events(events: &[SessionEvent]) -> Result<SessionStats, HarnessError> {
        ternilo_builtins::session_stats(events)
    }

    pub(crate) async fn projection(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SessionProjectionSnapshot, HarnessError> {
        let events = self.events(&session.session_id).await?;
        self.projection_from_events(session, &events, Self::profile(session)?)
    }

    pub(crate) fn projection_from_events(
        &self,
        session: &CloudSessionRecord,
        events: &[SessionEvent],
        profile: Profile,
    ) -> Result<SessionProjectionSnapshot, HarnessError> {
        let profile = control_projection_profile(profile);
        let active = ternilo_protocol::conversation_events(events);
        let mut values = BTreeMap::new();
        for unit in self.state.catalog.projection_units(&profile)? {
            let mut projection_state = unit.initial();
            for event in active.iter() {
                unit.apply(&mut projection_state, event)?;
            }
            values.insert(unit.key().to_owned(), unit.view(&projection_state)?);
        }
        Ok(SessionProjectionSnapshot {
            session_id: session.session_id.clone(),
            as_of_seq: events.last().map(|event| event.seq),
            values,
        })
    }

    pub(crate) async fn telemetry(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SessionTelemetrySharingStatus, HarnessError> {
        Ok(self
            .state
            .cloud
            .session_telemetry(self.tenant_id, &self.actor.user_id, &session.session_id)
            .await?
            .sharing)
    }

    pub(crate) async fn export(&self, session: CloudSessionRecord) -> Result<Value, HarnessError> {
        let events = self.events(&session.session_id).await?;
        Ok(json!({
            "schema_version": 1,
            "session": self.present(session, cloud_workspace_path()).await?,
            "events": events,
        }))
    }

    pub(crate) async fn inbox(
        &self,
        session: &CloudSessionRecord,
    ) -> Result<SessionInboxSnapshot, HarnessError> {
        self.state
            .cloud
            .session_inbox(self.tenant_id, &self.actor.user_id, &session.session_id)
            .await
    }

    pub(crate) async fn submit_inbox(
        &self,
        session: &CloudSessionRecord,
        request: SessionSubmissionRequest,
    ) -> Result<SessionSubmission, crate::platform::http::ApiError> {
        let delivery = request.delivery;
        if delivery == SubmissionDelivery::Steer {
            self.state
                .store
                .resource_access(
                    self.actor,
                    self.tenant_id,
                    ternilo_control::ResourceKind::Session,
                    session.session_id.as_str(),
                )
                .await?
                .require(ternilo_control::ResourceAction::Stop)?;
        }
        let submission = self.enqueue_submission(session, request).await?.submission;
        if should_begin_steering(delivery, submission.placement) {
            return self
                .steer_inbox(session, submission.id.clone())
                .await
                .map_err(Into::into);
        }
        Ok(submission)
    }

    async fn enqueue_submission(
        &self,
        session: &CloudSessionRecord,
        request: SessionSubmissionRequest,
    ) -> Result<ternilo_cloud::CloudSubmissionReceipt, crate::platform::http::ApiError> {
        request.validate()?;
        self.validate_profile(session).await?;
        let execution_input = self
            .submission_execution_input(session, &request.content)
            .await?;
        let draft = self.submission_draft(session, &request, execution_input)?;
        let compiled = self.compile_submission(session, draft).await?;
        Ok(self
            .state
            .cloud
            .enqueue_session_submission_as(&self.actor.user_id, &compiled, &request, now_ms()?)
            .await?)
    }

    pub(crate) async fn edit_inbox(
        &self,
        session: &CloudSessionRecord,
        submission_id: SubmissionId,
        edit: QueueEditRequest,
    ) -> Result<SessionSubmission, crate::platform::http::ApiError> {
        edit.validate()?;
        let current = self
            .inbox(session)
            .await?
            .items
            .into_iter()
            .find(|item| item.id == submission_id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown submission {submission_id}")))?;
        let mut content = current.content;
        match &mut content {
            SubmissionContent::Prompt { input }
            | SubmissionContent::Skill { input, .. }
            | SubmissionContent::Regenerate { input, .. } => input.clone_from(&edit.input),
        }
        let replacement = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(current.run_id),
            content,
            references: current.references,
            attachments: current.attachments,
        };
        replacement.validate()?;
        let original = self
            .state
            .cloud
            .queued_submission_run(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                &submission_id,
            )
            .await?;
        let mut frozen = session.clone();
        frozen.model = ternilo_cloud::profile_model_snapshot(&original.spec.profile)?;
        frozen.profile_plugins = original.spec.profile.plugins.clone();
        frozen.reserved_model_tokens = original.reserved_model_tokens;
        frozen.permissions = original.spec.permissions;
        frozen.mode = original.spec.mode;
        let execution_input = self
            .submission_execution_input(&frozen, &replacement.content)
            .await?;
        let mut compiled = original;
        compiled.actor_user_id = self.actor.user_id.clone();
        compiled.authorization_session_id = session.session_id.clone();
        compiled.spec.input = execution_input;
        compiled.spec.references = replacement.references;
        compiled.spec.attachments = replacement.attachments;
        self.state
            .worker_policy
            .validate(&compiled.spec, &self.state.catalog)?;
        if let Some(model) = &frozen.model {
            super::model_options::managed::current(
                self.state,
                self.actor,
                self.tenant_id,
                ResourceKind::Session,
                session.session_id.as_str(),
                model,
            )
            .await?;
            validate_task_model_budget(
                model,
                &compiled.spec.input,
                &compiled.spec.attachments,
                compiled.reserved_model_tokens,
            )?;
        }
        self.state
            .store
            .resolve_extensions(
                self.actor,
                self.tenant_id,
                &compiled.spec.profile,
                &self.state.worker_policy.extension_host_policy,
            )
            .await?;
        Ok(self
            .state
            .cloud
            .edit_queued_session_submission(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                &submission_id,
                edit,
                &compiled,
                now_ms()?,
            )
            .await?)
    }

    pub(crate) async fn remove_inbox(
        &self,
        session: &CloudSessionRecord,
        submission_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        self.state
            .cloud
            .remove_queued_session_submission(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                &submission_id,
                now_ms()?,
            )
            .await
    }

    pub(crate) async fn steer_inbox(
        &self,
        session: &CloudSessionRecord,
        submission_id: SubmissionId,
    ) -> Result<SessionSubmission, HarnessError> {
        self.state
            .cloud
            .restart_session_inbox(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                &submission_id,
                now_ms()?,
            )
            .await
    }

    pub(crate) async fn turn(
        &self,
        session: CloudSessionRecord,
        request: TurnRequest,
        skill: Option<String>,
    ) -> Result<Value, crate::platform::http::ApiError> {
        let content = if let Some(name) = skill {
            SubmissionContent::Skill {
                name,
                input: request.input,
            }
        } else {
            SubmissionContent::Prompt {
                input: request.input,
            }
        };
        let receipt = self
            .enqueue_submission(
                &session,
                SessionSubmissionRequest {
                    delivery: SubmissionDelivery::Queue,
                    run_id: request.run_id.map(RunId::new),
                    content,
                    references: Vec::new(),
                    attachments: request.attachments,
                },
            )
            .await?;
        Ok(json!({ "run": receipt.run }))
    }

    async fn submission_execution_input(
        &self,
        session: &CloudSessionRecord,
        content: &SubmissionContent,
    ) -> Result<String, HarnessError> {
        match content.skill_name() {
            None => Ok(content.input().to_owned()),
            Some(name) => Ok(self
                .prepare_skill(session, name.to_owned(), content.input().to_owned())
                .await?
                .model_input),
        }
    }

    fn submission_draft(
        &self,
        session: &CloudSessionRecord,
        request: &SessionSubmissionRequest,
        execution_input: String,
    ) -> Result<CloudRunDraft, HarnessError> {
        let permissions = if session.mode == SessionMode::Plan {
            PermissionPreset::ReadOnly
        } else {
            session.permissions
        };
        Ok(CloudRunDraft {
            project_id: session.project_id.clone(),
            workspace_id: session.workspace_id.clone(),
            agent_id: session.agent_id.clone(),
            session_id: session.session_id.clone(),
            run_id: request.run_id.clone(),
            limits: self.state.worker_policy.maximum_limits,
            permissions,
            mode: session.mode,
            profile: Self::profile(session)?,
            input: execution_input,
            references: request.references.clone(),
            reference_contexts: Vec::new(),
            attachments: request.attachments.clone(),
            reserved_model_tokens: session.reserved_model_tokens,
        })
    }

    pub(crate) async fn cancel(
        &self,
        session: &CloudSessionRecord,
        run_id: &RunId,
    ) -> Result<(), HarnessError> {
        let run = self.state.cloud.get_run(self.tenant_id, run_id).await?;
        if run.user_id != session.user_id || run.session_id != session.session_id {
            return Err(HarnessError::invalid("run does not exist"));
        }
        self.state
            .cloud
            .pause_session_inbox(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                None,
                now_ms()?,
            )
            .await?;
        self.state
            .cloud
            .cancel_run_as(
                self.tenant_id,
                &self.actor.user_id,
                &session.session_id,
                run_id,
                now_ms()?,
            )
            .await
            .map(|_| ())
    }

    pub(crate) async fn followup_subagent(
        &self,
        parent: &CloudSessionRecord,
        subagent_id: &SubagentId,
        message: String,
    ) -> Result<SubagentSnapshot, crate::platform::http::ApiError> {
        let message = message.trim().to_owned();
        if message.is_empty() {
            return Err(HarnessError::invalid("subagent follow-up must not be empty").into());
        }
        let mapped = self
            .state
            .cloud
            .cloud_subagent(
                self.tenant_id,
                &self.actor.user_id,
                &parent.session_id,
                subagent_id,
            )
            .await?;
        let metadata = mapped.child.subagent.as_ref().ok_or_else(|| {
            HarnessError::execution("cloud Subagent Session has no canonical metadata")
        })?;
        if metadata.provider != "in-process"
            || metadata.transcript_kind != ternilo_protocol::SubagentTranscriptKind::Conversation
        {
            return Err(HarnessError::invalid(format!(
                "subagent provider {:?} is one-shot and does not support follow-up messages",
                metadata.provider
            ))
            .into());
        }
        self.enqueue_submission(
            &mapped.child,
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Queue,
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: message.clone(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await?;
        Ok(cloud_subagent_snapshot(
            &mapped.child,
            message,
            SubagentStatus::Running,
            now_ms()?,
        ))
    }

    pub(crate) async fn interrupt_subagent(
        &self,
        parent: &CloudSessionRecord,
        subagent_id: &SubagentId,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let mapped = self
            .state
            .cloud
            .cloud_subagent(
                self.tenant_id,
                &self.actor.user_id,
                &parent.session_id,
                subagent_id,
            )
            .await?;
        if let Some(run) = self
            .state
            .cloud
            .active_subagent_run(
                self.tenant_id,
                &self.actor.user_id,
                &mapped.child.session_id,
            )
            .await?
        {
            self.cancel(&mapped.child, &run.run_id).await?;
        }
        Ok(cloud_subagent_snapshot(
            &mapped.child,
            mapped.child.title.clone(),
            SubagentStatus::Cancelled,
            now_ms()?,
        ))
    }

    async fn compile_submission(
        &self,
        session: &CloudSessionRecord,
        mut draft: CloudRunDraft,
    ) -> Result<ternilo_cloud::CompiledRun, crate::platform::http::ApiError> {
        crate::platform::require_managed_execution(self.state)?;
        self.state
            .store
            .resource_access(
                self.actor,
                self.tenant_id,
                ResourceKind::Session,
                session.session_id.as_str(),
            )
            .await?
            .require(ResourceAction::Submit)?;
        if draft.session_id != session.session_id
            || draft.workspace_id != session.workspace_id
            || draft.project_id != session.project_id
        {
            return Err(HarnessError::policy(
                "cloud run must retain the canonical session binding",
            )
            .into());
        }
        let selected = session.model.as_ref().ok_or_else(|| {
            HarnessError::policy("Choose an available model before submitting a managed task")
        })?;
        if ternilo_cloud::profile_model_snapshot(&draft.profile)?.as_ref() != Some(selected) {
            return Err(
                HarnessError::policy("run model must match the selected session model").into(),
            );
        }
        let current = super::model_options::managed::current(
            self.state,
            self.actor,
            self.tenant_id,
            ResourceKind::Session,
            session.session_id.as_str(),
            selected,
        )
        .await?;
        validate_task_model_budget(
            &current,
            &draft.input,
            &draft.attachments,
            draft.reserved_model_tokens,
        )?;
        for entry in &mut draft.profile.plugins {
            if entry.enabled && entry.kind == ternilo_cloud::BROKERED_MODEL_KIND {
                entry.config = json!({"snapshot": current});
            }
        }
        let compiled = self.state.worker_policy.compile_run(
            draft,
            self.tenant_id.clone(),
            session.user_id.clone(),
            self.actor.user_id.clone(),
            &self.state.catalog,
        )?;
        self.state
            .store
            .resolve_extensions(
                self.actor,
                self.tenant_id,
                &compiled.spec.profile,
                &self.state.worker_policy.extension_host_policy,
            )
            .await?;
        Ok(compiled)
    }

    async fn model_update(
        &self,
        selection: Option<&Value>,
        resource: Option<(ResourceKind, &str)>,
    ) -> Result<Option<Option<RunModelSnapshot>>, HarnessError> {
        let Some(selection) = selection else {
            return Ok(None);
        };
        let mut selection: DefaultModelSelection = serde_json::from_value(selection.clone())
            .map_err(|error| HarnessError::invalid(format!("invalid model selection: {error}")))?;
        if matches!(selection, DefaultModelSelection::ProfileDefault)
            && let Some((kind, id)) = resource
        {
            selection = self
                .state
                .cloud
                .resource_default_model(self.tenant_id, &self.actor.user_id, kind, id)
                .await?;
        }
        self.model_update_value(selection, resource).await.map(Some)
    }

    async fn model_update_value(
        &self,
        selection: DefaultModelSelection,
        resource: Option<(ResourceKind, &str)>,
    ) -> Result<Option<RunModelSnapshot>, HarnessError> {
        let owner = if let Some((kind, id)) = resource {
            let access = self
                .state
                .store
                .resource_access(self.actor, self.tenant_id, kind, id)
                .await?;
            access.require(ResourceAction::View)?;
            access.storage_user_id
        } else {
            self.actor.user_id.clone()
        };
        let previous = if let Some((ResourceKind::Session, id)) = resource {
            self.state
                .cloud
                .get_session(self.tenant_id, &SessionId::new(id))
                .await?
                .model
        } else {
            None
        };
        super::model_options::managed::selected(
            self.state,
            self.actor,
            self.tenant_id,
            resource,
            &owner,
            previous.as_ref(),
            selection,
        )
        .await
    }

    async fn validate_profile(&self, session: &CloudSessionRecord) -> Result<(), HarnessError> {
        self.validate_profile_parts(session.model.as_ref(), &session.profile_plugins)
            .await
    }

    async fn validate_profile_parts(
        &self,
        model: Option<&RunModelSnapshot>,
        plugins: &[ternilo_protocol::PluginEntry],
    ) -> Result<(), HarnessError> {
        let profile = compose_model_profile(model, plugins)?;
        self.state
            .worker_policy
            .validate_profile_composition(&profile, &self.state.catalog)?;
        if profile
            .plugins
            .iter()
            .any(|entry| entry.enabled && entry.kind == ternilo_extension::EXTENSION_PACKAGE_KIND)
        {
            self.state
                .store
                .resolve_extensions(
                    self.actor,
                    self.tenant_id,
                    &profile,
                    &self.state.worker_policy.extension_host_policy,
                )
                .await?;
        }
        Ok(())
    }
}

const fn should_begin_steering(
    delivery: SubmissionDelivery,
    placement: SubmissionPlacement,
) -> bool {
    matches!(
        (delivery, placement),
        (SubmissionDelivery::Steer, SubmissionPlacement::Queued)
    )
}

fn cloud_subagent_snapshot(
    child: &CloudSessionRecord,
    task: String,
    status: SubagentStatus,
    now_ms: u64,
) -> SubagentSnapshot {
    let metadata = child
        .subagent
        .as_ref()
        .expect("canonical cloud Subagent has metadata");
    SubagentSnapshot {
        subagent_id: metadata.subagent_id.clone(),
        provider: metadata.provider.clone(),
        label: child.title.clone(),
        task,
        supports_followup: metadata.provider == "in-process"
            && metadata.transcript_kind == ternilo_protocol::SubagentTranscriptKind::Conversation,
        session_id: Some(child.session_id.clone()),
        transcript_kind: metadata.transcript_kind,
        status,
        output: None,
        error: None,
        created_at_ms: child.created_at_ms,
        updated_at_ms: now_ms,
    }
}

fn control_projection_profile(mut profile: Profile) -> Profile {
    profile
        .plugins
        .retain(|entry| entry.kind != ternilo_extension::EXTENSION_PACKAGE_KIND);
    profile
}

fn validate_task_model_budget(
    model: &RunModelSnapshot,
    input_text: &str,
    attachments: &[ternilo_protocol::Attachment],
    token_limit: u64,
) -> Result<(), HarnessError> {
    let input = if attachments
        .iter()
        .any(|attachment| attachment.media_type.starts_with("image/"))
    {
        model.defaults.context_window
    } else {
        u64::try_from(input_text.len())
            .map_err(|_| HarnessError::invalid("task input exceeds supported size"))?
            .checked_add(4_096)
            .ok_or_else(|| HarnessError::invalid("task token estimate overflow"))?
    };
    let minimum = input
        .checked_add(model.defaults.max_output_tokens)
        .ok_or_else(|| HarnessError::invalid("task token estimate overflow"))?;
    if token_limit < minimum {
        return Err(HarnessError::invalid(format!(
            "This task needs a reservation of at least {minimum} tokens for its model and attachments; the task token limit is {token_limit}. Increase the task token limit in the session menu before submitting. Context and retries may require additional capacity.",
        )));
    }
    Ok(())
}

fn compose_model_profile(
    model: Option<&RunModelSnapshot>,
    plugins: &[ternilo_protocol::PluginEntry],
) -> Result<Profile, HarnessError> {
    let profile = ternilo_cloud::cloud_child_profile(ternilo_kernel::compose_profiles([
        ternilo_cloud::cloud_profile(model),
        Profile {
            plugins: plugins.to_vec(),
        },
    ]));
    if ternilo_cloud::profile_model_snapshot(&profile)?.as_ref() != model {
        return Err(HarnessError::policy(
            "plugin overrides cannot change the selected model binding",
        ));
    }
    Ok(profile)
}

fn cloud_workspace_path() -> String {
    "云端 / 已绑定 Workspace".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{ReasoningEffort, UserId};
    fn reasoning_snapshot() -> ternilo_protocol::RunModelSnapshot {
        ternilo_protocol::RunModelSnapshot {
            binding: ternilo_protocol::RunModelBinding::Platform {
                grant_id: "grant".to_owned(),
                model_id: "reasoning-model".to_owned(),
                beneficiary_user_id: UserId::new("owner"),
            },
            protocol: ternilo_protocol::ProviderProtocol::OpenAiResponses,
            defaults: ternilo_protocol::ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 8_192,
                reasoning: Some(ternilo_protocol::ProviderModelReasoning {
                    default_effort: ReasoningEffort::Medium,
                    efforts: [
                        (ReasoningEffort::Medium, Some("medium".to_owned())),
                        (ReasoningEffort::High, Some("ultra".to_owned())),
                    ]
                    .into_iter()
                    .collect(),
                }),
            },
            reasoning_effort: None,
            display_name: "Reasoning model".to_owned(),
            source_name: "Owner allowance".to_owned(),
        }
    }

    #[test]
    fn cloud_model_snapshot_uses_defaults_and_rejects_undeclared_efforts() {
        let mut snapshot = reasoning_snapshot();
        snapshot.validate().unwrap();
        assert_eq!(
            snapshot
                .resolved_model()
                .reasoning_value(snapshot.reasoning_effort)
                .unwrap(),
            Some("medium")
        );
        snapshot.reasoning_effort = Some(ReasoningEffort::High);
        snapshot.validate().unwrap();
        assert_eq!(
            snapshot
                .resolved_model()
                .reasoning_value(snapshot.reasoning_effort)
                .unwrap(),
            Some("ultra")
        );
        snapshot.reasoning_effort = Some(ReasoningEffort::Xhigh);
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn cloud_plugin_overrides_cannot_replace_the_selected_budget_source() {
        let snapshot = reasoning_snapshot();
        let selected = compose_model_profile(Some(&snapshot), &[]).unwrap();
        assert_eq!(
            ternilo_cloud::profile_model_snapshot(&selected).unwrap(),
            Some(snapshot.clone())
        );
        let mut changed = snapshot.clone();
        if let ternilo_protocol::RunModelBinding::Platform { grant_id, .. } = &mut changed.binding {
            *grant_id = "another-grant".to_owned();
        }
        let overriding = ternilo_cloud::cloud_profile(Some(&changed))
            .plugins
            .into_iter()
            .filter(|entry| entry.kind == ternilo_cloud::BROKERED_MODEL_KIND)
            .collect::<Vec<_>>();
        assert!(compose_model_profile(Some(&snapshot), &overriding).is_err());
        let empty = compose_model_profile(None, &[]).unwrap();
        assert!(
            ternilo_cloud::profile_model_snapshot(&empty)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn cloud_skill_submission_remains_typed_until_worker_resolution() {
        let content = SubmissionContent::Skill {
            name: "review-code".to_owned(),
            input: "inspect the queue".to_owned(),
        };
        assert!(matches!(
            &content,
            SubmissionContent::Skill { name, input }
                if name == "review-code" && input == "inspect the queue"
        ));
    }

    #[test]
    fn cloud_submit_routes_only_a_queued_steer_request_into_the_active_window() {
        assert!(should_begin_steering(
            SubmissionDelivery::Steer,
            SubmissionPlacement::Queued,
        ));
        assert!(!should_begin_steering(
            SubmissionDelivery::Steer,
            SubmissionPlacement::Running,
        ));
        assert!(!should_begin_steering(
            SubmissionDelivery::Queue,
            SubmissionPlacement::Queued,
        ));
    }

    #[test]
    fn cloud_projection_accepts_worker_loaded_extension_tools() {
        let catalog = ternilo_cloud::catalog().unwrap();
        let mut profile = ternilo_cloud::cloud_profile(Some(&reasoning_snapshot()));
        profile.plugins.push(ternilo_protocol::PluginEntry {
            id: "workspace-extension-tool".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: serde_json::json!({
                "package_id": "dev.ternilo.workspace-tool",
                "version": "1.2.3",
                "settings": {}
            }),
        });

        let Err(error) = catalog.projection_units(&profile) else {
            panic!("unfiltered Worker-loaded extension package unexpectedly had a static factory");
        };
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Composition);
        assert_eq!(
            error.message,
            "plugin kind \"ternilo.extension.package\" is not in the catalog"
        );

        let profile = control_projection_profile(profile);
        let units = catalog.projection_units(&profile).unwrap();

        assert!(units.iter().any(|unit| unit.key() == "stats"));
    }
}
