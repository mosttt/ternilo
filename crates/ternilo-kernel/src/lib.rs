#![forbid(unsafe_code)]

mod activity;
mod harness;
mod host;
mod input_references;
mod plugin;
mod profile;
mod run_authorization;
pub use run_authorization::{
    ExecutionResourceControl, ExecutionResourceRegistry, RunAuthorization,
};
mod services;
mod tool_source;
mod workspace_execution;

pub use activity::{
    ActivityBranch, ActivityDelegation, ActivitySnapshot, ExecutionActivityOutput,
    ExecutionAdmission,
};
pub use harness::{HarnessSession, TokioSpawner};
pub use host::{
    AttachmentResolver, HostEnvironment, HostPolicy, HostSessionTelemetry, MemoryEventStore,
    SecretResolver, SessionArchive, SessionEventStore, SubagentSessionHost, UserInteraction,
    session_telemetry_record,
};
pub use input_references::InputReferenceResolver;
pub use plugin::{
    Catalog, HarnessPlugin, MountedPlugin, PluginFactory, PluginManifest, SessionProjectionUnit,
};
pub use profile::{compose_profiles, validate_profile};
pub use services::*;
pub use ternilo_protocol as protocol;
pub use tool_source::DeferredToolSource;
pub use workspace_execution::{WorkspaceExecution, WorkspaceExecutionLease};
