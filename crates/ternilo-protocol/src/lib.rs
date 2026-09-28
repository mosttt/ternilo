#![forbid(unsafe_code)]

mod model_gateway;
pub use model_gateway::{ModelGatewayFrame, NodeModelRequest, ScheduleModelOrigin};

mod model_device;
pub use model_device::*;
mod model_binding;
pub use model_binding::{RunModelBinding, RunModelSnapshot};
mod model_settings;
pub use model_settings::{ProviderModelValues, ProviderReasoningSetting};
mod input_provenance;
pub use input_provenance::{AutomatedInputSource, InputAuthor, InputProvenance};
mod conversation;
pub use conversation::{conversation_events, validate_regeneration};
mod workspace_browser;
pub use workspace_browser::*;

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RUN_SPEC_VERSION: u32 = 5;

macro_rules! string_id {
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

string_id!(TenantId);
string_id!(UserId);
string_id!(AgentId);
string_id!(SessionId);
string_id!(RunId);
string_id!(WorkspaceId);
string_id!(JobId);
string_id!(SubagentId);
string_id!(WorkflowRunId);
string_id!(TerminalId);
string_id!(ScheduleId);
string_id!(SubmissionId);

mod agent_presets;
mod agent_team;
mod file_inventory;
mod live;

pub use agent_presets::*;
pub use agent_team::*;
pub use file_inventory::*;
pub use live::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HarnessError {
    pub code: ErrorCode,
    pub message: String,
}

impl HarnessError {
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    #[must_use]
    pub fn composition(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Composition, message)
    }

    #[must_use]
    pub fn policy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PolicyDenied, message)
    }

    #[must_use]
    pub fn execution(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Execution, message)
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unavailable, message)
    }

    #[must_use]
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Cancelled, message)
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        matches!(self.code, ErrorCode::Cancelled)
    }

    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for HarnessError {}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    Composition,
    PolicyDenied,
    Execution,
    Unavailable,
    Cancelled,
    Conflict,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidInput => "invalid_input",
            Self::Composition => "composition",
            Self::PolicyDenied => "policy_denied",
            Self::Execution => "execution",
            Self::Unavailable => "unavailable",
            Self::Cancelled => "cancelled",
            Self::Conflict => "conflict",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionIdentity {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub workspace_id: WorkspaceId,
    pub path: String,
}

impl WorkspaceBinding {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.workspace_id.validate()?;
        if self.path.trim().is_empty() {
            return Err(HarnessError::invalid("workspace path must not be empty"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPreset {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    FullAccess,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Execute,
    Plan,
}

impl PermissionPreset {
    #[must_use]
    pub const fn allows_workspace_write(self) -> bool {
        matches!(self, Self::WorkspaceWrite | Self::FullAccess)
    }

    #[must_use]
    pub const fn allows_full_access(self) -> bool {
        matches!(self, Self::FullAccess)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Environment,
    Managed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialReferenceInfo {
    pub reference: String,
    pub configured: bool,
    pub source: Option<CredentialSource>,
    pub writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRecordInfo {
    pub key: String,
    pub kind: String,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialInventory {
    pub references: Vec<CredentialReferenceInfo>,
    pub records: Vec<CredentialRecordInfo>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationCredentialSpace {
    Reference,
    Record,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationCredentialKey {
    pub space: AuthorizationCredentialSpace,
    pub key: String,
}

impl AuthorizationCredentialKey {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.key.trim().is_empty() || self.key.len() > 512 {
            return Err(HarnessError::invalid(
                "authorization credential key must contain 1 to 512 bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationMethod {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationEntry {
    pub key: AuthorizationCredentialKey,
    pub label: String,
    pub methods: Vec<AuthorizationMethod>,
    pub in_flight: bool,
    pub configured: bool,
    pub writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationNotice {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationPromptOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorizationPrompt {
    Text {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Secret {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
    Select {
        message: String,
        options: Vec<AuthorizationPromptOption>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationAttemptStatus {
    Running,
    Authorized,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationAttempt {
    pub attempt_id: String,
    pub key: AuthorizationCredentialKey,
    pub method: String,
    pub status: AuthorizationAttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAuthorizationNotice {
    pub id: String,
    pub attempt_id: String,
    pub key: AuthorizationCredentialKey,
    pub notice: AuthorizationNotice,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingAuthorizationPrompt {
    pub id: String,
    pub attempt_id: String,
    pub key: AuthorizationCredentialKey,
    pub prompt: AuthorizationPrompt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationSnapshot {
    pub entries: Vec<AuthorizationEntry>,
    pub attempts: Vec<AuthorizationAttempt>,
    pub notices: Vec<PendingAuthorizationNotice>,
    pub prompts: Vec<PendingAuthorizationPrompt>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationBeginRequest {
    pub key: AuthorizationCredentialKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub surface_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationPromptAnswer {
    pub prompt_id: String,
    pub surface_id: String,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelReasoning {
    pub default_effort: ReasoningEffort,
    pub efforts: BTreeMap<ReasoningEffort, Option<String>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProtocol {
    #[default]
    #[serde(rename = "openai-chat-completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "deepseek-responses")]
    DeepSeekResponses,
    #[serde(rename = "google-gemini")]
    GoogleGemini,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

impl ProviderProtocol {
    pub fn validate_reasoning_value(
        self,
        value: &str,
        max_output_tokens: u64,
    ) -> Result<(), HarnessError> {
        let valid = match self {
            Self::GoogleGemini => {
                matches!(value, "none" | "minimal" | "low" | "medium" | "high")
                    || value.parse::<i32>().is_ok_and(|budget| budget >= -1)
            }
            Self::AnthropicMessages => {
                matches!(
                    value,
                    "none" | "0" | "adaptive" | "low" | "medium" | "high" | "xhigh" | "max"
                ) || value
                    .parse::<u64>()
                    .is_ok_and(|budget| budget >= 1024 && budget < max_output_tokens)
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(HarnessError::invalid(format!(
                "invalid reasoning value {value:?} for {}; use a supported native level or thinking token budget",
                self.as_str()
            )))
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::DeepSeekResponses => "deepseek-responses",
            Self::GoogleGemini => "google-gemini",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    /// Providers can share an HTTP API shape while retaining distinct parsing and history rules.
    #[must_use]
    pub const fn api_protocol(self) -> Self {
        match self {
            Self::DeepSeekResponses => Self::OpenAiResponses,
            protocol => protocol,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelDiscoveryRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProviderProtocol>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl ProviderModelDiscoveryRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self
            .provider_id
            .as_deref()
            .is_some_and(|provider_id| provider_id.trim().is_empty())
        {
            return Err(HarnessError::invalid("provider_id must not be empty"));
        }
        if let Some(base_url) = self.base_url.as_deref() {
            let base_url = base_url.trim();
            if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
                return Err(HarnessError::invalid(
                    "provider discovery base_url must begin with http:// or https://",
                ));
            }
        } else if self.provider_id.is_none() {
            return Err(HarnessError::invalid(
                "provider discovery requires provider_id or base_url",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidebarOrdering {
    #[serde(default)]
    pub workspace_order: Vec<String>,
    #[serde(default)]
    pub session_order_by_account: BTreeMap<String, Vec<String>>,
}

impl SidebarOrdering {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_sidebar_order(&self.workspace_order, "workspace_order")?;
        for (account, order) in &self.session_order_by_account {
            if account.trim().is_empty() {
                return Err(HarnessError::invalid(
                    "sidebar session ordering account must not be empty",
                ));
            }
            validate_sidebar_order(order, "session_order_by_account")?;
        }
        Ok(())
    }
}

fn validate_sidebar_order(order: &[String], label: &str) -> Result<(), HarnessError> {
    let mut ids = BTreeSet::new();
    for id in order {
        if id.trim().is_empty() || !ids.insert(id) {
            return Err(HarnessError::invalid(format!(
                "{label} must contain unique non-empty ids"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Model choice shared by a host (Local/Node) or a tenant user (Cloud).
///
/// Sessions copy this value when they are created. Changing the shared default
/// never rewrites an existing Session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum DefaultModelSelection {
    #[default]
    ProfileDefault,
    NamedProvider {
        provider_id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    AccountProvider {
        owner_user_id: UserId,
        provider_id: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    PlatformModel {
        grant_id: String,
        model_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<ReasoningEffort>,
    },
    OpenAiCompatible {
        base_url: String,
        model: String,
        api_key_env: Option<String>,
        #[serde(default = "default_model_timeout_ms")]
        timeout_ms: u64,
        #[serde(default = "default_model_max_attempts")]
        max_attempts: u32,
        #[serde(default = "default_model_retry_base_delay_ms")]
        retry_base_delay_ms: u64,
    },
}

impl DefaultModelSelection {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Self::AccountProvider { owner_user_id, .. } = self {
            owner_user_id.validate()?;
        }
        match self {
            Self::ProfileDefault => Ok(()),
            Self::PlatformModel {
                grant_id, model_id, ..
            } => {
                model_binding::validate_reference(grant_id, "model grant", 128)?;
                model_binding::validate_reference(model_id, "public model", 128)
            }
            Self::NamedProvider {
                provider_id, model, ..
            }
            | Self::AccountProvider {
                provider_id, model, ..
            } => {
                if provider_id.trim().is_empty() || model.trim().is_empty() {
                    Err(HarnessError::invalid(
                        "named provider selection requires non-empty provider_id and model",
                    ))
                } else {
                    Ok(())
                }
            }
            Self::OpenAiCompatible {
                base_url,
                model,
                api_key_env,
                timeout_ms: _,
                max_attempts,
                retry_base_delay_ms,
            } => {
                if !(base_url.starts_with("https://") || base_url.starts_with("http://"))
                    || model.trim().is_empty()
                    || api_key_env
                        .as_ref()
                        .is_some_and(|value| value.trim().is_empty())
                    || !(1..=8).contains(max_attempts)
                    || *retry_base_delay_ms == 0
                {
                    Err(HarnessError::invalid(
                        "OpenAI-compatible model requires an HTTP(S) base_url, model, optional non-empty api_key_env, non-negative timeout, positive retry delay, and 1 to 8 attempts",
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }
}

const fn default_model_timeout_ms() -> u64 {
    600_000
}

const fn default_model_max_attempts() -> u32 {
    3
}

const fn default_model_retry_base_delay_ms() -> u64 {
    250
}

impl ReasoningEffort {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub settings: ProviderModelSettings,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelDefaults {
    pub context_window: u64,
    pub max_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ProviderModelReasoning>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderModelSettings {
    Inherit,
    Automatic {
        #[serde(default)]
        upstream: ProviderModelValues,
        #[serde(default)]
        overrides: ProviderModelValues,
    },
    Override {
        context_window: u64,
        max_output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<ProviderModelReasoning>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProviderModel {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub reasoning: Option<ProviderModelReasoning>,
}

impl ResolvedProviderModel {
    pub fn reasoning_value(
        &self,
        effort: Option<ReasoningEffort>,
    ) -> Result<Option<&str>, HarnessError> {
        let Some(reasoning) = self.reasoning.as_ref() else {
            if let Some(effort) = effort {
                return Err(HarnessError::invalid(format!(
                    "provider model {:?} does not support reasoning effort {:?}",
                    self.id,
                    effort.as_str()
                )));
            }
            return Ok(None);
        };
        let selected = effort.unwrap_or(reasoning.default_effort);
        reasoning
            .efforts
            .get(&selected)
            .map(|value| value.as_deref())
            .ok_or_else(|| {
                HarnessError::invalid(format!(
                    "provider model {:?} does not support reasoning effort {:?}",
                    self.id,
                    selected.as_str()
                ))
            })
    }
}

pub trait ProviderModelCatalog {
    fn model_defaults(&self) -> &ProviderModelDefaults;
    fn provider_models(&self) -> &[ProviderModel];

    fn resolved_model(&self, id: &str) -> Result<ResolvedProviderModel, HarnessError> {
        let model = self
            .provider_models()
            .iter()
            .find(|model| model.id == id)
            .ok_or_else(|| HarnessError::invalid(format!("provider has no model {id:?}")))?;
        let defaults = self.model_defaults();
        let (context_window, max_output_tokens, reasoning) = match &model.settings {
            ProviderModelSettings::Inherit => (
                defaults.context_window,
                defaults.max_output_tokens,
                defaults.reasoning.clone(),
            ),
            ProviderModelSettings::Override {
                context_window,
                max_output_tokens,
                reasoning,
            } => (*context_window, *max_output_tokens, reasoning.clone()),
            ProviderModelSettings::Automatic {
                upstream,
                overrides,
            } => {
                let resolved = overrides.resolve(&upstream.resolve(defaults));
                (
                    resolved.context_window,
                    resolved.max_output_tokens,
                    resolved.reasoning,
                )
            }
        };
        Ok(ResolvedProviderModel {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            context_window,
            max_output_tokens,
            reasoning,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    #[serde(default)]
    pub protocol: ProviderProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
    pub defaults: ProviderModelDefaults,
    pub models: Vec<ProviderModel>,
    pub timeout_ms: u64,
    pub max_attempts: u32,
    pub retry_base_delay_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProviderMaterializeRequest {
    pub package_id: String,
    pub version: String,
    pub template: String,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
}

impl ExtensionProviderMaterializeRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        for (value, label) in [
            (&self.package_id, "extension package id"),
            (&self.version, "extension package version"),
            (&self.template, "extension Provider template id"),
        ] {
            if value.trim().is_empty() || value.len() > 200 {
                return Err(HarnessError::invalid(format!(
                    "{label} must contain 1 to 200 bytes"
                )));
            }
        }
        if !valid_provider_id(&self.provider_id) {
            return Err(HarnessError::invalid(
                "provider id must start with a lowercase letter and use lowercase letters, digits, dash, or underscore",
            ));
        }
        if self
            .api_key_ref
            .as_ref()
            .is_some_and(|reference| !valid_credential_reference(reference))
        {
            return Err(HarnessError::invalid(
                "provider api_key_ref must match [A-Za-z_][A-Za-z0-9_]*",
            ));
        }
        Ok(())
    }
}

impl ProviderProfile {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if !valid_provider_id(&self.id) {
            return Err(HarnessError::invalid(
                "provider id must start with a lowercase letter and use lowercase letters, digits, dash, or underscore",
            ));
        }
        if self.display_name.trim().is_empty() || self.display_name.chars().count() > 120 {
            return Err(HarnessError::invalid(
                "provider display_name must contain 1 to 120 characters",
            ));
        }
        if !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://")) {
            return Err(HarnessError::invalid(
                "provider base_url must use http:// or https://",
            ));
        }
        if self
            .api_key_ref
            .as_ref()
            .is_some_and(|reference| !valid_credential_reference(reference))
        {
            return Err(HarnessError::invalid(
                "provider api_key_ref must match [A-Za-z_][A-Za-z0-9_]*",
            ));
        }
        validate_model_defaults(&self.defaults, "provider defaults")?;
        if let Some(reasoning) = &self.defaults.reasoning {
            for value in reasoning.efforts.values().flatten() {
                self.protocol
                    .validate_reasoning_value(value, self.defaults.max_output_tokens)?;
            }
        }
        if self.models.is_empty() || self.models.len() > 512 {
            return Err(HarnessError::invalid(
                "provider requires 1 to 512 model entries",
            ));
        }
        let mut model_ids = std::collections::BTreeSet::new();
        for model in &self.models {
            let resolved = self.resolved_model(&model.id)?;
            if let Some(reasoning) = &resolved.reasoning {
                for value in reasoning.efforts.values().flatten() {
                    self.protocol
                        .validate_reasoning_value(value, resolved.max_output_tokens)?;
                }
            }
            if model.id.trim().is_empty()
                || model.id.chars().count() > 200
                || model
                    .display_name
                    .as_ref()
                    .is_some_and(|name| name.trim().is_empty() || name.chars().count() > 200)
            {
                return Err(HarnessError::invalid(
                    "model id/display name must be non-empty",
                ));
            }
            if !model_ids.insert(model.id.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "provider contains duplicate model id {:?}",
                    model.id
                )));
            }
            if let ProviderModelSettings::Override {
                context_window,
                max_output_tokens,
                reasoning,
            } = &model.settings
            {
                validate_model_defaults(
                    &ProviderModelDefaults {
                        context_window: *context_window,
                        max_output_tokens: *max_output_tokens,
                        reasoning: reasoning.clone(),
                    },
                    &format!("provider model {:?} override", model.id),
                )?;
            }
            if let ProviderModelSettings::Automatic {
                upstream,
                overrides,
            } = &model.settings
            {
                upstream.validate(&format!("provider model {:?} upstream", model.id))?;
                overrides.validate(&format!("provider model {:?} overrides", model.id))?;
            }
        }
        if !(1..=8).contains(&self.max_attempts) || self.retry_base_delay_ms == 0 {
            return Err(HarnessError::invalid(
                "provider retry delay must be positive and max_attempts must be 1 to 8",
            ));
        }
        Ok(())
    }
}

fn valid_provider_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    id.len() <= 64
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

impl ProviderModelCatalog for ProviderProfile {
    fn model_defaults(&self) -> &ProviderModelDefaults {
        &self.defaults
    }

    fn provider_models(&self) -> &[ProviderModel] {
        &self.models
    }
}

fn validate_model_defaults(
    defaults: &ProviderModelDefaults,
    label: &str,
) -> Result<(), HarnessError> {
    if defaults.context_window == 0
        || defaults.max_output_tokens == 0
        || defaults.max_output_tokens > u64::from(u32::MAX)
    {
        return Err(HarnessError::invalid(format!(
            "{label} context_window must be positive and max_output_tokens must fit u32"
        )));
    }
    if let Some(reasoning) = &defaults.reasoning
        && (reasoning.efforts.is_empty()
            || reasoning.efforts.len() > ReasoningEffort::ALL.len()
            || !reasoning.efforts.contains_key(&reasoning.default_effort)
            || reasoning.efforts.values().any(|value| {
                value.as_ref().is_some_and(|value| {
                    value.trim().is_empty()
                        || value.chars().count() > 64
                        || value.chars().any(char::is_control)
                })
            }))
    {
        return Err(HarnessError::invalid(format!(
            "{label} has invalid reasoning effort metadata"
        )));
    }
    Ok(())
}

fn valid_credential_reference(reference: &str) -> bool {
    let mut bytes = reference.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReadRequest {
    pub path: String,
    pub start_line: Option<u64>,
    pub line_count: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub start_line: u64,
    pub end_line: u64,
    pub total_lines: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteRequest {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteResult {
    pub path: String,
    pub bytes_written: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReplaceRequest {
    pub path: String,
    pub old: String,
    pub new: String,
    pub replace_all: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReplaceResult {
    pub path: String,
    pub replacements: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileListRequest {
    pub pattern: String,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileListResult {
    pub files: Vec<String>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSearchRequest {
    pub pattern: String,
    pub file_glob: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSearchMatch {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub preview: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileSearchResult {
    pub matches: Vec<FileSearchMatch>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellRequest {
    pub command: String,
    pub timeout_ms: u64,
    pub full_access: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
    Idle,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentTranscriptKind {
    #[default]
    ProcessLifecycle,
    Conversation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentSessionMetadata {
    pub subagent_id: SubagentId,
    pub provider: String,
    pub transcript_kind: SubagentTranscriptKind,
}

/// Identifies an accepted scheduled run; it does not grant access to that run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedSubagentRun {
    pub session_id: SessionId,
    pub run_id: RunId,
}

impl AcceptedSubagentRun {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.session_id.validate()?;
        self.run_id.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentSnapshot {
    pub subagent_id: SubagentId,
    pub provider: String,
    pub label: String,
    pub task: String,
    pub supports_followup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub transcript_kind: SubagentTranscriptKind,
    pub status: SubagentStatus,
    pub output: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPhaseDefinition {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowMeta {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub phases: Vec<WorkflowPhaseDefinition>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAgentOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStopReason {
    Completed,
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Running,
    Exited,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalSnapshot {
    pub terminal_id: TerminalId,
    pub name: Option<String>,
    pub status: TerminalStatus,
    pub exit_code: Option<i32>,
    pub output_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalRead {
    pub terminal: TerminalSnapshot,
    pub offset: u64,
    pub next_offset: u64,
    pub output: String,
    pub truncated: bool,
    pub timed_out: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSnapshot {
    pub job_id: JobId,
    pub command: String,
    pub status: JobStatus,
    pub result: Option<ShellResult>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserQuestion {
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<UserQuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<UserQuestionPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_approval: Option<ToolApprovalContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserQuestionPresentation {
    PlanReview {
        title: String,
        plan: String,
        approve_label: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolApprovalContext {
    pub tool_name: String,
    pub call_id: String,
    pub reason: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ToolPresentationDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserAnswer {
    pub question_id: String,
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<String>,
}

impl UserAnswer {
    #[must_use]
    pub fn chose(&self, label: &str) -> bool {
        self.selected.iter().any(|selected| selected == label)
    }

    #[must_use]
    pub fn display_text(&self) -> String {
        let mut values = self.selected.clone();
        if let Some(custom) = self.custom.as_deref().filter(|value| !value.is_empty()) {
            values.push(custom.to_owned());
        }
        values.join(", ")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub step: String,
    pub status: PlanItemStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Complete,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillInvocationPolicy {
    pub model_invocable: bool,
    pub user_invocable: bool,
}

impl Default for SkillInvocationPolicy {
    fn default() -> Self {
        Self {
            model_invocable: true,
            user_invocable: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDefinition {
    #[serde(flatten)]
    pub summary: SkillSummary,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_base: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogSnapshot {
    pub revision: u64,
    pub complete: bool,
    pub skills: Vec<SkillSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSkillInvocation {
    pub name: String,
    pub model_input: String,
    pub display_input: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCommandOutcomeKind {
    Success,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandOutcome {
    pub kind: SessionCommandOutcomeKind,
    /// Stable producer code translated by each client surface.
    pub code: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompaction {
    pub through_seq: u64,
    pub summary: String,
    pub estimated_tokens_before: u64,
    pub automatic: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRule {
    After { after_seconds: u64 },
    At,
    Every { every_seconds: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRecord {
    pub id: ScheduleId,
    pub prompt: String,
    pub rule: ScheduleRule,
    pub scheduled_at_ms: u64,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleChange {
    Create {
        schedule: ScheduleRecord,
    },
    Delete {
        id: ScheduleId,
    },
    Dispatch {
        id: ScheduleId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<RunId>,
        accepted_at_ms: u64,
        next_scheduled_at_ms: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStats {
    pub events: u64,
    pub turns: u64,
    pub completed_turns: u64,
    pub failed_turns: u64,
    pub cancelled_turns: u64,
    pub steps: u64,
    pub tool_calls: u64,
    pub user_messages: u64,
    pub assistant_messages: u64,
    pub estimated_logged_tokens: u64,
    pub exact_input_tokens: u64,
    pub exact_output_tokens: u64,
    pub cached_input_tokens: u64,
    pub exact_reasoning_tokens: u64,
    pub model_attempts: u64,
    pub measured_model_responses: u64,
    pub model_duration_ms: u64,
    pub tool_duration_ms: u64,
    pub first_token_duration_ms: u64,
    pub measured_first_tokens: u64,
    pub generation_duration_ms: u64,
    pub generation_output_tokens: u64,
}

/// A consistent, log-derived read-model cut for one session.
///
/// Projection values are conveniences for clients and indexes. The append-only
/// session event log remains authoritative and every value can be rebuilt from
/// it when a checkpoint is absent, stale, or invalid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionProjectionSnapshot {
    pub session_id: SessionId,
    /// Sequence reflected by every value, or `None` for an empty log.
    pub as_of_seq: Option<u64>,
    pub values: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTelemetrySharingStatus {
    #[default]
    Disabled,
    FeedbackOnly,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTelemetryChannel {
    Ledger,
    Operations,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTelemetrySeverity {
    Info,
    Warn,
    Error,
}

/// One logical record leaving the harness through an explicitly configured
/// telemetry backend. The body is always a detached JSON copy; redactors never
/// mutate the canonical session log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTelemetryRecord {
    pub channel: SessionTelemetryChannel,
    pub time_ms: u64,
    pub severity: SessionTelemetrySeverity,
    pub attributes: BTreeMap<String, Value>,
    pub body: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSearchRequest {
    pub query: String,
    pub session_id: Option<SessionId>,
    pub workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub filters: SessionSearchFilters,
    pub limit: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSearchFilters {
    pub run_id: Option<RunId>,
    pub category: Option<SessionEventCategory>,
    pub occurred_after_ms: Option<u64>,
    pub occurred_before_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventCategory {
    User,
    Assistant,
    Tool,
    Planning,
    Compaction,
    Deliverable,
    Interaction,
    Error,
    Lifecycle,
    Other,
}

impl SessionSearchRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.query.trim().is_empty() {
            return Err(HarnessError::invalid(
                "session search query must not be empty",
            ));
        }
        if !(1..=100).contains(&self.limit) {
            return Err(HarnessError::invalid(
                "session search limit must be between 1 and 100",
            ));
        }
        if let Some(session_id) = &self.session_id {
            session_id.validate()?;
        }
        if let Some(workspace_id) = &self.workspace_id {
            workspace_id.validate()?;
        }
        if let Some(run_id) = &self.filters.run_id {
            run_id.validate()?;
        }
        if self
            .filters
            .occurred_after_ms
            .zip(self.filters.occurred_before_ms)
            .is_some_and(|(after, before)| after > before)
        {
            return Err(HarnessError::invalid(
                "session search occurred_after_ms cannot exceed occurred_before_ms",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSearchHit {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub updated_at_ms: u64,
    pub event_seq: Option<u64>,
    pub occurred_at_ms: Option<u64>,
    pub run_id: Option<RunId>,
    pub category: Option<SessionEventCategory>,
    pub excerpt: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventReadRequest {
    pub session_id: SessionId,
    pub start_seq: u64,
    pub limit: u32,
}

impl SessionEventReadRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.session_id.validate()?;
        if !(1..=200).contains(&self.limit) {
            return Err(HarnessError::invalid(
                "session event read limit must be between 1 and 200",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTrace {
    pub identity: SessionIdentity,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub event_count: u64,
    pub run_count: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub parent_session_id: Option<SessionId>,
    pub descendant_session_ids: Vec<SessionId>,
}

impl SessionIdentity {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.tenant_id.validate()?;
        self.user_id.validate()?;
        self.agent_id.validate()?;
        self.session_id.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunMetadata {
    pub tenant_id: TenantId,
    pub user_id: UserId,
    pub project_id: Option<String>,
    pub workspace_id: WorkspaceId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub run_id: RunId,
}

impl RunMetadata {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.tenant_id.validate()?;
        self.user_id.validate()?;
        if let Some(project_id) = &self.project_id
            && (project_id.is_empty()
                || project_id.len() > 128
                || project_id
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()))
        {
            return Err(HarnessError::invalid(
                "project_id must contain 1 to 128 bytes without whitespace or control characters",
            ));
        }
        self.workspace_id.validate()?;
        self.agent_id.validate()?;
        self.session_id.validate()?;
        self.run_id.validate()
    }

    #[must_use]
    pub fn identity(&self) -> SessionIdentity {
        SessionIdentity {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            agent_id: self.agent_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    /// Maximum steps per turn; 0 leaves the count unrestricted at this layer.
    pub max_steps: u32,
    /// Maximum tool calls per turn; 0 leaves the count unrestricted at this layer.
    pub max_tool_calls: u32,
}

impl Default for RunLimits {
    fn default() -> Self {
        Self {
            max_steps: 0,
            max_tool_calls: 512,
        }
    }
}

#[cfg(test)]
mod run_limits_tests {
    use super::RunLimits;

    #[test]
    fn zero_steps_is_the_valid_unlimited_default() {
        let limits = RunLimits::default();
        assert_eq!(limits.max_steps, 0);
        assert_eq!(limits.max_tool_calls, 512);
    }

    #[test]
    fn explicit_tool_limits_and_unrestricted_zero_round_trip() {
        let configured: RunLimits = serde_json::from_value(serde_json::json!({
            "max_steps": 0, "max_tool_calls": 32,
        }))
        .unwrap();
        assert_eq!(configured.max_tool_calls, 32);
        let unlimited = RunLimits {
            max_steps: 0,
            max_tool_calls: 0,
        };
        assert_eq!(
            serde_json::from_value::<RunLimits>(serde_json::to_value(unlimited).unwrap()).unwrap(),
            unlimited
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntry {
    pub id: String,
    pub kind: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub config: Value,
}

const fn enabled_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCatalogEntry {
    pub kind: String,
    pub description: String,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
    pub config_schema: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_limits: Option<RunLimits>,
    pub revision: String,
    pub plugin_kinds: Vec<String>,
    pub plugins: Vec<PluginCatalogEntry>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPresetTrust {
    System,
    User,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetSummary {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub trust: AgentPresetTrust,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetDocument {
    #[serde(flatten)]
    pub summary: AgentPresetSummary,
    pub profile: Profile,
    /// Read-only composition context supplied only by an explicit editor GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_profile: Option<Profile>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetRoster {
    pub presets: Vec<AgentPresetSummary>,
    pub default_id: String,
    pub authorable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetCopyRequest {
    pub from: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPresetUpdateRequest {
    pub display_name: String,
    pub description: String,
    pub profile: Profile,
}

pub fn validate_agent_preset_id(id: &str) -> Result<(), HarnessError> {
    let mut bytes = id.bytes();
    if id.len() > 64
        || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(HarnessError::invalid(
            "agent preset id must start with a lowercase letter and use lowercase letters, digits, or dash",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    pub schema_version: u32,
    pub catalog_revision: String,
    pub policy_revision: String,
    pub metadata: RunMetadata,
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
}

impl RunSpec {
    pub fn validate_shape(&self) -> Result<(), HarnessError> {
        if self.schema_version != RUN_SPEC_VERSION {
            return Err(HarnessError::invalid(format!(
                "unsupported RunSpec version {}; expected {RUN_SPEC_VERSION}",
                self.schema_version
            )));
        }
        if self.catalog_revision.trim().is_empty() || self.policy_revision.trim().is_empty() {
            return Err(HarnessError::invalid(
                "catalog_revision and policy_revision must not be empty",
            ));
        }
        self.metadata.validate()?;
        if self.permissions == PermissionPreset::FullAccess {
            return Err(HarnessError::policy(
                "portable RunSpec must not grant full host access",
            ));
        }
        if self.input.trim().is_empty() && self.attachments.is_empty() && self.references.is_empty()
        {
            return Err(HarnessError::invalid(
                "run input must contain text, an attachment, or a reference",
            ));
        }
        validate_references(&self.references)?;
        validate_reference_contexts(&self.reference_contexts)?;
        if self.attachments.len() > 16 {
            return Err(HarnessError::invalid(
                "one RunSpec may contain at most 16 attachments",
            ));
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMessage {
    pub role: MessageRole,
    pub content: String,
    /// Provider-supplied reasoning or thinking text kept separate from the
    /// user-visible answer. Adapters replay this on assistant history entries
    /// when their wire protocol supports reasoning passback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state: Option<Box<ModelProviderState>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Signed, transport-safe UI metadata for one tool. Clients render this
/// declaratively; it never names or carries executable client code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPresentationIconKind {
    Wrench,
    Puzzle,
    Sparkles,
    File,
    Terminal,
    Search,
    Globe,
    Database,
    Code,
    #[serde(other)]
    Unknown,
}

/// A JSON value selected by an exact sequence of object keys.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPresentationField {
    pub label: String,
    pub path: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPresentationResultKind {
    Text,
    Markdown,
    Json,
    Table,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPresentationResultDescriptor {
    pub kind: ToolPresentationResultKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ToolPresentationField>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPresentationDescriptor {
    pub title: String,
    pub icon_kind: ToolPresentationIconKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_summary: Vec<ToolPresentationField>,
    pub result: ToolPresentationResultDescriptor,
}

/// Optional free-form input accepted by a human-facing slash command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInputDescriptor {
    pub hint: String,
    #[serde(default)]
    pub images: bool,
}

/// Handler-free command metadata exposed to interactive clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDescriptor {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<CommandInputDescriptor>,
}

/// The effective direct-command catalog for one composed Session runtime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandCatalog {
    pub session_id: SessionId,
    pub commands: Vec<CommandDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ToolPresentationDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl HookPoint {
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    #[default]
    None,
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRequest {
    pub point: HookPoint,
    pub run_id: RunId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<ToolOutput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookResult {
    pub handler_id: String,
    pub dialect: String,
    pub point: HookPoint,
    pub decision: HookDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub stop: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_summary: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    pub run_id: RunId,
    pub system_prompt: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSpec>,
    pub step: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponse {
    /// Provider route that actually served this request.
    pub provider: String,
    /// Exact provider model that actually served this request.
    pub model: String,
    pub content: String,
    /// Complete reasoning text (or provider-visible reasoning summary) for
    /// this response. It is durable session data, not part of `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_state: Option<Box<ModelProviderState>>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ModelUsage>,
    pub finish_reason: ModelFinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    #[serde(default = "default_model_attempts")]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderState {
    pub protocol: ProviderProtocol,
    pub model: String,
    pub blocks: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRetryFailure {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

const fn default_model_attempts() -> u32 {
    1
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Provider-reported cache creation/write tokens when the route exposes
    /// that bucket separately from cache reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Provider-reported reasoning tokens. This is an output-token detail and
    /// therefore is not subtracted from `output_tokens`.
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub name: String,
    pub media_type: String,
    pub content: String,
}

pub const ATTACHMENT_REFERENCE_PREFIX: &str = "ternilo-attachment://sha256/";

impl Attachment {
    #[must_use]
    pub fn is_text(&self) -> bool {
        let mime = self
            .media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        !mime.starts_with("image/")
            && (mime.starts_with("text/")
                || mime.ends_with("+json")
                || mime.ends_with("+xml")
                || matches!(
                    mime.as_str(),
                    "application/json"
                        | "application/xml"
                        | "application/javascript"
                        | "application/yaml"
                        | "application/x-yaml"
                        | "application/toml"
                ))
    }

    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.content.starts_with(ATTACHMENT_REFERENCE_PREFIX)
    }

    #[must_use]
    pub fn reference_digest(&self) -> Option<&str> {
        self.content.strip_prefix(ATTACHMENT_REFERENCE_PREFIX)
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.name.trim().is_empty() || self.media_type.trim().is_empty() {
            return Err(HarnessError::invalid(
                "attachment name and media_type must not be empty",
            ));
        }
        if let Some(digest) = self.reference_digest() {
            if digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(HarnessError::invalid(
                    "attachment references must contain one lowercase SHA-256 digest",
                ));
            }
            return Ok(());
        }
        if self.content.is_empty() || self.content.len() > 8 * 1024 * 1024 {
            return Err(HarnessError::invalid(
                "one attachment must contain 1 byte to 8 MiB of encoded content",
            ));
        }
        if self.media_type.starts_with("image/") && !self.content.starts_with("data:image/") {
            return Err(HarnessError::invalid(
                "image attachments must contain a data:image URL",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    pub run_id: RunId,
    pub input: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_inputs: Vec<SteeringInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<InputProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<UserMessageSource>,
    #[serde(default)]
    pub references: Vec<SubmissionReference>,
    #[serde(default)]
    pub reference_contexts: Vec<ReferenceContext>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFileKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionReference {
    File {
        path: String,
        file_kind: ReferenceFileKind,
    },
    Session {
        session_id: SessionId,
        label: String,
    },
}

impl SubmissionReference {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::File { path, .. } => validate_reference_path(path),
            Self::Session { session_id, label } => {
                session_id.validate()?;
                if label.trim().is_empty() || label.chars().count() > 256 {
                    return Err(HarnessError::invalid(
                        "session reference label must contain 1 to 256 characters",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceContextCompleteness {
    pub retained_items: u32,
    pub omitted_items: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceContext {
    pub reference: SubmissionReference,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness: Option<ReferenceContextCompleteness>,
}

impl ReferenceContext {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.reference.validate()?;
        if self.content.trim().is_empty() || self.content.len() > 128 * 1024 {
            return Err(HarnessError::invalid(
                "one reference context must contain 1 byte to 128 KiB",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub const fn handler_id(&self) -> &'static str {
        match self.reference {
            SubmissionReference::File { .. } => "reference:file",
            SubmissionReference::Session { .. } => "reference:session",
        }
    }

    #[must_use]
    pub const fn dialect(&self) -> &'static str {
        match self.reference {
            SubmissionReference::File { .. } => "ternilo.reference.file.v1",
            SubmissionReference::Session { .. } => "ternilo.reference.session.v1",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferenceCandidate {
    File {
        path: String,
        file_kind: ReferenceFileKind,
        label: String,
    },
    Session {
        session_id: SessionId,
        label: String,
        workspace: String,
        same_workspace: bool,
        updated_at_ms: u64,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCandidateRequest {
    #[serde(default)]
    pub directory: String,
    #[serde(default)]
    pub query: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCandidateSnapshot {
    pub directory: String,
    pub candidates: Vec<ReferenceCandidate>,
}

const MAX_SUBMISSION_REFERENCES: usize = 8;

fn validate_reference_path(path: &str) -> Result<(), HarnessError> {
    if path.is_empty()
        || path.len() > 4_096
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
        || path.chars().any(char::is_control)
    {
        return Err(HarnessError::invalid(
            "file reference path must be a normalized relative workspace path",
        ));
    }
    Ok(())
}

fn validate_references(references: &[SubmissionReference]) -> Result<(), HarnessError> {
    if references.len() > MAX_SUBMISSION_REFERENCES {
        return Err(HarnessError::invalid(
            "one submission may contain at most 8 references",
        ));
    }
    for reference in references {
        reference.validate()?;
    }
    Ok(())
}

fn validate_reference_contexts(contexts: &[ReferenceContext]) -> Result<(), HarnessError> {
    if contexts.len() > MAX_SUBMISSION_REFERENCES
        || contexts
            .iter()
            .map(|context| context.content.len())
            .sum::<usize>()
            > 256 * 1024
    {
        return Err(HarnessError::invalid(
            "reference contexts exceed the per-run count or 256 KiB budget",
        ));
    }
    for context in contexts {
        context.validate()?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionDelivery {
    #[default]
    Queue,
    Steer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionPlacement {
    Queued,
    Steering,
    Running,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubmissionContent {
    Prompt {
        input: String,
    },
    Skill {
        name: String,
        input: String,
    },
    Regenerate {
        target_seq: u64,
        input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill_name: Option<String>,
    },
}

impl SubmissionContent {
    #[must_use]
    pub fn input(&self) -> &str {
        match self {
            Self::Prompt { input } | Self::Skill { input, .. } | Self::Regenerate { input, .. } => {
                input
            }
        }
    }

    #[must_use]
    pub fn skill_name(&self) -> Option<&str> {
        match self {
            Self::Prompt { .. } => None,
            Self::Skill { name, .. } => Some(name),
            Self::Regenerate { skill_name, .. } => skill_name.as_deref(),
        }
    }

    #[must_use]
    pub const fn regeneration_target(&self) -> Option<u64> {
        match self {
            Self::Regenerate { target_seq, .. } => Some(*target_seq),
            _ => None,
        }
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::Regenerate {
                input, skill_name, ..
            } => {
                if let Some(name) = skill_name {
                    validate_skill_name(name)?;
                }
                if input.chars().count() > 100_000 {
                    return Err(HarnessError::invalid(
                        "input must not exceed 100000 characters",
                    ));
                }
                Ok(())
            }
            Self::Prompt { input } => {
                if input.chars().count() > 100_000 {
                    Err(HarnessError::invalid(
                        "prompt must not exceed 100000 characters",
                    ))
                } else {
                    Ok(())
                }
            }
            Self::Skill { name, input } => {
                validate_skill_name(name)?;
                if input.chars().count() > 100_000 {
                    Err(HarnessError::invalid(
                        "skill request must not exceed 100000 characters",
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }
}

fn validate_skill_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 128
        || name.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        Err(HarnessError::invalid(
            "skill name must be lowercase kebab-case",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSubmissionRequest {
    #[serde(default)]
    pub delivery: SubmissionDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub content: SubmissionContent,
    #[serde(default)]
    pub references: Vec<SubmissionReference>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl SessionSubmissionRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Some(run_id) = &self.run_id {
            run_id.validate()?;
        }
        self.content.validate()?;
        if self.content.regeneration_target().is_some()
            && self.delivery != SubmissionDelivery::Queue
        {
            return Err(HarnessError::invalid(
                "regeneration cannot be injected into an active turn",
            ));
        }
        if matches!(
            &self.content,
            SubmissionContent::Prompt { input } if input.trim().is_empty()
        ) && self.attachments.is_empty()
            && self.references.is_empty()
        {
            return Err(HarnessError::invalid(
                "submission must contain text, an attachment, or a reference",
            ));
        }
        validate_references(&self.references)?;
        AgentInput {
            additional_inputs: Vec::new(),
            run_id: self
                .run_id
                .clone()
                .unwrap_or_else(|| RunId::new("submission-run")),
            input: if self.content.skill_name().is_some() && self.content.input().trim().is_empty()
            {
                "skill invocation".to_owned()
            } else {
                self.content.input().to_owned()
            },
            display_input: None,
            source: None,
            provenance: None,
            references: self.references.clone(),
            reference_contexts: Vec::new(),
            attachments: self.attachments.clone(),
        }
        .validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSubmission {
    pub id: SubmissionId,
    pub run_id: RunId,
    pub content: SubmissionContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<InputProvenance>,
    #[serde(default)]
    pub references: Vec<SubmissionReference>,
    pub attachments: Vec<Attachment>,
    pub placement: SubmissionPlacement,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl SessionSubmission {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.id.validate()?;
        self.run_id.validate()?;
        self.content.validate()?;
        if let Some(provenance) = &self.provenance {
            provenance.validate()?;
            if provenance.input_id != self.id {
                return Err(HarnessError::invalid(
                    "submission provenance must identify the accepted submission",
                ));
            }
        }
        if matches!(
            &self.content,
            SubmissionContent::Prompt { input } if input.trim().is_empty()
        ) && self.attachments.is_empty()
            && self.references.is_empty()
        {
            return Err(HarnessError::invalid(
                "submission must contain text, an attachment, or a reference",
            ));
        }
        validate_references(&self.references)?;
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        if self.attachments.len() > 10
            || self
                .attachments
                .iter()
                .map(|attachment| attachment.content.len())
                .sum::<usize>()
                > 12 * 1024 * 1024
            || self.updated_at_ms < self.created_at_ms
        {
            return Err(HarnessError::invalid(
                "submission attachments or timestamps are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInboxSnapshot {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<RunId>,
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub items: Vec<SessionSubmission>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEditRequest {
    pub input: String,
    pub expected_updated_at_ms: u64,
}

impl QueueEditRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.input.trim().is_empty() || self.input.chars().count() > 100_000 {
            Err(HarnessError::invalid(
                "queued input must contain 1 to 100000 characters",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringInput {
    pub submission_id: SubmissionId,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<InputProvenance>,
    pub display_input: Option<String>,
    pub source: UserMessageSource,
    pub references: Vec<SubmissionReference>,
    pub reference_contexts: Vec<ReferenceContext>,
    pub attachments: Vec<Attachment>,
}

impl SteeringInput {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.submission_id.validate()?;
        if self
            .provenance
            .as_ref()
            .is_some_and(|value| value.input_id != self.submission_id)
        {
            return Err(HarnessError::invalid(
                "steering provenance must identify its submission",
            ));
        }
        AgentInput {
            additional_inputs: Vec::new(),
            run_id: RunId::new("steering-input"),
            input: self.input.clone(),
            provenance: self.provenance.clone(),
            display_input: self.display_input.clone(),
            source: Some(self.source.clone()),
            references: self.references.clone(),
            reference_contexts: self.reference_contexts.clone(),
            attachments: self.attachments.clone(),
        }
        .validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserMessageSource {
    Schedule {
        schedule_id: ScheduleId,
        created_seq: u64,
        dispatched_seq: u64,
    },
    SkillInvocation {
        name: String,
    },
    Submission {
        submission_id: SubmissionId,
        created_at_ms: u64,
        delivery: SubmissionDelivery,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regenerate_from: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill_name: Option<String>,
    },
}

impl AgentInput {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.run_id.validate()?;
        for input in &self.additional_inputs {
            input.validate()?;
        }
        if let Some(provenance) = &self.provenance {
            provenance.validate()?;
        }
        if self.input.trim().is_empty() && self.attachments.is_empty() && self.references.is_empty()
        {
            return Err(HarnessError::invalid(
                "user input must contain text, an attachment, or a reference",
            ));
        }
        if self
            .display_input
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 100_000)
        {
            return Err(HarnessError::invalid(
                "display_input must contain 1 to 100000 characters when supplied",
            ));
        }
        if let Some(source) = &self.source {
            match source {
                UserMessageSource::Schedule {
                    schedule_id,
                    created_seq,
                    dispatched_seq,
                } => {
                    schedule_id.validate()?;
                    if created_seq >= dispatched_seq
                        || !matches!(
                            self.provenance.as_ref().map(|value| &value.author),
                            Some(InputAuthor::Automation {
                                source: AutomatedInputSource::Schedule
                            })
                        )
                    {
                        return Err(HarnessError::policy(
                            "scheduled input must reference its creation and dispatch",
                        ));
                    }
                }
                UserMessageSource::SkillInvocation { name } => validate_skill_name(name)?,
                UserMessageSource::Submission {
                    submission_id,
                    skill_name,
                    ..
                } => {
                    submission_id.validate()?;
                    if self
                        .provenance
                        .as_ref()
                        .is_some_and(|value| value.input_id != *submission_id)
                    {
                        return Err(HarnessError::invalid(
                            "input provenance differs from its submission source",
                        ));
                    }
                    if let Some(name) = skill_name {
                        validate_skill_name(name)?;
                    }
                }
            }
        }
        validate_references(&self.references)?;
        validate_reference_contexts(&self.reference_contexts)?;
        if self.attachments.len() > 10 {
            return Err(HarnessError::invalid(
                "one user message may contain at most 10 attachments",
            ));
        }
        for attachment in &self.attachments {
            attachment.validate()?;
        }
        if self
            .attachments
            .iter()
            .map(|attachment| attachment.content.len())
            .sum::<usize>()
            > 12 * 1024 * 1024
        {
            return Err(HarnessError::invalid(
                "attachment content may not exceed 12 MiB per message",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionActivityPhase {
    Running,
    WaitingForSubagents,
    WaitingForCapacity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventKind {
    TurnStarted,
    WorkspaceExecutionWaiting,
    WorkspaceExecutionAcquired,
    ExecutionActivityChanged {
        phase: ExecutionActivityPhase,
    },
    UserMessage {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provenance: Option<InputProvenance>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<UserMessageSource>,
        #[serde(default)]
        references: Vec<SubmissionReference>,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    StepStarted {
        step: u32,
    },
    ModelRequestStarted {
        step: u32,
        system_prompt: String,
    },
    ModelRetryScheduled {
        retry_id: String,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    },
    ModelRetryStarted {
        retry_id: String,
        retry: u32,
    },
    ModelRetryCancelled {
        retry_id: String,
        retry: u32,
    },
    AssistantMessageDelta {
        step: u32,
        delta: String,
    },
    AssistantReasoningDelta {
        step: u32,
        delta: String,
    },
    AssistantMessage {
        step: u32,
        response: ModelResponse,
    },
    ToolCallStarted {
        call: ToolCall,
    },
    ToolCallFinished {
        call_id: String,
        name: String,
        output: ToolOutput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_output: Option<Attachment>,
    },
    JobUpdated {
        job: JobSnapshot,
    },
    CodeDispatchStarted {
        parent_call_id: String,
        call: ToolCall,
    },
    CodeDispatchFinished {
        parent_call_id: String,
        call_id: String,
        name: String,
        output: ToolOutput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_output: Option<Attachment>,
    },
    RuntimeExtensionChanged {
        package_id: String,
        version: String,
        action: RuntimeExtensionAction,
    },
    PlanUpdated {
        explanation: Option<String>,
        items: Vec<PlanItem>,
    },
    PlanReviewCompleted {
        plan: String,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    TodoUpdated {
        items: Vec<PlanItem>,
    },
    GoalUpdated {
        objective: String,
        status: GoalStatus,
    },
    GoalRoundStarted {
        objective: String,
        round: u32,
        max_rounds: u32,
    },
    ContextCompactionStarted {
        compaction_id: String,
        automatic: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_command_id: Option<String>,
        turn: u32,
    },
    ContextCompacted {
        compaction_id: String,
        compaction: ContextCompaction,
    },
    ScheduleChanged {
        change: ScheduleChange,
    },
    DeliverableProduced {
        path: String,
        operation: String,
        attachment: Attachment,
    },
    CommandStarted {
        command_id: String,
        command_name: String,
    },
    FeedbackSubmitted {
        command_id: String,
        text: String,
    },
    CommandFinished {
        command_id: String,
        outcome: SessionCommandOutcome,
    },
    FeedbackRecorded {
        target_seq: u64,
        /// Monotonic version scoped to this assistant message, starting at one.
        revision: u64,
        /// `None` retracts the current rating for this assistant message.
        rating: Option<FeedbackRating>,
        note: Option<String>,
    },
    SubagentUpdated {
        subagent: SubagentSnapshot,
    },
    WorkflowRunStarted {
        workflow_id: WorkflowRunId,
        meta: WorkflowMeta,
    },
    WorkflowPhaseChanged {
        workflow_id: WorkflowRunId,
        title: String,
    },
    WorkflowLogEmitted {
        workflow_id: WorkflowRunId,
        message: String,
    },
    WorkflowAgentStarted {
        workflow_id: WorkflowRunId,
        sequence: u32,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        subagent_id: SubagentId,
    },
    WorkflowAgentFinished {
        workflow_id: WorkflowRunId,
        sequence: u32,
        outcome: WorkflowAgentOutcome,
    },
    WorkflowRunFinished {
        workflow_id: WorkflowRunId,
        stop_reason: WorkflowStopReason,
        agents_started: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    UserQuestionAsked {
        question: UserQuestion,
    },
    UserQuestionAnswered {
        answer: UserAnswer,
    },
    HookResult {
        result: HookResult,
    },
    HookContextAdded {
        handler_id: String,
        dialect: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<SubmissionReference>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completeness: Option<ReferenceContextCompleteness>,
    },
    StepFinished {
        step: u32,
    },
    TurnFinished {
        answer: String,
        finish_reason: TurnFinishReason,
    },
    SessionTitleGenerationStarted,
    SessionTitleGenerated {
        title: String,
    },
    SessionTitleGenerationFinished {
        generated: bool,
    },
    TurnFailed {
        message: String,
    },
    TurnCancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionServiceKind {
    Mcp,
    Lsp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionServiceStatus {
    Idle,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionServiceSnapshot {
    pub id: String,
    pub name: String,
    pub kind: SessionServiceKind,
    pub status: SessionServiceStatus,
    pub active_calls: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFinishReason {
    Completed,
    MaxTokens,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeExtensionAction {
    Enabled,
    Disabled,
    Mounted,
    Unmounted,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub seq: u64,
    pub occurred_at_ms: u64,
    pub run_id: RunId,
    #[serde(flatten)]
    pub kind: SessionEventKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCommandReceipt {
    pub command_id: String,
    pub events: Vec<SessionEvent>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    pub id: String,
    pub order: i32,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutcome {
    pub answer: String,
    pub steps: u32,
    pub tool_calls: u32,
    pub events: Vec<SessionEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_subagent_run_roundtrips_and_validates_both_identifiers() {
        let accepted = AcceptedSubagentRun {
            session_id: SessionId::new("child-session"),
            run_id: RunId::new("accepted-child-run"),
        };
        accepted.validate().unwrap();
        let encoded = serde_json::to_value(&accepted).unwrap();
        assert_eq!(
            serde_json::from_value::<AcceptedSubagentRun>(encoded).unwrap(),
            accepted
        );
        for invalid in [
            AcceptedSubagentRun {
                session_id: SessionId::new(""),
                ..accepted.clone()
            },
            AcceptedSubagentRun {
                run_id: RunId::new(""),
                ..accepted
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    fn run_spec(mode: SessionMode) -> RunSpec {
        RunSpec {
            schema_version: RUN_SPEC_VERSION,
            catalog_revision: "catalog-v1".to_owned(),
            policy_revision: "policy-v1".to_owned(),
            metadata: RunMetadata {
                tenant_id: TenantId::new("tenant-a"),
                user_id: UserId::new("user-a"),
                project_id: Some("project-a".to_owned()),
                workspace_id: WorkspaceId::new("workspace-a"),
                agent_id: AgentId::new("agent-a"),
                session_id: SessionId::new("session-a"),
                run_id: RunId::new("run-a"),
            },
            limits: RunLimits::default(),
            permissions: PermissionPreset::WorkspaceWrite,
            mode,
            profile: Profile::default(),
            input: "hello".to_owned(),
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn run_spec_v5_requires_and_round_trips_session_mode() {
        for mode in [SessionMode::Execute, SessionMode::Plan] {
            let spec = run_spec(mode);
            let encoded = serde_json::to_value(&spec).unwrap();
            assert_eq!(encoded["schema_version"], RUN_SPEC_VERSION);
            assert_eq!(
                encoded["mode"],
                match mode {
                    SessionMode::Execute => "execute",
                    SessionMode::Plan => "plan",
                }
            );
            assert_eq!(serde_json::from_value::<RunSpec>(encoded).unwrap(), spec);
        }

        let mut missing_mode = serde_json::to_value(run_spec(SessionMode::Execute)).unwrap();
        missing_mode.as_object_mut().unwrap().remove("mode");
        assert!(serde_json::from_value::<RunSpec>(missing_mode).is_err());
    }

    #[test]
    fn provider_models_resolve_inheritance_and_full_override() {
        let inherited_reasoning = ProviderModelReasoning {
            default_effort: ReasoningEffort::Medium,
            efforts: BTreeMap::from([
                (ReasoningEffort::Low, Some("provider-low".to_owned())),
                (ReasoningEffort::Medium, Some("provider-medium".to_owned())),
            ]),
        };
        let override_reasoning = ProviderModelReasoning {
            default_effort: ReasoningEffort::High,
            efforts: BTreeMap::from([
                (ReasoningEffort::High, Some("model-high".to_owned())),
                (ReasoningEffort::Max, Some("model-max".to_owned())),
            ]),
        };
        let provider = ProviderProfile {
            id: "provider-a".to_owned(),
            display_name: "Provider A".to_owned(),
            base_url: "https://models.example.test/v1".to_owned(),
            protocol: ProviderProtocol::OpenAiResponses,
            api_key_ref: Some("PROVIDER_A_KEY".to_owned()),
            defaults: ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 8_192,
                reasoning: Some(inherited_reasoning.clone()),
            },
            models: vec![
                ProviderModel {
                    id: "model-a".to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Inherit,
                },
                ProviderModel {
                    id: "model-b".to_owned(),
                    display_name: None,
                    settings: ProviderModelSettings::Override {
                        context_window: 1_000_000,
                        max_output_tokens: 64_000,
                        reasoning: Some(override_reasoning.clone()),
                    },
                },
            ],
            timeout_ms: 120_000,
            max_attempts: 3,
            retry_base_delay_ms: 250,
        };
        provider.validate().unwrap();

        let inherited = provider.resolved_model("model-a").unwrap();
        assert_eq!(inherited.context_window, 128_000);
        assert_eq!(inherited.max_output_tokens, 8_192);
        assert_eq!(inherited.reasoning, Some(inherited_reasoning));
        assert_eq!(
            inherited.reasoning_value(None).unwrap(),
            Some("provider-medium")
        );

        let overridden = provider.resolved_model("model-b").unwrap();
        assert_eq!(overridden.context_window, 1_000_000);
        assert_eq!(overridden.max_output_tokens, 64_000);
        assert_eq!(overridden.reasoning, Some(override_reasoning));
        assert_eq!(
            overridden
                .reasoning_value(Some(ReasoningEffort::Max))
                .unwrap(),
            Some("model-max")
        );

        let json = serde_json::to_value(&provider).unwrap();
        assert_eq!(json["models"][0]["settings"]["mode"], "inherit");
        assert_eq!(json["models"][1]["settings"]["mode"], "override");
        assert_eq!(
            serde_json::from_value::<ProviderProfile>(json).unwrap(),
            provider
        );
    }

    #[test]
    fn ids_are_bounded_at_the_boundary() {
        assert!(TenantId::new("  ").validate().is_err());
        assert!(TenantId::new("x".repeat(129)).validate().is_err());
        assert!(TenantId::new("tenant\nadmin").validate().is_err());
        assert!(TenantId::new("tenant-a").validate().is_ok());
    }

    #[test]
    fn event_kind_uses_a_stable_external_tag() {
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: RunId::new("run-1"),
            kind: SessionEventKind::UserMessage {
                content: "hello".to_owned(),
                provenance: None,
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: Vec::new(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["content"], "hello");
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);
    }

    #[test]
    fn job_lifecycle_event_keeps_the_canonical_wire_shape() {
        let event = SessionEvent {
            seq: 4,
            occurred_at_ms: 12,
            run_id: RunId::new("run-job"),
            kind: SessionEventKind::JobUpdated {
                job: JobSnapshot {
                    job_id: JobId::new("job-1"),
                    command: "cargo test".to_owned(),
                    status: JobStatus::Running,
                    result: None,
                    error: None,
                },
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "job_updated");
        assert_eq!(json["job"]["job_id"], "job-1");
        assert_eq!(json["job"]["status"], "running");
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);
    }

    #[test]
    fn command_events_keep_localizable_outcomes_on_the_wire() {
        let event = SessionEvent {
            seq: 2,
            occurred_at_ms: 3,
            run_id: RunId::new("command-feedback-1"),
            kind: SessionEventKind::CommandFinished {
                command_id: "feedback-1".to_owned(),
                outcome: SessionCommandOutcome {
                    kind: SessionCommandOutcomeKind::Success,
                    code: "feedback_recorded".to_owned(),
                    parameters: BTreeMap::from([("session_id".to_owned(), "session-1".to_owned())]),
                },
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "command_finished");
        assert_eq!(json["command_id"], "feedback-1");
        assert_eq!(json["outcome"]["kind"], "success");
        assert_eq!(json["outcome"]["code"], "feedback_recorded");
        assert_eq!(json["outcome"]["parameters"]["session_id"], "session-1");
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);
    }

    #[test]
    fn reasoning_event_and_usage_keep_the_canonical_wire_shape() {
        let event = SessionEvent {
            seq: 7,
            occurred_at_ms: 42,
            run_id: RunId::new("run-reasoning"),
            kind: SessionEventKind::AssistantReasoningDelta {
                step: 2,
                delta: "consider".to_owned(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "assistant_reasoning_delta");
        assert_eq!(json["step"], 2);
        assert_eq!(json["delta"], "consider");
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);

        let usage: ModelUsage = serde_json::from_value(serde_json::json!({
            "input_tokens": 11,
            "output_tokens": 5,
            "cached_input_tokens": 3,
            "cache_write_tokens": 2
        }))
        .unwrap();
        assert_eq!(usage.reasoning_tokens, 0);
        assert_eq!(usage.cache_write_tokens, Some(2));
        assert_eq!(
            serde_json::to_value(ModelUsage {
                reasoning_tokens: 4,
                ..usage
            })
            .unwrap()["reasoning_tokens"],
            4
        );
    }

    #[test]
    fn model_and_turn_completion_keep_exact_route_usage_and_finish_reasons() {
        let response = ModelResponse {
            provider: "openai".to_owned(),
            model: "gpt-test".to_owned(),
            content: "partial".to_owned(),
            reasoning_content: None,
            provider_state: None,
            tool_calls: Vec::new(),
            usage: Some(ModelUsage {
                input_tokens: 20,
                output_tokens: 7,
                cached_input_tokens: 5,
                cache_write_tokens: Some(3),
                reasoning_tokens: 2,
            }),
            finish_reason: ModelFinishReason::MaxTokens,
            provider_request_id: Some("request-1".to_owned()),
            attempts: 1,
            request_digest: None,
            replayed: false,
        };
        let response_json = serde_json::to_value(&response).unwrap();
        assert_eq!(response_json["provider"], "openai");
        assert_eq!(response_json["model"], "gpt-test");
        assert_eq!(response_json["finish_reason"], "max_tokens");
        assert_eq!(response_json["usage"]["cache_write_tokens"], 3);
        assert_eq!(
            serde_json::from_value::<ModelResponse>(response_json).unwrap(),
            response
        );

        let event = SessionEvent {
            seq: 12,
            occurred_at_ms: 90,
            run_id: RunId::new("run-max-tokens"),
            kind: SessionEventKind::TurnFinished {
                answer: "partial".to_owned(),
                finish_reason: TurnFinishReason::MaxTokens,
            },
        };
        let event_json = serde_json::to_value(&event).unwrap();
        assert_eq!(event_json["finish_reason"], "max_tokens");
        assert_eq!(
            serde_json::from_value::<SessionEvent>(event_json).unwrap(),
            event
        );
    }

    #[test]
    fn compaction_lifecycle_keeps_one_stable_identity_and_trigger_context() {
        let start = SessionEvent {
            seq: 20,
            occurred_at_ms: 100,
            run_id: RunId::new("run-compact"),
            kind: SessionEventKind::ContextCompactionStarted {
                compaction_id: "compaction-run-compact-9".to_owned(),
                automatic: false,
                source_command_id: Some("direct-run-compact".to_owned()),
                turn: 3,
            },
        };
        let started_json = serde_json::to_value(&start).unwrap();
        assert_eq!(started_json["type"], "context_compaction_started");
        assert_eq!(started_json["compaction_id"], "compaction-run-compact-9");
        assert_eq!(started_json["source_command_id"], "direct-run-compact");
        assert_eq!(started_json["turn"], 3);
        assert_eq!(
            serde_json::from_value::<SessionEvent>(started_json).unwrap(),
            start
        );

        let finished = SessionEvent {
            seq: 21,
            occurred_at_ms: 120,
            run_id: RunId::new("run-compact"),
            kind: SessionEventKind::ContextCompacted {
                compaction_id: "compaction-run-compact-9".to_owned(),
                compaction: ContextCompaction {
                    through_seq: 9,
                    summary: "summary".to_owned(),
                    estimated_tokens_before: 4_096,
                    automatic: false,
                },
            },
        };
        let finished_json = serde_json::to_value(&finished).unwrap();
        assert_eq!(finished_json["type"], "context_compacted");
        assert_eq!(finished_json["compaction_id"], "compaction-run-compact-9");
        assert_eq!(
            serde_json::from_value::<SessionEvent>(finished_json).unwrap(),
            finished
        );
    }

    #[test]
    fn model_request_event_preserves_the_exact_system_prompt() {
        let event = SessionEvent {
            seq: 8,
            occurred_at_ms: 43,
            run_id: RunId::new("run-request"),
            kind: SessionEventKind::ModelRequestStarted {
                step: 3,
                system_prompt: "opaque # text\n\n<tool>exact</tool>".to_owned(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "model_request_started");
        assert_eq!(json["step"], 3);
        assert_eq!(json["system_prompt"], "opaque # text\n\n<tool>exact</tool>");
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);
    }

    #[test]
    fn reference_context_event_carries_typed_provenance_and_completeness() {
        let event = SessionEvent {
            seq: 9,
            occurred_at_ms: 44,
            run_id: RunId::new("run-reference"),
            kind: SessionEventKind::HookContextAdded {
                handler_id: "reference:session".to_owned(),
                dialect: "ternilo.reference.session.v1".to_owned(),
                content: "opaque context".to_owned(),
                reference: Some(SubmissionReference::Session {
                    session_id: SessionId::new("source"),
                    label: "Source".to_owned(),
                }),
                completeness: Some(ReferenceContextCompleteness {
                    retained_items: 40,
                    omitted_items: 7,
                    truncated: true,
                }),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["reference"]["kind"], "session");
        assert_eq!(json["reference"]["label"], "Source");
        assert_eq!(json["completeness"]["retained_items"], 40);
        assert_eq!(json["completeness"]["omitted_items"], 7);
        assert_eq!(json["completeness"]["truncated"], true);
        assert_eq!(serde_json::from_value::<SessionEvent>(json).unwrap(), event);
    }

    #[test]
    fn session_submission_contract_is_typed_and_strict() {
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Steer,
            run_id: Some(RunId::new("run-queued")),
            content: SubmissionContent::Skill {
                name: "review-code".to_owned(),
                input: "Review the queue".to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
        };
        request.validate().unwrap();
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["delivery"], "steer");
        assert_eq!(json["content"]["kind"], "skill");
        assert_eq!(
            serde_json::from_value::<SessionSubmissionRequest>(json).unwrap(),
            request
        );
        assert!(
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Queue,
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: "   ".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            }
            .validate()
            .is_err()
        );

        let image = Attachment {
            name: "clipboard.png".to_owned(),
            media_type: "image/png".to_owned(),
            content: "data:image/png;base64,AA==".to_owned(),
        };
        SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: None,
            content: SubmissionContent::Prompt {
                input: String::new(),
            },
            references: Vec::new(),
            attachments: vec![image.clone()],
        }
        .validate()
        .unwrap();
        AgentInput {
            additional_inputs: Vec::new(),
            run_id: RunId::new("image-only"),
            input: String::new(),
            provenance: None,
            display_input: None,
            source: None,
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: vec![image.clone()],
        }
        .validate()
        .unwrap();
        RunSpec {
            schema_version: RUN_SPEC_VERSION,
            catalog_revision: "catalog".to_owned(),
            policy_revision: "policy".to_owned(),
            metadata: RunMetadata {
                tenant_id: TenantId::new("tenant"),
                user_id: UserId::new("user"),
                project_id: Some("project".to_owned()),
                workspace_id: WorkspaceId::new("workspace"),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new("session"),
                run_id: RunId::new("image-only"),
            },
            limits: RunLimits::default(),
            permissions: PermissionPreset::WorkspaceWrite,
            mode: SessionMode::Execute,
            profile: Profile::default(),
            input: String::new(),
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: vec![image],
        }
        .validate_shape()
        .unwrap();

        SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: None,
            content: SubmissionContent::Skill {
                name: "review-code".to_owned(),
                input: String::new(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn public_submission_cannot_claim_an_authenticated_author() {
        let request = serde_json::json!({
            "content": {"kind": "prompt", "input": "A task"}
        });
        assert!(serde_json::from_value::<SessionSubmissionRequest>(request.clone()).is_ok());
        let mut forged = request;
        forged["provenance"] = serde_json::json!({
            "input_id": "claimed-input",
            "author": {"kind": "account", "user_id": "someone-else", "username": "someone"}
        });
        assert!(
            serde_json::from_value::<SessionSubmissionRequest>(forged).is_err(),
            "public submissions cannot claim their own authenticated author"
        );
    }
}
