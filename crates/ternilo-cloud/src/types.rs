use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    AgentId, Attachment, HarnessError, PermissionPreset, Profile, ReferenceContext, RunId,
    RunLimits, RunModelSnapshot, RunOutcome, RunSpec, SessionEvent, SessionId, SessionMode,
    SubagentSessionMetadata, SubmissionReference, TenantId, UserId, UserMessageSource,
    UserQuestion, WorkspaceBinding, WorkspaceId,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudRunDraft {
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub limits: RunLimits,
    pub permissions: PermissionPreset,
    pub mode: SessionMode,
    pub profile: Profile,
    pub input: String,
    #[serde(default)]
    pub references: Vec<SubmissionReference>,
    #[serde(default)]
    pub reference_contexts: Vec<ReferenceContext>,
    pub attachments: Vec<Attachment>,
    pub reserved_model_tokens: u64,
}

impl CloudRunDraft {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.project_id.is_empty()
            || self.project_id.len() > 128
            || self
                .project_id
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(HarnessError::invalid(
                "cloud project_id must contain 1 to 128 bytes without whitespace or control characters",
            ));
        }
        self.agent_id.validate()?;
        self.workspace_id.validate()?;
        self.session_id.validate()?;
        if let Some(run_id) = &self.run_id {
            run_id.validate()?;
        }
        if self.reserved_model_tokens == 0 {
            return Err(HarnessError::invalid(
                "reserved_model_tokens must be positive",
            ));
        }
        if self.permissions == PermissionPreset::FullAccess {
            return Err(HarnessError::policy(
                "cloud runs cannot request full host access",
            ));
        }
        if self.references.len() > 8 || self.reference_contexts.len() > 8 {
            return Err(HarnessError::invalid(
                "one cloud run may contain at most 8 references",
            ));
        }
        for reference in &self.references {
            reference.validate()?;
        }
        for context in &self.reference_contexts {
            context.validate()?;
        }
        if self.attachments.len() > 16 {
            return Err(HarnessError::invalid(
                "one cloud run may contain at most 16 attachments",
            ));
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledRun {
    /// Trusted automation origin; browser run requests never populate this field.
    pub automated_input: Option<ternilo_protocol::AutomatedInputSource>,
    pub actor_user_id: UserId,
    pub authorization_session_id: SessionId,
    pub spec: RunSpec,
    pub reserved_model_tokens: u64,
    pub priority: i32,
    pub max_attempts: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRunState {
    Queued,
    Leased,
    Running,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudSessionState {
    Idle,
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Indeterminate,
}

impl CloudSessionState {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "idle" => Ok(Self::Idle),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "indeterminate" => Ok(Self::Indeterminate),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown cloud session state {value:?}"
            ))),
        }
    }
}

impl CloudRunState {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "queued" => Ok(Self::Queued),
            "leased" => Ok(Self::Leased),
            "running" => Ok(Self::Running),
            "cancel_requested" => Ok(Self::CancelRequested),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "indeterminate" => Ok(Self::Indeterminate),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown cloud run state {value:?}"
            ))),
        }
    }

    #[must_use]
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Indeterminate
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudRunRecord {
    pub actor_user_id: UserId,
    pub authorization_session_id: SessionId,
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub user_id: UserId,
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub spec_digest_hex: String,
    pub state: CloudRunState,
    pub attempt: u32,
    pub max_attempts: u32,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub outcome: Option<RunOutcome>,
    pub error: Option<HarnessError>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudSessionRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ternilo_protocol::SessionExecutionActivity>,
    pub tenant_id: TenantId,
    pub session_id: SessionId,
    pub user_id: UserId,
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub parent_session_id: Option<SessionId>,
    pub subagent: Option<SubagentSessionMetadata>,
    pub agent_id: AgentId,
    pub title: String,
    pub archived_at_ms: Option<u64>,
    pub state: CloudSessionState,
    pub permissions: PermissionPreset,
    pub model: Option<RunModelSnapshot>,
    pub reserved_model_tokens: u64,
    pub agent_preset: String,
    pub profile_plugins: Vec<ternilo_protocol::PluginEntry>,
    pub mode: SessionMode,
    pub last_seq: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudSessionDraft {
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub session_id: Option<SessionId>,
    pub agent_id: AgentId,
    pub title: String,
    pub permissions: PermissionPreset,
    pub model: Option<RunModelSnapshot>,
    pub reserved_model_tokens: u64,
    pub agent_preset: String,
    pub profile_plugins: Vec<ternilo_protocol::PluginEntry>,
    pub mode: SessionMode,
}

impl CloudSessionDraft {
    pub fn validate(&self) -> Result<(), HarnessError> {
        bounded_text(&self.project_id, "cloud session project_id", 128)?;
        self.workspace_id.validate()?;
        if let Some(session_id) = &self.session_id {
            session_id.validate()?;
        }
        self.agent_id.validate()?;
        bounded_text(&self.title, "cloud session title", 256)?;
        if let Some(model) = &self.model {
            model.validate()?;
        }
        bounded_text(&self.agent_preset, "cloud session Agent preset", 128)?;
        if self.permissions == PermissionPreset::FullAccess {
            return Err(HarnessError::policy(
                "cloud sessions cannot grant full host access",
            ));
        }
        if self.reserved_model_tokens == 0 {
            return Err(HarnessError::invalid(
                "cloud session model token budget must be positive",
            ));
        }
        if self.profile_plugins.len() > 128 {
            return Err(HarnessError::invalid(
                "cloud session plugin override count exceeds 128",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudSessionUpdate {
    pub title: Option<String>,
    pub permissions: Option<PermissionPreset>,
    pub model: Option<Option<RunModelSnapshot>>,
    pub reserved_model_tokens: Option<u64>,
    pub agent_preset: Option<String>,
    pub profile_plugins: Option<Vec<ternilo_protocol::PluginEntry>>,
    pub mode: Option<SessionMode>,
}

fn bounded_text(value: &str, label: &str, maximum: usize) -> Result<(), HarnessError> {
    if value.trim().is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(HarnessError::invalid(format!(
            "{label} must contain 1 to {maximum} bytes without control characters"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CloudRunClaim {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ternilo_protocol::InputProvenance>,
    pub actor_user_id: UserId,
    pub authorization_session_id: SessionId,
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub session_id: SessionId,
    pub workspace_use: crate::WorkspaceUseTicket,
    pub lease_token: u64,
    pub spec: RunSpec,
    pub spec_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartedRun {
    pub claim: CloudRunClaim,
    pub fencing_token: u64,
    pub prior_events: Vec<SessionEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionEnvelope {
    #[serde(default)]
    pub additional_inputs: Vec<ternilo_protocol::SteeringInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ternilo_protocol::InputProvenance>,
    pub spec: RunSpec,
    pub workspace: WorkspaceBinding,
    pub prior_events: Vec<SessionEvent>,
    #[serde(default)]
    pub attachment_objects: Vec<ExecutionAttachmentObject>,
    pub extensions: Vec<ternilo_extension::ExtensionDistribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<UserMessageSource>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionAttachmentObject {
    pub attachment: Attachment,
    pub content_base64: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudPendingQuestion {
    pub session_id: SessionId,
    pub question: UserQuestion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TerminalState {
    Succeeded,
    Failed,
    Cancelled,
    Indeterminate,
}

impl TerminalState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Indeterminate => "indeterminate",
        }
    }
}
