#![forbid(unsafe_code)]

mod account_status_store;
mod account_store;
pub use account_status_store::AccountStatusAction;
mod auth;
mod computer_management;
mod computer_models;
pub use computer_management::{ComputerDetails, ComputerManagement, ComputerUpdate};
pub use computer_models::{
    ComputerModelAttemptRecord, ComputerModelRequestPage, ComputerModelRequestRecord,
};
mod authentication_settings;
mod crypto;
mod edge_models;
mod edge_store;
mod fork_access;
mod group_store;
mod identity_session_details;
mod identity_sessions;
mod identity_store;
mod model_store;
mod native_recovery;
mod node_account_cleanup;
mod node_resources;
pub use node_account_cleanup::AccountNodeCleanup;
mod oidc_sessions;
mod placement_store;
mod project_sharing;
mod registration_store;
mod resource_audit;
mod resource_ownership;
mod server_secrets;
mod service_accounts;
mod settings_store;
mod shared_resources;
mod sharing_store;
mod store;
mod types;

#[cfg(test)]
#[path = "../tests/support/postgres.rs"]
mod postgres_test;

pub use account_store::{
    AccountListQuery, AccountPage, AccountRecord, PlatformAction, PlatformRole,
    authorize_platform_in,
};
pub use auth::{OidcAuthenticator, OidcConfig, VerifiedOidcIdentity};
pub use crypto::{EncryptedSecret, SecretCipher};
pub use edge_store::EdgeStore;
pub use group_store::{GroupInput, GroupPage, GroupRecord, MemberPage, PageQuery};
pub use identity_sessions::{
    BrowserLoginKind, BrowserSession, BrowserSessionAuthentication, BrowserSessionRevocation,
    BrowserSessions,
};
pub use identity_store::{
    AccountLoginMethods, IdentitySession, InstanceMode, InstanceSettings, NativeRegistration,
    NativeSessionGrant, OidcAccountLink, UserInvitationGrant, UserInvitationRequest,
    VerifiedNativeCredentials,
};
pub use model_store::*;
pub use native_recovery::NativePasswordReset;
pub use node_resources::{NodeSessionResource, NodeWorkspaceResource};
pub use oidc_sessions::{OidcRefreshSession, OidcSessionGrant, OidcSessionIdentity};
pub use project_sharing::ProjectSharingInheritance;
pub use registration_store::{
    AccountStatus, NativeRegistrationOutcome, OidcRegistrationOutcome, RegistrationDecision,
    RegistrationMode, RegistrationSettings,
};
pub use resource_audit::{event_references_attachment, question_resource_action};
pub use resource_ownership::{ResourceOwnership, ResourceOwnershipTransfer};
pub use service_accounts::{
    ServiceAccount, ServiceAccountCreate, ServiceAccountUpdate, ServiceCredential,
    ServiceCredentialCreate, ServiceCredentialGrant, ServicePrincipal, ServiceScope,
    ServiceWorkspaceAccess, ServiceWorkspacePage, ServiceWorkspaceUpdate,
};
pub use sharing_store::{
    CandidatePage, GrantPage, ResourceAccess, ResourceAccessSource, ResourceAccessSourceKind,
    ResourceAction, ResourceKind, ResourcePermissions, ShareSubject, SharedGrant,
    resource_access_in,
};
pub use store::{ComputerProviderUsage, ComputerProviderUsagePage, ControlStore};
pub use types::{
    AuditEntry, ControlAction, ControlUser, EdgeSessionMetadata, EdgeSessionRecord,
    EnrollmentGrant, ExecutorRecord, MembershipRecord, ModelUsageAnomalyKind, ModelUsageGroup,
    ModelUsageLedgerEntry, ModelUsageQuotaSnapshot, ModelUsageReport, ModelUsageReservation,
    ModelUsageTotals, NodeCredentialGrant, NodePrincipal, OidcPrincipal, ProjectRecord,
    QuotaReservation, SecretMetadata, SpaceKind, TenantQuota, TenantRole, TenantSummary,
    WorkspacePlacement, WorkspaceRecord, WorkspaceStorage,
};
