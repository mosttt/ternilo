//! The remote Worker receives capabilities scoped to canonical leases, never database authority.

use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    AcceptedSubagentRun, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamTaskCreate,
    AgentTeamTaskId, AgentTeamTaskReplace, Attachment, HarnessError, ModelRequest,
    ReferenceContext, RunId, RunOutcome, SessionEvent, SessionId, SessionSubmission,
    SubagentSessionMetadata, TenantId, UserAnswer, UserId, UserQuestion,
};
use ternilo_transport::{CommandId, CommandOutcome, CommandReply, ExecutorHello, ExecutorId};

use crate::{
    ClaimedCloudSessionCommand, ClaimedCloudTelemetry, CloudRunClaim, CloudRunState,
    CloudSessionRecord, CloudWorkerIdentity, StartedRun, TerminalState, WorkerPolicy,
    WorkerSubagentRun,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLease {
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub lease_token: u64,
    pub writer_fencing_token: u64,
}

impl From<&StartedRun> for RunLease {
    fn from(run: &StartedRun) -> Self {
        Self {
            tenant_id: run.claim.tenant_id.clone(),
            run_id: run.claim.run_id.clone(),
            lease_token: run.claim.lease_token,
            writer_fencing_token: run.fencing_token,
        }
    }
}

impl From<&CloudRunClaim> for RunLease {
    fn from(run: &CloudRunClaim) -> Self {
        Self {
            tenant_id: run.tenant_id.clone(),
            run_id: run.run_id.clone(),
            lease_token: run.lease_token,
            writer_fencing_token: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandLease {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub command_id: CommandId,
    pub attempt_count: u32,
}

impl From<&ClaimedCloudSessionCommand> for CommandLease {
    fn from(command: &ClaimedCloudSessionCommand) -> Self {
        Self {
            tenant_id: command.tenant_id.clone(),
            user_id: command.user_id.clone(),
            command_id: command.command.command_id.clone(),
            attempt_count: command.attempt_count,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryLease {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub occurrence_id: String,
    pub attempt_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfiguration {
    pub worker_id: ExecutorId,
    pub storage_id: String,
    pub expected_root_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegisterRequest {
    #[serde(default)]
    pub capacity: crate::WorkerCapacity,
    pub hello: ExecutorHello,
    pub storage_id: String,
    pub root_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRegistration {
    pub capacity: crate::WorkerCapacity,
    pub identity: CloudWorkerIdentity,
    pub storage_id: String,
    pub policy: WorkerPolicy,
    pub lease_seconds: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRpcRequest {
    pub identity: CloudWorkerIdentity,
    pub request: WorkerRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerRequest {
    Heartbeat,
    ClaimRun,
    WorkspaceRecoveryCandidates {
        after: Option<crate::WorkspaceRecoveryCursor>,
    },
    ConfirmWorkspaceRecovery {
        ticket: crate::WorkspaceRecoveryTicket,
    },
    ParkRun {
        run: RunLease,
        activity_revision: u64,
        dependencies: Vec<AcceptedSubagentRun>,
    },
    ResumeRun {
        run: RunLease,
        activity_revision: u64,
        parked_revision: u64,
    },
    ReleaseResident {
        run: RunLease,
        worker_generation: u64,
    },
    StartRun {
        run: RunLease,
    },
    ReleaseClaim {
        run: RunLease,
    },
    RenewRun {
        run: RunLease,
    },
    CancelRequested {
        run: RunLease,
    },
    AppendEvent {
        run: RunLease,
        event: Box<SessionEvent>,
    },
    RecordQuestion {
        run: RunLease,
        question: Box<UserQuestion>,
    },
    QuestionAnswer {
        run: RunLease,
        question_id: String,
    },
    FinishRun {
        run: RunLease,
        terminal: TerminalState,
        outcome: Option<RunOutcome>,
        error: Option<HarnessError>,
    },
    RunSubmission {
        run: RunLease,
    },
    ReferenceContexts {
        run: RunLease,
    },
    Extensions {
        run: RunLease,
    },
    ExtensionsActive {
        run: RunLease,
    },
    StoreAttachment {
        run: RunLease,
        attachment: Attachment,
        content_base64: String,
    },
    DownloadAttachment {
        run: RunLease,
        attachment: Attachment,
    },
    ClaimCommands,
    CompleteCommand {
        command: CommandLease,
        outcome: CommandOutcome,
    },
    DeferCommand {
        command: CommandLease,
    },
    Inspection {
        command: CommandLease,
    },
    SteeringSubmission {
        command: CommandLease,
    },
    CompleteSteering {
        command: CommandLease,
        accepted: bool,
    },
    RequeueSteering {
        run: RunLease,
    },
    CreateSubagent {
        run: RunLease,
        child_session_id: SessionId,
        metadata: SubagentSessionMetadata,
        label: String,
    },
    EnqueueSubagent {
        run: RunLease,
        child_session_id: SessionId,
        child_run_id: RunId,
        input: String,
    },
    ReadSubagent {
        run: RunLease,
        child_session_id: SessionId,
        child_run_id: RunId,
    },
    CancelSubagent {
        run: RunLease,
        child_session_id: SessionId,
        child_run_id: RunId,
    },
    AgentTeam {
        run: RunLease,
        request: WorkerTeamRequest,
    },
    ClaimTelemetry {
        lease_ms: u64,
    },
    AcknowledgeTelemetry {
        occurrence: TelemetryLease,
    },
    FailTelemetry {
        occurrence: TelemetryLease,
        error: String,
    },
    Drain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerTeamRequest {
    Snapshot,
    CreateTask {
        request: AgentTeamTaskCreate,
    },
    ReplaceTask {
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    },
    DeleteTask {
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    },
    SendMessage {
        request: AgentTeamMessageSend,
    },
    ReadMessage {
        message_id: AgentTeamMessageId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerReply {
    Unit,
    WorkspaceRecovery {
        page: crate::WorkspaceRecoveryPage,
    },
    Parked {
        revision: u64,
    },
    Resumed {
        admission: crate::RunAdmission,
    },
    Count {
        count: u32,
    },
    Flag {
        value: bool,
    },
    Claim {
        claim: Option<CloudRunClaim>,
    },
    Started {
        run: Option<StartedRun>,
    },
    Commands {
        commands: Vec<ClaimedCloudSessionCommand>,
    },
    Command {
        reply: CommandReply,
    },
    Submission {
        submission: Option<SessionSubmission>,
        #[serde(default)]
        additional_inputs: Vec<ternilo_protocol::SteeringInput>,
    },
    References {
        contexts: Vec<ReferenceContext>,
    },
    Extensions {
        extensions: Vec<ternilo_extension::ExtensionDistribution>,
    },
    AttachmentObject {
        content_base64: String,
    },
    Inspection {
        session: CloudSessionRecord,
        profile: Option<ternilo_protocol::Profile>,
        extensions: Vec<ternilo_extension::ExtensionDistribution>,
    },
    Answer {
        answer: Option<UserAnswer>,
    },
    SessionId {
        session_id: SessionId,
    },
    SubagentAccepted {
        run: AcceptedSubagentRun,
    },
    Subagent {
        run: Option<WorkerSubagentRun>,
    },
    RunState {
        state: CloudRunState,
    },
    Team {
        value: serde_json::Value,
    },
    Telemetry {
        occurrences: Vec<ClaimedCloudTelemetry>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerModelRequest {
    pub identity: CloudWorkerIdentity,
    pub run: RunLease,
    pub request_id: u64,
    pub binding: ternilo_protocol::RunModelBinding,
    pub request: ModelRequest,
}

pub use ternilo_protocol::ModelGatewayFrame as WorkerModelFrame;
