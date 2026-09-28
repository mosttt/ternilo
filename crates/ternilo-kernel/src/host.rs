use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use linorun_core::CallContext;
use ternilo_protocol::{
    AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamSnapshot, AgentTeamTask,
    AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace, Attachment, HarnessError,
    ModelRequest, ModelResponse, PermissionPreset, ReferenceContext, RunId, RunLimits, RunOutcome,
    SessionEvent, SessionEventReadRequest, SessionId, SessionIdentity, SessionMode,
    SessionSearchHit, SessionSearchRequest, SessionTelemetryChannel, SessionTelemetryRecord,
    SessionTelemetrySeverity, SessionTelemetrySharingStatus, SessionTrace, SubagentSnapshot,
    SubmissionReference, UserAnswer, UserQuestion, WorkspaceBinding,
};

use crate::{
    ActivityBranch, AgentTeamProvider, AttachmentsProvider, ExecutionActivityOutput,
    ExecutionAdmission, InputReferenceResolver, ModelGatewayProvider, ModelOutput, RunCancellation,
    RunEnvironmentProvider, RuntimeExtensionsProvider, SessionQueriesProvider,
    SessionTelemetryBackendRegistration, SessionTelemetryProvider, SessionTelemetryRedactor,
    SubagentRunStart, SubagentSessionBinding, SubagentSessionRequest, ToolAuthorization,
    ToolEffect, WorkspaceExecution, WorkspaceExecutionLease,
};

struct UnavailableModelGateway;

impl ModelGatewayProvider for UnavailableModelGateway {
    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        _: ternilo_protocol::RunModelBinding,
        _: ModelRequest,
        _: Arc<dyn ModelOutput>,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            Err(HarnessError::policy(
                "this host does not provide an external model gateway",
            ))
        })
    }
}

struct UnavailableAgentTeam;

impl AgentTeamProvider for UnavailableAgentTeam {
    fn snapshot<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }

    fn create_task<'a>(
        &'a self,
        _: CallContext<()>,
        _: AgentTeamTaskCreate,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }

    fn replace_task<'a>(
        &'a self,
        _: CallContext<()>,
        _: AgentTeamTaskId,
        _: AgentTeamTaskReplace,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamTask, HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }

    fn delete_task<'a>(
        &'a self,
        _: CallContext<()>,
        _: AgentTeamTaskId,
        _: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }

    fn send_message<'a>(
        &'a self,
        _: CallContext<()>,
        _: AgentTeamMessageSend,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }

    fn mark_message_read<'a>(
        &'a self,
        _: CallContext<()>,
        _: AgentTeamMessageId,
    ) -> Pin<Box<dyn Future<Output = Result<AgentTeamMessage, HarnessError>> + Send + 'a>> {
        Box::pin(async { Err(agent_team_unavailable()) })
    }
}

fn agent_team_unavailable() -> HarnessError {
    HarnessError::policy("capability unavailable: this host has no persistent Agent Team")
}

#[derive(Clone, Debug)]
pub struct HostPolicy {
    pub limits: RunLimits,
    pub denied_tools: BTreeSet<String>,
    pub permissions: PermissionPreset,
    pub allow_mutating_tools: bool,
}

impl HostPolicy {
    #[must_use]
    pub fn local(limits: RunLimits) -> Self {
        Self {
            limits,
            denied_tools: BTreeSet::new(),
            permissions: PermissionPreset::WorkspaceWrite,
            allow_mutating_tools: true,
        }
    }
}

pub trait UserInteraction: Send + Sync + 'static {
    fn ask<'a>(
        &'a self,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>>;
}

pub trait SecretResolver: Send + Sync + 'static {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, HarnessError>> + Send + 'a>>;
}

pub trait AttachmentResolver: Send + Sync + 'static {
    fn store<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>>;

    fn resolve<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>>;
}

struct InlineAttachments;

impl AttachmentResolver for InlineAttachments {
    fn store<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            attachment.validate()?;
            if attachment.is_reference() {
                return Err(HarnessError::execution(
                    "this host cannot retain attachment references",
                ));
            }
            Ok(attachment)
        })
    }

    fn resolve<'a>(
        &'a self,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            attachment.validate()?;
            if attachment.is_reference() {
                return Err(HarnessError::execution(
                    "this host cannot resolve durable attachment references",
                ));
            }
            Ok(attachment)
        })
    }
}

struct EnvironmentSecrets;

impl SecretResolver for EnvironmentSecrets {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(HarnessError::execution(format!(
                    "read credential environment variable {name:?}: {error}"
                ))),
            }
        })
    }
}

struct NonInteractive;

impl UserInteraction for NonInteractive {
    fn ask<'a>(
        &'a self,
        _: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host has no interactive user-answer channel",
            ))
        })
    }
}

pub trait SessionEventStore: Send + Sync + 'static {
    fn load<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>>;

    fn append<'a>(
        &'a self,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

pub trait SubagentSessionHost: Send + Sync + 'static {
    fn create<'a>(
        &'a self,
        parent: SessionIdentity,
        request: SubagentSessionRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SubagentSessionBinding>, HarnessError>> + Send + 'a>,
    >;

    fn run<'a>(
        &'a self,
        session_id: SessionId,
        run_id: RunId,
        input: String,
        cancellation: RunCancellation,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>>;

    fn append_lifecycle<'a>(
        &'a self,
        session_id: SessionId,
        run_id: RunId,
        snapshot: SubagentSnapshot,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>>;
}

struct UnavailableSubagentSessionHost;

impl SubagentSessionHost for UnavailableSubagentSessionHost {
    fn create<'a>(
        &'a self,
        _: SessionIdentity,
        _: SubagentSessionRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SubagentSessionBinding>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async { Ok(None) })
    }

    fn run<'a>(
        &'a self,
        _: SessionId,
        _: RunId,
        _: String,
        _: RunCancellation,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let error =
                HarnessError::policy("this host does not provide canonical Subagent Sessions");
            start.resolve(Err(error.clone()));
            Err(error)
        })
    }

    fn append_lifecycle<'a>(
        &'a self,
        _: SessionId,
        _: RunId,
        _: SubagentSnapshot,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not provide canonical Subagent Sessions",
            ))
        })
    }
}

pub trait SessionArchive: Send + Sync + 'static {
    fn search<'a>(
        &'a self,
        requester: SessionIdentity,
        request: SessionSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionSearchHit>, HarnessError>> + Send + 'a>>;

    fn read_events<'a>(
        &'a self,
        requester: SessionIdentity,
        request: SessionEventReadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>>;

    fn trace<'a>(
        &'a self,
        requester: SessionIdentity,
        session_id: SessionId,
    ) -> Pin<Box<dyn Future<Output = Result<SessionTrace, HarnessError>> + Send + 'a>>;
}

struct UnavailableSessionArchive;

impl SessionArchive for UnavailableSessionArchive {
    fn search<'a>(
        &'a self,
        _: SessionIdentity,
        _: SessionSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionSearchHit>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not expose a session archive",
            ))
        })
    }

    fn read_events<'a>(
        &'a self,
        _: SessionIdentity,
        _: SessionEventReadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not expose a session archive",
            ))
        })
    }

    fn trace<'a>(
        &'a self,
        _: SessionIdentity,
        _: SessionId,
    ) -> Pin<Box<dyn Future<Output = Result<SessionTrace, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not expose a session archive",
            ))
        })
    }
}

struct UnavailableRuntimeExtensions;

impl RuntimeExtensionsProvider for UnavailableRuntimeExtensions {
    fn inspect<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Ok(serde_json::json!({
                "supported": false,
                "message": "this host does not expose a mutable runtime extension inventory"
            }))
        })
    }

    fn set_enabled<'a>(
        &'a self,
        _: CallContext<()>,
        _: String,
        _: String,
        _: bool,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not permit runtime extension changes",
            ))
        })
    }

    fn revoke<'a>(
        &'a self,
        _: CallContext<()>,
        _: String,
        _: String,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not permit runtime extension changes",
            ))
        })
    }

    fn set_mounted<'a>(
        &'a self,
        _: CallContext<()>,
        _: String,
        _: String,
        _: String,
        _: bool,
        _: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            Err(HarnessError::policy(
                "this host does not permit runtime extension changes",
            ))
        })
    }
}

#[derive(Default)]
pub struct MemoryEventStore {
    events: Mutex<Vec<SessionEvent>>,
}

impl SessionEventStore for MemoryEventStore {
    fn load<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .map_err(|_| HarnessError::execution("memory event store lock poisoned"))
                .map(|events| events.clone())
        })
    }

    fn append<'a>(
        &'a self,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.events
                .lock()
                .map_err(|_| HarnessError::execution("memory event store lock poisoned"))?
                .push(event);
            Ok(())
        })
    }
}

#[derive(Default)]
struct TelemetryState {
    next: u64,
    backend: Option<(u64, SessionTelemetryBackendRegistration)>,
    redactors: BTreeMap<u64, Arc<dyn SessionTelemetryRedactor>>,
    emitted_chunks: BTreeSet<(TelemetrySessionKey, String, u32)>,
    feedback_sessions: BTreeMap<TelemetrySessionKey, FeedbackSessionCapture>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TelemetrySessionKey {
    tenant: String,
    user: String,
    agent: String,
    session: String,
}

impl From<&SessionIdentity> for TelemetrySessionKey {
    fn from(identity: &SessionIdentity) -> Self {
        Self {
            tenant: identity.tenant_id.as_str().to_owned(),
            user: identity.user_id.as_str().to_owned(),
            agent: identity.agent_id.as_str().to_owned(),
            session: identity.session_id.as_str().to_owned(),
        }
    }
}

#[derive(Default)]
struct FeedbackSessionCapture {
    handoff_cursor: Option<u64>,
    pending: Vec<SessionEvent>,
}

/// Per-harness telemetry coordinator. A deployment plugin may install one
/// backend and any number of copy-only redactors. With no backend it is a
/// zero-cost, explicitly disclosed `disabled` capability.
#[derive(Default)]
pub struct HostSessionTelemetry {
    state: Mutex<TelemetryState>,
}

impl HostSessionTelemetry {
    fn next_id(state: &mut TelemetryState) -> Result<u64, HarnessError> {
        let id = state.next;
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("telemetry registration id exhausted"))?;
        Ok(id)
    }

    #[must_use]
    pub fn sharing_status(&self) -> SessionTelemetrySharingStatus {
        self.state
            .lock()
            .expect("telemetry coordinator lock poisoned")
            .backend
            .as_ref()
            .map_or(SessionTelemetrySharingStatus::Disabled, |(_, backend)| {
                backend.sharing
            })
    }

    fn capture_committed(&self, identity: &SessionIdentity, event: SessionEvent) {
        let selected = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some((_, backend)) = state.backend.as_ref() else {
                return;
            };
            let sharing = backend.sharing;
            let sink = Arc::clone(&backend.sink);
            let session_key = TelemetrySessionKey::from(identity);
            let events = match sharing {
                SessionTelemetrySharingStatus::Disabled => Vec::new(),
                SessionTelemetrySharingStatus::FeedbackOnly => {
                    feedback_suffix(&mut state, &session_key, event)
                }
                SessionTelemetrySharingStatus::Full => vec![event],
            }
            .into_iter()
            .filter(|event| project_telemetry_event(&mut state, &session_key, event))
            .collect::<Vec<_>>();
            if events.is_empty() {
                return;
            }
            Some((
                sink,
                state.redactors.values().cloned().collect::<Vec<_>>(),
                events,
            ))
        };
        let Some((sink, redactors, events)) = selected else {
            return;
        };
        for event in events {
            let Some(record) = session_telemetry_record(identity, &event) else {
                continue;
            };
            let mut record = Some(record);
            for redactor in &redactors {
                record = record.and_then(|record| redactor.redact(record));
                if record.is_none() {
                    break;
                }
            }
            if let Some(record) = record {
                sink.emit(record);
            }
        }
    }
}

fn feedback_suffix(
    state: &mut TelemetryState,
    session_key: &TelemetrySessionKey,
    event: SessionEvent,
) -> Vec<SessionEvent> {
    let release = matches!(
        &event.kind,
        ternilo_protocol::SessionEventKind::FeedbackRecorded { .. }
            | ternilo_protocol::SessionEventKind::FeedbackSubmitted { .. }
    );
    let session = state
        .feedback_sessions
        .entry(session_key.clone())
        .or_default();
    if session
        .handoff_cursor
        .is_some_and(|cursor| event.seq <= cursor)
        || session
            .pending
            .last()
            .is_some_and(|pending| event.seq <= pending.seq)
    {
        return Vec::new();
    }
    session.pending.push(event);
    if !release {
        return Vec::new();
    }
    session.handoff_cursor = session.pending.last().map(|event| event.seq);
    std::mem::take(&mut session.pending)
}

fn project_telemetry_event(
    state: &mut TelemetryState,
    session_key: &TelemetrySessionKey,
    event: &SessionEvent,
) -> bool {
    match &event.kind {
        ternilo_protocol::SessionEventKind::AssistantMessageDelta { step, .. }
        | ternilo_protocol::SessionEventKind::AssistantReasoningDelta { step, .. } => state
            .emitted_chunks
            .insert((session_key.clone(), event.run_id.as_str().to_owned(), *step)),
        _ => true,
    }
}

impl SessionTelemetryProvider for HostSessionTelemetry {
    fn register_backend<'a>(
        &'a self,
        _: CallContext<()>,
        backend: SessionTelemetryBackendRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("telemetry coordinator lock poisoned"))?;
            if state.backend.is_some() {
                return Err(HarnessError::composition(
                    "a session telemetry backend is already registered",
                ));
            }
            let id = Self::next_id(&mut state)?;
            state.backend = Some((id, backend));
            Ok(id)
        })
    }

    fn unregister_backend<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let sink = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| HarnessError::execution("telemetry coordinator lock poisoned"))?;
                let (registered, _) = state.backend.as_ref().ok_or_else(|| {
                    HarnessError::execution("no session telemetry backend is registered")
                })?;
                if *registered != registration {
                    return Err(HarnessError::execution(format!(
                        "unknown telemetry backend registration {registration}"
                    )));
                }
                state.emitted_chunks.clear();
                state.feedback_sessions.clear();
                state
                    .backend
                    .take()
                    .expect("backend was checked above")
                    .1
                    .sink
            };
            sink.shutdown().await;
            Ok(())
        })
    }

    fn register_redactor<'a>(
        &'a self,
        _: CallContext<()>,
        redactor: Arc<dyn SessionTelemetryRedactor>,
    ) -> Pin<Box<dyn Future<Output = u64> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .expect("telemetry coordinator lock poisoned");
            let id = Self::next_id(&mut state).expect("telemetry registration id exhausted");
            state.redactors.insert(id, redactor);
            id
        })
    }

    fn unregister_redactor<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.state
                .lock()
                .map_err(|_| HarnessError::execution("telemetry coordinator lock poisoned"))?
                .redactors
                .remove(&registration)
                .map(|_| ())
                .ok_or_else(|| {
                    HarnessError::execution(format!(
                        "unknown telemetry redactor registration {registration}"
                    ))
                })
        })
    }

    fn sharing<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = SessionTelemetrySharingStatus> + Send + 'a>> {
        Box::pin(async move {
            self.state
                .lock()
                .expect("telemetry coordinator lock poisoned")
                .backend
                .as_ref()
                .map_or(SessionTelemetrySharingStatus::Disabled, |(_, backend)| {
                    backend.sharing
                })
        })
    }

    fn capture<'a>(
        &'a self,
        _: CallContext<()>,
        identity: SessionIdentity,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.capture_committed(&identity, event);
        })
    }
}

/// Project one canonical Session event into the shared telemetry wire record.
///
/// Durable runtimes use this same projection outside an in-process Harness so
/// the Local, Node, and Cloud exporters keep one record shape.
#[must_use]
pub fn session_telemetry_record(
    identity: &SessionIdentity,
    event: &SessionEvent,
) -> Option<SessionTelemetryRecord> {
    let body = serde_json::to_value(&event.kind).ok()?;
    let event_type = body.get("type")?.as_str()?.to_owned();
    let severity = match &event.kind {
        ternilo_protocol::SessionEventKind::TurnFailed { .. }
        | ternilo_protocol::SessionEventKind::TurnCancelled => SessionTelemetrySeverity::Error,
        ternilo_protocol::SessionEventKind::ToolCallFinished { output, .. }
        | ternilo_protocol::SessionEventKind::CodeDispatchFinished { output, .. }
            if output.is_error =>
        {
            SessionTelemetrySeverity::Error
        }
        _ => SessionTelemetrySeverity::Info,
    };
    let attributes = BTreeMap::from([
        (
            "tenant.id".to_owned(),
            serde_json::Value::String(identity.tenant_id.as_str().to_owned()),
        ),
        (
            "user.id".to_owned(),
            serde_json::Value::String(identity.user_id.as_str().to_owned()),
        ),
        (
            "session.id".to_owned(),
            serde_json::Value::String(identity.session_id.as_str().to_owned()),
        ),
        (
            "agent.id".to_owned(),
            serde_json::Value::String(identity.agent_id.as_str().to_owned()),
        ),
        (
            "run.id".to_owned(),
            serde_json::Value::String(event.run_id.as_str().to_owned()),
        ),
        (
            "event.type".to_owned(),
            serde_json::Value::String(event_type),
        ),
        ("event.seq".to_owned(), serde_json::Value::from(event.seq)),
    ]);
    Some(SessionTelemetryRecord {
        channel: SessionTelemetryChannel::Ledger,
        time_ms: event.occurred_at_ms,
        severity,
        attributes,
        body,
    })
}

pub struct HostEnvironment {
    identity: SessionIdentity,
    workspace: Option<WorkspaceBinding>,
    workspace_execution: Option<Arc<dyn WorkspaceExecution>>,
    execution_admission: Option<(RunId, Arc<dyn ExecutionAdmission>)>,
    input_reference_resolver: Option<Arc<dyn InputReferenceResolver>>,
    session_mode: SessionMode,
    policy: HostPolicy,
    event_store: Arc<dyn SessionEventStore>,
    interaction: Arc<dyn UserInteraction>,
    secrets: Arc<dyn SecretResolver>,
    attachments: Arc<dyn AttachmentResolver>,
    session_archive: Arc<dyn SessionArchive>,
    runtime_extensions: Arc<dyn RuntimeExtensionsProvider>,
    model_gateway: Arc<dyn ModelGatewayProvider>,
    subagent_sessions: Arc<dyn SubagentSessionHost>,
    agent_team: Arc<dyn AgentTeamProvider>,
    telemetry: Arc<HostSessionTelemetry>,
}

impl HostEnvironment {
    #[must_use]
    pub fn new(
        identity: SessionIdentity,
        workspace: Option<WorkspaceBinding>,
        policy: HostPolicy,
        event_store: Arc<dyn SessionEventStore>,
    ) -> Self {
        Self::with_interaction(
            identity,
            workspace,
            policy,
            event_store,
            Arc::new(NonInteractive),
        )
    }

    #[must_use]
    pub fn with_interaction(
        identity: SessionIdentity,
        workspace: Option<WorkspaceBinding>,
        policy: HostPolicy,
        event_store: Arc<dyn SessionEventStore>,
        interaction: Arc<dyn UserInteraction>,
    ) -> Self {
        Self::with_interaction_and_secrets(
            identity,
            workspace,
            policy,
            event_store,
            interaction,
            Arc::new(EnvironmentSecrets),
        )
    }

    #[must_use]
    pub fn with_interaction_and_secrets(
        identity: SessionIdentity,
        workspace: Option<WorkspaceBinding>,
        policy: HostPolicy,
        event_store: Arc<dyn SessionEventStore>,
        interaction: Arc<dyn UserInteraction>,
        secrets: Arc<dyn SecretResolver>,
    ) -> Self {
        Self {
            identity,
            workspace,
            workspace_execution: None,
            execution_admission: None,
            input_reference_resolver: None,
            session_mode: SessionMode::Execute,
            policy,
            event_store,
            interaction,
            secrets,
            attachments: Arc::new(InlineAttachments),
            session_archive: Arc::new(UnavailableSessionArchive),
            runtime_extensions: Arc::new(UnavailableRuntimeExtensions),
            model_gateway: Arc::new(UnavailableModelGateway),
            subagent_sessions: Arc::new(UnavailableSubagentSessionHost),
            agent_team: Arc::new(UnavailableAgentTeam),
            telemetry: Arc::new(HostSessionTelemetry::default()),
        }
    }

    #[must_use]
    pub fn with_attachment_resolver(mut self, attachments: Arc<dyn AttachmentResolver>) -> Self {
        self.attachments = attachments;
        self
    }

    #[must_use]
    pub fn with_execution_admission(
        mut self,
        run_id: RunId,
        admission: Arc<dyn ExecutionAdmission>,
    ) -> Self {
        self.execution_admission = Some((run_id, admission));
        self
    }

    #[must_use]
    pub fn with_workspace_execution(mut self, execution: Arc<dyn WorkspaceExecution>) -> Self {
        self.workspace_execution = Some(execution);
        self
    }

    #[must_use]
    pub fn with_input_reference_resolver(
        mut self,
        resolver: Arc<dyn InputReferenceResolver>,
    ) -> Self {
        self.input_reference_resolver = Some(resolver);
        self
    }

    #[must_use]
    pub fn with_session_mode(mut self, mode: SessionMode) -> Self {
        self.session_mode = mode;
        self
    }

    #[must_use]
    pub fn with_session_archive(mut self, session_archive: Arc<dyn SessionArchive>) -> Self {
        self.session_archive = session_archive;
        self
    }

    #[must_use]
    pub fn with_runtime_extensions(
        mut self,
        runtime_extensions: Arc<dyn RuntimeExtensionsProvider>,
    ) -> Self {
        self.runtime_extensions = runtime_extensions;
        self
    }

    #[must_use]
    pub fn with_model_gateway(mut self, gateway: Arc<dyn ModelGatewayProvider>) -> Self {
        self.model_gateway = gateway;
        self
    }

    #[must_use]
    pub fn with_subagent_sessions(mut self, host: Arc<dyn SubagentSessionHost>) -> Self {
        self.subagent_sessions = host;
        self
    }

    #[must_use]
    pub fn with_agent_team(mut self, provider: Arc<dyn AgentTeamProvider>) -> Self {
        self.agent_team = provider;
        self
    }

    pub(crate) fn agent_team(&self) -> Arc<dyn AgentTeamProvider> {
        Arc::clone(&self.agent_team)
    }

    pub(crate) fn model_gateway(&self) -> Arc<dyn ModelGatewayProvider> {
        Arc::clone(&self.model_gateway)
    }

    #[must_use]
    pub fn with_session_telemetry(mut self, telemetry: Arc<HostSessionTelemetry>) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub(crate) fn session_telemetry(&self) -> Arc<HostSessionTelemetry> {
        Arc::clone(&self.telemetry)
    }

    #[must_use]
    pub fn memory(
        identity: SessionIdentity,
        workspace: Option<WorkspaceBinding>,
        policy: HostPolicy,
    ) -> Self {
        Self::new(
            identity,
            workspace,
            policy,
            Arc::new(MemoryEventStore::default()),
        )
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        self.identity.validate()?;
        if let Some(workspace) = &self.workspace {
            workspace.validate()?;
        }
        Ok(())
    }
}

impl AttachmentsProvider for HostEnvironment {
    fn store<'a>(
        &'a self,
        _: CallContext<()>,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        self.attachments.store(attachment)
    }

    fn resolve<'a>(
        &'a self,
        _: CallContext<()>,
        attachment: Attachment,
    ) -> Pin<Box<dyn Future<Output = Result<Attachment, HarnessError>> + Send + 'a>> {
        self.attachments.resolve(attachment)
    }
}

impl RunEnvironmentProvider for HostEnvironment {
    fn begin_activity<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
        cancellation: RunCancellation,
        output: Arc<dyn ExecutionActivityOutput>,
    ) -> Pin<Box<dyn Future<Output = Result<ActivityBranch, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            run_id.validate()?;
            match &self.execution_admission {
                Some((expected, admission)) if *expected == run_id => {
                    Ok(ActivityBranch::managed_with_output(
                        Arc::clone(admission),
                        cancellation,
                        output,
                    ))
                }
                Some(_) => Err(HarnessError::policy(
                    "execution admission belongs to another run",
                )),
                None => Ok(ActivityBranch::untracked()),
            }
        })
    }

    fn resolve_input_references<'a>(
        &'a self,
        _: CallContext<()>,
        references: Vec<SubmissionReference>,
        prepared_contexts: Vec<ReferenceContext>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ReferenceContext>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            match &self.input_reference_resolver {
                Some(resolver) => resolver.resolve(references).await,
                None => Ok(prepared_contexts),
            }
        })
    }

    fn try_acquire_workspace<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<WorkspaceExecutionLease>, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match &self.workspace_execution {
                Some(execution) => execution.try_acquire().await,
                None => Ok(Some(WorkspaceExecutionLease::hold(()))),
            }
        })
    }

    fn acquire_workspace<'a>(
        &'a self,
        _: CallContext<()>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<WorkspaceExecutionLease, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            cancellation.check()?;
            match &self.workspace_execution {
                Some(execution) => execution.acquire(cancellation).await,
                None => Ok(WorkspaceExecutionLease::hold(())),
            }
        })
    }

    fn identity<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = SessionIdentity> + Send + 'a>> {
        let identity = self.identity.clone();
        Box::pin(async move { identity })
    }

    fn workspace<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<WorkspaceBinding>> + Send + 'a>> {
        let workspace = self.workspace.clone();
        Box::pin(async move { workspace })
    }

    fn session_mode<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = SessionMode> + Send + 'a>> {
        let mode = self.session_mode;
        Box::pin(async move { mode })
    }

    fn permissions<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = PermissionPreset> + Send + 'a>> {
        let permissions = self.policy.permissions;
        Box::pin(async move { permissions })
    }

    fn resolve_secret<'a>(
        &'a self,
        _: CallContext<()>,
        name: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.secrets.resolve(&name).await })
    }

    fn limits<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = RunLimits> + Send + 'a>> {
        let limits = self.policy.limits;
        Box::pin(async move { limits })
    }

    fn check_tool<'a>(
        &'a self,
        _: CallContext<()>,
        call: ternilo_protocol::ToolCall,
        effect: ToolEffect,
    ) -> Pin<Box<dyn Future<Output = Result<ToolAuthorization, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mutating = matches!(effect, ToolEffect::Mutating | ToolEffect::Dangerous);
            if self.policy.denied_tools.contains(&call.name)
                || (!self.policy.allow_mutating_tools && mutating)
                || (!self.policy.permissions.allows_workspace_write() && mutating)
            {
                return Err(HarnessError::policy(format!(
                    "tool {:?} is denied by host policy",
                    call.name
                )));
            }

            let shell_requests_full_access = call.name == "shell"
                && call
                    .arguments
                    .get("full_access")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
            let needs_dangerous_approval = effect == ToolEffect::Dangerous;
            let needs_sandbox_escalation =
                shell_requests_full_access && !self.policy.permissions.allows_full_access();
            if needs_dangerous_approval || needs_sandbox_escalation {
                let reason = if needs_sandbox_escalation {
                    "the shell call requests one-time access outside the workspace sandbox"
                        .to_owned()
                } else {
                    format!("tool {:?} can change host or external state", call.name)
                };
                return Ok(ToolAuthorization::Ask { reason });
            }

            Ok(ToolAuthorization::Allow)
        })
    }

    fn ask_user<'a>(
        &'a self,
        _: CallContext<()>,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
        self.interaction.ask(question)
    }

    fn load_events<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        self.event_store.load()
    }

    fn commit_event<'a>(
        &'a self,
        _: CallContext<()>,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        self.event_store.append(event)
    }

    fn create_subagent_session<'a>(
        &'a self,
        _: CallContext<()>,
        request: SubagentSessionRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<SubagentSessionBinding>, HarnessError>> + Send + 'a>,
    > {
        self.subagent_sessions
            .create(self.identity.clone(), request)
    }

    fn run_subagent_session<'a>(
        &'a self,
        _: CallContext<()>,
        session_id: SessionId,
        run_id: RunId,
        input: String,
        cancellation: RunCancellation,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>> {
        self.subagent_sessions
            .run(session_id, run_id, input, cancellation, start)
    }

    fn append_subagent_lifecycle<'a>(
        &'a self,
        _: CallContext<()>,
        session_id: SessionId,
        run_id: RunId,
        snapshot: SubagentSnapshot,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>> {
        self.subagent_sessions
            .append_lifecycle(session_id, run_id, snapshot)
    }
}

impl SessionQueriesProvider for HostEnvironment {
    fn search<'a>(
        &'a self,
        _: CallContext<()>,
        request: SessionSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionSearchHit>, HarnessError>> + Send + 'a>>
    {
        self.session_archive.search(self.identity.clone(), request)
    }

    fn read_events<'a>(
        &'a self,
        _: CallContext<()>,
        request: SessionEventReadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        self.session_archive
            .read_events(self.identity.clone(), request)
    }

    fn trace<'a>(
        &'a self,
        _: CallContext<()>,
        session_id: SessionId,
    ) -> Pin<Box<dyn Future<Output = Result<SessionTrace, HarnessError>> + Send + 'a>> {
        self.session_archive
            .trace(self.identity.clone(), session_id)
    }
}

impl RuntimeExtensionsProvider for HostEnvironment {
    fn inspect<'a>(
        &'a self,
        context: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        self.runtime_extensions.inspect(context)
    }

    fn set_enabled<'a>(
        &'a self,
        context: CallContext<()>,
        package_id: String,
        version: String,
        enabled: bool,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        self.runtime_extensions
            .set_enabled(context, package_id, version, enabled)
    }

    fn revoke<'a>(
        &'a self,
        context: CallContext<()>,
        package_id: String,
        version: String,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        self.runtime_extensions.revoke(context, package_id, version)
    }

    fn set_mounted<'a>(
        &'a self,
        context: CallContext<()>,
        session_id: String,
        package_id: String,
        version: String,
        mounted: bool,
        settings: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, HarnessError>> + Send + 'a>> {
        self.runtime_extensions
            .set_mounted(context, session_id, package_id, version, mounted, settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionTelemetrySink;
    use ternilo_protocol::{AgentId, RunId, SessionEventKind, TenantId, UserId};

    #[derive(Default)]
    struct CollectingSink {
        records: Mutex<Vec<SessionTelemetryRecord>>,
    }

    impl SessionTelemetrySink for CollectingSink {
        fn emit(&self, record: SessionTelemetryRecord) {
            self.records.lock().expect("records lock").push(record);
        }

        fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            Box::pin(async {})
        }
    }

    fn identity(session_id: &str) -> SessionIdentity {
        SessionIdentity {
            tenant_id: TenantId::new("tenant"),
            user_id: UserId::new("user"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new(session_id),
        }
    }

    fn event(seq: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq,
            occurred_at_ms: seq + 1,
            run_id: RunId::new("shared-run"),
            kind,
        }
    }

    fn coordinator(
        sharing: SessionTelemetrySharingStatus,
    ) -> (HostSessionTelemetry, Arc<CollectingSink>) {
        let telemetry = HostSessionTelemetry::default();
        let sink = Arc::new(CollectingSink::default());
        telemetry.state.lock().expect("telemetry lock").backend = Some((
            0,
            SessionTelemetryBackendRegistration {
                sharing,
                sink: sink.clone(),
            },
        ));
        (telemetry, sink)
    }

    fn emitted_coordinates(sink: &CollectingSink) -> Vec<(String, u64)> {
        sink.records
            .lock()
            .expect("records lock")
            .iter()
            .map(|record| {
                (
                    record.attributes["session.id"]
                        .as_str()
                        .expect("session id")
                        .to_owned(),
                    record.attributes["event.seq"].as_u64().expect("event seq"),
                )
            })
            .collect()
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "Keep the canonical feedback suffix lifecycle scenario together."
    )]
    fn feedback_only_releases_each_canonical_suffix_once_per_session() {
        let (telemetry, sink) = coordinator(SessionTelemetrySharingStatus::FeedbackOnly);
        let session_a = identity("session-a");
        let session_b = identity("session-b");

        telemetry.capture_committed(&session_a, event(0, SessionEventKind::TurnStarted));
        telemetry.capture_committed(
            &session_a,
            event(
                1,
                SessionEventKind::AssistantMessageDelta {
                    step: 1,
                    delta: "first".to_owned(),
                },
            ),
        );
        telemetry.capture_committed(
            &session_a,
            event(
                2,
                SessionEventKind::AssistantMessageDelta {
                    step: 1,
                    delta: "continuation".to_owned(),
                },
            ),
        );
        telemetry.capture_committed(
            &session_a,
            event(
                3,
                SessionEventKind::TurnFinished {
                    answer: "done".to_owned(),
                    finish_reason: ternilo_protocol::TurnFinishReason::Completed,
                },
            ),
        );
        telemetry.capture_committed(&session_b, event(0, SessionEventKind::TurnStarted));
        assert!(emitted_coordinates(&sink).is_empty());

        telemetry.capture_committed(
            &session_a,
            event(
                4,
                SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-a-1".to_owned(),
                    text: "useful".to_owned(),
                },
            ),
        );
        assert_eq!(
            emitted_coordinates(&sink),
            vec![
                ("session-a".to_owned(), 0),
                ("session-a".to_owned(), 1),
                ("session-a".to_owned(), 3),
                ("session-a".to_owned(), 4),
            ]
        );

        telemetry.capture_committed(
            &session_a,
            event(
                5,
                SessionEventKind::UserMessage {
                    provenance: None,
                    content: "next".to_owned(),
                    display_content: None,
                    source: None,
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            ),
        );
        telemetry.capture_committed(
            &session_a,
            event(
                6,
                SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-a-2".to_owned(),
                    text: "second".to_owned(),
                },
            ),
        );
        telemetry.capture_committed(
            &session_b,
            event(
                1,
                SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-b-1".to_owned(),
                    text: "other session".to_owned(),
                },
            ),
        );
        assert_eq!(
            emitted_coordinates(&sink),
            vec![
                ("session-a".to_owned(), 0),
                ("session-a".to_owned(), 1),
                ("session-a".to_owned(), 3),
                ("session-a".to_owned(), 4),
                ("session-a".to_owned(), 5),
                ("session-a".to_owned(), 6),
                ("session-b".to_owned(), 0),
                ("session-b".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn full_chunk_projection_is_scoped_by_session() {
        let (telemetry, sink) = coordinator(SessionTelemetrySharingStatus::Full);
        let delta = |text: &str| SessionEventKind::AssistantMessageDelta {
            step: 1,
            delta: text.to_owned(),
        };
        telemetry.capture_committed(&identity("session-a"), event(0, delta("a-first")));
        telemetry.capture_committed(&identity("session-a"), event(1, delta("a-next")));
        telemetry.capture_committed(&identity("session-b"), event(0, delta("b-first")));

        assert_eq!(
            emitted_coordinates(&sink),
            vec![("session-a".to_owned(), 0), ("session-b".to_owned(), 0),]
        );
    }
}
