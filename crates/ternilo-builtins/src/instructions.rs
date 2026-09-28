use std::sync::Arc;

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use ternilo_kernel::{HarnessPlugin, PluginFactory, PluginManifest, Prompts, WorkspaceFiles};
use ternilo_protocol::{FileReadRequest, PromptSection};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.prompt.workspace_instructions";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-workspace-instructions@1",
        requires: [Prompts, WorkspaceFiles],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/prompts@1", "ternilo/workspace-files@2"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(WorkspaceInstructionsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct WorkspaceInstructionsPlugin;

impl HarnessPlugin for WorkspaceInstructionsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("workspace instructions declares Prompts");
        let files = context
            .context()
            .service::<WorkspaceFiles>()
            .expect("workspace instructions declares WorkspaceFiles");
        Activation::Once(Box::pin(async move {
            let mut sections = Vec::new();
            for path in ["AGENTS.md", "CLAUDE.md"] {
                if let Ok(content) = files
                    .read_text(FileReadRequest {
                        path: path.to_owned(),
                        start_line: None,
                        line_count: Some(2_000),
                    })
                    .await
                {
                    sections.push(format!("# {path}\n{}", content.content));
                }
            }
            if sections.is_empty() {
                return Ok(None);
            }
            let registration = prompts
                .register(PromptSection {
                    id: "workspace-instructions".to_owned(),
                    order: 200,
                    content: format!(
                        "Follow these workspace-owned instructions:\n\n{}",
                        sections.join("\n\n")
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
