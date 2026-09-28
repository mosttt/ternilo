use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use ternilo_kernel::{
    AgentTeamProvider, Catalog, HarnessSession, HostEnvironment, HostPolicy, RunCancellation,
    SessionArchive, SessionEventStore, SubagentAdmission, SubagentRunStart, SubagentSessionBinding,
    SubagentSessionHost, SubagentSessionRequest, compose_profiles, validate_profile,
};
use ternilo_protocol::{
    AgentId, AgentInput, AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster,
    AgentPresetUpdateRequest, AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend,
    AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace,
    Attachment, AuthorizationAttempt, AuthorizationBeginRequest, AuthorizationCredentialKey,
    AuthorizationPromptAnswer, AuthorizationSnapshot, ExtensionProviderMaterializeRequest,
    FeedbackRating, HarnessError, InputAuthor, InputProvenance, PermissionPreset, PluginEntry,
    Profile, ProviderModel, ProviderModelDiscoveryRequest, ProviderProfile, ProviderProtocol,
    ReferenceCandidate, ReferenceCandidateRequest, ReferenceCandidateSnapshot, RunId, RunOutcome,
    SessionCommandCatalog, SessionCommandOutcome, SessionCommandOutcomeKind, SessionCommandReceipt,
    SessionEvent, SessionEventKind, SessionEventReadRequest, SessionId, SessionIdentity,
    SessionMode, SessionProjectionSnapshot, SessionSearchHit, SessionSearchRequest, SessionStats,
    SessionSubmission, SessionSubmissionRequest, SessionTelemetrySharingStatus, SessionTrace,
    SidebarOrdering, SkillCatalogSnapshot, SteeringInput, SubagentId, SubagentSessionMetadata,
    SubagentSnapshot, SubmissionContent, SubmissionDelivery, SubmissionId, SubmissionReference,
    TenantId, UserId, UserMessageSource, WorkspaceBinding, WorkspaceId,
};
use tokio::sync::{Mutex, RwLock, broadcast};

#[cfg(test)]
mod execution_activity_tests;
#[cfg(test)]
mod execution_scope_tests;

#[cfg(all(test, unix))]
mod execution_resource_tests;

#[cfg(test)]
mod directory_execution_tests;
mod file_inventory;
mod runtime;
mod subagents;
use runtime::{resolve_named_provider, session_profile, validate_local_profile};
mod history;
mod input_references;
mod presets;
mod providers;
mod services;
mod sessions;
mod turns;
mod workspaces;
use input_references::LocalInputReferences;
mod directory_admission;
mod model_availability;
mod model_connections;
mod model_origins;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod shutdown_tests;
#[cfg(test)]
mod subagent_start_tests;
mod submissions;
#[cfg(test)]
mod test_model;
#[cfg(test)]
mod workflow_activity_tests;
pub(crate) mod workspace_browser;

use crate::{
    DirectoryListing, InteractionBroker, LocalAttachments, LocalCredentials, LocalSession,
    LocalStateSnapshot, ModelSelection, PendingQuestion, Workspace,
    agent_team::{LocalAgentTeamProvider, LocalAgentTeamStore},
    canonical_directory, create_directory,
    event_store::{ExecutionActivityCache, JsonlEventStore},
    inbox::LocalInboxStore,
    list_directory,
    notifications::{event_session_dirty, event_updates_workbench, publish_invalidation},
    session_query::LocalSessionArchive,
    state::{LocalState, validate_model},
    workspace_reference_candidates,
};

struct ManagedSession {
    harness: HarnessSession,
    gate: Arc<Mutex<()>>,
    telemetry: Arc<ternilo_kernel::HostSessionTelemetry>,
    boot_mode: SessionMode,
}

type LiveSessions = RwLock<BTreeMap<String, Arc<ManagedSession>>>;

enum TurnInput {
    Prompt(String),
    Skill { name: String, request: String },
}

struct TurnRequest {
    run_id: Option<String>,
    input: TurnInput,
    references: Vec<SubmissionReference>,
    attachments: Vec<Attachment>,
    source_override: Option<UserMessageSource>,
    provenance: Option<InputProvenance>,
    require_unpaused_inbox: bool,
    additional_submissions: Vec<SessionSubmission>,
}

struct PreparedTurnInput {
    model_input: String,
    display_input: Option<String>,
    source: Option<UserMessageSource>,
    title_input: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionExport {
    pub schema_version: u32,
    pub session: LocalSession,
    pub workspace: Workspace,
    pub events: Vec<SessionEvent>,
}

#[derive(Default)]
pub struct LocalSessionUpdate {
    pub title: Option<String>,
    pub permissions: Option<PermissionPreset>,
    pub model: Option<ModelSelection>,
    pub server_model: Option<ternilo_protocol::RunModelSnapshot>,
    pub agent_preset: Option<String>,
    pub profile_plugins: Option<Vec<PluginEntry>>,
    pub mode: Option<SessionMode>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LocalEventNotification {
    pub session_id: String,
    pub event: SessionEvent,
}

impl LocalEventNotification {
    #[must_use]
    pub fn session_dirty(&self) -> ternilo_protocol::SessionLiveDirty {
        event_session_dirty(&self.event.kind)
    }

    #[must_use]
    pub fn updates_workbench(&self) -> bool {
        event_updates_workbench(&self.event.kind)
    }
}

impl LocalSessionUpdate {
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.permissions.is_none()
            && self.model.is_none()
            && self.agent_preset.is_none()
            && self.profile_plugins.is_none()
            && self.mode.is_none()
    }

    fn restarts_runtime(&self) -> bool {
        self.permissions.is_some()
            || self.model.is_some()
            || self.agent_preset.is_some()
            || self.profile_plugins.is_some()
            || self.mode.is_some()
    }
}

pub struct LocalApplication {
    directory_coordinator: crate::DirectoryCoordinator,
    directory_account_owner: Arc<std::sync::RwLock<Option<UserId>>>,
    data_dir: PathBuf,
    catalog: Arc<Catalog>,
    profile: Profile,
    policy: HostPolicy,
    state: Arc<LocalState>,
    agent_team: Arc<LocalAgentTeamStore>,
    live: Arc<LiveSessions>,
    session_lifecycle: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    interaction: Arc<InteractionBroker>,
    credentials: Arc<LocalCredentials>,
    providers: Arc<crate::LocalProviders>,
    server_models: Arc<crate::server_models::ServerModelGateways>,
    authorizations: Arc<crate::LocalAuthorizations>,
    preferences: Arc<crate::preferences::LocalPreferences>,
    presets: Arc<crate::LocalAgentPresets>,
    attachments: Arc<LocalAttachments>,
    session_archive: Arc<LocalSessionArchive>,
    extension_registry: Arc<ternilo_extension::ExtensionRegistry>,
    runtime_extensions: Arc<crate::LocalRuntimeExtensions>,
    next_id: AtomicU64,
    event_notifications: broadcast::Sender<LocalEventNotification>,
    execution_activity: Arc<ExecutionActivityCache>,
    invalidations: broadcast::Sender<crate::LocalInvalidationNotification>,
    inbox: Arc<LocalInboxStore>,
    submission_drivers: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    submission_tasks: std::sync::Mutex<tokio::task::JoinSet<()>>,
    stopping: RunCancellation,
    _data_lock: File,
}

impl LocalApplication {
    pub fn set_directory_account_owner(&self, owner: UserId) -> Result<(), HarnessError> {
        owner.validate()?;
        *self
            .directory_account_owner
            .write()
            .expect("directory account owner lock") = Some(owner);
        Ok(())
    }

    pub fn install_server_model_gateway(&self, gateway: Arc<dyn crate::ServerModelGateway>) {
        self.server_models.install(gateway);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "initialize durable stores and execution scopes before recovering work"
    )]
    pub async fn open(
        mut catalog: Catalog,
        profile: Profile,
        policy: HostPolicy,
        data_dir: PathBuf,
    ) -> Result<Self, HarnessError> {
        let data_lock = acquire_data_lock(&data_dir).await?;
        let extension_registry = ternilo_extension::ExtensionRegistry::open(
            data_dir.join("extensions"),
            ternilo_extension::ExtensionHostPolicy::default(),
        )?;
        catalog.register(ternilo_extension::extension_mount_factory(Arc::clone(
            &extension_registry,
        )))?;
        validate_local_profile(&profile, &catalog, &extension_registry)?;
        let presets = crate::LocalAgentPresets::open(data_dir.clone()).await?;
        for preset in presets.documents().await {
            validate_local_profile(
                &compose_profiles([profile.clone(), preset.profile]),
                &catalog,
                &extension_registry,
            )?;
        }
        let credentials = LocalCredentials::open(data_dir.clone()).await?;
        let attachments = Arc::new(LocalAttachments::open(&data_dir).await?);
        let providers = crate::LocalProviders::open(data_dir.clone()).await?;
        let credential_store: Arc<dyn ternilo_authorization::AuthorizationCredentialStore> =
            credentials.clone();
        let authorizations = crate::LocalAuthorizations::new(
            ternilo_authorization::AuthorizationService::new(credential_store),
        );
        if std::env::var_os("TERNILO_TEST_AUTHORIZATION_FIXTURE").is_some() {
            authorizations.register_fixture_flow().await?;
        }
        let state = Arc::new(LocalState::open(data_dir.clone()).await?);
        let agent_team =
            Arc::new(LocalAgentTeamStore::open(&data_dir.join("agent-team.sqlite3")).await?);
        let preferences = Arc::new(
            crate::preferences::LocalPreferences::open(&data_dir.join("preferences.sqlite3"))
                .await?,
        );
        let runtime_extensions = Arc::new(crate::LocalRuntimeExtensions::new(
            &catalog,
            Arc::clone(&extension_registry),
            Arc::clone(&state),
            Arc::clone(&presets),
            profile.clone(),
        ));
        for session in state.snapshot().await.sessions {
            let named_provider = resolve_named_provider(&session.model, &providers).await?;
            validate_local_profile(
                &session_profile(
                    &profile,
                    &session.model,
                    session.server_model.as_ref(),
                    named_provider.as_ref(),
                    &session.preset_plugins,
                    &session.profile_plugins,
                    session.mode,
                ),
                &catalog,
                &extension_registry,
            )?;
        }
        let (event_notifications, _) = broadcast::channel(2_048);
        let (invalidations, _) = broadcast::channel(256);
        let inbox = Arc::new(
            LocalInboxStore::open(&data_dir.join("inbox.sqlite3"), invalidations.clone()).await?,
        );
        inbox
            .initialize_execution_scopes(&state.snapshot().await.sessions)
            .await?;
        let session_archive =
            Arc::new(LocalSessionArchive::open(Arc::clone(&state), Arc::clone(&inbox)).await?);
        let application = Self {
            directory_coordinator: crate::DirectoryCoordinator::for_user()?,
            directory_account_owner: Arc::new(std::sync::RwLock::new(None)),
            data_dir,
            catalog: Arc::new(catalog),
            profile,
            policy,
            state,
            agent_team,
            live: Arc::new(RwLock::new(BTreeMap::new())),
            session_lifecycle: Arc::new(Mutex::new(BTreeMap::new())),
            interaction: Arc::new(InteractionBroker::with_invalidations(invalidations.clone())),
            credentials,
            providers,
            server_models: Arc::new(crate::server_models::ServerModelGateways::default()),
            authorizations,
            preferences,
            presets,
            attachments,
            session_archive,
            extension_registry,
            runtime_extensions,
            next_id: AtomicU64::new(1),
            event_notifications,
            execution_activity: Arc::new(ExecutionActivityCache::new(invalidations.clone())),
            invalidations,
            inbox,
            submission_drivers: Mutex::new(BTreeMap::new()),
            submission_tasks: std::sync::Mutex::new(tokio::task::JoinSet::new()),
            stopping: RunCancellation::new(),
            _data_lock: data_lock,
        };
        if let Err(error) = application.recover_session_work().await {
            let _ = application.shutdown().await;
            return Err(error);
        }
        Ok(application)
    }

    #[must_use]
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    #[must_use]
    pub fn application_catalog(&self) -> ternilo_protocol::ApplicationCatalog {
        let mut catalog = self.catalog.describe();
        catalog.host_limits = Some(self.policy.limits);
        catalog
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Return the durable coordination identity established by trusted session creation.
    pub async fn execution_scope(&self, session_id: &str) -> Result<String, HarnessError> {
        self.inbox.execution_scope(session_id).await
    }

    pub async fn snapshot(&self) -> LocalStateSnapshot {
        self.state.snapshot().await
    }

    pub async fn live_activities(&self) -> Vec<ternilo_protocol::SessionLiveActivity> {
        let sessions = self.state.snapshot().await.sessions;
        let live = self.live.read().await.clone();
        let mut activity = Vec::with_capacity(sessions.len());
        for session in sessions {
            let session_id = session.identity.session_id;
            let active_run = match live.get(session_id.as_str()) {
                Some(managed) => managed.harness.active_run().await,
                None => None,
            };
            let execution = active_run
                .as_ref()
                .and_then(|run_id| self.execution_activity.for_run(session_id.as_str(), run_id));
            activity.push(ternilo_protocol::SessionLiveActivity {
                session_id,
                running: active_run.is_some(),
                updated_at_ms: session.updated_at_ms,
                execution,
            });
        }
        activity
    }

    pub async fn live_activity(
        &self,
        session_id: &str,
    ) -> Option<ternilo_protocol::SessionLiveActivity> {
        let session = self.state.session(session_id).await?;
        let managed = self.live.read().await.get(session_id).cloned();
        let active_run = match managed {
            Some(managed) => managed.harness.active_run().await,
            None => None,
        };
        let execution = active_run
            .as_ref()
            .and_then(|run_id| self.execution_activity.for_run(session_id, run_id));
        Some(ternilo_protocol::SessionLiveActivity {
            session_id: session.identity.session_id,
            running: active_run.is_some(),
            updated_at_ms: session.updated_at_ms,
            execution,
        })
    }

    pub async fn sidebar_ordering(&self) -> Result<SidebarOrdering, HarnessError> {
        self.preferences.sidebar_ordering().await
    }

    pub async fn set_sidebar_ordering(
        &self,
        ordering: SidebarOrdering,
    ) -> Result<SidebarOrdering, HarnessError> {
        let ordering = self.preferences.set_sidebar_ordering(ordering).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(ordering)
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<LocalEventNotification> {
        self.event_notifications.subscribe()
    }

    pub fn subscribe_invalidations(
        &self,
    ) -> broadcast::Receiver<crate::LocalInvalidationNotification> {
        self.invalidations.subscribe()
    }

    fn invalidate(
        &self,
        session_id: Option<&str>,
        category: crate::LocalInvalidationCategory,
        revision: Option<u64>,
    ) {
        publish_invalidation(&self.invalidations, session_id, category, revision);
    }

    fn next_id(&self, prefix: &str) -> Result<String, HarnessError> {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(format!("{prefix}-{}-{sequence}", now_ms()?))
    }
}

async fn acquire_data_lock(root: &Path) -> Result<File, HarnessError> {
    tokio::fs::create_dir_all(root).await.map_err(|error| {
        HarnessError::execution(format!(
            "create Ternilo data directory {}: {error}",
            root.display()
        ))
    })?;
    let path = root.join(".writer.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            HarnessError::execution(format!(
                "open data directory lock {}: {error}",
                path.display()
            ))
        })?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(HarnessError::policy(format!(
            "Ternilo data directory {} is already open by another process",
            root.display()
        ))),
        Err(std::fs::TryLockError::Error(error)) => Err(HarnessError::execution(format!(
            "lock Ternilo data directory {}: {error}",
            root.display()
        ))),
    }
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(test)]
mod tests;
