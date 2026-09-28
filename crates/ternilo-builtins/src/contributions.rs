use std::sync::Arc;

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{HarnessPlugin, PluginFactory, PluginManifest, Prompts, RunEnvironment};
use ternilo_protocol::{HarnessError, PromptSection};

use crate::{EmptyConfig, factory, parse_config};

pub const SYSTEM_PROMPT_KIND: &str = "ternilo.prompt.system";
pub const IDENTITY_PROMPT_KIND: &str = "ternilo.prompt.identity";
pub const PROMPT_SECTION_KIND: &str = "ternilo.prompt.section";

component_descriptor! {
    static SYSTEM_PROMPT_DESCRIPTOR: () {
        id: "ternilo/builtin-system-prompt@1",
        requires: [Prompts],
        provides: [],
    }
}

component_descriptor! {
    static PROMPT_SECTION_DESCRIPTOR: () {
        id: "ternilo/builtin-prompt-section@1",
        requires: [Prompts],
        provides: [],
    }
}

component_descriptor! {
    static IDENTITY_PROMPT_DESCRIPTOR: () {
        id: "ternilo/builtin-identity-prompt@1",
        requires: [RunEnvironment, Prompts],
        provides: [],
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SystemPromptConfig {
    content: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PromptSectionConfig {
    id: String,
    #[serde(default = "default_section_order")]
    order: i32,
    content: String,
}

const fn default_section_order() -> i32 {
    500
}

pub fn system_prompt_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: SYSTEM_PROMPT_KIND,
            requires: &["ternilo/prompts@1"],
            provides: &[],
        },
        |value| {
            let config: SystemPromptConfig = parse_config(value)?;
            if config.content.trim().is_empty() {
                return Err(HarnessError::invalid("system prompt must not be empty"));
            }
            Ok(Arc::new(SystemPromptPlugin {
                content: config.content,
            }))
        },
    )
    .with_config_schema::<SystemPromptConfig>()
}

pub fn identity_prompt_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: IDENTITY_PROMPT_KIND,
            requires: &["ternilo/run-environment@1", "ternilo/prompts@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(IdentityPromptPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

pub fn prompt_section_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: PROMPT_SECTION_KIND,
            requires: &["ternilo/prompts@1"],
            provides: &[],
        },
        |value| {
            let config: PromptSectionConfig = parse_config(value)?;
            if config.id.trim().is_empty() || config.content.trim().is_empty() {
                return Err(HarnessError::invalid(
                    "prompt section id and content must not be empty",
                ));
            }
            Ok(Arc::new(PromptSectionPlugin { config }))
        },
    )
    .with_config_schema::<PromptSectionConfig>()
}

struct SystemPromptPlugin {
    content: String,
}

impl HarnessPlugin for SystemPromptPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SYSTEM_PROMPT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("system prompt declares Prompts");
        let prompt_text = self.content.clone();
        Activation::Once(Box::pin(async move {
            let registration = prompts
                .register(PromptSection {
                    id: "system".to_owned(),
                    order: 0,
                    content: prompt_text,
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                prompts
                    .unregister(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct IdentityPromptPlugin;

struct PromptSectionPlugin {
    config: PromptSectionConfig,
}

impl HarnessPlugin for PromptSectionPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PROMPT_SECTION_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("prompt section declares Prompts");
        let section = PromptSection {
            id: self.config.id.clone(),
            order: self.config.order,
            content: self.config.content.clone(),
        };
        Activation::Once(Box::pin(async move {
            let registration = prompts
                .register(section)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                prompts
                    .unregister(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

impl HarnessPlugin for IdentityPromptPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &IDENTITY_PROMPT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("identity prompt declares RunEnvironment");
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("identity prompt declares Prompts");
        Activation::Once(Box::pin(async move {
            let identity = environment.identity().await;
            let workspace = environment.workspace().await;
            let workspace_fact = workspace.map_or_else(
                || "No workspace is attached to this session.".to_owned(),
                |workspace| format!("Working directory: {}.", workspace.path),
            );
            let registration = prompts
                .register(PromptSection {
                    id: "session-identity".to_owned(),
                    order: 100,
                    content: format!(
                        "Session identity: tenant={}, user={}, agent={}, session={}. {}",
                        identity.tenant_id,
                        identity.user_id,
                        identity.agent_id,
                        identity.session_id,
                        workspace_fact
                    ),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                prompts
                    .unregister(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}
