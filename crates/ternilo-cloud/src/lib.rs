#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use serde::{Deserialize, Serialize};
use ternilo_kernel::{Catalog, HostPolicy};
use ternilo_protocol::{
    HarnessError, RUN_SPEC_VERSION, RunId, RunLimits, RunMetadata, RunModelSnapshot, RunSpec,
    SessionIdentity, TenantId, UserId,
};

mod account_cleanup;
mod agent_team;
mod execution_admission;
mod execution_families;
pub use execution_admission::{ExecutionPhase, RunAdmission, RunExecutionStatus, WorkerCapacity};
mod commands;
mod event_notifications;
mod inbox;
mod input_provenance;
mod maintenance;
mod model_delegation;
mod outer_sandbox;
mod resource_settings;
mod run_lineage;
mod session_search;
mod shared_attachments;
mod sharing;
mod steering;
mod store;
mod subagents;
mod telemetry;
mod types;
mod worker_access;
mod worker_protocol;
mod workspace_occupancy;
mod workspace_recovery;
mod workspace_storage;
mod workspace_waiting;
pub use worker_access::{WorkerCredentialGrant, WorkerCredentialRecord};
pub use worker_protocol::*;
pub use workspace_occupancy::WorkspaceUseTicket;
pub use workspace_recovery::{
    WorkspaceRecoveryCursor, WorkspaceRecoveryPage, WorkspaceRecoveryTicket,
};

pub use commands::{
    ClaimedCloudSessionCommand, CloudCommandDelivery, CloudSessionCommandDraft,
    CloudSessionCommandRecord, CloudSessionCommandState, CloudWorkerIdentity, CloudWorkerRecord,
};
pub use event_notifications::{CloudLiveNotification, CloudSessionEventFeed};
pub use inbox::CloudSubmissionReceipt;
pub use maintenance::ExecutionMaintenance;
pub use outer_sandbox::{OUTER_SANDBOX_KIND, outer_sandbox_factory};
pub use run_lineage::CloudRunLineage;
pub use steering::CloudSteeringTicket;
pub use store::{CloudStore, EncryptedUserProviderCredential, UserProviderRoute, spec_digest};
pub use subagents::{CloudSubagent, WorkerSubagentRun};
pub use telemetry::{ClaimedCloudTelemetry, CloudSessionTelemetry};
pub use ternilo_builtins::{
    BROKERED_MODEL_KIND, BrokeredModelConfig, model_gateway_factory, profile_model_snapshot,
};
pub use types::{
    CloudPendingQuestion, CloudRunClaim, CloudRunDraft, CloudRunRecord, CloudRunState,
    CloudSessionDraft, CloudSessionRecord, CloudSessionState, CloudSessionUpdate, CompiledRun,
    ExecutionAttachmentObject, ExecutionEnvelope, StartedRun, TerminalState,
};

pub const CLOUD_CATALOG_REVISION: &str = "ternilo-cloud-v2";

/// Remove exporters owned by the trusted Cloud parent before a profile is
/// serialized into a child envelope or booted for an inspection child.
#[must_use]
pub fn cloud_child_profile(mut profile: ternilo_protocol::Profile) -> ternilo_protocol::Profile {
    profile
        .plugins
        .retain(|entry| entry.kind != ternilo_builtins::OTLP_TELEMETRY_KIND);
    profile
}

pub fn catalog() -> Result<Catalog, HarnessError> {
    let mut catalog = Catalog::new(CLOUD_CATALOG_REVISION);
    ternilo_builtins::register(&mut catalog)?;
    catalog.register(model_gateway_factory())?;
    catalog.register(ternilo_code_runtime::runtime_factory())?;
    catalog.register(ternilo_code_runtime::code_mode_factory())?;
    catalog.register(outer_sandbox_factory())?;
    catalog.register(ternilo_local::local_files_factory())?;
    catalog.register(ternilo_local::local_shell_factory())?;
    Ok(catalog)
}

#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the ordered Cloud plugin profile in one constructor."
)]
pub fn cloud_profile(model: Option<&RunModelSnapshot>) -> ternilo_protocol::Profile {
    let entry = |id: &str, kind: &str, config: serde_json::Value| ternilo_protocol::PluginEntry {
        id: id.to_owned(),
        kind: kind.to_owned(),
        enabled: true,
        config,
    };
    ternilo_protocol::Profile {
        plugins: vec![
            entry("session", "ternilo.session.log", serde_json::json!({})),
            entry(
                "prompt-registry",
                "ternilo.prompt.registry",
                serde_json::json!({}),
            ),
            entry(
                "tool-registry",
                "ternilo.tools.registry",
                serde_json::json!({}),
            ),
            entry(
                "hook-registry",
                "ternilo.hooks.registry",
                serde_json::json!({}),
            ),
            entry(
                "system-prompt",
                "ternilo.prompt.system",
                serde_json::json!({
                    "content": "You are Ternilo, a concise and capable software agent. Work only inside the mounted workspace."
                }),
            ),
            entry(
                "identity-prompt",
                "ternilo.prompt.identity",
                serde_json::json!({}),
            ),
            entry(
                "model",
                BROKERED_MODEL_KIND,
                serde_json::json!({
                    "snapshot": model,
                }),
            ),
            entry(
                "session-title",
                ternilo_builtins::SESSION_TITLE_KIND,
                serde_json::json!({}),
            ),
            entry(
                "context",
                "ternilo.context.compaction",
                serde_json::json!({}),
            ),
            entry(
                "cloud-outer-sandbox",
                OUTER_SANDBOX_KIND,
                serde_json::json!({}),
            ),
            entry(
                "workspace-files",
                ternilo_local::LOCAL_FILES_KIND,
                serde_json::json!({}),
            ),
            entry(
                "workspace-shell",
                ternilo_local::LOCAL_SHELL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "workspace-instructions",
                ternilo_builtins::INSTRUCTIONS_KIND,
                serde_json::json!({}),
            ),
            entry(
                "file-tools",
                ternilo_builtins::FILE_TOOLS_KIND,
                serde_json::json!({}),
            ),
            entry(
                "shell-tool",
                ternilo_builtins::SHELL_TOOL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "ask-user-tool",
                ternilo_builtins::ASK_USER_TOOL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "plan-tool",
                ternilo_builtins::PLAN_TOOL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "skill-registry",
                ternilo_builtins::SKILL_REGISTRY_KIND,
                serde_json::json!({}),
            ),
            entry(
                "filesystem-skills",
                ternilo_builtins::FILESYSTEM_SKILL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "skill-tools",
                ternilo_builtins::SKILL_TOOL_KIND,
                serde_json::json!({}),
            ),
            entry(
                "subagents",
                ternilo_builtins::SUBAGENT_KIND,
                serde_json::json!({}),
            ),
            entry(
                "agent-team-tools",
                ternilo_builtins::AGENT_TEAM_TOOLS_KIND,
                serde_json::json!({}),
            ),
            entry(
                "rhai-code-runtime",
                ternilo_code_runtime::RUNTIME_KIND,
                serde_json::json!({}),
            ),
            entry(
                "code-mode",
                ternilo_code_runtime::CODE_MODE_KIND,
                serde_json::json!({}),
            ),
            entry("agent-loop", "ternilo.agent.react", serde_json::json!({})),
        ],
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerPolicy {
    pub catalog_revision: String,
    pub policy_revision: String,
    pub maximum_limits: RunLimits,
    pub max_run_attempts: u32,
    pub max_tenant_workspace_bytes: u64,
    pub max_tenant_workspace_entries: u64,
    pub minimum_workspace_free_bytes: u64,
    pub allowed_plugin_kinds: BTreeSet<String>,
    pub max_extension_packages_per_run: u32,
    pub extension_host_policy: ternilo_extension::ExtensionHostPolicy,
    #[serde(default)]
    pub denied_tools: BTreeSet<String>,
}

pub struct ValidatedRun {
    pub identity: SessionIdentity,
    pub host_policy: HostPolicy,
}

const MAX_EXTENSION_PACKAGES_PER_RUN: u32 = 4;

impl WorkerPolicy {
    pub fn validate_operational_limits(&self) -> Result<(), HarnessError> {
        if self.max_tenant_workspace_bytes == 0 || self.max_tenant_workspace_entries == 0 {
            return Err(HarnessError::policy(
                "worker tenant workspace byte and entry limits must be positive",
            ));
        }
        Ok(())
    }

    pub fn validate_profile_composition(
        &self,
        profile: &ternilo_protocol::Profile,
        catalog: &Catalog,
    ) -> Result<(), HarnessError> {
        let extension_references = ternilo_extension::extension_mounts(profile)?;
        if extension_references.len()
            > usize::try_from(self.max_extension_packages_per_run).unwrap_or(usize::MAX)
        {
            return Err(HarnessError::policy(
                "profile exceeds the worker extension package count ceiling",
            ));
        }
        for entry in profile.plugins.iter().filter(|entry| entry.enabled) {
            if !self.allowed_plugin_kinds.contains(&entry.kind) {
                return Err(HarnessError::policy(format!(
                    "plugin kind {:?} is not allowed by worker policy",
                    entry.kind
                )));
            }
            if entry.kind == "ternilo.model.openai_compatible" {
                return Err(HarnessError::policy(
                    "cloud runs must use the host-gateway model; direct network model plugins are forbidden",
                ));
            }
        }
        for entry in profile.plugins.iter().filter(|entry| {
            entry.enabled && entry.kind != ternilo_extension::EXTENSION_PACKAGE_KIND
        }) {
            catalog.factory(&entry.kind)?.build(entry.config.clone())?;
        }
        Ok(())
    }

    pub fn validate(
        &self,
        spec: &RunSpec,
        catalog: &Catalog,
    ) -> Result<ValidatedRun, HarnessError> {
        spec.validate_shape()?;
        self.validate_operational_limits()?;
        if !(1..=10).contains(&self.max_run_attempts) {
            return Err(HarnessError::policy(
                "worker max_run_attempts must be between 1 and 10",
            ));
        }
        if self.max_extension_packages_per_run > MAX_EXTENSION_PACKAGES_PER_RUN {
            return Err(HarnessError::policy(format!(
                "worker max_extension_packages_per_run may not exceed {MAX_EXTENSION_PACKAGES_PER_RUN}",
            )));
        }
        if self.catalog_revision != catalog.revision()
            || spec.catalog_revision != self.catalog_revision
        {
            return Err(HarnessError::policy(format!(
                "catalog revision mismatch: worker={}, spec={}, linked={}",
                self.catalog_revision,
                spec.catalog_revision,
                catalog.revision()
            )));
        }
        if spec.policy_revision != self.policy_revision {
            return Err(HarnessError::policy(format!(
                "policy revision mismatch: worker={}, spec={}",
                self.policy_revision, spec.policy_revision
            )));
        }
        if (self.maximum_limits.max_steps != 0
            && (spec.limits.max_steps == 0
                || spec.limits.max_steps > self.maximum_limits.max_steps))
            || (self.maximum_limits.max_tool_calls != 0
                && (spec.limits.max_tool_calls == 0
                    || spec.limits.max_tool_calls > self.maximum_limits.max_tool_calls))
        {
            return Err(HarnessError::policy(
                "RunSpec limits exceed the worker policy ceiling",
            ));
        }
        self.validate_profile_composition(&spec.profile, catalog)?;
        if let Some(model) = profile_model_snapshot(&spec.profile)?
            && matches!(
                model.binding,
                ternilo_protocol::RunModelBinding::ComputerProvider { .. }
            )
        {
            return Err(HarnessError::policy(
                "computer models require a remote computer session",
            ));
        }
        Ok(ValidatedRun {
            identity: spec.metadata.identity(),
            host_policy: HostPolicy {
                limits: spec.limits,
                denied_tools: self.denied_tools.clone(),
                permissions: spec.permissions,
                allow_mutating_tools: spec.permissions.allows_workspace_write(),
            },
        })
    }

    pub fn compile_run(
        &self,
        draft: CloudRunDraft,
        tenant_id: TenantId,
        user_id: UserId,
        actor_user_id: UserId,
        catalog: &Catalog,
    ) -> Result<CompiledRun, HarnessError> {
        draft.validate()?;
        actor_user_id.validate()?;
        self.validate_profile_composition(&draft.profile, catalog)?;
        let run_id = draft.run_id.clone().unwrap_or_else(random_run_id);
        let spec = RunSpec {
            schema_version: RUN_SPEC_VERSION,
            catalog_revision: self.catalog_revision.clone(),
            policy_revision: self.policy_revision.clone(),
            metadata: RunMetadata {
                tenant_id,
                user_id,
                project_id: Some(draft.project_id),
                workspace_id: draft.workspace_id,
                agent_id: draft.agent_id,
                session_id: draft.session_id,
                run_id,
            },
            limits: draft.limits,
            permissions: draft.permissions,
            mode: draft.mode,
            profile: cloud_child_profile(draft.profile),
            input: draft.input,
            references: draft.references,
            reference_contexts: draft.reference_contexts,
            attachments: draft.attachments,
        };
        self.validate(&spec, catalog)?;
        Ok(CompiledRun {
            automated_input: None,
            actor_user_id,
            authorization_session_id: spec.metadata.session_id.clone(),
            spec,
            reserved_model_tokens: draft.reserved_model_tokens,
            priority: 0,
            max_attempts: self.max_run_attempts,
        })
    }
}

fn random_run_id() -> RunId {
    RunId::new(format!(
        "run_{}",
        URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
    ))
}

fn random_session_id() -> ternilo_protocol::SessionId {
    ternilo_protocol::SessionId::new(format!(
        "ses_{}",
        URL_SAFE_NO_PAD.encode(random::<[u8; 16]>())
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ternilo_kernel::{Catalog, compose_profiles, validate_profile};
    use ternilo_protocol::{
        AgentId, PluginEntry, Profile, RUN_SPEC_VERSION, RunId, RunMetadata, SessionId, TenantId,
        UserId, WorkspaceId, system_agent_presets,
    };

    use super::*;

    fn spec() -> RunSpec {
        RunSpec {
            schema_version: RUN_SPEC_VERSION,
            catalog_revision: "catalog-v1".to_owned(),
            policy_revision: "policy-v1".to_owned(),
            metadata: RunMetadata {
                tenant_id: TenantId::new("tenant-a"),
                user_id: UserId::new("user-a"),
                project_id: None,
                workspace_id: WorkspaceId::new("workspace-a"),
                agent_id: AgentId::new("agent-a"),
                session_id: SessionId::new("session-a"),
                run_id: RunId::new("run-a"),
            },
            limits: RunLimits::default(),
            permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
            mode: ternilo_protocol::SessionMode::Execute,
            profile: Profile::default(),
            input: "hello".to_owned(),
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn policy() -> WorkerPolicy {
        WorkerPolicy {
            catalog_revision: "catalog-v1".to_owned(),
            policy_revision: "policy-v1".to_owned(),
            maximum_limits: RunLimits {
                max_steps: 2,
                max_tool_calls: 4,
            },
            max_run_attempts: 2,
            max_tenant_workspace_bytes: 1024 * 1024 * 1024,
            max_tenant_workspace_entries: 100_000,
            minimum_workspace_free_bytes: 0,
            allowed_plugin_kinds: BTreeSet::new(),
            max_extension_packages_per_run: 0,
            extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
            denied_tools: BTreeSet::new(),
        }
    }

    #[test]
    fn rejects_limits_above_the_operator_ceiling() {
        let catalog = Catalog::new("catalog-v1");
        let policy = policy();
        let mut request = spec();
        request.limits.max_tool_calls = policy.maximum_limits.max_tool_calls;
        request.limits.max_steps = policy.maximum_limits.max_steps;
        assert!(policy.validate(&request, &catalog).is_ok());
        request.limits.max_steps = 3;
        assert!(policy.validate(&request, &catalog).is_err());
        request.limits.max_steps = 0;
        assert!(policy.validate(&request, &catalog).is_err());
    }

    #[test]
    fn zero_step_ceiling_accepts_unlimited_and_bounded_runs() {
        let catalog = Catalog::new("catalog-v1");
        let mut policy = policy();
        policy.maximum_limits.max_steps = 0;
        let mut request = spec();
        request.limits.max_tool_calls = policy.maximum_limits.max_tool_calls;
        request.limits.max_steps = 512;
        assert!(policy.validate(&request, &catalog).is_ok());
        request.limits.max_steps = 0;
        assert!(policy.validate(&request, &catalog).is_ok());
    }

    #[test]
    fn tool_call_ceiling_rejects_unlimited_requests_unless_the_operator_allows_them() {
        let catalog = Catalog::new("catalog-v1");
        let mut policy = policy();
        let mut request = spec();
        request.limits.max_steps = policy.maximum_limits.max_steps;
        request.limits.max_tool_calls = policy.maximum_limits.max_tool_calls;
        assert!(policy.validate(&request, &catalog).is_ok());
        request.limits.max_tool_calls += 1;
        assert!(policy.validate(&request, &catalog).is_err());
        request.limits.max_tool_calls = 0;
        assert!(policy.validate(&request, &catalog).is_err());
        policy.maximum_limits.max_tool_calls = 0;
        assert!(policy.validate(&request, &catalog).is_ok());
        request.limits.max_tool_calls = 512;
        assert!(policy.validate(&request, &catalog).is_ok());
    }

    #[test]
    fn rejects_extension_count_ceiling_that_cannot_fit_the_child_envelope() {
        let catalog = Catalog::new("catalog-v1");
        let mut policy = policy();
        policy.max_extension_packages_per_run = 5;
        assert!(policy.validate(&spec(), &catalog).is_err());
    }

    #[test]
    fn profile_composition_accepts_valid_extension_mounts_without_a_static_factory() {
        let catalog = catalog().unwrap();
        let mut policy = policy();
        policy.max_extension_packages_per_run = 1;
        let mut profile = cloud_profile(Some(&snapshot(None)));
        profile.plugins.push(PluginEntry {
            id: "workspace-extension-tool".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: serde_json::json!({
                "package_id": "dev.ternilo.workspace-tool",
                "version": "1.2.3",
                "settings": {}
            }),
        });
        policy.allowed_plugin_kinds = profile
            .plugins
            .iter()
            .map(|entry| entry.kind.clone())
            .collect();

        policy
            .validate_profile_composition(&profile, &catalog)
            .unwrap();
    }

    #[test]
    fn profile_composition_still_rejects_unknown_non_extension_kinds() {
        let catalog = catalog().unwrap();
        let mut policy = policy();
        let mut profile = cloud_profile(Some(&snapshot(None)));
        profile.plugins.push(PluginEntry {
            id: "unknown".to_owned(),
            kind: "example.unknown".to_owned(),
            enabled: true,
            config: serde_json::json!({}),
        });
        policy.allowed_plugin_kinds = profile
            .plugins
            .iter()
            .map(|entry| entry.kind.clone())
            .collect();

        let error = policy
            .validate_profile_composition(&profile, &catalog)
            .unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Composition);
        assert!(error.message.contains("example.unknown"));
    }

    #[test]
    fn every_system_preset_composes_with_the_cloud_profile() {
        let catalog = catalog().unwrap();
        let mut policy = policy();
        policy.allowed_plugin_kinds = catalog.kinds().map(str::to_owned).collect();

        for preset in system_agent_presets() {
            let profile = compose_profiles([cloud_profile(Some(&snapshot(None))), preset.profile]);
            validate_profile(&profile, &catalog).unwrap();
            policy
                .validate_profile_composition(&profile, &catalog)
                .unwrap();
        }
    }

    #[test]
    fn queue_priority_and_retry_budget_are_operator_owned() {
        let catalog = Catalog::new("catalog-v1");
        let compiled = policy()
            .compile_run(
                CloudRunDraft {
                    project_id: "project-a".to_owned(),
                    workspace_id: WorkspaceId::new("workspace-a"),
                    agent_id: AgentId::new("agent-a"),
                    session_id: SessionId::new("session-a"),
                    run_id: None,
                    limits: RunLimits {
                        max_steps: 1,
                        max_tool_calls: 1,
                    },
                    permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
                    mode: ternilo_protocol::SessionMode::Execute,
                    profile: Profile::default(),
                    input: "hello".to_owned(),
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                    reserved_model_tokens: 10,
                },
                TenantId::new("tenant-a"),
                UserId::new("user-a"),
                UserId::new("user-a"),
                &catalog,
            )
            .unwrap();
        assert_eq!(compiled.priority, 0);
        assert_eq!(compiled.max_attempts, 2);

        let mut request = serde_json::to_value(CloudRunDraft {
            project_id: "project-a".to_owned(),
            workspace_id: WorkspaceId::new("workspace-a"),
            agent_id: AgentId::new("agent-a"),
            session_id: SessionId::new("session-a"),
            run_id: None,
            limits: RunLimits {
                max_steps: 1,
                max_tool_calls: 1,
            },
            permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
            mode: ternilo_protocol::SessionMode::Execute,
            profile: Profile::default(),
            input: "hello".to_owned(),
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: Vec::new(),
            reserved_model_tokens: 10,
        })
        .unwrap();
        request["priority"] = serde_json::json!(1_000);
        assert!(serde_json::from_value::<CloudRunDraft>(request).is_err());
    }

    #[test]
    fn compile_run_preserves_session_mode_independently_of_permissions() {
        let catalog = Catalog::new("catalog-v1");
        for (mode, permissions) in [
            (
                ternilo_protocol::SessionMode::Execute,
                ternilo_protocol::PermissionPreset::ReadOnly,
            ),
            (
                ternilo_protocol::SessionMode::Plan,
                ternilo_protocol::PermissionPreset::WorkspaceWrite,
            ),
        ] {
            let compiled = policy()
                .compile_run(
                    CloudRunDraft {
                        project_id: "project-a".to_owned(),
                        workspace_id: WorkspaceId::new("workspace-a"),
                        agent_id: AgentId::new("agent-a"),
                        session_id: SessionId::new("session-a"),
                        run_id: None,
                        limits: RunLimits {
                            max_steps: 1,
                            max_tool_calls: 1,
                        },
                        permissions,
                        mode,
                        profile: Profile::default(),
                        input: "hello".to_owned(),
                        references: Vec::new(),
                        reference_contexts: Vec::new(),
                        attachments: Vec::new(),
                        reserved_model_tokens: 10,
                    },
                    TenantId::new("tenant-a"),
                    UserId::new("user-a"),
                    UserId::new("user-a"),
                    &catalog,
                )
                .unwrap();
            assert_eq!(compiled.spec.mode, mode);
            assert_eq!(compiled.spec.permissions, permissions);
        }
    }

    fn snapshot(effort: Option<ternilo_protocol::ReasoningEffort>) -> RunModelSnapshot {
        use ternilo_protocol::{
            ProviderModelDefaults, ProviderModelReasoning, ProviderProtocol, ReasoningEffort,
            RunModelBinding,
        };
        RunModelSnapshot {
            binding: RunModelBinding::Platform {
                grant_id: "grant-primary".to_owned(),
                model_id: "model-a".to_owned(),
                beneficiary_user_id: UserId::new("user-a"),
            },
            protocol: ProviderProtocol::OpenAiChatCompletions,
            defaults: ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 2_048,
                reasoning: Some(ProviderModelReasoning {
                    default_effort: ReasoningEffort::Medium,
                    efforts: [
                        (ReasoningEffort::Low, Some("low".to_owned())),
                        (ReasoningEffort::Medium, Some("medium".to_owned())),
                        (ReasoningEffort::High, Some("ultra".to_owned())),
                    ]
                    .into_iter()
                    .collect(),
                }),
            },
            reasoning_effort: effort,
            display_name: "Model A".to_owned(),
            source_name: "Primary allocation".to_owned(),
        }
    }

    #[test]
    fn host_gateway_snapshot_preserves_capabilities_and_allows_delegated_beneficiary() {
        use ternilo_protocol::ReasoningEffort;
        let mut catalog = Catalog::new("catalog-v1");
        catalog.register(model_gateway_factory()).unwrap();
        let mut policy = policy();
        policy
            .allowed_plugin_kinds
            .insert(BROKERED_MODEL_KIND.to_owned());
        let model = snapshot(Some(ReasoningEffort::High));
        let mut request = spec();
        request.limits = policy.maximum_limits;
        request.profile.plugins.push(PluginEntry {
            id: "model".to_owned(),
            kind: BROKERED_MODEL_KIND.to_owned(),
            enabled: true,
            config: serde_json::json!({"snapshot": model}),
        });
        policy.validate(&request, &catalog).unwrap();
        assert_eq!(
            model
                .resolved_model()
                .reasoning_value(model.reasoning_effort)
                .unwrap(),
            Some("ultra")
        );
        assert_eq!(model.defaults.max_output_tokens, 2_048);
        request.profile.plugins[0].config["snapshot"]["reasoning_effort"] =
            serde_json::json!("xhigh");
        assert!(policy.validate(&request, &catalog).is_err());
        request.profile.plugins[0].config["snapshot"]["reasoning_effort"] =
            serde_json::json!("high");
        request.profile.plugins[0].config["snapshot"]["binding"]["beneficiary_user_id"] =
            serde_json::json!("other-user");
        policy.validate(&request, &catalog).unwrap();
        assert_eq!(request.metadata.user_id.as_str(), "user-a");
        request.profile.plugins[0].config["snapshot"]["binding"] = serde_json::json!({
            "kind": "computer_provider", "tenant_id": "team", "owner_user_id": "other-user",
            "executor_id": "computer", "provider_id": "source", "model": "test-model",
        });
        assert!(policy.validate(&request, &catalog).is_err());
    }

    #[test]
    fn cloud_profile_pins_the_unified_reasoning_selection() {
        let model = snapshot(Some(ternilo_protocol::ReasoningEffort::High));
        let profile = cloud_profile(Some(&model));
        assert!(
            profile
                .plugins
                .iter()
                .any(|entry| entry.kind == ternilo_builtins::ASK_USER_TOOL_KIND)
        );
        assert_eq!(profile_model_snapshot(&profile).unwrap(), Some(model));
    }

    #[test]
    fn empty_cloud_profile_supports_inspection_without_a_model() {
        let profile = cloud_profile(None);
        assert!(profile_model_snapshot(&profile).unwrap().is_none());
        assert!(
            profile
                .plugins
                .iter()
                .any(|entry| entry.kind == ternilo_builtins::FILE_TOOLS_KIND)
        );
        let catalog = catalog().unwrap();
        validate_profile(&profile, &catalog).unwrap();
        for preset in system_agent_presets() {
            validate_profile(
                &compose_profiles([profile.clone(), preset.profile]),
                &catalog,
            )
            .unwrap();
        }
    }

    #[test]
    fn docker_worker_policy_only_configures_execution() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/docker/worker-policy.json"))
                .unwrap();
        assert!(value.get("model_routes").is_none());
        let policy: WorkerPolicy = serde_json::from_value(value).unwrap();
        policy.validate_operational_limits().unwrap();
        assert_eq!(policy.maximum_limits.max_steps, 0);
        assert_eq!(
            policy.maximum_limits.max_tool_calls,
            RunLimits::default().max_tool_calls
        );
        assert!(
            policy
                .allowed_plugin_kinds
                .contains(ternilo_builtins::ASK_USER_TOOL_KIND)
        );
    }

    #[test]
    fn cloud_rejects_direct_network_model_plugins() {
        let catalog = Catalog::new("catalog-v1");
        let mut policy = policy();
        policy
            .allowed_plugin_kinds
            .insert("ternilo.model.openai_compatible".to_owned());
        let mut request = spec();
        request.limits = policy.maximum_limits;
        request.profile.plugins.push(PluginEntry {
            id: "model".to_owned(),
            kind: "ternilo.model.openai_compatible".to_owned(),
            enabled: true,
            config: serde_json::json!({}),
        });
        let error = policy.validate(&request, &catalog).err().unwrap();
        assert!(error.message.contains("host-gateway"));
    }

    #[test]
    fn cloud_child_profile_never_contains_parent_otlp_configuration() {
        let profile = Profile {
            plugins: vec![
                PluginEntry {
                    id: "telemetry".to_owned(),
                    kind: ternilo_builtins::OTLP_TELEMETRY_KIND.to_owned(),
                    enabled: true,
                    config: serde_json::json!({
                        "mode": "full",
                        "endpoint": "https://collector.example.test/v1/logs",
                        "headers": {"authorization": "parent-only-sentinel"}
                    }),
                },
                PluginEntry {
                    id: "agent".to_owned(),
                    kind: "ternilo.agent.react".to_owned(),
                    enabled: true,
                    config: serde_json::json!({}),
                },
            ],
        };
        let child = cloud_child_profile(profile);
        assert_eq!(child.plugins.len(), 1);
        assert_eq!(child.plugins[0].kind, "ternilo.agent.react");
        assert!(
            !serde_json::to_string(&child)
                .unwrap()
                .contains("parent-only-sentinel")
        );
    }

    #[test]
    fn run_spec_digest_is_independent_of_json_object_order() {
        let mut first = spec();
        first.profile.plugins.push(PluginEntry {
            id: "ordered".to_owned(),
            kind: "fixture".to_owned(),
            enabled: true,
            config: serde_json::from_str(r#"{"z":1.00,"nested":{"b":2,"a":1}}"#).unwrap(),
        });
        let mut second = first.clone();
        second.profile.plugins[0].config =
            serde_json::from_str(r#"{"nested":{"a":1,"b":2},"z":1.0}"#).unwrap();
        assert_eq!(spec_digest(&first).unwrap(), spec_digest(&second).unwrap());
    }
}
