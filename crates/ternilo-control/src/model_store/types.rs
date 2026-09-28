use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_protocol::{
    ProviderProfile, ProviderProtocol, RunId, RunModelBinding, SessionId, TenantId, UserId,
    WorkspaceId,
};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderInput {
    pub profile: ProviderProfile,
    pub enabled: bool,
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelProviderRecord {
    pub profile: ProviderProfile,
    pub enabled: bool,
    pub has_api_key: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPublicationInput {
    pub model_id: String,
    pub display_name: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub enabled: bool,
}

pub use ternilo_protocol::PublishedModel as PublicModel;

#[derive(Clone, Debug, Serialize)]
pub struct ModelPublicationRecord {
    #[serde(flatten)]
    pub model: PublicModel,
    pub provider_id: String,
    pub upstream_model: String,
    pub enabled: bool,
    pub provider_enabled: bool,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelGrantSubject {
    User { id: String },
    Group { id: String },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelGrantInput {
    pub name: String,
    pub subject: ModelGrantSubject,
    pub model_ids: Vec<String>,
    pub monthly_tokens: u64,
    pub max_concurrent_requests: u32,
    pub expires_at_ms: Option<u64>,
    #[serde(default = "default_resource_sharing")]
    pub allow_resource_sharing: bool,
}

const fn default_resource_sharing() -> bool {
    true
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelQuotaSnapshot {
    pub month: String,
    pub limit_tokens: u64,
    pub used_tokens: u64,
    pub reserved_tokens: u64,
    pub active_requests: u64,
    pub max_concurrent_requests: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelDeviceUsage {
    pub month: String,
    pub used_tokens: u64,
    pub reserved_tokens: u64,
    pub active_requests: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelGrantRecord {
    pub grant_id: String,
    pub name: String,
    pub subject: ModelGrantSubject,
    pub subject_name: Option<String>,
    pub model_ids: Vec<String>,
    pub quota: ModelQuotaSnapshot,
    pub expires_at_ms: Option<u64>,
    pub allow_resource_sharing: bool,
    pub revoked_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelEntitlement {
    pub grant: ModelGrantRecord,
    pub models: Vec<PublicModel>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelGroupRecord {
    pub group_id: String,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u64,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelKeyInput {
    pub name: String,
    pub grant_id: String,
    pub model_ids: Vec<String>,
    pub monthly_tokens: Option<u64>,
    pub max_concurrent_requests: Option<u32>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCredentialKind {
    ApiKey,
    ClientDevice,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelKeyRecord {
    pub key_id: String,
    pub user_id: UserId,
    pub name: String,
    pub token_prefix: String,
    pub kind: ModelCredentialKind,
    pub grant_id: String,
    pub grant_name: String,
    pub model_ids: Vec<String>,
    pub monthly_tokens: Option<u64>,
    pub max_concurrent_requests: Option<u32>,
    pub expires_at_ms: Option<u64>,
    pub revoked_at_ms: Option<u64>,
    pub created_at_ms: u64,
    pub last_used_at_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct ModelKeyCreation {
    pub key: ModelKeyRecord,
    pub token: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelKeyPrincipal {
    pub key_id: String,
    pub user_id: UserId,
    pub grant_id: String,
}

#[derive(Clone, Debug)]
pub struct ModelRequestInput {
    pub request_key: String,
    pub payload_hash: String,
    pub model_id: String,
    pub protocol: ProviderProtocol,
    pub reserved_tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRequestState {
    Pending,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub raw_usage: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequestSettlement {
    pub state: ModelRequestState,
    pub usage: Option<ServiceModelUsage>,
    pub upstream_request_id: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelServiceRequest {
    pub request_id: String,
    pub origin: ModelRequestOrigin,
    pub source: ModelRequestSource,
    pub key_id: Option<String>,
    pub actor_user_id: UserId,
    pub resource_owner_user_id: Option<UserId>,
    pub model_beneficiary_user_id: UserId,
    pub grant_id: Option<String>,
    pub grant_name: Option<String>,
    pub workload: Option<WorkloadModelPrincipal>,
    pub model_id: String,
    pub provider_id: String,
    pub upstream_model: String,
    pub protocol: ProviderProtocol,
    pub state: ModelRequestState,
    pub attempted: bool,
    pub reserved_tokens: u64,
    pub accounted_tokens: Option<u64>,
    pub usage: Option<ServiceModelUsage>,
    pub upstream_request_id: Option<String>,
    pub error_code: Option<String>,
    pub month: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub settled_at_ms: Option<u64>,
    pub attempts: Vec<ModelServiceAttempt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRequestOrigin {
    ApiKey,
    ClientDevice,
    Workload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRequestSource {
    PlatformGrant,
    UserProvider,
}

/// Constructed from a canonical workload by the trusted Server, never from a model HTTP body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadModelPrincipal {
    pub tenant_id: TenantId,
    pub project_id: String,
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub authorization_session_id: SessionId,
    pub run_id: RunId,
    pub actor_user_id: UserId,
    pub resource_owner_user_id: UserId,
    pub execution_owner_user_id: UserId,
    pub execution_reservation_id: String,
    pub worker_id: String,
    pub worker_generation: u64,
    pub lease_token: u64,
    pub writer_fencing_token: u64,
    pub model: RunModelBinding,
    pub run_token_limit: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelServiceAttempt {
    pub attempt: u32,
    pub state: ModelRequestState,
    pub attempted: bool,
    pub reserved_tokens: u64,
    pub accounted_tokens: Option<u64>,
    pub usage: Option<ServiceModelUsage>,
    pub upstream_request_id: Option<String>,
    pub error_code: Option<String>,
    pub created_at_ms: u64,
    pub settled_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkloadReservationSummary {
    pub state: String,
    pub period_start: String,
    pub reserved_model_tokens: u64,
    pub used_model_tokens: u64,
    pub unknown_model_tokens: u64,
}

pub struct ResolvedModelRoute {
    pub provider: ProviderProfile,
    pub model: PublicModel,
    pub upstream_model: String,
    pub api_key: Option<Zeroizing<String>>,
}

pub struct ModelRequestPermit {
    pub request: ModelServiceRequest,
    pub route: ResolvedModelRoute,
    pub newly_accepted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelAccessErrorKind {
    Unauthorized,
    Forbidden,
    InvalidInput,
    Conflict,
    QuotaExceeded,
    Internal,
}

#[derive(Debug)]
pub struct ModelAccessError {
    pub kind: ModelAccessErrorKind,
    pub error: ternilo_protocol::HarnessError,
}

impl std::fmt::Display for ModelAccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for ModelAccessError {}

impl From<ternilo_protocol::HarnessError> for ModelAccessError {
    fn from(error: ternilo_protocol::HarnessError) -> Self {
        use ternilo_protocol::ErrorCode;
        let kind = match error.code {
            ErrorCode::InvalidInput => ModelAccessErrorKind::InvalidInput,
            ErrorCode::PolicyDenied => ModelAccessErrorKind::Forbidden,
            ErrorCode::Conflict => ModelAccessErrorKind::Conflict,
            _ => ModelAccessErrorKind::Internal,
        };
        Self { kind, error }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ModelServiceUsageReport {
    pub month: String,
    pub request_count: u64,
    pub active_requests: u64,
    pub unknown_requests: u64,
    pub used_tokens: u64,
    pub reserved_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
}

macro_rules! page {
    ($name:ident, $field:ident, $item:ty) => {
        #[derive(Clone, Debug, Serialize)]
        pub struct $name {
            pub $field: Vec<$item>,
            pub next_cursor: Option<String>,
        }
    };
}

page!(ModelProviderPage, providers, ModelProviderRecord);
page!(ModelPublicationPage, models, ModelPublicationRecord);
page!(ModelGroupPage, groups, ModelGroupRecord);
page!(ModelGroupMemberPage, users, crate::ControlUser);
page!(ModelGrantPage, grants, ModelGrantRecord);
page!(ModelEntitlementPage, entitlements, ModelEntitlement);
page!(ModelKeyPage, keys, ModelKeyRecord);
page!(ModelRequestPage, requests, ModelServiceRequest);
