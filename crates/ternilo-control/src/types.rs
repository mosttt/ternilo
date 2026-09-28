use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_protocol::{
    HarnessError, PermissionPreset, PluginEntry, SessionId, SessionMode, SubagentSessionMetadata,
    TenantId, UserId, WorkspaceId,
};
use ternilo_transport::{ExecutorId, ExecutorScope};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcPrincipal {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

impl OidcPrincipal {
    pub fn validate(&self) -> Result<(), HarnessError> {
        require_bounded(&self.issuer, "OIDC issuer", 2_048)?;
        require_bounded(&self.subject, "OIDC subject", 512)?;
        validate_optional(self.email.as_deref(), "OIDC email", 512)?;
        validate_optional(self.display_name.as_deref(), "OIDC display name", 512)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlUser {
    pub user_id: UserId,
    pub username: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipRecord {
    pub user_id: UserId,
    pub username: String,
    pub role: TenantRole,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantRole {
    Viewer,
    Member,
    Admin,
    Owner,
}

impl TenantRole {
    #[must_use]
    pub const fn allows(self, action: ControlAction) -> bool {
        match action {
            ControlAction::TenantRead | ControlAction::ExecutorRead => true,
            ControlAction::RunReserve | ControlAction::SecretUse => {
                matches!(self, Self::Member | Self::Admin | Self::Owner)
            }
            ControlAction::ProjectManage
            | ControlAction::ExecutorManage
            | ControlAction::PluginManage
            | ControlAction::SecretManage
            | ControlAction::MembershipManage
            | ControlAction::AuditRead
            | ControlAction::UsageRead => matches!(self, Self::Admin | Self::Owner),
            ControlAction::QuotaManage | ControlAction::TenantManage => {
                matches!(self, Self::Owner)
            }
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "viewer" => Ok(Self::Viewer),
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown tenant role {value:?}"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlAction {
    TenantRead,
    TenantManage,
    ProjectManage,
    ExecutorRead,
    ExecutorManage,
    PluginManage,
    RunReserve,
    QuotaManage,
    SecretUse,
    SecretManage,
    MembershipManage,
    AuditRead,
    UsageRead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Personal,
    Team,
}

impl SpaceKind {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "personal" => Ok(Self::Personal),
            "team" => Ok(Self::Team),
            _ => Err(HarnessError::execution("stored space kind is invalid")),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantSummary {
    pub tenant_id: TenantId,
    pub kind: SpaceKind,
    pub slug: String,
    pub display_name: String,
    pub role: TenantRole,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecord {
    pub tenant_id: TenantId,
    pub project_id: String,
    pub name: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePlacement {
    LocalNode,
    Cloud,
}

impl WorkspacePlacement {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "local_node" => Ok(Self::LocalNode),
            "cloud" => Ok(Self::Cloud),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown workspace placement {value:?}"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LocalNode => "local_node",
            Self::Cloud => "cloud",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStorage {
    LocalPath,
    CloudVolume,
    GitWorktree,
}

impl WorkspaceStorage {
    pub(crate) fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "local_path" => Ok(Self::LocalPath),
            "cloud_volume" => Ok(Self::CloudVolume),
            "git_worktree" => Ok(Self::GitWorktree),
            _ => Err(HarnessError::execution(format!(
                "database contains unknown workspace storage {value:?}"
            ))),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::CloudVolume => "cloud_volume",
            Self::GitWorktree => "git_worktree",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceRecord {
    pub tenant_id: TenantId,
    pub workspace_id: WorkspaceId,
    pub project_id: String,
    pub owner_user_id: UserId,
    pub name: String,
    pub placement: WorkspacePlacement,
    pub storage: WorkspaceStorage,
    pub executor_id: Option<ExecutorId>,
    /// Opaque Node-owned identifier. It is deliberately not a filesystem path.
    pub executor_workspace_id: Option<WorkspaceId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSessionRecord {
    pub tenant_id: TenantId,
    /// Stable browser-facing identifier. This is deliberately independent from
    /// the identifier allocated by the Node.
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub executor_id: ExecutorId,
    pub owner_user_id: UserId,
    pub node_session_id: SessionId,
    /// Sanitized presentation metadata. Host paths and Node-local identities
    /// are never part of this durable cache.
    pub metadata: EdgeSessionMetadata,
    pub last_event_seq: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSessionMetadata {
    pub parent_session_id: Option<SessionId>,
    pub subagent: Option<SubagentSessionMetadata>,
    pub title: String,
    pub archived_at_ms: Option<u64>,
    pub blank: bool,
    pub permissions: PermissionPreset,
    pub model: Value,
    #[serde(default)]
    pub server_model: Option<ternilo_protocol::RunModelSnapshot>,
    pub agent_preset: String,
    pub preset_plugins: Vec<PluginEntry>,
    pub profile_plugins: Vec<PluginEntry>,
    pub mode: SessionMode,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl EdgeSessionMetadata {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Some(parent_session_id) = &self.parent_session_id {
            parent_session_id.validate()?;
        }
        require_bounded(&self.title, "edge session title", 256)?;
        ternilo_protocol::validate_agent_preset_id(&self.agent_preset)?;
        if self.preset_plugins.len() > 128 || self.profile_plugins.len() > 128 {
            return Err(HarnessError::invalid(
                "edge session plugin metadata exceeds 128 entries",
            ));
        }
        if self.updated_at_ms < self.created_at_ms
            || self
                .archived_at_ms
                .is_some_and(|archived| archived < self.created_at_ms)
        {
            return Err(HarnessError::invalid(
                "edge session metadata timestamps are inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantQuota {
    pub max_nodes: u32,
    pub max_concurrent_runs: u32,
    pub monthly_model_tokens: u64,
    pub max_secrets: u32,
}

impl Default for TenantQuota {
    fn default() -> Self {
        Self {
            max_nodes: 10,
            max_concurrent_runs: 4,
            monthly_model_tokens: 10_000_000,
            max_secrets: 100,
        }
    }
}

impl TenantQuota {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.max_nodes == 0
            || self.max_concurrent_runs == 0
            || self.monthly_model_tokens == 0
            || self.max_secrets == 0
        {
            Err(HarnessError::invalid(
                "all tenant quota limits must be positive",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentGrant {
    pub enrollment_id: String,
    pub tenant_id: TenantId,
    pub executor_id: ExecutorId,
    pub expires_at_ms: u64,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCredentialGrant {
    pub credential_id: String,
    pub scope: ExecutorScope,
    pub executor_id: ExecutorId,
    pub project_id: Option<String>,
    pub token: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePrincipal {
    pub credential_id: String,
    pub scope: ExecutorScope,
    pub executor_id: ExecutorId,
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorRecord {
    pub executor_id: ExecutorId,
    pub project_id: Option<String>,
    pub state: String,
    pub enrolled_at_ms: u64,
    pub last_seen_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretMetadata {
    pub secret_id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub version: u64,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaReservation {
    pub reservation_id: String,
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub run_id: Option<String>,
    pub reserved_model_tokens: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageTotals {
    pub requests: u64,
    pub attempts: u64,
    pub unknown_attempts: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageQuotaSnapshot {
    pub monthly_limit_tokens: u64,
    pub settled_tokens: u64,
    /// Unallocated active Run capacity, excluding known usage and unknown attempt reservations.
    pub active_reserved_tokens: u64,
    pub unknown_reserved_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageGroup {
    pub provider: String,
    pub model: String,
    pub usage: ModelUsageTotals,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageLedgerEntry {
    pub run_id: String,
    pub actor_user_id: UserId,
    pub resource_owner_user_id: UserId,
    pub model_beneficiary_user_id: UserId,
    pub lease_token: u64,
    pub request_id: String,
    pub attempt: u32,
    pub provider: String,
    pub model: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub accounted_tokens: Option<u64>,
    pub provider_request_id: Option<String>,
    pub recorded_at_ms: u64,
    pub reservation_id: String,
    pub reservation_state: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelUsageAnomalyKind {
    ExpiredActive,
    TerminalRunActive,
    MissingCommittedTokens,
    CommittedUsageMismatch,
    UnknownModelUsage,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageReservation {
    pub reservation_id: String,
    pub user_id: UserId,
    pub run_id: Option<String>,
    pub reserved_tokens: u64,
    pub committed_tokens: Option<u64>,
    pub state: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub run_state: Option<String>,
    pub ledger_tokens: u64,
    pub unknown_tokens: u64,
    pub issues: Vec<ModelUsageAnomalyKind>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsageReport {
    pub tenant_id: TenantId,
    pub period: String,
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub limit: u32,
    pub quota: ModelUsageQuotaSnapshot,
    pub totals: ModelUsageTotals,
    pub groups: Vec<ModelUsageGroup>,
    pub ledger: Vec<ModelUsageLedgerEntry>,
    pub ledger_truncated: bool,
    pub reservations: Vec<ModelUsageReservation>,
    pub reservations_truncated: bool,
    pub anomalies: Vec<ModelUsageReservation>,
    pub anomalies_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub audit_id: String,
    pub tenant_id: TenantId,
    pub actor_user_id: Option<UserId>,
    pub actor_kind: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub outcome: String,
    pub metadata: Value,
    pub occurred_at_ms: u64,
    pub entry_hash_hex: String,
}

pub(crate) fn require_bounded(
    value: &str,
    label: &str,
    maximum: usize,
) -> Result<(), HarnessError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(HarnessError::invalid(format!(
            "{label} must contain 1 to {maximum} bytes without control characters"
        )))
    } else {
        Ok(())
    }
}

fn validate_optional(value: Option<&str>, label: &str, maximum: usize) -> Result<(), HarnessError> {
    if let Some(value) = value {
        require_bounded(value, label, maximum)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_permissions_are_monotonic() {
        let actions = [
            ControlAction::TenantRead,
            ControlAction::RunReserve,
            ControlAction::SecretManage,
            ControlAction::UsageRead,
            ControlAction::TenantManage,
        ];
        let roles = [
            TenantRole::Viewer,
            TenantRole::Member,
            TenantRole::Admin,
            TenantRole::Owner,
        ];
        for action in actions {
            let mut granted = false;
            for role in roles {
                if granted {
                    assert!(role.allows(action));
                }
                granted |= role.allows(action);
            }
        }
    }

    #[test]
    fn usage_reports_are_admin_only() {
        assert!(!TenantRole::Viewer.allows(ControlAction::UsageRead));
        assert!(!TenantRole::Member.allows(ControlAction::UsageRead));
        assert!(TenantRole::Admin.allows(ControlAction::UsageRead));
        assert!(TenantRole::Owner.allows(ControlAction::UsageRead));
    }
}
