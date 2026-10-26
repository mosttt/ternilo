use std::{collections::BTreeSet, future::Future, pin::Pin, sync::Arc};

use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    ShellClient, ToolEffect, ToolExecutionContext, ToolHandler, ToolRegistration, ToolsClient,
};
use ternilo_protocol::{HarnessError, ShellRequest, ToolOutput, ToolSpec};

#[derive(Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellToolConfig {
    /// Named checks approved in plugin settings, inherited by read-only agents.
    #[serde(default)]
    checks: Vec<Check>,
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Check {
    name: String,
    command: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

const fn default_timeout() -> u64 {
    30_000
}

impl ShellToolConfig {
    pub(super) fn validate(&self) -> Result<(), HarnessError> {
        let mut names = BTreeSet::new();
        if self.checks.len() > 32 {
            return Err(HarnessError::invalid(
                "at most 32 workspace checks may be configured",
            ));
        }
        for check in &self.checks {
            if check.name.trim().is_empty()
                || !names.insert(&check.name)
                || check.command.trim().is_empty()
                || !(100..=300_000).contains(&check.timeout_ms)
            {
                return Err(HarnessError::invalid(
                    "workspace checks require unique nonempty names, nonempty commands and timeout_ms between 100 and 300000",
                ));
            }
        }
        Ok(())
    }
}

pub(super) async fn register(
    tools: &ToolsClient,
    shell: ShellClient,
    config: ShellToolConfig,
) -> Result<Option<u64>, HarnessError> {
    if config.checks.is_empty() {
        return Ok(None);
    }
    let names: Vec<_> = config.checks.iter().map(|check| &check.name).collect();
    tools.register_tool(ToolRegistration {
        spec: ToolSpec {
            name: "run_check".into(),
            description: "Run a named check approved in the shell plugin settings. Available to read-only reviewers. Commands cannot be supplied or changed by this call; the workspace stays read-only and temporary caches are writable. No full-access escalation. Check exit_code, stdout, stderr and timed_out in the result. Tests that require workspace writes need a different approved workflow.".into(),
            input_schema: json!({"type":"object","properties":{"name":{"type":"string","enum":names}},"required":["name"],"additionalProperties":false}),
        },
        effect: ToolEffect::ReadOnly,
        handler: Arc::new(CheckTool { shell, checks: config.checks }),
    }).await.map(Some)
}

struct CheckTool {
    shell: ShellClient,
    checks: Vec<Check>,
}

impl ToolHandler for CheckTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                name: String,
            }
            let args: Arguments = serde_json::from_value(arguments).map_err(|error| {
                HarnessError::invalid(format!("invalid check arguments: {error}"))
            })?;
            let check = self
                .checks
                .iter()
                .find(|check| check.name == args.name)
                .ok_or_else(|| {
                    HarnessError::policy("choose a check configured in the shell plugin settings")
                })?;
            let result = self
                .shell
                .execute(
                    context.run_id,
                    ShellRequest {
                        command: check.command.clone(),
                        timeout_ms: check.timeout_ms,
                        full_access: false,
                        read_only: true,
                        stdin: None,
                        env: std::collections::BTreeMap::new(),
                    },
                )
                .await?;
            Ok(ToolOutput {
                is_error: result.timed_out || result.exit_code != Some(0),
                content: serde_json::to_string(&result)
                    .map_err(|error| HarnessError::execution(error.to_string()))?,
            })
        })
    }
}
