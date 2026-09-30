#![forbid(unsafe_code)]

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_protocol::{
    AcceptedUploadChange, AcceptedUploadChangeKind, AgentId, AgentPresetCopyRequest,
    AgentPresetUpdateRequest, AgentTeamMessageId, AgentTeamMessageSend, AgentTeamTaskCreate,
    AgentTeamTaskId, AgentTeamTaskReplace, Attachment, AuthorizationBeginRequest,
    AuthorizationCredentialKey, AuthorizationPromptAnswer, DefaultModelSelection,
    ExtensionProviderMaterializeRequest, FeedbackRating, HarnessError, PermissionPreset,
    PluginEntry, ProviderModelDiscoveryRequest, ProviderProfile, QueueEditRequest,
    ReferenceCandidateRequest, RunId, RunSpec, SessionEvent, SessionId, SessionMode,
    SessionSearchFilters, SessionSubmissionRequest, SidebarOrdering, SubagentId, SubmissionId,
    TenantId, UserAnswer, UserId, WorkspaceId, WorkspaceRequest, validate_agent_preset_id,
};

pub const EXECUTOR_PROTOCOL_VERSION: u32 = 45;

mod cleanup;
pub use cleanup::{
    NodeAccountAuthorization, NodeCleanupReceipt, NodeCleanupRequest, NodeCleanupSnapshot,
    NodeCleanupState, NodeInputAuthorization,
};

macro_rules! transport_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), HarnessError> {
                if self.0.is_empty()
                    || self.0.len() > 128
                    || self
                        .0
                        .chars()
                        .any(|character| character.is_whitespace() || character.is_control())
                {
                    Err(HarnessError::invalid(format!(
                        "{} must contain 1 to 128 bytes without whitespace or control characters",
                        stringify!($name)
                    )))
                } else {
                    Ok(())
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

transport_id!(ExecutorId);
transport_id!(ConnectionId);
transport_id!(CommandId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    EdgeNode,
    CloudWorker,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorCapability {
    ApplicationRpc,
    CloudRun,
    PersistentSessions,
    WorkspaceFiles,
    InteractiveQuestions,
    LocalCredentials,
    SessionEventDelta,
    RunCancellation,
    ExtensionManagement,
    AuthorizationFlows,
    AgentPresets,
    SessionProjections,
    TelemetryDisclosure,
    Skills,
    SessionSteering,
    AddressedSessionCommands,
    AddressedSubagents,
    AgentTeamHost,
    LiveInvalidations,
}

pub type ExecutorCapabilities = BTreeSet<ExecutorCapability>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorHello {
    pub protocol_version: u32,
    pub executor_id: ExecutorId,
    pub executor_kind: ExecutorKind,
    pub instance_nonce: String,
    pub catalog_revision: String,
    pub capabilities: ExecutorCapabilities,
}

impl ExecutorHello {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.protocol_version != EXECUTOR_PROTOCOL_VERSION {
            return Err(HarnessError::invalid(format!(
                "unsupported executor protocol version {}; expected {EXECUTOR_PROTOCOL_VERSION}",
                self.protocol_version
            )));
        }
        self.executor_id.validate()?;
        if self.instance_nonce.trim().is_empty() || self.catalog_revision.trim().is_empty() {
            return Err(HarnessError::invalid(
                "instance_nonce and catalog_revision must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorScope {
    pub tenant_id: TenantId,
    pub user_id: UserId,
}

impl ExecutorScope {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.tenant_id.validate()?;
        self.user_id.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCursor {
    pub session_id: SessionId,
    pub last_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationOperation {
    Catalog,
    AgentPresetList,
    AgentPresetGet {
        preset_id: String,
    },
    AgentPresetCopy {
        request: AgentPresetCopyRequest,
    },
    AgentPresetUpdate {
        preset_id: String,
        request: AgentPresetUpdateRequest,
    },
    AgentPresetDelete {
        preset_id: String,
    },
    AgentPresetSetDefault {
        preset_id: String,
    },
    ExtensionInventory,
    PublisherTrust {
        publisher: Value,
    },
    PublisherRevoke {
        key_id: String,
    },
    ExtensionInstall {
        request: Value,
    },
    ExtensionSetEnabled {
        package_id: String,
        version: String,
        enabled: bool,
    },
    ExtensionRevoke {
        package_id: String,
        version: String,
    },
    ExtensionUninstall {
        package_id: String,
        version: String,
    },
    Snapshot,
    LiveActivities,
    SidebarOrderingGet,
    SidebarOrderingSet {
        ordering: SidebarOrdering,
    },
    CredentialList,
    CredentialSet {
        name: String,
        value: String,
    },
    CredentialRemove {
        name: String,
    },
    CredentialRecordSet {
        key: String,
        kind: String,
        payload: Value,
    },
    CredentialRecordDelete {
        key: String,
    },
    ProviderList,
    ProviderUpsert {
        provider: ProviderProfile,
    },
    ProviderMaterialize {
        request: ExtensionProviderMaterializeRequest,
    },
    ProviderDelete {
        id: String,
    },
    ProviderDiscover {
        request: ProviderModelDiscoveryRequest,
    },
    DefaultModelGet,
    DefaultModelSet {
        selection: DefaultModelSelection,
    },
    AuthorizationSnapshot {
        surface_id: String,
    },
    AuthorizationBegin {
        request: AuthorizationBeginRequest,
    },
    AuthorizationAnswer {
        answer: AuthorizationPromptAnswer,
    },
    AuthorizationCancel {
        key: AuthorizationCredentialKey,
    },
    AttachmentResolve {
        attachment: Attachment,
    },
    DirectoryList {
        path: Option<String>,
    },
    DirectoryCreate {
        parent: String,
        name: String,
    },
    WorkspaceCreate {
        path: String,
    },
    WorkspaceLocation {
        workspace_id: WorkspaceId,
    },
    WorkspaceRename {
        workspace_id: WorkspaceId,
        title: String,
    },
    WorkspaceUnregister {
        workspace_id: WorkspaceId,
    },
    SessionCreate {
        workspace_id: WorkspaceId,
        session_id: Option<String>,
        agent_id: Option<String>,
        agent_preset: Option<String>,
        permissions: Option<PermissionPreset>,
    },
    SessionUpdate {
        session_id: SessionId,
        title: Option<String>,
        permissions: Option<PermissionPreset>,
        model: Option<Value>,
        #[serde(default)]
        server_model: Option<ternilo_protocol::RunModelSnapshot>,
        agent_preset: Option<String>,
        profile_plugins: Option<Vec<PluginEntry>>,
        mode: Option<SessionMode>,
    },
    SessionFork {
        session_id: SessionId,
        at_seq: Option<u64>,
    },
    SessionArchive {
        session_id: SessionId,
    },
    SessionRestore {
        session_id: SessionId,
    },
    SessionDelete {
        session_id: SessionId,
    },
    SessionEvents {
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_seq: Option<u64>,
    },
    SessionHistory {
        session_id: SessionId,
        query: ternilo_protocol::SessionHistoryQuery,
    },
    SessionFileContent {
        session_id: SessionId,
        file_id: String,
    },
    SessionReferenceCandidates {
        session_id: SessionId,
        request: ReferenceCandidateRequest,
    },
    SessionWorkspace {
        session_id: SessionId,
        request: WorkspaceRequest,
    },
    SessionPlugins {
        session_id: SessionId,
    },
    SessionCommands {
        session_id: SessionId,
    },
    SessionServices {
        session_id: SessionId,
    },
    SessionServiceStart {
        session_id: SessionId,
        service_id: String,
    },
    SessionServiceStop {
        session_id: SessionId,
        service_id: String,
    },
    SessionProjection {
        session_id: SessionId,
    },
    SessionTelemetry {
        session_id: SessionId,
    },
    SessionSkills {
        session_id: SessionId,
    },
    SessionSkillResolve {
        session_id: SessionId,
        name: String,
        input: String,
    },
    SessionStats {
        session_id: SessionId,
    },
    SessionExport {
        session_id: SessionId,
    },
    SessionFeedback {
        session_id: SessionId,
        target_seq: u64,
        expected_revision: u64,
        rating: Option<FeedbackRating>,
        note: Option<String>,
    },
    SessionCommandFeedback {
        session_id: SessionId,
        text: String,
    },
    SessionSubagentFollowup {
        session_id: SessionId,
        subagent_id: SubagentId,
        message: String,
    },
    SessionSubagentInterrupt {
        session_id: SessionId,
        subagent_id: SubagentId,
    },
    SessionAgentTeamSnapshot {
        session_id: SessionId,
    },
    SessionAgentTeamTaskCreate {
        session_id: SessionId,
        request: AgentTeamTaskCreate,
    },
    SessionAgentTeamTaskReplace {
        session_id: SessionId,
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    },
    SessionAgentTeamTaskDelete {
        session_id: SessionId,
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    },
    SessionAgentTeamMessageSend {
        session_id: SessionId,
        request: AgentTeamMessageSend,
    },
    SessionAgentTeamMessageRead {
        session_id: SessionId,
        message_id: AgentTeamMessageId,
    },
    SessionInbox {
        session_id: SessionId,
    },
    SessionSubmit {
        session_id: SessionId,
        request: SessionSubmissionRequest,
    },
    SessionQueueEdit {
        session_id: SessionId,
        submission_id: SubmissionId,
        request: QueueEditRequest,
    },
    SessionQueueRemove {
        session_id: SessionId,
        submission_id: SubmissionId,
    },
    SessionQueueSteer {
        session_id: SessionId,
        submission_id: SubmissionId,
    },
    SessionTurn {
        session_id: SessionId,
        run_id: Option<String>,
        input: String,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    SessionSkillTurn {
        session_id: SessionId,
        run_id: Option<String>,
        name: String,
        input: String,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    SessionSearch {
        query: String,
        session_id: Option<SessionId>,
        workspace_id: Option<WorkspaceId>,
        #[serde(default)]
        filters: SessionSearchFilters,
        limit: u32,
    },
    PendingQuestions {
        session_id: Option<SessionId>,
    },
    AnswerQuestion {
        answer: UserAnswer,
    },
}

impl ApplicationOperation {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            operation @ (Self::AgentPresetGet { .. }
            | Self::AgentPresetCopy { .. }
            | Self::AgentPresetUpdate { .. }
            | Self::AgentPresetDelete { .. }
            | Self::AgentPresetSetDefault { .. }) => validate_agent_preset_operation(operation),
            operation @ (Self::ExtensionInventory
            | Self::PublisherTrust { .. }
            | Self::PublisherRevoke { .. }
            | Self::ExtensionInstall { .. }
            | Self::ExtensionSetEnabled { .. }
            | Self::ExtensionRevoke { .. }
            | Self::ExtensionUninstall { .. }) => validate_extension_operation(operation)
                .expect("all extension application operations are handled"),
            operation @ (Self::CredentialSet { .. }
            | Self::CredentialRemove { .. }
            | Self::CredentialRecordSet { .. }
            | Self::CredentialRecordDelete { .. }
            | Self::SidebarOrderingSet { .. }
            | Self::ProviderUpsert { .. }
            | Self::ProviderMaterialize { .. }
            | Self::ProviderDelete { .. }
            | Self::ProviderDiscover { .. }
            | Self::DefaultModelSet { .. }) => validate_configuration_operation(operation),
            operation @ (Self::AuthorizationSnapshot { .. }
            | Self::AuthorizationBegin { .. }
            | Self::AuthorizationAnswer { .. }
            | Self::AuthorizationCancel { .. }) => validate_authorization_operation(operation),
            Self::AttachmentResolve { attachment } => attachment.validate(),
            Self::DirectoryCreate { parent, name } => {
                require_text(parent, "directory parent")?;
                require_text(name, "directory name")
            }
            Self::WorkspaceCreate { path } => require_text(path, "workspace path"),
            Self::WorkspaceRename {
                workspace_id,
                title,
            } => {
                workspace_id.validate()?;
                require_text(title, "workspace title")
            }
            Self::WorkspaceLocation { workspace_id }
            | Self::WorkspaceUnregister { workspace_id } => workspace_id.validate(),
            Self::SessionHistory { session_id, query } => {
                session_id.validate()?;
                query.validate()
            }
            operation @ (Self::SessionCreate { .. }
            | Self::SessionUpdate { .. }
            | Self::SessionFork { .. }
            | Self::SessionArchive { .. }
            | Self::SessionRestore { .. }
            | Self::SessionDelete { .. }
            | Self::SessionEvents { .. }
            | Self::SessionFileContent { .. }
            | Self::SessionReferenceCandidates { .. }
            | Self::SessionWorkspace { .. }
            | Self::SessionPlugins { .. }
            | Self::SessionCommands { .. }
            | Self::SessionServices { .. }
            | Self::SessionServiceStart { .. }
            | Self::SessionServiceStop { .. }
            | Self::SessionProjection { .. }
            | Self::SessionTelemetry { .. }
            | Self::SessionSkills { .. }
            | Self::SessionSkillResolve { .. }
            | Self::SessionStats { .. }
            | Self::SessionExport { .. }
            | Self::SessionFeedback { .. }
            | Self::SessionCommandFeedback { .. }
            | Self::SessionSubagentFollowup { .. }
            | Self::SessionSubagentInterrupt { .. }
            | Self::SessionAgentTeamSnapshot { .. }
            | Self::SessionAgentTeamTaskCreate { .. }
            | Self::SessionAgentTeamTaskReplace { .. }
            | Self::SessionAgentTeamTaskDelete { .. }
            | Self::SessionAgentTeamMessageSend { .. }
            | Self::SessionAgentTeamMessageRead { .. }
            | Self::SessionInbox { .. }
            | Self::SessionSubmit { .. }
            | Self::SessionQueueEdit { .. }
            | Self::SessionQueueRemove { .. }
            | Self::SessionQueueSteer { .. }
            | Self::SessionTurn { .. }
            | Self::SessionSkillTurn { .. }
            | Self::SessionSearch { .. }
            | Self::PendingQuestions { .. }
            | Self::AnswerQuestion { .. }) => validate_session_operation(operation),
            Self::Catalog
            | Self::AgentPresetList
            | Self::Snapshot
            | Self::LiveActivities
            | Self::SidebarOrderingGet
            | Self::CredentialList
            | Self::ProviderList
            | Self::DefaultModelGet
            | Self::DirectoryList { .. } => Ok(()),
        }
    }

    pub fn validate_executor_capabilities(
        &self,
        capabilities: &ExecutorCapabilities,
    ) -> Result<(), HarnessError> {
        let required = match self {
            Self::ExtensionInventory
            | Self::PublisherTrust { .. }
            | Self::PublisherRevoke { .. }
            | Self::ExtensionInstall { .. }
            | Self::ExtensionSetEnabled { .. }
            | Self::ExtensionRevoke { .. }
            | Self::ExtensionUninstall { .. }
            | Self::ProviderMaterialize { .. } => Some(ExecutorCapability::ExtensionManagement),
            _ => None,
        };
        if required.is_some_and(|capability| !capabilities.contains(&capability)) {
            return Err(HarnessError::unavailable(
                "selected Ternilo node does not support extension management; upgrade or reconnect it",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub const fn contains_secret_material(&self) -> bool {
        matches!(
            self,
            Self::CredentialSet { .. }
                | Self::CredentialRecordSet { .. }
                | Self::AuthorizationAnswer { .. }
                | Self::ProviderDiscover {
                    request: ProviderModelDiscoveryRequest {
                        api_key: Some(_),
                        ..
                    }
                }
        )
    }

    #[must_use]
    pub const fn requires_ephemeral_delivery(&self) -> bool {
        self.contains_secret_material()
            || matches!(
                self,
                Self::AuthorizationBegin { .. }
                    | Self::AuthorizationCancel { .. }
                    | Self::SessionFileContent { .. }
                    | Self::SessionWorkspace { .. }
                    | Self::WorkspaceLocation { .. }
                    | Self::AgentPresetGet { .. }
            )
    }
}

fn validate_agent_preset_operation(operation: &ApplicationOperation) -> Result<(), HarnessError> {
    match operation {
        ApplicationOperation::AgentPresetGet { preset_id }
        | ApplicationOperation::AgentPresetDelete { preset_id }
        | ApplicationOperation::AgentPresetSetDefault { preset_id } => {
            validate_agent_preset_id(preset_id)
        }
        ApplicationOperation::AgentPresetCopy { request } => {
            validate_agent_preset_id(&request.from)?;
            validate_agent_preset_id(&request.id)?;
            validate_optional_text(request.display_name.as_deref(), "agent preset display name")
        }
        ApplicationOperation::AgentPresetUpdate { preset_id, request } => {
            validate_agent_preset_id(preset_id)?;
            require_text(&request.display_name, "agent preset display name")
        }
        _ => unreachable!("agent preset validation received another operation"),
    }
}

fn validate_configuration_operation(operation: &ApplicationOperation) -> Result<(), HarnessError> {
    match operation {
        ApplicationOperation::CredentialSet { name, value } => {
            require_text(name, "credential name")?;
            require_text(value, "credential value")
        }
        ApplicationOperation::CredentialRemove { name } => require_text(name, "credential name"),
        ApplicationOperation::CredentialRecordSet { key, kind, payload } => {
            require_text(key, "credential record key")?;
            require_text(kind, "credential record kind")?;
            validate_credential_record_payload(payload)
        }
        ApplicationOperation::CredentialRecordDelete { key } => {
            require_text(key, "credential record key")
        }
        ApplicationOperation::SidebarOrderingSet { ordering } => ordering.validate(),
        ApplicationOperation::ProviderUpsert { provider } => provider.validate(),
        ApplicationOperation::ProviderMaterialize { request } => request.validate(),
        ApplicationOperation::ProviderDelete { id } => require_text(id, "provider id"),
        ApplicationOperation::ProviderDiscover { request } => request.validate(),
        ApplicationOperation::DefaultModelSet { selection } => selection.validate(),
        _ => unreachable!("configuration validation received another operation"),
    }
}

fn validate_credential_record_payload(payload: &Value) -> Result<(), HarnessError> {
    let encoded = serde_json::to_vec(payload).map_err(|error| {
        HarnessError::invalid(format!("serialize credential record payload: {error}"))
    })?;
    if encoded.len() > 64 * 1024 {
        Err(HarnessError::invalid(
            "credential record payload may not exceed 64 KiB",
        ))
    } else {
        Ok(())
    }
}

fn validate_authorization_operation(operation: &ApplicationOperation) -> Result<(), HarnessError> {
    match operation {
        ApplicationOperation::AuthorizationSnapshot { surface_id } => {
            validate_authorization_surface(surface_id)
        }
        ApplicationOperation::AuthorizationBegin { request } => {
            request.key.validate()?;
            validate_authorization_surface(&request.surface_id)?;
            validate_optional_text(request.method.as_deref(), "authorization method")
        }
        ApplicationOperation::AuthorizationAnswer { answer } => {
            validate_authorization_surface(&answer.surface_id)?;
            require_text(&answer.prompt_id, "authorization prompt id")?;
            require_text(&answer.value, "authorization prompt answer")?;
            if answer.value.len() > 64 * 1024 {
                return Err(HarnessError::invalid(
                    "authorization prompt answer may not exceed 64 KiB",
                ));
            }
            Ok(())
        }
        ApplicationOperation::AuthorizationCancel { key } => key.validate(),
        _ => unreachable!("authorization validation received another operation"),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep validation for session-scoped operations together."
)]
fn validate_session_operation(operation: &ApplicationOperation) -> Result<(), HarnessError> {
    match operation {
        ApplicationOperation::SessionCreate {
            workspace_id,
            session_id,
            agent_id,
            agent_preset,
            permissions: _,
        } => validate_session_create(
            workspace_id,
            session_id.as_deref(),
            agent_id.as_deref(),
            agent_preset.as_deref(),
        ),
        ApplicationOperation::SessionUpdate {
            session_id,
            title,
            permissions,
            model,
            agent_preset,
            profile_plugins,
            mode,
            server_model,
        } => {
            if let Some(snapshot) = server_model {
                snapshot.validate()?;
            }
            validate_session_update(
                session_id,
                title.as_deref(),
                agent_preset.as_deref(),
                title.is_some()
                    || permissions.is_some()
                    || model.is_some()
                    || agent_preset.is_some()
                    || profile_plugins.is_some()
                    || mode.is_some(),
            )
        }
        ApplicationOperation::SessionDelete { session_id }
        | ApplicationOperation::SessionFork { session_id, .. }
        | ApplicationOperation::SessionArchive { session_id }
        | ApplicationOperation::SessionRestore { session_id }
        | ApplicationOperation::SessionEvents { session_id, .. }
        | ApplicationOperation::SessionHistory { session_id, .. }
        | ApplicationOperation::SessionPlugins { session_id }
        | ApplicationOperation::SessionCommands { session_id }
        | ApplicationOperation::SessionServices { session_id }
        | ApplicationOperation::SessionProjection { session_id }
        | ApplicationOperation::SessionTelemetry { session_id }
        | ApplicationOperation::SessionSkills { session_id }
        | ApplicationOperation::SessionStats { session_id }
        | ApplicationOperation::SessionExport { session_id }
        | ApplicationOperation::SessionFeedback { session_id, .. }
        | ApplicationOperation::SessionCommandFeedback { session_id, .. }
        | ApplicationOperation::SessionAgentTeamSnapshot { session_id }
        | ApplicationOperation::SessionInbox { session_id } => session_id.validate(),
        ApplicationOperation::SessionServiceStart {
            session_id,
            service_id,
        }
        | ApplicationOperation::SessionServiceStop {
            session_id,
            service_id,
        } => {
            session_id.validate()?;
            require_text(service_id, "runtime service ID")
        }
        ApplicationOperation::SessionFileContent {
            session_id,
            file_id,
        } => {
            session_id.validate()?;
            ternilo_protocol::SessionFileLocator::parse(file_id).map(|_| ())
        }
        ApplicationOperation::SessionSkillResolve {
            session_id,
            name,
            input,
        } => validate_session_skill_turn(session_id, None, name, input, &[]),
        ApplicationOperation::SessionSubagentFollowup {
            session_id,
            subagent_id,
            message,
        } => {
            session_id.validate()?;
            subagent_id.validate()?;
            require_text(message, "subagent follow-up message")
        }
        ApplicationOperation::SessionSubagentInterrupt {
            session_id,
            subagent_id,
        } => {
            session_id.validate()?;
            subagent_id.validate()
        }
        ApplicationOperation::SessionAgentTeamTaskCreate {
            session_id,
            request,
        } => {
            session_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionAgentTeamTaskReplace {
            session_id,
            task_id,
            request,
        } => {
            session_id.validate()?;
            task_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionAgentTeamTaskDelete {
            session_id,
            task_id,
            expected_revision,
        } => {
            session_id.validate()?;
            task_id.validate()?;
            if *expected_revision == 0 {
                return Err(HarnessError::invalid(
                    "Agent Team task expected revision must be positive",
                ));
            }
            Ok(())
        }
        ApplicationOperation::SessionAgentTeamMessageSend {
            session_id,
            request,
        } => {
            session_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionAgentTeamMessageRead {
            session_id,
            message_id,
        } => {
            session_id.validate()?;
            message_id.validate()
        }
        ApplicationOperation::SessionWorkspace {
            session_id,
            request,
        } => {
            session_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionReferenceCandidates {
            session_id,
            request,
        } => {
            session_id.validate()?;
            if request.directory.len() > 4_096 || request.query.chars().count() > 256 {
                return Err(HarnessError::invalid(
                    "reference candidate request is too large",
                ));
            }
            Ok(())
        }
        ApplicationOperation::SessionSubmit {
            session_id,
            request,
        } => {
            session_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionQueueEdit {
            session_id,
            submission_id,
            request,
        } => {
            session_id.validate()?;
            submission_id.validate()?;
            request.validate()
        }
        ApplicationOperation::SessionQueueRemove {
            session_id,
            submission_id,
        }
        | ApplicationOperation::SessionQueueSteer {
            session_id,
            submission_id,
        } => {
            session_id.validate()?;
            submission_id.validate()
        }
        ApplicationOperation::SessionTurn {
            session_id,
            run_id,
            input,
            attachments,
        } => validate_session_turn(session_id, run_id.as_deref(), input, attachments),
        ApplicationOperation::SessionSkillTurn {
            session_id,
            run_id,
            name,
            input,
            attachments,
        } => validate_session_skill_turn(session_id, run_id.as_deref(), name, input, attachments),
        operation @ (ApplicationOperation::SessionSearch { .. }
        | ApplicationOperation::PendingQuestions { .. }
        | ApplicationOperation::AnswerQuestion { .. }) => {
            validate_session_query_operation(operation)
        }
        _ => unreachable!("session validation received another operation"),
    }
}

fn validate_session_query_operation(operation: &ApplicationOperation) -> Result<(), HarnessError> {
    match operation {
        ApplicationOperation::SessionSearch {
            query,
            session_id,
            workspace_id,
            filters,
            limit,
        } => ternilo_protocol::SessionSearchRequest {
            query: query.clone(),
            session_id: session_id.clone(),
            workspace_id: workspace_id.clone(),
            filters: filters.clone(),
            limit: *limit,
        }
        .validate(),
        ApplicationOperation::PendingQuestions { session_id } => {
            if let Some(session_id) = session_id {
                session_id.validate()?;
            }
            Ok(())
        }
        ApplicationOperation::AnswerQuestion { answer } => {
            require_text(&answer.question_id, "question id")
        }
        _ => unreachable!("session query validation received another operation"),
    }
}

fn validate_session_create(
    workspace_id: &WorkspaceId,
    session_id: Option<&str>,
    agent_id: Option<&str>,
    agent_preset: Option<&str>,
) -> Result<(), HarnessError> {
    workspace_id.validate()?;
    if let Some(session_id) = session_id {
        SessionId::new(session_id).validate()?;
    }
    if let Some(agent_id) = agent_id {
        AgentId::new(agent_id).validate()?;
    }
    if let Some(agent_preset) = agent_preset {
        ternilo_protocol::validate_agent_preset_id(agent_preset)?;
    }
    Ok(())
}

fn validate_session_update(
    session_id: &SessionId,
    title: Option<&str>,
    agent_preset: Option<&str>,
    has_fields: bool,
) -> Result<(), HarnessError> {
    session_id.validate()?;
    if !has_fields {
        return Err(HarnessError::invalid("session update has no fields"));
    }
    validate_optional_text(title, "session title")?;
    if let Some(agent_preset) = agent_preset {
        ternilo_protocol::validate_agent_preset_id(agent_preset)?;
    }
    Ok(())
}

fn validate_session_turn(
    session_id: &SessionId,
    run_id: Option<&str>,
    input: &str,
    attachments: &[Attachment],
) -> Result<(), HarnessError> {
    session_id.validate()?;
    ternilo_protocol::AgentInput {
        additional_inputs: Vec::new(),
        provenance: None,
        run_id: RunId::new(run_id.unwrap_or("remote-run")),
        input: input.to_owned(),
        display_input: None,
        source: None,
        references: Vec::new(),
        reference_contexts: Vec::new(),
        attachments: attachments.to_vec(),
    }
    .validate()
}

fn validate_session_skill_turn(
    session_id: &SessionId,
    run_id: Option<&str>,
    name: &str,
    input: &str,
    attachments: &[Attachment],
) -> Result<(), HarnessError> {
    session_id.validate()?;
    RunId::new(run_id.unwrap_or("remote-run")).validate()?;
    if name.is_empty()
        || name.len() > 128
        || name.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err(HarnessError::invalid(
            "skill name must be lowercase kebab-case",
        ));
    }
    if input.chars().count() > 100_000 || attachments.len() > 10 {
        return Err(HarnessError::invalid(
            "skill request or attachment count exceeds its limit",
        ));
    }
    for attachment in attachments {
        attachment.validate()?;
    }
    Ok(())
}

fn validate_authorization_surface(surface_id: &str) -> Result<(), HarnessError> {
    if surface_id.is_empty()
        || surface_id.len() > 128
        || !surface_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(HarnessError::invalid(
            "authorization surface_id must contain 1 to 128 safe ASCII characters",
        ));
    }
    Ok(())
}

fn validate_extension_operation(
    operation: &ApplicationOperation,
) -> Option<Result<(), HarnessError>> {
    match operation {
        ApplicationOperation::PublisherTrust { publisher } => {
            Some(require_json_object(publisher, "extension publisher trust"))
        }
        ApplicationOperation::PublisherRevoke { key_id } => {
            Some(require_text(key_id, "extension publisher key id"))
        }
        ApplicationOperation::ExtensionInstall { request } => {
            Some(require_json_object(request, "extension install request"))
        }
        ApplicationOperation::ExtensionSetEnabled {
            package_id,
            version,
            ..
        }
        | ApplicationOperation::ExtensionRevoke {
            package_id,
            version,
        }
        | ApplicationOperation::ExtensionUninstall {
            package_id,
            version,
        } => Some(
            require_text(package_id, "extension package id")
                .and_then(|()| require_text(version, "extension package version")),
        ),
        ApplicationOperation::ExtensionInventory => Some(Ok(())),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutorCommandBody {
    Application {
        request: ApplicationOperation,
    },
    CloudRun {
        spec: RunSpec,
    },
    CancelRun {
        session_id: SessionId,
        run_id: RunId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorCommand {
    pub command_id: CommandId,
    pub scope: ExecutorScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_provenance: Option<ternilo_protocol::InputProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_authorization: Option<NodeInputAuthorization>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub body: ExecutorCommandBody,
}

impl ExecutorCommand {
    pub fn validate(&self, now_ms: u64) -> Result<(), HarnessError> {
        self.command_id.validate()?;
        self.scope.validate()?;
        if let Some(provenance) = &self.input_provenance {
            provenance.validate()?;
        }
        if self.input_authorization.is_some()
            && !matches!(
                self.input_provenance.as_ref().map(|value| &value.author),
                Some(ternilo_protocol::InputAuthor::Account { .. })
            )
        {
            return Err(HarnessError::invalid(
                "account authorization requires account input provenance",
            ));
        }
        if self.expires_at_ms <= self.issued_at_ms {
            return Err(HarnessError::invalid(
                "command expiry must be later than issue time",
            ));
        }
        if now_ms > self.expires_at_ms {
            return Err(HarnessError::policy("executor command has expired"));
        }
        match &self.body {
            ExecutorCommandBody::Application { request } => request.validate(),
            ExecutorCommandBody::CloudRun { spec } => spec.validate_shape(),
            ExecutorCommandBody::CancelRun { session_id, run_id } => {
                session_id.validate()?;
                run_id.validate()
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOutcome {
    Ok { value: Value },
    Error { error: HarnessError },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandReply {
    pub command_id: CommandId,
    pub completed_at_ms: u64,
    pub outcome: CommandOutcome,
}

impl CommandReply {
    #[must_use]
    pub fn success(command_id: CommandId, completed_at_ms: u64, value: Value) -> Self {
        Self {
            command_id,
            completed_at_ms,
            outcome: CommandOutcome::Ok { value },
        }
    }

    #[must_use]
    pub fn failure(command_id: CommandId, completed_at_ms: u64, error: HarnessError) -> Self {
        Self {
            command_id,
            completed_at_ms,
            outcome: CommandOutcome::Error { error },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    pub scope: ExecutorScope,
    pub session_id: SessionId,
    pub after_seq: Option<u64>,
    pub events: Vec<SessionEvent>,
}

impl EventBatch {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.scope.validate()?;
        self.session_id.validate()?;
        let mut expected = self.after_seq.map_or(0, |seq| seq.saturating_add(1));
        for event in &self.events {
            if event.seq != expected {
                return Err(HarnessError::invalid(format!(
                    "event batch sequence mismatch: expected {expected}, found {}",
                    event.seq
                )));
            }
            expected = expected.saturating_add(1);
        }
        Ok(())
    }
}

/// A bounded delta from one durable local upload ledger. Attachment contents and
/// private object references are deliberately absent from this wire contract.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedUploadBatch {
    pub scope: ExecutorScope,
    pub stream_id: String,
    pub after_seq: Option<u64>,
    pub changes: Vec<AcceptedUploadChange>,
}

pub fn validate_upload_stream_id(stream_id: &str) -> Result<(), HarnessError> {
    if stream_id.len() != 32 || !stream_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(HarnessError::invalid(
            "upload stream ID must be 32 hexadecimal characters",
        ));
    }
    Ok(())
}

impl AcceptedUploadBatch {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.scope.validate()?;
        validate_upload_stream_id(&self.stream_id)?;
        if self.changes.is_empty() || self.changes.len() > 200 {
            return Err(HarnessError::invalid(
                "upload delta must contain 1 to 200 changes",
            ));
        }
        let mut previous = self.after_seq.unwrap_or(0);
        for change in &self.changes {
            change.session_id.validate()?;
            if previous.checked_add(1) != Some(change.seq) || i64::try_from(change.seq).is_err() {
                return Err(HarnessError::invalid(
                    "upload delta contains a sequence gap",
                ));
            }
            if let AcceptedUploadChangeKind::UploadAccepted { upload } = &change.kind {
                upload.validate()?;
                if i64::try_from(upload.created_at_ms).is_err() {
                    return Err(HarnessError::invalid(
                        "upload timestamp exceeds storage range",
                    ));
                }
            }
            previous = change.seq;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutorFrame {
    Hello {
        hello: ExecutorHello,
    },
    Heartbeat {
        sent_at_ms: u64,
        active_sessions: Vec<SessionId>,
    },
    Reply {
        reply: CommandReply,
    },
    EventBatch {
        batch: EventBatch,
    },
    LiveInvalidation {
        invalidation: ExecutorLiveInvalidation,
    },
    UploadSyncStarted {
        scope: ExecutorScope,
        stream_id: String,
    },
    AcceptedUploads {
        batch: AcceptedUploadBatch,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorLiveInvalidation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub dirty: ternilo_protocol::SessionLiveDirty,
    #[serde(default)]
    pub workbench: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ternilo_protocol::SessionLiveActivity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlFrame {
    Welcome {
        protocol_version: u32,
        connection_id: ConnectionId,
        scope: ExecutorScope,
        heartbeat_interval_ms: u64,
        event_cursors: Vec<SessionCursor>,
    },
    Command {
        command: Box<ExecutorCommand>,
    },
    UploadsAcknowledged {
        stream_id: String,
        last_seq: Option<u64>,
    },
    EventsAcknowledged {
        cursor: SessionCursor,
    },
    Shutdown {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorSummary {
    pub executor_id: ExecutorId,
    pub executor_kind: ExecutorKind,
    pub catalog_revision: String,
    pub capabilities: ExecutorCapabilities,
    pub connected_at_ms: u64,
    pub last_seen_at_ms: u64,
}

fn require_text(value: &str, label: &str) -> Result<(), HarnessError> {
    if value.trim().is_empty() {
        Err(HarnessError::invalid(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
}

fn require_json_object(value: &Value, label: &str) -> Result<(), HarnessError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(HarnessError::invalid(format!(
            "{label} must be a JSON object"
        )))
    }
}

fn validate_optional_text(value: Option<&str>, label: &str) -> Result<(), HarnessError> {
    if let Some(value) = value {
        require_text(value, label)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_delta_contract_is_bounded_sequential_and_metadata_only() {
        let change = AcceptedUploadChange {
            seq: 1,
            session_id: SessionId::new("session"),
            kind: AcceptedUploadChangeKind::UploadAccepted {
                upload: ternilo_protocol::AcceptedUploadMetadata {
                    submission_id: SubmissionId::new("submission"),
                    attachment_index: 0,
                    created_at_ms: 100,
                    submitted_run_id: RunId::new("run"),
                    name: "file.txt".to_owned(),
                    media_type: "text/plain".to_owned(),
                },
            },
        };
        let batch = AcceptedUploadBatch {
            scope: ExecutorScope {
                tenant_id: TenantId::new("tenant"),
                user_id: UserId::new("owner"),
            },
            stream_id: "1234567890abcdef1234567890abcdef".to_owned(),
            after_seq: None,
            changes: vec![change],
        };
        batch.validate().unwrap();
        let mut wire = serde_json::to_value(&batch).unwrap();
        assert_eq!(
            serde_json::from_value::<AcceptedUploadBatch>(wire.clone()).unwrap(),
            batch
        );
        wire["changes"][0]["kind"]["upload"]["content"] = serde_json::json!("private");
        assert!(serde_json::from_value::<AcceptedUploadBatch>(wire).is_err());
        let mut gap = batch.clone();
        gap.changes[0].seq = 2;
        assert!(gap.validate().is_err());
        let mut too_many = batch;
        too_many.changes = (1..=201)
            .map(|seq| AcceptedUploadChange {
                seq,
                ..too_many.changes[0].clone()
            })
            .collect();
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn protocol_round_trip_preserves_a_scoped_command() {
        let frame = ControlFrame::Command {
            command: Box::new(ExecutorCommand {
                command_id: CommandId::new("command-1"),
                input_provenance: None,
                scope: ExecutorScope {
                    tenant_id: TenantId::new("tenant-a"),
                    user_id: UserId::new("user-a"),
                },
                input_authorization: None,
                issued_at_ms: 10,
                expires_at_ms: 20,
                body: ExecutorCommandBody::Application {
                    request: ApplicationOperation::Snapshot,
                },
            }),
        };
        let encoded = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlFrame>(&encoded).unwrap(),
            frame
        );
    }

    #[test]
    fn executor_live_capabilities_have_an_explicit_versioned_wire() {
        assert_eq!(EXECUTOR_PROTOCOL_VERSION, 45);
        let mut peer = ExecutorHello {
            protocol_version: 40,
            executor_id: ExecutorId::new("native-model-peer"),
            executor_kind: ExecutorKind::EdgeNode,
            instance_nonce: "native-instance".to_owned(),
            catalog_revision: "native-catalog".to_owned(),
            capabilities: BTreeSet::new(),
        };
        assert!(peer.validate().is_err());
        peer.protocol_version = 41;
        assert!(peer.validate().is_err());
        peer.protocol_version = 42;
        assert!(peer.validate().is_err());
        peer.protocol_version = 43;
        assert!(peer.validate().is_err());
        peer.protocol_version = EXECUTOR_PROTOCOL_VERSION;
        peer.validate().unwrap();
        let workspace = ApplicationOperation::SessionWorkspace {
            session_id: SessionId::new("session-a"),
            request: WorkspaceRequest::Read {
                path: "src/main.rs".to_owned(),
            },
        };
        workspace.validate().unwrap();
        assert!(workspace.requires_ephemeral_delivery());
        let encoded = serde_json::to_value(&workspace).unwrap();
        assert_eq!(encoded["request"]["kind"], "read");
        assert_eq!(
            serde_json::from_value::<ApplicationOperation>(encoded).unwrap(),
            workspace
        );
        assert_eq!(
            serde_json::to_value(ExecutorCapability::ExtensionManagement).unwrap(),
            serde_json::json!("extension_management")
        );
        assert_eq!(
            serde_json::to_value(ExecutorCapability::AddressedSessionCommands).unwrap(),
            serde_json::json!("addressed_session_commands")
        );
        assert_eq!(
            serde_json::to_value(ExecutorCapability::AddressedSubagents).unwrap(),
            serde_json::json!("addressed_subagents")
        );
        assert_eq!(
            serde_json::to_value(ExecutorCapability::AgentTeamHost).unwrap(),
            serde_json::json!("agent_team_host")
        );
        assert_eq!(
            serde_json::to_value(ExecutorCapability::LiveInvalidations).unwrap(),
            serde_json::json!("live_invalidations")
        );
        let frame = ExecutorFrame::LiveInvalidation {
            invalidation: ExecutorLiveInvalidation {
                session_id: Some(SessionId::new("session-a")),
                dirty: ternilo_protocol::SessionLiveDirty {
                    inbox: true,
                    ..Default::default()
                },
                workbench: false,
                activity: Some(ternilo_protocol::SessionLiveActivity {
                    session_id: SessionId::new("session-a"),
                    running: true,
                    execution: None,
                    updated_at_ms: 42,
                }),
            },
        };
        let encoded = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            serde_json::from_str::<ExecutorFrame>(&encoded).unwrap(),
            frame
        );
    }

    #[test]
    fn extension_lifecycle_operations_have_distinct_validated_wires() {
        let revoke = ApplicationOperation::ExtensionRevoke {
            package_id: "dev.ternilo.example".to_owned(),
            version: "1.0.0".to_owned(),
        };
        let uninstall = ApplicationOperation::ExtensionUninstall {
            package_id: "dev.ternilo.example".to_owned(),
            version: "1.0.0".to_owned(),
        };
        for operation in [&revoke, &uninstall] {
            operation.validate().unwrap();
            assert!(
                operation
                    .validate_executor_capabilities(&ExecutorCapabilities::new())
                    .is_err()
            );
            operation
                .validate_executor_capabilities(&ExecutorCapabilities::from([
                    ExecutorCapability::ExtensionManagement,
                ]))
                .unwrap();
            assert_eq!(
                serde_json::from_value::<ApplicationOperation>(
                    serde_json::to_value(operation).unwrap()
                )
                .unwrap(),
                *operation
            );
        }
        assert_eq!(
            serde_json::to_value(revoke).unwrap()["operation"],
            "extension_revoke"
        );
        assert_eq!(
            serde_json::to_value(uninstall).unwrap()["operation"],
            "extension_uninstall"
        );
        assert!(
            ApplicationOperation::ExtensionSetEnabled {
                package_id: String::new(),
                version: "1.0.0".to_owned(),
                enabled: true,
            }
            .validate()
            .is_err()
        );
        assert!(
            ApplicationOperation::PublisherTrust {
                publisher: serde_json::json!([]),
            }
            .validate()
            .is_err()
        );
        ApplicationOperation::Snapshot
            .validate_executor_capabilities(&ExecutorCapabilities::new())
            .unwrap();
    }

    #[test]
    fn event_batches_must_be_contiguous() {
        let batch = EventBatch {
            scope: ExecutorScope {
                tenant_id: TenantId::new("tenant"),
                user_id: UserId::new("user"),
            },
            session_id: SessionId::new("session"),
            after_seq: Some(4),
            events: vec![SessionEvent {
                seq: 6,
                occurred_at_ms: 0,
                run_id: RunId::new("run"),
                kind: ternilo_protocol::SessionEventKind::TurnStarted,
            }],
        };
        assert!(batch.validate().is_err());
    }

    #[test]
    fn event_batch_wire_preserves_compaction_model_and_turn_evidence() {
        let frame = ExecutorFrame::EventBatch {
            batch: EventBatch {
                scope: ExecutorScope {
                    tenant_id: TenantId::new("tenant"),
                    user_id: UserId::new("user"),
                },
                session_id: SessionId::new("session"),
                after_seq: None,
                events: vec![
                    SessionEvent {
                        seq: 0,
                        occurred_at_ms: 10,
                        run_id: RunId::new("run"),
                        kind: ternilo_protocol::SessionEventKind::ContextCompactionStarted {
                            compaction_id: "compaction-run-0".to_owned(),
                            automatic: true,
                            source_command_id: None,
                            turn: 2,
                        },
                    },
                    SessionEvent {
                        seq: 1,
                        occurred_at_ms: 20,
                        run_id: RunId::new("run"),
                        kind: ternilo_protocol::SessionEventKind::AssistantMessage {
                            step: 1,
                            response: ternilo_protocol::ModelResponse {
                                provider: "openai".to_owned(),
                                model: "gpt-test".to_owned(),
                                content: "partial".to_owned(),
                                reasoning_content: None,
                                provider_state: None,
                                tool_calls: Vec::new(),
                                usage: Some(ternilo_protocol::ModelUsage {
                                    input_tokens: 20,
                                    output_tokens: 7,
                                    cached_input_tokens: 5,
                                    cache_write_tokens: Some(3),
                                    reasoning_tokens: 2,
                                }),
                                finish_reason: ternilo_protocol::ModelFinishReason::MaxTokens,
                                provider_request_id: None,
                                attempts: 1,
                                request_digest: None,
                                replayed: false,
                            },
                        },
                    },
                    SessionEvent {
                        seq: 2,
                        occurred_at_ms: 30,
                        run_id: RunId::new("run"),
                        kind: ternilo_protocol::SessionEventKind::TurnFinished {
                            answer: "partial".to_owned(),
                            finish_reason: ternilo_protocol::TurnFinishReason::MaxTokens,
                        },
                    },
                ],
            },
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["batch"]["events"][0]["turn"], 2);
        assert_eq!(json["batch"]["events"][1]["response"]["provider"], "openai");
        assert_eq!(
            json["batch"]["events"][1]["response"]["usage"]["cache_write_tokens"],
            3
        );
        assert_eq!(json["batch"]["events"][2]["finish_reason"], "max_tokens");
        let decoded = serde_json::from_value::<ExecutorFrame>(json).unwrap();
        assert_eq!(decoded, frame);
        if let ExecutorFrame::EventBatch { batch } = decoded {
            batch.validate().unwrap();
        }
    }

    #[test]
    fn authorization_interactions_and_record_writes_are_never_durable_commands() {
        let location = ApplicationOperation::WorkspaceLocation {
            workspace_id: WorkspaceId::new("private-workspace"),
        };
        assert!(location.requires_ephemeral_delivery());
        location.validate().unwrap();
        assert!(
            ApplicationOperation::WorkspaceLocation {
                workspace_id: WorkspaceId::new("invalid workspace"),
            }
            .validate()
            .is_err()
        );
        let file = ApplicationOperation::SessionFileContent {
            session_id: SessionId::new("files-session"),
            file_id: "upload-1-0".to_owned(),
        };
        assert!(file.requires_ephemeral_delivery());
        let key = AuthorizationCredentialKey {
            space: ternilo_protocol::AuthorizationCredentialSpace::Reference,
            key: "PROVIDER_KEY".to_owned(),
        };
        let answer = ApplicationOperation::AuthorizationAnswer {
            answer: AuthorizationPromptAnswer {
                prompt_id: "prompt-1".to_owned(),
                surface_id: "web-tab".to_owned(),
                value: "secret-value".to_owned(),
            },
        };
        assert!(answer.contains_secret_material());
        assert!(answer.requires_ephemeral_delivery());

        let begin = ApplicationOperation::AuthorizationBegin {
            request: AuthorizationBeginRequest {
                key,
                method: Some("api-key".to_owned()),
                surface_id: "web-tab".to_owned(),
            },
        };
        assert!(!begin.contains_secret_material());
        assert!(begin.requires_ephemeral_delivery());

        let record = ApplicationOperation::CredentialRecordSet {
            key: "oauth/account".to_owned(),
            kind: "oauth".to_owned(),
            payload: serde_json::json!({"access_token": "secret-value"}),
        };
        assert!(record.contains_secret_material());
        assert!(record.requires_ephemeral_delivery());

        let discovery = ApplicationOperation::ProviderDiscover {
            request: ProviderModelDiscoveryRequest {
                provider_id: None,
                base_url: Some("https://draft.example/v1".to_owned()),
                protocol: Some(ternilo_protocol::ProviderProtocol::OpenAiResponses),
                timeout_ms: Some(30_000),
                api_key: Some("draft-secret".to_owned()),
            },
        };
        discovery.validate().unwrap();
        assert!(discovery.contains_secret_material());
        assert!(discovery.requires_ephemeral_delivery());
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(
                &serde_json::to_string(&discovery).unwrap()
            )
            .unwrap(),
            discovery
        );
    }

    #[test]
    fn extension_provider_materialization_is_typed_validated_and_not_secret_material() {
        let operation = ApplicationOperation::ProviderMaterialize {
            request: ExtensionProviderMaterializeRequest {
                package_id: "dev.example.models".to_owned(),
                version: "1.0.0".to_owned(),
                template: "primary".to_owned(),
                provider_id: "example-models".to_owned(),
                api_key_ref: Some("EXAMPLE_API_KEY".to_owned()),
            },
        };
        operation.validate().unwrap();
        assert!(!operation.contains_secret_material());
        assert!(!operation.requires_ephemeral_delivery());
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(
                &serde_json::to_string(&operation).unwrap()
            )
            .unwrap(),
            operation
        );
    }

    #[test]
    fn sidebar_ordering_round_trips_through_application_rpc() {
        let operation = ApplicationOperation::SidebarOrderingSet {
            ordering: SidebarOrdering {
                workspace_order: vec!["workspace-b".to_owned(), "workspace-a".to_owned()],
                session_order_by_account: std::collections::BTreeMap::from([(
                    "workspace-a".to_owned(),
                    vec!["session-2".to_owned(), "session-1".to_owned()],
                )]),
            },
        };
        operation.validate().unwrap();
        assert!(!operation.contains_secret_material());
        assert!(!operation.requires_ephemeral_delivery());
        let encoded = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            operation
        );
    }

    #[test]
    fn agent_preset_selection_is_typed_and_validated() {
        let create = ApplicationOperation::SessionCreate {
            workspace_id: WorkspaceId::new("workspace"),
            session_id: None,
            agent_id: None,
            agent_preset: Some("code-agent".to_owned()),
            permissions: Some(PermissionPreset::ReadOnly),
        };
        create.validate().unwrap();
        let encoded = serde_json::to_value(&create).unwrap();
        assert_eq!(encoded["agent_preset"], "code-agent");
        assert_eq!(encoded["permissions"], "read_only");

        let update = ApplicationOperation::SessionUpdate {
            server_model: None,
            session_id: SessionId::new("session"),
            title: None,
            permissions: None,
            model: None,
            agent_preset: Some("Invalid Agent".to_owned()),
            profile_plugins: None,
            mode: None,
        };
        assert!(update.validate().is_err());
        assert!(!ApplicationOperation::AgentPresetList.requires_ephemeral_delivery());
    }

    #[test]
    fn agent_preset_lifecycle_round_trips_through_application_rpc() {
        let operations = [
            ApplicationOperation::AgentPresetGet {
                preset_id: "custom-agent".to_owned(),
            },
            ApplicationOperation::AgentPresetCopy {
                request: AgentPresetCopyRequest {
                    from: "standard".to_owned(),
                    id: "custom-agent".to_owned(),
                    display_name: Some("Custom Agent".to_owned()),
                },
            },
            ApplicationOperation::AgentPresetUpdate {
                preset_id: "custom-agent".to_owned(),
                request: AgentPresetUpdateRequest {
                    display_name: "Updated Agent".to_owned(),
                    description: "Updated through the Node boundary".to_owned(),
                    profile: ternilo_protocol::Profile::default(),
                },
            },
            ApplicationOperation::AgentPresetDelete {
                preset_id: "custom-agent".to_owned(),
            },
            ApplicationOperation::AgentPresetSetDefault {
                preset_id: "custom-agent".to_owned(),
            },
        ];
        for operation in operations {
            operation.validate().unwrap();
            assert_eq!(
                serde_json::from_value::<ApplicationOperation>(
                    serde_json::to_value(&operation).unwrap()
                )
                .unwrap(),
                operation
            );
            assert_eq!(
                operation.requires_ephemeral_delivery(),
                matches!(operation, ApplicationOperation::AgentPresetGet { .. }),
                "editor GET includes the host base and must never enter the durable relay journal"
            );
        }
        assert!(
            ApplicationOperation::AgentPresetGet {
                preset_id: "Invalid Agent".to_owned(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn default_model_selection_round_trips_through_application_rpc() {
        let operation = ApplicationOperation::DefaultModelSet {
            selection: DefaultModelSelection::NamedProvider {
                provider_id: "provider-a".to_owned(),
                model: "model-a".to_owned(),
                reasoning_effort: None,
            },
        };
        operation.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<ApplicationOperation>(
                serde_json::to_value(&operation).unwrap()
            )
            .unwrap(),
            operation
        );
        assert!(!ApplicationOperation::DefaultModelGet.requires_ephemeral_delivery());
    }

    #[test]
    fn skill_turn_round_trip_is_typed_and_validated() {
        let operation = ApplicationOperation::SessionSkillTurn {
            session_id: SessionId::new("session"),
            run_id: Some("run-1".to_owned()),
            name: "review-code".to_owned(),
            input: "Review src/lib.rs".to_owned(),
            attachments: Vec::new(),
        };
        operation.validate().unwrap();
        let encoded = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            operation
        );

        let invalid = ApplicationOperation::SessionSkillTurn {
            session_id: SessionId::new("session"),
            run_id: None,
            name: "Review Code".to_owned(),
            input: String::new(),
            attachments: Vec::new(),
        };
        assert!(invalid.validate().is_err());

        let resolve = ApplicationOperation::SessionSkillResolve {
            session_id: SessionId::new("session"),
            name: "review-code".to_owned(),
            input: "Review src/lib.rs".to_owned(),
        };
        resolve.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<ApplicationOperation>(serde_json::to_value(&resolve).unwrap())
                .unwrap(),
            resolve
        );
    }

    #[test]
    fn feedback_command_preserves_empty_input_for_a_durable_usage_error() {
        let operation = ApplicationOperation::SessionCommandFeedback {
            session_id: SessionId::new("session"),
            text: String::new(),
        };
        operation.validate().unwrap();
        let encoded = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            operation
        );
        assert!(
            ApplicationOperation::SessionCommandFeedback {
                session_id: SessionId::new("invalid session"),
                text: "feedback".to_owned(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn addressed_subagent_actions_round_trip_and_validate() {
        let followup = ApplicationOperation::SessionSubagentFollowup {
            session_id: SessionId::new("session"),
            subagent_id: SubagentId::new("agent-1"),
            message: "continue with the focused review".to_owned(),
        };
        followup.validate().unwrap();
        let encoded = serde_json::to_string(&followup).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            followup
        );

        let interrupt = ApplicationOperation::SessionSubagentInterrupt {
            session_id: SessionId::new("session"),
            subagent_id: SubagentId::new("agent-1"),
        };
        interrupt.validate().unwrap();
        let encoded = serde_json::to_string(&interrupt).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            interrupt
        );

        assert!(
            ApplicationOperation::SessionSubagentFollowup {
                session_id: SessionId::new("session"),
                subagent_id: SubagentId::new("agent-1"),
                message: "  ".to_owned(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn agent_team_operations_round_trip_and_validate() {
        let member_id = ternilo_protocol::AgentTeamMemberId::new("member-lead");
        let task_id = AgentTeamTaskId::new("task-review");
        let operations = [
            ApplicationOperation::SessionAgentTeamSnapshot {
                session_id: SessionId::new("session"),
            },
            ApplicationOperation::SessionAgentTeamTaskCreate {
                session_id: SessionId::new("session"),
                request: AgentTeamTaskCreate {
                    subject: "Review".to_owned(),
                    description: String::new(),
                    status: ternilo_protocol::AgentTeamTaskStatus::Pending,
                    dependencies: Vec::new(),
                    owner: Some(member_id.clone()),
                },
            },
            ApplicationOperation::SessionAgentTeamTaskReplace {
                session_id: SessionId::new("session"),
                task_id: task_id.clone(),
                request: AgentTeamTaskReplace {
                    expected_revision: 1,
                    subject: "Review".to_owned(),
                    description: "Complete the review".to_owned(),
                    status: ternilo_protocol::AgentTeamTaskStatus::InProgress,
                    dependencies: Vec::new(),
                    owner: Some(member_id.clone()),
                },
            },
            ApplicationOperation::SessionAgentTeamTaskDelete {
                session_id: SessionId::new("session"),
                task_id,
                expected_revision: 2,
            },
            ApplicationOperation::SessionAgentTeamMessageSend {
                session_id: SessionId::new("session"),
                request: AgentTeamMessageSend {
                    to: member_id,
                    content: "Ready".to_owned(),
                },
            },
            ApplicationOperation::SessionAgentTeamMessageRead {
                session_id: SessionId::new("session"),
                message_id: AgentTeamMessageId::new("message-ready"),
            },
        ];
        for operation in operations {
            operation.validate().unwrap();
            let encoded = serde_json::to_string(&operation).unwrap();
            assert_eq!(
                serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
                operation
            );
        }

        assert!(
            ApplicationOperation::SessionAgentTeamTaskDelete {
                session_id: SessionId::new("session"),
                task_id: AgentTeamTaskId::new("task"),
                expected_revision: 0,
            }
            .validate()
            .is_err()
        );
        assert!(
            ApplicationOperation::SessionAgentTeamMessageSend {
                session_id: SessionId::new("session"),
                request: AgentTeamMessageSend {
                    to: ternilo_protocol::AgentTeamMemberId::new("member"),
                    content: " ".to_owned(),
                },
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn workspace_lifecycle_operations_round_trip_and_validate() {
        let rename = ApplicationOperation::WorkspaceRename {
            workspace_id: WorkspaceId::new("workspace"),
            title: "Renamed workspace".to_owned(),
        };
        rename.validate().unwrap();
        let encoded = serde_json::to_string(&rename).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            rename
        );

        let unregister = ApplicationOperation::WorkspaceUnregister {
            workspace_id: WorkspaceId::new("workspace"),
        };
        unregister.validate().unwrap();

        assert!(
            ApplicationOperation::WorkspaceRename {
                workspace_id: WorkspaceId::new("workspace"),
                title: "  ".to_owned(),
            }
            .validate()
            .is_err()
        );
        assert!(
            ApplicationOperation::WorkspaceUnregister {
                workspace_id: WorkspaceId::new("invalid workspace"),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn session_fork_and_archive_operations_round_trip_and_validate() {
        let fork = ApplicationOperation::SessionFork {
            session_id: SessionId::new("session"),
            at_seq: Some(42),
        };
        fork.validate().unwrap();
        let encoded = serde_json::to_string(&fork).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            fork
        );

        let archive = ApplicationOperation::SessionArchive {
            session_id: SessionId::new("session"),
        };
        archive.validate().unwrap();
        let restore = ApplicationOperation::SessionRestore {
            session_id: SessionId::new("session"),
        };
        restore.validate().unwrap();
        let encoded = serde_json::to_value(&restore).unwrap();
        assert_eq!(encoded["operation"], "session_restore");
        assert_eq!(
            serde_json::from_value::<ApplicationOperation>(encoded).unwrap(),
            restore
        );
        assert!(
            ApplicationOperation::SessionRestore {
                session_id: SessionId::new("invalid session"),
            }
            .validate()
            .is_err()
        );

        assert!(
            ApplicationOperation::SessionFork {
                session_id: SessionId::new("invalid session"),
                at_seq: None,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn runtime_service_operations_keep_exact_session_and_source_identifiers() {
        for operation in [
            "session_services",
            "session_service_start",
            "session_service_stop",
        ] {
            let mut value =
                serde_json::json!({"operation": operation, "session_id": "service-session"});
            if operation != "session_services" {
                value["service_id"] = serde_json::json!("mcp:project-tools");
            }
            let request: ApplicationOperation = serde_json::from_value(value.clone()).unwrap();
            request.validate().unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), value);
            value["session_id"] = serde_json::json!("invalid session");
            assert!(
                serde_json::from_value::<ApplicationOperation>(value)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for operation in ["session_service_start", "session_service_stop"] {
            let request: ApplicationOperation = serde_json::from_value(serde_json::json!({"operation":operation,"session_id":"service-session","service_id":"  "})).unwrap();
            assert!(request.validate().is_err());
        }
    }

    #[test]
    fn session_queue_operations_round_trip_and_validate_exact_occurrences() {
        let submit = ApplicationOperation::SessionSubmit {
            session_id: SessionId::new("session"),
            request: SessionSubmissionRequest {
                delivery: ternilo_protocol::SubmissionDelivery::Steer,
                run_id: Some(RunId::new("run")),
                content: ternilo_protocol::SubmissionContent::Prompt {
                    input: "steer now".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        };
        submit.validate().unwrap();
        let encoded = serde_json::to_string(&submit).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            submit
        );

        let edit = ApplicationOperation::SessionQueueEdit {
            session_id: SessionId::new("session"),
            submission_id: ternilo_protocol::SubmissionId::new("submission"),
            request: QueueEditRequest {
                input: "edited".to_owned(),
                expected_updated_at_ms: 10,
            },
        };
        edit.validate().unwrap();
        let mut encoded = serde_json::to_value(&edit).unwrap();
        assert_eq!(encoded["request"]["expected_updated_at_ms"], 10);
        assert_eq!(
            serde_json::from_value::<ApplicationOperation>(encoded.clone()).unwrap(),
            edit
        );
        encoded["request"]
            .as_object_mut()
            .unwrap()
            .remove("expected_updated_at_ms");
        assert!(serde_json::from_value::<ApplicationOperation>(encoded).is_err());
        assert!(
            ApplicationOperation::SessionQueueRemove {
                session_id: SessionId::new("session"),
                submission_id: ternilo_protocol::SubmissionId::new("invalid id"),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn structured_question_answer_round_trips_through_the_typed_operation() {
        let operation = ApplicationOperation::AnswerQuestion {
            answer: UserAnswer {
                question_id: "question-1".to_owned(),
                selected: vec!["Tests".to_owned(), "Docs".to_owned()],
                custom: Some("Release notes".to_owned()),
            },
        };
        operation.validate().unwrap();
        let encoded = serde_json::to_string(&operation).unwrap();
        assert_eq!(
            serde_json::from_str::<ApplicationOperation>(&encoded).unwrap(),
            operation
        );
    }
}
