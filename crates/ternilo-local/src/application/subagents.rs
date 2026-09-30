use super::{
    AgentInput, AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamProvider,
    AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace,
    Arc, BTreeMap, Catalog, Digest, ExecutionActivityCache, Future, HarnessError, HarnessSession,
    HostEnvironment, HostPolicy, InputProvenance, InteractionBroker, JsonlEventStore, LiveSessions,
    LocalAgentTeamProvider, LocalAgentTeamStore, LocalApplication, LocalAttachments,
    LocalCredentials, LocalEventNotification, LocalInboxStore, LocalInputReferences, LocalSession,
    LocalSessionArchive, LocalState, ManagedSession, Mutex, PathBuf, PermissionPreset, Pin,
    Profile, RunCancellation, RunId, RunOutcome, SessionEvent, SessionEventKind, SessionEventStore,
    SessionId, SessionIdentity, SessionMode, Sha256, SubagentAdmission, SubagentId,
    SubagentRunStart, SubagentSessionBinding, SubagentSessionHost, SubagentSessionMetadata,
    SubagentSessionRequest, SubagentSnapshot, Weak, WorkspaceBinding, broadcast, now_ms,
    publish_invalidation, resolve_named_provider, session_profile,
};
use std::fmt::Write as _;

impl LocalApplication {
    pub async fn followup_subagent(
        &self,
        session_id: &str,
        subagent_id: SubagentId,
        message: String,
    ) -> Result<SubagentSnapshot, HarnessError> {
        self.followup_subagent_with_provenance(
            session_id,
            subagent_id,
            message,
            self.local_input_provenance()?,
        )
        .await
    }

    pub async fn followup_subagent_with_provenance(
        &self,
        session_id: &str,
        subagent_id: SubagentId,
        message: String,
        provenance: InputProvenance,
    ) -> Result<SubagentSnapshot, HarnessError> {
        provenance.validate()?;
        self.account_authorizations.check(Some(&provenance)).await?;
        self.validate_device_input(session_id, Some(&provenance))
            .await?;
        subagent_id.validate()?;
        let managed = self.addressable_session(session_id).await?;
        let subagents = managed.harness.subagents().ok_or_else(|| {
            HarnessError::policy(
                "capability unavailable: this Session profile has no Subagents service",
            )
        })?;
        let run_id = provenance
            .run_id
            .clone()
            .map_or_else(|| self.next_id("subagent-followup").map(RunId::new), Ok)?;
        run_id.validate()?;
        let snapshot = subagents
            .followup(run_id, subagent_id, message, Some(provenance))
            .await?;
        self.state
            .touch_session(session_id, None, now_ms()?)
            .await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Activity,
            None,
        );
        self.checkpoint_session_fail_soft(session_id).await;
        Ok(snapshot)
    }

    pub async fn interrupt_subagent(
        &self,
        session_id: &str,
        subagent_id: SubagentId,
    ) -> Result<SubagentSnapshot, HarnessError> {
        subagent_id.validate()?;
        let managed = self.addressable_session(session_id).await?;
        let subagents = managed.harness.subagents().ok_or_else(|| {
            HarnessError::policy(
                "capability unavailable: this Session profile has no Subagents service",
            )
        })?;
        let run_id = RunId::new(self.next_id("subagent-interrupt")?);
        run_id.validate()?;
        let snapshot = subagents.interrupt(run_id, subagent_id).await?;
        self.state
            .touch_session(session_id, None, now_ms()?)
            .await?;
        self.invalidate(
            Some(session_id),
            crate::LocalInvalidationCategory::Activity,
            None,
        );
        self.checkpoint_session_fail_soft(session_id).await;
        Ok(snapshot)
    }

    pub(super) fn subagent_session_host(&self) -> Arc<dyn SubagentSessionHost> {
        Arc::new(LocalSubagentSessionHost {
            execution_resources: Arc::clone(&self.execution_resources),
            account_authorizations: Arc::clone(&self.account_authorizations),
            directory_coordinator: self.directory_coordinator.clone(),
            directory_account_owner: Arc::clone(&self.directory_account_owner),
            catalog: Arc::clone(&self.catalog),
            profile: self.profile.clone(),
            policy: self.policy.clone(),
            state: Arc::clone(&self.state),
            inbox: Arc::clone(&self.inbox),
            agent_team: Arc::clone(&self.agent_team),
            live: Arc::downgrade(&self.live),
            session_lifecycle: Arc::downgrade(&self.session_lifecycle),
            interaction: Arc::clone(&self.interaction),
            credentials: Arc::clone(&self.credentials),
            providers: Arc::clone(&self.providers),
            server_models: Arc::clone(&self.server_models),
            attachments: Arc::clone(&self.attachments),
            session_archive: Arc::clone(&self.session_archive),
            runtime_extensions: Arc::clone(&self.runtime_extensions),
            event_notifications: self.event_notifications.clone(),
            execution_activity: Arc::clone(&self.execution_activity),
            invalidations: self.invalidations.clone(),
            stopping: self.stopping.clone(),
        })
    }

    pub(super) fn agent_team_provider(
        &self,
        session_id: &str,
    ) -> Result<LocalAgentTeamProvider, HarnessError> {
        let session_id = SessionId::new(session_id);
        session_id.validate()?;
        Ok(LocalAgentTeamProvider::new(
            Arc::clone(&self.agent_team),
            Arc::clone(&self.state),
            session_id,
            self.invalidations.clone(),
        ))
    }

    pub async fn agent_team_snapshot(
        &self,
        session_id: &str,
    ) -> Result<AgentTeamSnapshot, HarnessError> {
        self.agent_team_provider(session_id)?
            .current_snapshot()
            .await
    }

    pub async fn create_agent_team_task(
        &self,
        session_id: &str,
        request: AgentTeamTaskCreate,
    ) -> Result<AgentTeamTask, HarnessError> {
        self.agent_team_provider(session_id)?
            .current_create_task(request)
            .await
    }

    pub async fn replace_agent_team_task(
        &self,
        session_id: &str,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    ) -> Result<AgentTeamTask, HarnessError> {
        self.agent_team_provider(session_id)?
            .current_replace_task(task_id, request)
            .await
    }

    pub async fn delete_agent_team_task(
        &self,
        session_id: &str,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    ) -> Result<(), HarnessError> {
        self.agent_team_provider(session_id)?
            .current_delete_task(task_id, expected_revision)
            .await
    }

    pub async fn send_agent_team_message(
        &self,
        session_id: &str,
        request: AgentTeamMessageSend,
    ) -> Result<AgentTeamMessage, HarnessError> {
        self.agent_team_provider(session_id)?
            .current_send_message(request)
            .await
    }

    pub async fn mark_agent_team_message_read(
        &self,
        session_id: &str,
        message_id: AgentTeamMessageId,
    ) -> Result<AgentTeamMessage, HarnessError> {
        self.agent_team_provider(session_id)?
            .current_mark_message_read(message_id)
            .await
    }
}

#[derive(Clone)]
struct LocalSubagentSessionHost {
    execution_resources: Arc<crate::execution_resources::ExecutionResources>,
    account_authorizations: Arc<crate::account_authorizations::AccountAuthorizations>,
    directory_coordinator: crate::DirectoryCoordinator,
    directory_account_owner: Arc<std::sync::RwLock<Option<ternilo_protocol::UserId>>>,
    catalog: Arc<Catalog>,
    profile: Profile,
    policy: HostPolicy,
    state: Arc<LocalState>,
    inbox: Arc<LocalInboxStore>,
    agent_team: Arc<LocalAgentTeamStore>,
    live: Weak<LiveSessions>,
    session_lifecycle: Weak<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    interaction: Arc<InteractionBroker>,
    credentials: Arc<LocalCredentials>,
    providers: Arc<crate::LocalProviders>,
    server_models: Arc<crate::server_models::ServerModelGateways>,
    attachments: Arc<LocalAttachments>,
    session_archive: Arc<LocalSessionArchive>,
    runtime_extensions: Arc<crate::LocalRuntimeExtensions>,
    event_notifications: broadcast::Sender<LocalEventNotification>,
    execution_activity: Arc<ExecutionActivityCache>,
    invalidations: broadcast::Sender<crate::LocalInvalidationNotification>,
    stopping: RunCancellation,
}
impl LocalSubagentSessionHost {
    fn live(&self) -> Result<Arc<LiveSessions>, HarnessError> {
        self.stopping.check()?;
        self.live
            .upgrade()
            .ok_or_else(|| HarnessError::cancelled("local Session host is shutting down"))
    }

    async fn lifecycle(&self, id: &str) -> Result<Arc<Mutex<()>>, HarnessError> {
        let sessions = self
            .session_lifecycle
            .upgrade()
            .ok_or_else(|| HarnessError::cancelled("local Session host is shutting down"))?;
        let mut sessions = sessions.lock().await;
        Ok(Arc::clone(
            sessions
                .entry(id.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        ))
    }

    fn event_store(&self, session: &LocalSession) -> JsonlEventStore {
        JsonlEventStore::new(&self.state.sessions_dir(), &session.identity.session_id)
            .with_index(
                self.session_archive.index(),
                &session.identity.session_id,
                &session.workspace_id,
            )
            .with_notifications(
                self.event_notifications.clone(),
                &session.identity.session_id,
            )
            .with_execution_activity(
                Arc::clone(&self.execution_activity),
                &session.identity.session_id,
            )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "assemble trusted child services before publishing its runtime"
    )]
    async fn managed(&self, id: &str) -> Result<Arc<ManagedSession>, HarnessError> {
        let live = self.live()?;
        if let Some(managed) = live.read().await.get(id).cloned() {
            return Ok(managed);
        }
        let lifecycle = self.lifecycle(id).await?;
        let _lifecycle = lifecycle.lock().await;
        if let Some(managed) = live.read().await.get(id).cloned() {
            return Ok(managed);
        }
        let session = self
            .state
            .session(id)
            .await
            .ok_or_else(|| HarnessError::invalid(format!("unknown session {id:?}")))?;
        let store = Arc::new(self.event_store(&session));
        let mut policy = self.policy.clone();
        if session.mode == SessionMode::Plan {
            policy.permissions = PermissionPreset::ReadOnly;
            policy.allow_mutating_tools = false;
        } else {
            policy.permissions = session.permissions;
        }
        let interaction = self
            .interaction
            .for_session(session.identity.session_id.clone());
        let named_provider = resolve_named_provider(&session.model, &self.providers).await?;
        let profile = session_profile(
            &self.profile,
            &session.model,
            session.server_model.as_ref(),
            named_provider.as_ref(),
            &session.preset_plugins,
            &session.profile_plugins,
            session.mode,
        );
        let secrets: Arc<dyn ternilo_kernel::SecretResolver> = self.credentials.clone();
        let session_archive: Arc<dyn ternilo_kernel::SessionArchive> = self.session_archive.clone();
        let attachments: Arc<dyn ternilo_kernel::AttachmentResolver> = self.attachments.clone();
        let runtime_extensions: Arc<dyn ternilo_kernel::RuntimeExtensionsProvider> =
            self.runtime_extensions.clone();
        let agent_team: Arc<dyn AgentTeamProvider> = Arc::new(LocalAgentTeamProvider::new(
            Arc::clone(&self.agent_team),
            Arc::clone(&self.state),
            session.identity.session_id.clone(),
            self.invalidations.clone(),
        ));
        let telemetry = Arc::new(ternilo_kernel::HostSessionTelemetry::default());
        let execution = super::directory_admission::SessionDirectoryExecution::bind(
            self.directory_coordinator.clone(),
            Arc::clone(&self.state),
            self.live.clone(),
            Arc::clone(&self.directory_account_owner),
            session.identity.session_id.as_str().to_owned(),
            self.inbox
                .execution_scope(session.identity.session_id.as_str())
                .await?,
            PathBuf::from(&session.workspace_path),
        );
        let input_references = Arc::new(LocalInputReferences::new(
            Arc::clone(&self.state),
            session.identity.session_id.clone(),
        ));
        let environment = HostEnvironment::with_interaction_and_secrets(
            session.identity,
            Some(WorkspaceBinding {
                workspace_id: session.workspace_id,
                path: session.workspace_path,
            }),
            policy,
            store,
            interaction,
            secrets,
        )
        .with_model_gateway(self.server_models.for_session(id.to_owned()))
        .with_session_mode(session.mode)
        .with_workspace_execution(execution)
        .with_run_authorization(Arc::new(
            super::account_authorizations::AccountRunAuthorization {
                origins: super::model_origins::InputOrigins::new(
                    Arc::clone(&self.state),
                    self.live.clone(),
                ),
                accounts: Arc::clone(&self.account_authorizations),
                session_id: id.to_owned(),
            },
        ))
        .with_execution_resources(self.execution_resources.for_session(id.to_owned()))
        .with_input_reference_resolver(input_references)
        .with_attachment_resolver(attachments)
        .with_session_archive(session_archive)
        .with_runtime_extensions(runtime_extensions)
        .with_subagent_sessions(Arc::new(self.clone()))
        .with_agent_team(agent_team)
        .with_session_telemetry(Arc::clone(&telemetry));
        let harness = HarnessSession::boot(&self.catalog, &profile, environment).await?;
        let created = Arc::new(ManagedSession {
            harness,
            gate: Arc::new(Mutex::new(())),
            telemetry,
            boot_mode: session.mode,
        });
        let mut sessions = live.write().await;
        if self.stopping.is_cancelled() {
            drop(sessions);
            created.harness.shutdown().await?;
            return Err(HarnessError::cancelled(
                "local application is shutting down",
            ));
        }
        if let Some(existing) = sessions.get(id).cloned() {
            drop(sessions);
            created.harness.shutdown().await?;
            Ok(existing)
        } else {
            sessions.insert(id.to_owned(), Arc::clone(&created));
            Ok(created)
        }
    }
}

impl SubagentSessionHost for LocalSubagentSessionHost {
    fn create<'a>(
        &'a self,
        parent: SessionIdentity,
        request: SubagentSessionRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SubagentSessionBinding>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            parent.validate()?;
            request.subagent_id.validate()?;
            if request.provider.trim().is_empty()
                || request.label.trim().is_empty()
                || request.task.trim().is_empty()
            {
                return Err(HarnessError::invalid(
                    "Subagent Session provider, label, and task must not be empty",
                ));
            }
            let parent_session = self
                .state
                .session(parent.session_id.as_str())
                .await
                .ok_or_else(|| HarnessError::invalid("Subagent parent Session does not exist"))?;
            if parent_session.identity != parent {
                return Err(HarnessError::invalid(
                    "Subagent parent identity does not match its persisted Session",
                ));
            }
            let mut key = Sha256::new();
            key.update(parent.session_id.as_str().as_bytes());
            key.update([0]);
            key.update(request.subagent_id.as_str().as_bytes());
            let mut encoded = String::with_capacity(32);
            for byte in &key.finalize()[..16] {
                let _ = write!(encoded, "{byte:02x}");
            }
            let session_id = SessionId::new(format!("child-{encoded}"));
            session_id.validate()?;
            let lifecycle = self.lifecycle(session_id.as_str()).await?;
            let _lifecycle = lifecycle.lock().await;
            self.inbox
                .require_live_session_id(session_id.as_str())
                .await?;
            if self.state.session(session_id.as_str()).await.is_some() {
                return Err(HarnessError::conflict("Subagent Session already exists"));
            }
            let now = now_ms()?;
            let child = LocalSession {
                identity: SessionIdentity {
                    tenant_id: parent.tenant_id,
                    user_id: parent.user_id,
                    agent_id: parent.agent_id,
                    session_id: session_id.clone(),
                },
                workspace_id: parent_session.workspace_id,
                workspace_path: parent_session.workspace_path,
                parent_session_id: Some(parent.session_id),
                subagent: Some(SubagentSessionMetadata {
                    subagent_id: request.subagent_id,
                    provider: request.provider,
                    transcript_kind: request.transcript_kind,
                }),
                title: request.label,
                archived_at_ms: None,
                blank: false,
                permissions: parent_session.permissions,
                model: parent_session.model,
                server_model: parent_session.server_model,
                agent_preset: parent_session.agent_preset,
                preset_plugins: parent_session.preset_plugins,
                profile_plugins: parent_session.profile_plugins,
                mode: parent_session.mode,
                created_at_ms: now,
                updated_at_ms: now,
            };
            self.inbox.register_execution_scope(&child).await?;
            self.state.insert_forked_session(child).await?;
            LocalAgentTeamProvider::new(
                Arc::clone(&self.agent_team),
                Arc::clone(&self.state),
                session_id.clone(),
                self.invalidations.clone(),
            )
            .invalidate_current_team(None)
            .await;
            publish_invalidation(
                &self.invalidations,
                Some(session_id.as_str()),
                crate::LocalInvalidationCategory::Workbench,
                None,
            );
            Ok(Some(SubagentSessionBinding { session_id }))
        })
    }

    fn run<'a>(
        &'a self,
        session_id: SessionId,
        run_id: RunId,
        input: String,
        cancellation: RunCancellation,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            session_id.validate()?;
            run_id.validate()?;
            let session = self
                .state
                .session(session_id.as_str())
                .await
                .ok_or_else(|| HarnessError::invalid("Subagent Session does not exist"))?;
            if session.subagent.as_ref().is_none_or(|metadata| {
                metadata.transcript_kind != ternilo_protocol::SubagentTranscriptKind::Conversation
            }) {
                return Err(HarnessError::policy(
                    "this Subagent Session exposes process lifecycle only",
                ));
            }
            self.state
                .mark_session_started(session_id.as_str(), now_ms()?)
                .await?;
            publish_invalidation(
                &self.invalidations,
                Some(session_id.as_str()),
                crate::LocalInvalidationCategory::Activity,
                None,
            );
            let managed = self.managed(session_id.as_str()).await?;
            let _gate = Arc::clone(&managed.gate).lock_owned().await;
            self.stopping.check()?;
            start.resolve(Ok(SubagentAdmission::Direct));
            let running = managed.harness.run_input(AgentInput {
                additional_inputs: Vec::new(),
                run_id: run_id.clone(),
                input,
                provenance: start.provenance.clone(),
                display_input: None,
                source: None,
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
            });
            tokio::pin!(running);
            let outcome = tokio::select! {
                result = &mut running => result,
                () = cancellation.cancelled() => {
                    managed.harness.cancel(run_id).await?;
                    running.await
                }
            }?;
            self.state
                .touch_session(session_id.as_str(), None, now_ms()?)
                .await?;
            publish_invalidation(
                &self.invalidations,
                Some(session_id.as_str()),
                crate::LocalInvalidationCategory::Activity,
                None,
            );
            Ok(outcome)
        })
    }

    fn append_lifecycle<'a>(
        &'a self,
        session_id: SessionId,
        run_id: RunId,
        snapshot: SubagentSnapshot,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            session_id.validate()?;
            run_id.validate()?;
            let lifecycle = self.lifecycle(session_id.as_str()).await?;
            let _lifecycle = lifecycle.lock().await;
            let session = self
                .state
                .session(session_id.as_str())
                .await
                .ok_or_else(|| HarnessError::invalid("Subagent Session does not exist"))?;
            if session.subagent.as_ref().is_none_or(|metadata| {
                metadata.transcript_kind
                    != ternilo_protocol::SubagentTranscriptKind::ProcessLifecycle
            }) {
                return Err(HarnessError::policy(
                    "conversation Subagent Sessions do not accept process lifecycle rows",
                ));
            }
            let store = self.event_store(&session);
            let seq = store
                .load_events()
                .await?
                .len()
                .try_into()
                .map_err(|_| HarnessError::execution("Session sequence exceeds u64"))?;
            let event = SessionEvent {
                seq,
                occurred_at_ms: now_ms()?,
                run_id,
                kind: SessionEventKind::SubagentUpdated { subagent: snapshot },
            };
            store.append(event.clone()).await?;
            self.state
                .touch_session(session_id.as_str(), None, event.occurred_at_ms)
                .await?;
            publish_invalidation(
                &self.invalidations,
                Some(session_id.as_str()),
                crate::LocalInvalidationCategory::Activity,
                None,
            );
            Ok(event)
        })
    }
}
