use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{DefaultModelSelection, HarnessError, PluginEntry, Profile, RunModelSnapshot};

pub const SUBAGENTS_PLUGIN_KIND: &str = "ternilo.subagents.in_process";

pub type AgentRoleModels = BTreeMap<AgentRole, Option<RunModelSnapshot>>;

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationMode {
    Independent,
    #[default]
    Assisted,
    Parallel,
}

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Investigator,
    Implementer,
    Reviewer,
}

impl AgentRole {
    pub const ALL: [Self; 3] = [Self::Investigator, Self::Implementer, Self::Reviewer];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Investigator => "investigator",
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
        }
    }

    #[must_use]
    pub const fn instructions(self) -> &'static str {
        match self {
            Self::Investigator => {
                "Investigate the assigned question. Read the relevant sources, distinguish observations from assumptions, and return a concise conclusion with file locations or other evidence."
            }
            Self::Implementer => {
                "Implement the assigned scope. Coordinate dependencies with the lead, respect other contributors' changes, and report the changes, verification results, and unresolved issues."
            }
            Self::Reviewer => {
                "Review the assigned result against the requirements. Prioritize reproducible correctness issues and missing verification. Cite evidence for findings and state the limits of the review."
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRoleConfig {
    /// An absent model inherits the lead's accepted model selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub model: Option<DefaultModelSelection>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub read_only: Option<bool>,
}

fn read_only_role() -> AgentRoleConfig {
    AgentRoleConfig {
        read_only: Some(true),
        ..AgentRoleConfig::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoordinationConfig {
    #[serde(default)]
    pub mode: CoordinationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u16>,
    #[serde(default = "read_only_role")]
    pub investigator: AgentRoleConfig,
    #[serde(default)]
    pub implementer: AgentRoleConfig,
    #[serde(default = "read_only_role")]
    pub reviewer: AgentRoleConfig,
    /// Hosts resolve these snapshots from model selections before accepting execution.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(skip)]
    pub resolved_models: BTreeMap<AgentRole, RunModelSnapshot>,
}

impl Default for CoordinationConfig {
    fn default() -> Self {
        Self {
            mode: CoordinationMode::default(),
            max_parallel: None,
            investigator: read_only_role(),
            implementer: AgentRoleConfig::default(),
            reviewer: read_only_role(),
            resolved_models: BTreeMap::new(),
        }
    }
}

impl CoordinationConfig {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self
            .max_parallel
            .is_some_and(|limit| !(1..=32).contains(&limit))
        {
            return Err(HarnessError::invalid(
                "coordination max_parallel must be between 1 and 32",
            ));
        }
        for role in AgentRole::ALL {
            let config = self.role(role);
            if config.instructions.len() > 16_384 {
                return Err(HarnessError::invalid(
                    "agent role instructions exceed 16 KiB",
                ));
            }
            if let Some(model) = &config.model {
                model.validate()?;
                if matches!(model, DefaultModelSelection::OpenAiCompatible { .. }) {
                    return Err(HarnessError::invalid(
                        "choose a saved model connection for an agent role",
                    ));
                }
            }
            if let Some(snapshot) = self.resolved_models.get(&role) {
                snapshot.validate()?;
                if config.model.as_ref().is_some_and(|selection| {
                    !matches!(selection, DefaultModelSelection::ProfileDefault)
                        && snapshot.selection() != *selection
                }) {
                    return Err(HarnessError::policy(
                        "agent role snapshot differs from its selected model",
                    ));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn role(&self, role: AgentRole) -> &AgentRoleConfig {
        match role {
            AgentRole::Investigator => &self.investigator,
            AgentRole::Implementer => &self.implementer,
            AgentRole::Reviewer => &self.reviewer,
        }
    }

    pub const fn role_mut(&mut self, role: AgentRole) -> &mut AgentRoleConfig {
        match role {
            AgentRole::Investigator => &mut self.investigator,
            AgentRole::Implementer => &mut self.implementer,
            AgentRole::Reviewer => &mut self.reviewer,
        }
    }

    #[must_use]
    pub fn role_read_only(&self, role: AgentRole) -> bool {
        self.role(role)
            .read_only
            .unwrap_or(role != AgentRole::Implementer)
    }

    #[must_use]
    pub fn parallel_limit(&self) -> usize {
        match self.mode {
            CoordinationMode::Independent => 0,
            CoordinationMode::Assisted => usize::from(self.max_parallel.unwrap_or(4)),
            CoordinationMode::Parallel => usize::from(self.max_parallel.unwrap_or(8)),
        }
    }

    pub fn from_profile(profile: &Profile) -> Result<Self, HarnessError> {
        let value = profile
            .plugins
            .iter()
            .find(|entry| entry.enabled && entry.kind == SUBAGENTS_PLUGIN_KIND)
            .and_then(|entry| entry.config.get("coordination"));
        let config: Self = value.map_or_else(
            || Ok(Self::default()),
            |value| {
                serde_json::from_value(value.clone()).map_err(|error| {
                    HarnessError::invalid(format!("invalid coordination configuration: {error}"))
                })
            },
        )?;
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub fn child_overrides(&self, profile: &Profile, role: AgentRole) -> Vec<PluginEntry> {
        let mut overrides = Vec::new();
        if let Some(entry) = profile
            .plugins
            .iter()
            .find(|entry| entry.enabled && entry.kind == SUBAGENTS_PLUGIN_KIND)
        {
            let mut child = entry.clone();
            let mut coordination = self.clone();
            coordination.mode = CoordinationMode::Independent;
            child.config["coordination"] =
                serde_json::to_value(coordination).expect("coordination is serializable");
            overrides.push(child);
        }
        let extra = self.role(role).instructions.trim();
        let instructions = format!("{}\n\n{}", role.instructions(), extra);
        overrides.push(PluginEntry {
            id: "agent-role-guidance".to_owned(),
            kind: "ternilo.prompt.section".to_owned(),
            enabled: true,
            config: serde_json::json!({"id":"agent-role", "order":470, "content":instructions.trim()}),
        });
        overrides
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordination_defaults_and_explicit_limits_preserve_role_policy() {
        let assisted: CoordinationConfig = serde_json::from_value(serde_json::json!({
            "investigator": {"instructions":"Find evidence"},
            "reviewer": {"model":{"provider":"profile_default"}}
        }))
        .unwrap();
        assert_eq!(assisted.parallel_limit(), 4);
        assert!(assisted.role_read_only(AgentRole::Investigator));
        assert!(assisted.role_read_only(AgentRole::Reviewer));
        assert!(!assisted.role_read_only(AgentRole::Implementer));
        let mut config = CoordinationConfig {
            mode: CoordinationMode::Parallel,
            ..assisted
        };
        assert_eq!(config.parallel_limit(), 8);
        config.max_parallel = Some(6);
        assert_eq!(config.parallel_limit(), 6);
        config.mode = CoordinationMode::Assisted;
        assert_eq!(config.parallel_limit(), 6);
        config.mode = CoordinationMode::Independent;
        assert_eq!(config.parallel_limit(), 0);
        config.max_parallel = Some(0);
        assert!(config.validate().is_err());
        config.max_parallel = Some(33);
        assert!(config.validate().is_err());
    }
}
