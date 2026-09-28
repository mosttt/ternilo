#![forbid(unsafe_code)]

mod server_models;
pub use server_models::{ModelInputOrigin, ServerModelGateway};

mod agent_team;
mod application;
mod attachments;
mod authorization;
mod capabilities;
mod credentials;
mod directory;
mod event_store;
mod extensions;
mod inbox;
mod interaction;
mod jobs;
mod model_connections;
mod notifications;
mod persistence;
mod preferences;
mod presets;
mod process_group;
mod projection_cache;
mod providers;
pub use model_connections::{ConnectionAuthorization, ConnectionPoll, ModelConnection};
mod desktop_apps;
mod references;
mod sandbox;
mod search_index;
mod session_query;
mod state;
mod terminal;
mod workspace_execution;
pub use application::workspace_browser::browse_workspace;

pub use application::{
    LocalApplication, LocalEventNotification, LocalSessionUpdate, SessionExport,
};
pub use attachments::{LocalAttachmentReader, LocalAttachments, inline_file_attachment_bytes};
pub use authorization::LocalAuthorizations;
pub use capabilities::{
    LOCAL_FILES_KIND, LOCAL_SHELL_KIND, local_files_factory, local_shell_factory,
};
pub use credentials::LocalCredentials;
pub use directory::{
    DirectoryEntry, DirectoryListing, canonical_directory, create_directory, home_directory,
    list_directory,
};
pub use extensions::LocalRuntimeExtensions;
pub use interaction::{InteractionBroker, PendingQuestion};
pub use jobs::local_jobs_factory;
pub use notifications::{LocalInvalidationCategory, LocalInvalidationNotification};
pub use presets::{DEFAULT_AGENT_PRESET, LocalAgentPresets};
pub use providers::LocalProviders;
pub use references::{
    fit_reference_contexts, resolve_file_references, session_reference_context,
    workspace_reference_candidates,
};
pub use sandbox::{LOCAL_SANDBOX_KIND, local_sandbox_factory};
pub use search_index::{event_category as session_event_category, searchable_event_text};
pub use state::{LocalSession, LocalStateSnapshot, ModelSelection, Workspace};
pub use terminal::local_terminals_factory;
pub use workspace_execution::DirectoryCoordinator;

use std::path::PathBuf;

use serde_json::json;
use ternilo_kernel::Catalog;
use ternilo_protocol::{HarnessError, PluginEntry, Profile};

pub const LOCAL_CATALOG_REVISION: &str = "ternilo-local-v19";

pub fn catalog() -> Result<Catalog, HarnessError> {
    let mut catalog = Catalog::new(LOCAL_CATALOG_REVISION);
    ternilo_builtins::register(&mut catalog)?;
    catalog.register(ternilo_builtins::model_gateway_factory())?;
    catalog.register(ternilo_code_runtime::runtime_factory())?;
    catalog.register(ternilo_code_runtime::code_mode_factory())?;
    catalog.register(local_sandbox_factory())?;
    catalog.register(local_files_factory())?;
    catalog.register(local_shell_factory())?;
    catalog.register(local_jobs_factory())?;
    catalog.register(local_terminals_factory())?;
    Ok(catalog)
}

#[must_use]
pub fn local_profile() -> Profile {
    let mut profile = ternilo_builtins::local_profile();
    profile.plugins.extend([
        plugin("rhai-code-runtime", ternilo_code_runtime::RUNTIME_KIND),
        plugin("local-sandbox", sandbox::LOCAL_SANDBOX_KIND),
        plugin("local-files", capabilities::LOCAL_FILES_KIND),
        plugin("local-shell", capabilities::LOCAL_SHELL_KIND),
        plugin("local-jobs", jobs::LOCAL_JOBS_KIND),
        plugin("local-terminals", terminal::LOCAL_TERMINALS_KIND),
        plugin("schedule-tools", ternilo_builtins::SCHEDULE_TOOL_KIND),
        plugin(
            "session-query-tools",
            ternilo_builtins::SESSION_QUERY_TOOLS_KIND,
        ),
        plugin("terminal-tools", ternilo_builtins::TERMINAL_TOOLS_KIND),
        plugin("job-tools", ternilo_builtins::JOB_TOOLS_KIND),
        plugin("agent-team-tools", ternilo_builtins::AGENT_TEAM_TOOLS_KIND),
        plugin(
            "workspace-instructions",
            ternilo_builtins::INSTRUCTIONS_KIND,
        ),
        plugin("file-tools", ternilo_builtins::FILE_TOOLS_KIND),
        plugin("shell-tool", ternilo_builtins::SHELL_TOOL_KIND),
        plugin("ask-user-tool", ternilo_builtins::ASK_USER_TOOL_KIND),
        plugin("plan-tool", ternilo_builtins::PLAN_TOOL_KIND),
        plugin("skill-registry", ternilo_builtins::SKILL_REGISTRY_KIND),
        plugin("filesystem-skills", ternilo_builtins::FILESYSTEM_SKILL_KIND),
        plugin("skill-tools", ternilo_builtins::SKILL_TOOL_KIND),
        plugin("web-fetch", ternilo_builtins::WEB_FETCH_KIND),
        plugin("code-mode", ternilo_code_runtime::CODE_MODE_KIND),
    ]);
    profile
}

fn plugin(id: &str, kind: &str) -> PluginEntry {
    PluginEntry {
        id: id.to_owned(),
        kind: kind.to_owned(),
        enabled: true,
        config: json!({}),
    }
}

pub fn default_data_dir() -> Result<PathBuf, HarnessError> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("ternilo"));
    }
    let home = home_directory()?;
    Ok(home.join(".local").join("share").join("ternilo"))
}
