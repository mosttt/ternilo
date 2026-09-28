use serde_json::json;

use crate::{AgentPresetDocument, AgentPresetSummary, AgentPresetTrust, PluginEntry, Profile};

pub const DEFAULT_AGENT_PRESET_ID: &str = "standard";
pub const SYSTEM_AGENT_PRESET_IDS: [&str; 4] = ["standard", "ptc", "minimal", "creative"];

const CODE_MODE_KIND: &str = "ternilo.tools.code_mode";
const RHAI_RUNTIME_KIND: &str = "ternilo.code_runtime.rhai";

#[must_use]
pub fn system_agent_presets() -> Vec<AgentPresetDocument> {
    SYSTEM_AGENT_PRESET_IDS
        .into_iter()
        .map(|id| system_agent_preset(id).expect("system preset id is defined"))
        .collect()
}

#[must_use]
pub fn system_agent_preset(id: &str) -> Option<AgentPresetDocument> {
    match id {
        "standard" => Some(document(
            id,
            "Standard Mode",
            "Full software Agent with native tools and the Rhai Code Mode SDK.",
            code_mode_profile("both"),
        )),
        "ptc" => Some(document(
            id,
            "PTC Mode",
            "Uses the full Standard capability set while presenting only the Rhai Code Mode SDK to the model.",
            code_mode_profile("code"),
        )),
        "minimal" => Some(document(
            id,
            "Minimal Mode",
            "Streamlined software Agent centered on workspace files and shell.",
            minimal_profile(),
        )),
        "creative" => Some(document(
            id,
            "Creative Mode",
            "Full Standard capabilities with guidance for exploring presets, plugins, and runtime behavior.",
            creative_profile(),
        )),
        _ => None,
    }
}

#[must_use]
pub fn is_system_agent_preset(id: &str) -> bool {
    SYSTEM_AGENT_PRESET_IDS.contains(&id)
}

fn document(
    id: &str,
    display_name: &str,
    description: &str,
    profile: Profile,
) -> AgentPresetDocument {
    AgentPresetDocument {
        base_profile: None,
        summary: AgentPresetSummary {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            description: description.to_owned(),
            trust: AgentPresetTrust::System,
        },
        profile,
    }
}

fn code_mode_profile(mode: &str) -> Profile {
    Profile {
        plugins: vec![entry(
            "code-mode",
            CODE_MODE_KIND,
            true,
            json!({ "mode": mode }),
        )],
    }
}

fn creative_profile() -> Profile {
    Profile {
        plugins: vec![
            entry("code-mode", CODE_MODE_KIND, true, json!({ "mode": "both" })),
            entry(
                "creative-guidance",
                "ternilo.prompt.section",
                true,
                json!({
                    "id": "creative-mode",
                    "order": 440,
                    "content": "Creative mode is active. Explore multiple implementation directions, inspect the live workspace and available tools before choosing an approach, and test small prototypes with the tools actually present. For Profile or plugin work, derive changes from the current catalog and configuration instead of assuming capabilities; verify the selected composition and runtime behavior before reporting success."
                }),
            ),
        ],
    }
}

fn minimal_profile() -> Profile {
    let disabled = [
        ("identity-prompt", "ternilo.prompt.identity"),
        ("workflow-engine", "ternilo.workflow.rhai"),
        ("workflow-tool", "ternilo.tools.workflow"),
        ("subagents", "ternilo.subagents.in_process"),
        ("agent-team-tools", "ternilo.tools.agent_team"),
        (
            "runtime-extension-tools",
            "ternilo.tools.runtime_extensions",
        ),
        ("schedule-tools", "ternilo.tool.schedule"),
        ("session-query-tools", "ternilo.tool.session_query"),
        ("terminal-tools", "ternilo.tools.terminal"),
        ("job-tools", "ternilo.tools.jobs"),
        (
            "workspace-instructions",
            "ternilo.prompt.workspace_instructions",
        ),
        ("ask-user-tool", "ternilo.tool.ask_user"),
        ("plan-tool", "ternilo.tool.plan"),
        ("skill-tools", "ternilo.tools.skills"),
        ("filesystem-skills", "ternilo.skills.filesystem"),
        ("skill-registry", "ternilo.skills.registry"),
        ("web-fetch", "ternilo.tool.web_fetch"),
        ("rhai-code-runtime", RHAI_RUNTIME_KIND),
        ("code-mode", CODE_MODE_KIND),
    ];
    let mut plugins = vec![entry(
        "system-prompt",
        "ternilo.prompt.system",
        true,
        json!({ "content": "You are Ternilo, a concise software engineering assistant. Use the workspace file and shell tools to complete the requested task." }),
    )];
    plugins.extend(
        disabled
            .into_iter()
            .map(|(id, kind)| entry(id, kind, false, json!({}))),
    );
    Profile { plugins }
}

fn entry(id: &str, kind: &str, enabled: bool, config: serde_json::Value) -> PluginEntry {
    PluginEntry {
        id: id.to_owned(),
        kind: kind.to_owned(),
        enabled,
        config,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn system_catalog_has_stable_order_and_unique_ids() {
        let presets = system_agent_presets();
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.summary.id.as_str())
                .collect::<Vec<_>>(),
            SYSTEM_AGENT_PRESET_IDS,
        );
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.summary.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            SYSTEM_AGENT_PRESET_IDS.len(),
        );
        assert!(
            presets
                .iter()
                .all(|preset| preset.summary.trust == AgentPresetTrust::System)
        );
    }

    #[test]
    fn standard_and_creative_expose_both_while_ptc_exposes_only_code_mode() {
        for (id, mode) in [("standard", "both"), ("ptc", "code"), ("creative", "both")] {
            let preset = system_agent_preset(id).unwrap();
            let code_mode = preset
                .profile
                .plugins
                .iter()
                .find(|entry| entry.id == "code-mode")
                .unwrap();
            assert!(code_mode.enabled);
            assert_eq!(code_mode.config["mode"], mode);
        }
    }

    #[test]
    fn minimal_keeps_core_rows_untouched_and_disables_optional_tooling() {
        let preset = system_agent_preset("minimal").unwrap();
        assert!(
            preset
                .profile
                .plugins
                .iter()
                .any(|entry| entry.id == "code-mode" && !entry.enabled)
        );
        assert!(
            !preset
                .profile
                .plugins
                .iter()
                .any(|entry| matches!(entry.id.as_str(), "file-tools" | "shell-tool"))
        );
    }
}
