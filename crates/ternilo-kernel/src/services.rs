use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use linorun_macros::service_contract;
use serde_json::Value;
use ternilo_protocol::{
    AcceptedSubagentRun, AgentInput, AgentTeamMessage, AgentTeamMessageId, AgentTeamMessageSend,
    AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskCreate, AgentTeamTaskId, AgentTeamTaskReplace,
    Attachment, CommandDescriptor, ContextCompaction, FileContent, FileListRequest, FileListResult,
    FileReadRequest, FileReplaceRequest, FileReplaceResult, FileSearchRequest, FileSearchResult,
    FileWriteRequest, FileWriteResult, HarnessError, HookRequest, HookResult, JobId, JobSnapshot,
    ModelMessage, ModelRequest, ModelResponse, ModelRetryFailure, PermissionPreset, PromptSection,
    ReferenceContext, RunId, RunLimits, RunOutcome, SessionEvent, SessionEventKind,
    SessionEventReadRequest, SessionIdentity, SessionMode, SessionSearchHit, SessionSearchRequest,
    SessionTelemetryRecord, SessionTelemetrySharingStatus, SessionTrace, ShellRequest, ShellResult,
    SkillCatalogSnapshot, SkillDefinition, SkillSummary, SteeringInput, SubagentId,
    SubagentSnapshot, SubagentTranscriptKind, SubmissionReference, TerminalId, TerminalRead,
    TerminalSnapshot, ToolCall, ToolOutput, ToolPresentationDescriptor, ToolSpec, UserAnswer,
    UserQuestion, WorkflowMeta, WorkflowRunId, WorkflowStopReason, WorkspaceBinding,
};
use tokio::sync::Notify;

use crate::{ActivityBranch, DeferredToolSource, ExecutionActivityOutput, WorkspaceExecutionLease};

#[allow(clippy::trivially_copy_pass_by_ref)]
fn merge_unit(_: &(), _: &()) {}

#[derive(Clone, Debug, Default)]
pub struct RunCancellation {
    state: Arc<RunCancellationState>,
}

#[derive(Debug, Default)]
struct RunCancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl RunCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if !self.state.cancelled.swap(true, Ordering::AcqRel) {
            self.state.notify.notify_waiters();
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    pub fn check(&self) -> Result<(), HarnessError> {
        if self.is_cancelled() {
            Err(HarnessError::cancelled("run was cancelled"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubagentAdmission {
    Direct,
    Scheduled(AcceptedSubagentRun),
}

#[derive(Clone, Debug, Default)]
pub struct SubagentRunStart {
    pub provenance: Option<ternilo_protocol::InputProvenance>,
    state: Arc<SubagentRunStartState>,
}

#[derive(Debug, Default)]
struct SubagentRunStartState {
    status: Mutex<SubagentStartStatus>,
    notify: Notify,
}

#[derive(Debug, Default)]
struct SubagentStartStatus {
    result: Option<Result<SubagentAdmission, HarnessError>>,
    delivered: bool,
    preserved: bool,
}

impl SubagentRunStart {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_provenance(provenance: ternilo_protocol::InputProvenance) -> Self {
        Self {
            provenance: Some(provenance),
            ..Self::default()
        }
    }

    pub fn resolve(&self, result: Result<SubagentAdmission, HarnessError>) {
        let mut current = self
            .state
            .status
            .lock()
            .expect("Subagent run start lock poisoned");
        if current.result.is_none() {
            current.result = Some(result);
            self.state.notify.notify_waiters();
        }
    }

    /// Mark the accepted operation as committed to the caller by the Subagent core.
    pub fn mark_delivered(&self) {
        let mut status = self
            .state
            .status
            .lock()
            .expect("Subagent run start lock poisoned");
        if matches!(status.result, Some(Ok(_))) {
            status.delivered = true;
        }
    }

    /// Preserve a delivered scheduled run when its parent closes successfully.
    #[must_use]
    pub fn preserve_delivered(&self) -> bool {
        let mut status = self
            .state
            .status
            .lock()
            .expect("Subagent run start lock poisoned");
        if status.delivered && matches!(status.result, Some(Ok(SubagentAdmission::Scheduled(_)))) {
            status.preserved = true;
        }
        status.preserved
    }

    #[must_use]
    pub fn is_preserved(&self) -> bool {
        self.state
            .status
            .lock()
            .expect("Subagent run start lock poisoned")
            .preserved
    }

    pub async fn wait(&self) -> Result<SubagentAdmission, HarnessError> {
        loop {
            let notified = self.state.notify.notified();
            if let Some(result) = self
                .state
                .status
                .lock()
                .expect("Subagent run start lock poisoned")
                .result
                .clone()
            {
                return result;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod subagent_admission_tests {
    use super::{AcceptedSubagentRun, HarnessError, RunId, SubagentAdmission, SubagentRunStart};
    use ternilo_protocol::SessionId;

    #[tokio::test]
    async fn admission_waits_for_the_host_and_preserves_its_first_result() {
        let start = SubagentRunStart::new();
        let waiting = start.wait();
        tokio::pin!(waiting);
        tokio::select! {
            biased;
            result = &mut waiting => panic!("unacknowledged start returned: {result:?}"),
            () = tokio::task::yield_now() => {},
        }
        let admission = SubagentAdmission::Scheduled(AcceptedSubagentRun {
            session_id: SessionId::new("child-session"),
            run_id: RunId::new("accepted-run"),
        });
        start.clone().resolve(Ok(admission.clone()));
        assert_eq!(waiting.await.unwrap(), admission);
        start.resolve(Ok(SubagentAdmission::Direct));
        assert_eq!(start.wait().await.unwrap(), admission);
    }

    #[test]
    fn preservation_requires_a_delivered_scheduled_admission() {
        let start = SubagentRunStart::new();
        start.mark_delivered();
        assert!(!start.preserve_delivered());
        start.resolve(Ok(SubagentAdmission::Scheduled(AcceptedSubagentRun {
            session_id: SessionId::new("child-session"),
            run_id: RunId::new("accepted-run"),
        })));
        assert!(
            !start.preserve_delivered(),
            "acceptance alone does not deliver the task to its caller"
        );
        start.mark_delivered();
        assert!(start.preserve_delivered());
        assert!(start.clone().is_preserved());
        for result in [
            Ok(SubagentAdmission::Direct),
            Err(HarnessError::policy("rejected")),
        ] {
            let other = SubagentRunStart::new();
            other.resolve(result);
            other.mark_delivered();
            assert!(!other.preserve_delivered());
        }
    }

    #[tokio::test]
    async fn a_rejected_admission_cannot_become_a_successful_start() {
        let start = SubagentRunStart::new();
        start.resolve(Err(HarnessError::policy("child quota denied")));
        start.resolve(Ok(SubagentAdmission::Direct));
        assert_eq!(
            start.wait().await.unwrap_err().message,
            "child quota denied"
        );
    }
}

pub trait ModelOutput: Send + Sync + 'static {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

    /// Emit reasoning/thinking text without mixing it into the visible answer.
    /// The default keeps existing non-streaming and sink implementations
    /// source-compatible while model-facing session outputs persist the delta.
    fn emit_reasoning<'a>(
        &'a self,
        _: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn retry_scheduled<'a>(
        &'a self,
        _: u32,
        _: u32,
        _: u64,
        _: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn retry_started<'a>(
        &'a self,
        _: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn retry_cancelled<'a>(
        &'a self,
        _: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxEnforcement {
    Full,
    Partial,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxPolicy {
    pub mode: SandboxMode,
    pub workspace_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfinedCommand {
    pub program: OsString,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub backend: String,
    pub enforcement: SandboxEnforcement,
}

service_contract! {
    pub contract Sandboxes {
        id: "ternilo/sandbox@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn confine(program: OsString, arguments: Vec<OsString>, policy: SandboxPolicy) -> Result<ConfinedCommand, HarnessError>;
        ],
    }
}

#[derive(Default)]
pub struct DiscardModelOutput;

impl ModelOutput for DiscardModelOutput {
    fn emit<'a>(
        &'a self,
        _: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentSessionRequest {
    pub subagent_id: SubagentId,
    pub provider: String,
    pub label: String,
    pub task: String,
    pub transcript_kind: SubagentTranscriptKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubagentSessionBinding {
    pub session_id: ternilo_protocol::SessionId,
}

service_contract! {
    pub contract RunEnvironment {
        id: "ternilo/run-environment@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn begin_activity(run_id: RunId, cancellation: RunCancellation, output: Arc<dyn ExecutionActivityOutput>) -> Result<ActivityBranch, HarnessError>;
            async fn identity() -> SessionIdentity;
            async fn workspace() -> Option<WorkspaceBinding>;
            async fn check_run_authorization(run_id: RunId) -> Result<(), HarnessError>;
            async fn register_execution_resource(run_id: RunId, resource_id: String, control: Arc<dyn crate::ExecutionResourceControl>) -> Result<(), HarnessError>;
            async fn try_acquire_workspace() -> Result<Option<WorkspaceExecutionLease>, HarnessError>;
            async fn acquire_workspace(cancellation: RunCancellation) -> Result<WorkspaceExecutionLease, HarnessError>;
            async fn resolve_input_references(references: Vec<SubmissionReference>, prepared_contexts: Vec<ReferenceContext>) -> Result<Vec<ReferenceContext>, HarnessError>;
            async fn session_mode() -> SessionMode;
            async fn permissions() -> PermissionPreset;
            async fn resolve_secret(name: String) -> Result<Option<String>, HarnessError>;
            async fn limits() -> RunLimits;
            async fn check_tool(call: ToolCall, effect: ToolEffect) -> Result<ToolAuthorization, HarnessError>;
            async fn ask_user(question: UserQuestion) -> Result<UserAnswer, HarnessError>;
            async fn load_events() -> Result<Vec<SessionEvent>, HarnessError>;
            async fn commit_event(event: SessionEvent) -> Result<(), HarnessError>;
            async fn create_subagent_session(request: SubagentSessionRequest) -> Result<Option<SubagentSessionBinding>, HarnessError>;
            async fn run_subagent_session(session_id: ternilo_protocol::SessionId, run_id: RunId, input: String, cancellation: RunCancellation, start: SubagentRunStart) -> Result<RunOutcome, HarnessError>;
            async fn append_subagent_lifecycle(session_id: ternilo_protocol::SessionId, run_id: RunId, snapshot: SubagentSnapshot) -> Result<SessionEvent, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Attachments {
        id: "ternilo/attachments@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn store(attachment: Attachment) -> Result<Attachment, HarnessError>;
            async fn resolve(attachment: Attachment) -> Result<Attachment, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract AgentTeam {
        id: "ternilo/agent-team@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn snapshot() -> Result<AgentTeamSnapshot, HarnessError>;
            async fn create_task(request: AgentTeamTaskCreate) -> Result<AgentTeamTask, HarnessError>;
            async fn replace_task(task_id: AgentTeamTaskId, request: AgentTeamTaskReplace) -> Result<AgentTeamTask, HarnessError>;
            async fn delete_task(task_id: AgentTeamTaskId, expected_revision: u64) -> Result<(), HarnessError>;
            async fn send_message(request: AgentTeamMessageSend) -> Result<AgentTeamMessage, HarnessError>;
            async fn mark_message_read(message_id: AgentTeamMessageId) -> Result<AgentTeamMessage, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract WorkspaceFiles {
        id: "ternilo/workspace-files@2",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn read_text(request: FileReadRequest) -> Result<FileContent, HarnessError>;
            async fn write_text(request: FileWriteRequest) -> Result<FileWriteResult, HarnessError>;
            async fn replace_text(request: FileReplaceRequest) -> Result<FileReplaceResult, HarnessError>;
            async fn list_files(request: FileListRequest) -> Result<FileListResult, HarnessError>;
            async fn search_text(request: FileSearchRequest) -> Result<FileSearchResult, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Shell {
        id: "ternilo/shell@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn execute(run_id: RunId, request: ShellRequest) -> Result<ShellResult, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Jobs {
        id: "ternilo/jobs@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn spawn(run_id: RunId, request: ShellRequest) -> Result<JobSnapshot, HarnessError>;
            async fn get(job_id: JobId) -> Result<JobSnapshot, HarnessError>;
            async fn list() -> Result<Vec<JobSnapshot>, HarnessError>;
            async fn kill(job_id: JobId) -> Result<JobSnapshot, HarnessError>;
        ],
    }
}

#[derive(Clone, Debug)]
pub struct SubagentBackendContext {
    pub subagent_id: SubagentId,
    pub label: String,
    pub workspace: Option<WorkspaceBinding>,
}

pub trait SubagentDriver: Send + Sync + 'static {
    fn supports_followup(&self) -> bool {
        true
    }

    fn transcript_kind(&self) -> SubagentTranscriptKind {
        SubagentTranscriptKind::Conversation
    }

    fn run<'a>(
        &'a self,
        parent_run_id: RunId,
        message: String,
        cancellation: RunCancellation,
        session: Option<SubagentSessionBinding>,
        start: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>>;
}

pub trait SubagentBackend: Send + Sync + 'static {
    fn create(
        &self,
        context: SubagentBackendContext,
    ) -> Result<Arc<dyn SubagentDriver>, HarnessError>;
}

#[derive(Clone)]
pub struct SubagentBackendRegistration {
    pub name: String,
    pub backend: Arc<dyn SubagentBackend>,
}

service_contract! {
    pub contract Subagents {
        id: "ternilo/subagents@2",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_backend(backend: SubagentBackendRegistration) -> Result<u64, HarnessError>;
            async fn unregister_backend(registration: u64) -> Result<(), HarnessError>;
            async fn providers() -> Vec<String>;
            async fn spawn(parent_run_id: RunId, task: String, label: Option<String>, background: bool, activity: ActivityBranch) -> Result<SubagentSnapshot, HarnessError>;
            #[allow(clippy::too_many_arguments, reason = "The provider adds routing context to explicit activity and spawn options.")]
            async fn spawn_on(provider: String, parent_run_id: RunId, task: String, label: Option<String>, background: bool, activity: ActivityBranch) -> Result<SubagentSnapshot, HarnessError>;
            async fn followup(parent_run_id: RunId, subagent_id: SubagentId, message: String, provenance: Option<ternilo_protocol::InputProvenance>) -> Result<SubagentSnapshot, HarnessError>;
            async fn get(subagent_id: SubagentId) -> Result<SubagentSnapshot, HarnessError>;
            async fn list() -> Vec<SubagentSnapshot>;
            async fn wait(subagent_id: SubagentId, timeout_ms: u64, activity: ActivityBranch) -> Result<SubagentSnapshot, HarnessError>;
            async fn interrupt(parent_run_id: RunId, subagent_id: SubagentId) -> Result<SubagentSnapshot, HarnessError>;
            async fn dispose(parent_run_id: RunId, subagent_id: SubagentId) -> Result<SubagentSnapshot, HarnessError>;
        ],
    }
}

#[derive(Clone)]
pub struct WorkflowRunRequest {
    pub parent_run_id: RunId,
    pub meta: WorkflowMeta,
    pub script: String,
    pub args: Value,
    pub cancellation: RunCancellation,
    pub activity: ActivityBranch,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowRunResult {
    pub workflow_id: WorkflowRunId,
    pub stop_reason: WorkflowStopReason,
    pub agents_started: u32,
    pub value: Option<Value>,
    pub error: Option<String>,
}

service_contract! {
    pub contract Workflows {
        id: "ternilo/workflows@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn run(request: WorkflowRunRequest) -> Result<WorkflowRunResult, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Terminals {
        id: "ternilo/terminals@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn open(run_id: RunId, name: Option<String>) -> Result<TerminalSnapshot, HarnessError>;
            async fn send(terminal_id: TerminalId, input: String, wait_ms: u64) -> Result<TerminalRead, HarnessError>;
            async fn read(terminal_id: TerminalId, offset: u64) -> Result<TerminalRead, HarnessError>;
            async fn signal(terminal_id: TerminalId, signal: String) -> Result<TerminalSnapshot, HarnessError>;
            async fn close(terminal_id: TerminalId) -> Result<TerminalSnapshot, HarnessError>;
            async fn list() -> Vec<TerminalSnapshot>;
        ],
    }
}

service_contract! {
    pub contract Sessions {
        id: "ternilo/sessions@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn append(run_id: RunId, kind: SessionEventKind) -> Result<SessionEvent, HarnessError>;
            async fn append_if_next_seq(next_seq: u64, run_id: RunId, kind: SessionEventKind) -> Result<Option<SessionEvent>, HarnessError>;
            async fn events() -> Vec<SessionEvent>;
            async fn events_after(after_seq: Option<u64>) -> Vec<SessionEvent>;
            async fn history(query: ternilo_protocol::SessionHistoryQuery) -> Result<ternilo_protocol::SessionEventPage, HarnessError>;
            async fn derive_messages() -> Vec<ModelMessage>;
        ],
    }
}

pub trait SessionTelemetrySink: Send + Sync + 'static {
    /// Enqueue one detached record. Implementations must return quickly and
    /// contain exporter failures so telemetry never becomes part of the run's
    /// correctness path.
    fn emit(&self, record: SessionTelemetryRecord);

    fn shutdown<'a>(&'a self) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

pub trait SessionTelemetryRedactor: Send + Sync + 'static {
    /// Return the outbound copy, or `None` to withhold this record.
    fn redact(&self, record: SessionTelemetryRecord) -> Option<SessionTelemetryRecord>;
}

#[derive(Clone)]
pub struct SessionTelemetryBackendRegistration {
    pub sharing: SessionTelemetrySharingStatus,
    pub sink: Arc<dyn SessionTelemetrySink>,
}

service_contract! {
    pub contract SessionTelemetry {
        id: "ternilo/session-telemetry@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_backend(backend: SessionTelemetryBackendRegistration) -> Result<u64, HarnessError>;
            async fn unregister_backend(registration: u64) -> Result<(), HarnessError>;
            async fn register_redactor(redactor: Arc<dyn SessionTelemetryRedactor>) -> u64;
            async fn unregister_redactor(registration: u64) -> Result<(), HarnessError>;
            async fn sharing() -> SessionTelemetrySharingStatus;
            async fn capture(identity: SessionIdentity, event: SessionEvent) -> ();
        ],
    }
}

service_contract! {
    pub contract SessionQueries {
        id: "ternilo/session-queries@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn search(request: SessionSearchRequest) -> Result<Vec<SessionSearchHit>, HarnessError>;
            async fn read_events(request: SessionEventReadRequest) -> Result<Vec<SessionEvent>, HarnessError>;
            async fn trace(session_id: ternilo_protocol::SessionId) -> Result<SessionTrace, HarnessError>;
        ],
    }
}

#[derive(Clone, Debug)]
pub struct SkillCandidate {
    pub summary: SkillSummary,
    pub rank: u32,
    pub locator: String,
}

#[derive(Clone, Debug)]
pub struct SkillProviderObservation {
    pub candidates: Vec<SkillCandidate>,
    pub complete: bool,
}

pub trait SkillProvider: Send + Sync + 'static {
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<SkillProviderObservation, HarnessError>> + Send + 'a>>;

    fn load<'a>(
        &'a self,
        locator: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>;
}

#[derive(Clone)]
pub struct SkillProviderRegistration {
    pub name: String,
    pub provider: Arc<dyn SkillProvider>,
}

service_contract! {
    pub contract Skills {
        id: "ternilo/skills@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_provider(provider: SkillProviderRegistration) -> Result<u64, HarnessError>;
            async fn unregister_provider(registration: u64) -> Result<(), HarnessError>;
            async fn invalidate(registration: u64) -> Result<(), HarnessError>;
            async fn snapshot() -> Result<SkillCatalogSnapshot, HarnessError>;
            async fn get(name: String) -> Result<Option<SkillDefinition>, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract RuntimeExtensions {
        id: "ternilo/runtime-extensions@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn inspect() -> Result<Value, HarnessError>;
            async fn set_enabled(package_id: String, version: String, enabled: bool) -> Result<Value, HarnessError>;
            async fn set_mounted(session_id: String, package_id: String, version: String, mounted: bool, settings: Value) -> Result<Value, HarnessError>;
            async fn revoke(package_id: String, version: String) -> Result<Value, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Contexts {
        id: "ternilo/contexts@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn prepare(run_id: RunId) -> Result<Option<ContextCompaction>, HarnessError>;
            async fn compact(run_id: RunId) -> Result<ContextCompaction, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Prompts {
        id: "ternilo/prompts@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register(section: PromptSection) -> Result<u64, HarnessError>;
            async fn unregister(registration: u64) -> Result<(), HarnessError>;
            async fn assemble() -> String;
        ],
    }
}

#[derive(Clone)]
pub struct HookRegistration {
    pub handler_id: String,
    pub handler: Arc<dyn HookHandler>,
}

pub trait HookHandler: Send + Sync + 'static {
    fn matches(&self, request: &HookRequest) -> bool;

    fn execute<'a>(
        &'a self,
        request: HookRequest,
    ) -> Pin<Box<dyn Future<Output = HookResult> + Send + 'a>>;
}

service_contract! {
    pub contract Hooks {
        id: "ternilo/hooks@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_hook(hook: HookRegistration) -> Result<u64, HarnessError>;
            async fn unregister_hook(registration: u64) -> Result<(), HarnessError>;
            async fn run(request: HookRequest) -> Vec<HookResult>;
        ],
    }
}

#[derive(Clone)]
pub struct CommandRegistration {
    pub descriptor: CommandDescriptor,
    pub tool_name: String,
    pub resolver: Arc<dyn CommandResolver>,
}

pub trait CommandResolver: Send + Sync + 'static {
    fn resolve(&self, input: &str) -> Result<Value, HarnessError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandCatalogEntry {
    pub descriptor: CommandDescriptor,
    pub tool_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCommand {
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandResolution {
    pub command_name: String,
    pub result: Result<ResolvedCommand, HarnessError>,
}

service_contract! {
    pub contract Commands {
        id: "ternilo/commands@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_command(command: CommandRegistration) -> Result<u64, HarnessError>;
            async fn unregister_command(registration: u64) -> Result<(), HarnessError>;
            async fn catalog() -> Vec<CommandCatalogEntry>;
            async fn resolve(input: String) -> Option<CommandResolution>;
        ],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    ReadOnly,
    Mutating,
    Dangerous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolAuthorization {
    Allow,
    Ask { reason: String },
}

#[derive(Clone)]
pub struct ToolRegistration {
    pub spec: ToolSpec,
    pub effect: ToolEffect,
    pub handler: Arc<dyn ToolHandler>,
}

#[derive(Clone, Debug)]
pub struct ToolExecutionContext {
    pub identity: SessionIdentity,
    pub workspace: Option<WorkspaceBinding>,
    pub run_id: RunId,
    pub call_id: String,
    pub cancellation: RunCancellation,
    pub activity: ActivityBranch,
}

pub trait ToolHandler: Send + Sync + 'static {
    fn presentation(&self) -> Option<ToolPresentationDescriptor> {
        None
    }

    fn approval_reason(&self, _arguments: &Value) -> Option<String> {
        None
    }

    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>>;
}

pub trait ToolGuard: Send + Sync + 'static {
    fn deny(&self, context: &ToolExecutionContext, call: &ToolCall) -> Option<String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolPresentation {
    pub tools: Vec<ToolSpec>,
    pub system_prompt: Option<String>,
    pub code_only: bool,
}

impl ToolPresentation {
    #[must_use]
    pub fn native(tools: Vec<ToolSpec>) -> Self {
        Self {
            tools,
            system_prompt: None,
            code_only: false,
        }
    }
}

pub trait ToolPresenter: Send + Sync + 'static {
    fn present<'a>(
        &'a self,
        tools: Vec<ToolSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolPresentation, HarnessError>> + Send + 'a>>;
}

pub trait CodeBindingHandler: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        name: String,
        arguments: Value,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeBindingSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone)]
pub struct CodeRunRequest {
    pub program: String,
    pub bindings: Vec<CodeBindingSpec>,
    pub binding: Arc<dyn CodeBindingHandler>,
    pub cancellation: RunCancellation,
    pub activity: ActivityBranch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeRuntimeInfo {
    pub language: String,
    pub isolation: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeRunFailureKind {
    Parse,
    Exception,
    InvalidOutput,
    OutputLimit,
    OperationLimit,
    WallTime,
    Cancelled,
    Substrate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeRunFailure {
    pub kind: CodeRunFailureKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeRunResult {
    pub value: Option<Value>,
    pub logs: Vec<String>,
    pub failure: Option<CodeRunFailure>,
}

service_contract! {
    pub contract CodeRuntime {
        id: "ternilo/code-runtime@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn info() -> CodeRuntimeInfo;
            async fn run(request: CodeRunRequest) -> Result<CodeRunResult, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Tools {
        id: "ternilo/tools@1",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn register_tool(tool: ToolRegistration) -> Result<u64, HarnessError>;
            async fn register_source(source: Arc<dyn DeferredToolSource>) -> Result<u64, HarnessError>;
            async fn unregister_source(registration: u64) -> Result<(), HarnessError>;
            async fn prepare(cancellation: RunCancellation) -> Result<(), HarnessError>;
            async fn sources() -> Vec<ternilo_protocol::SessionServiceSnapshot>;
            async fn start_source(id: String, cancellation: RunCancellation) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError>;
            async fn stop_source(id: String) -> Result<ternilo_protocol::SessionServiceSnapshot, HarnessError>;
            async fn unregister_tool(registration: u64) -> Result<(), HarnessError>;
            async fn register_guard(guard: Arc<dyn ToolGuard>) -> u64;
            async fn unregister_guard(registration: u64) -> Result<(), HarnessError>;
            async fn register_presenter(presenter: Arc<dyn ToolPresenter>) -> Result<u64, HarnessError>;
            async fn unregister_presenter(registration: u64) -> Result<(), HarnessError>;
            async fn list() -> Vec<ToolSpec>;
            async fn present() -> Result<ToolPresentation, HarnessError>;
            async fn describe(name: String) -> Option<ToolPresentationDescriptor>;
            async fn execute(run_id: RunId, call: ToolCall, cancellation: RunCancellation, activity: ActivityBranch) -> Result<ToolOutput, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract ModelGateway {
        id: "ternilo/model-gateway@3",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn complete(binding: ternilo_protocol::RunModelBinding, request: ModelRequest, output: Arc<dyn ModelOutput>, cancellation: RunCancellation) -> Result<ModelResponse, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Models {
        id: "ternilo/models@3",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn context_window() -> Option<u64>;
            async fn complete(request: ModelRequest, output: Arc<dyn ModelOutput>, cancellation: RunCancellation) -> Result<ModelResponse, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract SessionTitles {
        id: "ternilo/session-titles@2",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn generate(run_id: RunId, request: String, answer: String) -> Result<String, HarnessError>;
        ],
    }
}

service_contract! {
    pub contract Agents {
        id: "ternilo/agents@3",
        intercept: (),
        identity: (),
        merge: merge_unit,
        methods: [
            async fn run(input: AgentInput) -> Result<RunOutcome, HarnessError>;
            async fn cancel(run_id: RunId) -> Result<(), HarnessError>;
            async fn active_run() -> Option<RunId>;
            async fn steer(input: SteeringInput) -> Result<bool, HarnessError>;
        ],
    }
}
