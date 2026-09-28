#![forbid(unsafe_code)]

mod account_status_store;
mod account_store;
pub use account_status_store::AccountStatusAction;
mod auth;
mod authentication_settings;
mod crypto;
mod edge_models;
mod edge_store;
mod fork_access;
mod group_store;
mod identity_sessions;
mod identity_store;
mod model_store;
mod native_recovery;
mod node_resources;
mod oidc_sessions;
mod placement_store;
mod registration_store;
mod resource_audit;
mod server_secrets;
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
    BrowserLoginKind, BrowserSessionAuthentication, BrowserSessionRevocation, BrowserSessions,
    NativeBrowserSession,
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
pub use registration_store::{
    AccountStatus, NativeRegistrationOutcome, OidcRegistrationOutcome, RegistrationDecision,
    RegistrationMode, RegistrationSettings,
};
pub use resource_audit::{event_references_attachment, question_resource_action};
pub use sharing_store::{
    CandidatePage, GrantPage, ResourceAccess, ResourceAccessSource, ResourceAccessSourceKind,
    ResourceAction, ResourceKind, ResourcePermissions, ShareSubject, SharedGrant,
    resource_access_in,
};
pub use store::ControlStore;
pub use types::{
    AuditEntry, ControlAction, ControlUser, EdgeSessionMetadata, EdgeSessionRecord,
    EnrollmentGrant, ExecutorRecord, MembershipRecord, ModelUsageAnomalyKind, ModelUsageGroup,
    ModelUsageLedgerEntry, ModelUsageQuotaSnapshot, ModelUsageReport, ModelUsageReservation,
    ModelUsageTotals, NodeCredentialGrant, NodePrincipal, OidcPrincipal, ProjectRecord,
    QuotaReservation, SecretMetadata, SpaceKind, TenantQuota, TenantRole, TenantSummary,
    WorkspacePlacement, WorkspaceRecord, WorkspaceStorage,
};
