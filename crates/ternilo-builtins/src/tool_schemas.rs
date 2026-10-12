use std::{collections::BTreeSet, fmt::Write as _, future::Future, pin::Pin, sync::Weak};

use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{ToolEffect, ToolExecutionContext, ToolHandler, ToolRegistration};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};

use super::ToolRegistry;

pub(super) const SEARCH_TOOL: &str = "search_tools";

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SchemaLoadingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_max_loaded")]
    pub max_loaded: usize,
}

const fn default_max_loaded() -> usize {
    32
}

impl Default for SchemaLoadingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_loaded: default_max_loaded(),
        }
    }
}

impl SchemaLoadingConfig {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if !(1..=128).contains(&self.max_loaded) {
            return Err(HarnessError::composition(
                "tool schema max_loaded must be between 1 and 128",
            ));
        }
        Ok(())
    }
}

pub(super) struct DiscoverySummary {
    pub deferred: BTreeSet<String>,
    pub prompt: String,
}

impl ToolRegistry {
    pub(super) fn prepare_schema_tools(
        &self,
        tools: &mut Vec<ToolSpec>,
    ) -> Result<Option<DiscoverySummary>, HarnessError> {
        if !self.schema_loading.enabled {
            return Ok(None);
        }
        let loaded = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?
            .loaded_schemas
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut deferred = BTreeSet::new();
        let mut prompt = String::from(
            "# Tool discovery\n\nCommon tools and recently requested schemas are available directly. Use search_tools with a query or exact names to load other tool schemas before calling them. Discovery does not execute a tool or grant permission. A schema can be evicted when the configured limit is reached; search again when needed.\n\nAvailable tool directory:\n",
        );
        for tool in tools.iter() {
            let description = tool
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let description = description.chars().take(120).collect::<String>();
            let _ = writeln!(prompt, "- {}: {description}", tool.name);
            if !always_visible(&tool.name) && !loaded.contains(&tool.name) {
                deferred.insert(tool.name.clone());
            }
        }
        tools.retain(|tool| !deferred.contains(&tool.name));
        Ok(Some(DiscoverySummary { deferred, prompt }))
    }

    fn load_schemas(
        &self,
        tools: &[ToolSpec],
        arguments: &SearchArguments,
    ) -> Result<ToolOutput, HarnessError> {
        if !(1..=16).contains(&arguments.limit)
            || arguments.names.len() > 16
            || arguments.query.len() > 1024
        {
            return Err(HarnessError::invalid(
                "tool discovery accepts up to 16 names, a query of at most 1024 bytes, and a limit between 1 and 16",
            ));
        }
        let terms = arguments
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        if terms.is_empty() && arguments.names.is_empty() {
            return Err(HarnessError::invalid(
                "provide a tool query or exact tool names",
            ));
        }
        for name in &arguments.names {
            if !tools.iter().any(|tool| tool.name == *name) {
                return Err(HarnessError::invalid(format!(
                    "tool {name:?} is not available in this session"
                )));
            }
        }
        let mut matches = tools
            .iter()
            .filter_map(|tool| {
                let score = if arguments.names.is_empty() {
                    let name = tool.name.to_lowercase();
                    let description = tool.description.to_lowercase();
                    terms
                        .iter()
                        .map(|term| {
                            if name == *term {
                                10
                            } else if name.contains(term) {
                                3
                            } else {
                                usize::from(description.contains(term))
                            }
                        })
                        .sum::<usize>()
                } else {
                    usize::from(arguments.names.contains(&tool.name))
                };
                (score > 0).then_some((score, tool))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
        });
        let limit = arguments.limit.min(self.schema_loading.max_loaded);
        let has_more = matches.len() > limit;
        let selected = matches
            .into_iter()
            .take(limit)
            .map(|(_, tool)| tool)
            .collect::<Vec<_>>();
        let mut state = self
            .state
            .lock()
            .map_err(|_| HarnessError::execution("tool registry lock poisoned"))?;
        for tool in &selected {
            if !always_visible(&tool.name) {
                state.loaded_schemas.retain(|name| name != &tool.name);
                state.loaded_schemas.push_back(tool.name.clone());
            }
        }
        while state.loaded_schemas.len() > self.schema_loading.max_loaded {
            state.loaded_schemas.pop_front();
        }
        drop(state);
        Ok(ToolOutput {
            content: serde_json::to_string(&json!({"tools":selected,"has_more":has_more}))
                .map_err(|error| HarnessError::execution(error.to_string()))?,
            is_error: false,
        })
    }
}

fn always_visible(name: &str) -> bool {
    matches!(
        name,
        "search_tools"
            | "run_code"
            | "read_file"
            | "write_file"
            | "replace_in_file"
            | "glob_files"
            | "search_files"
            | "shell"
            | "ask_user"
            | "spawn_agent"
            | "wait_agent"
            | "update_plan"
            | "todo_write"
            | "update_goal"
            | "exit_plan_mode"
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    #[serde(default)]
    query: String,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    5
}

struct SchemaSearch(Weak<ToolRegistry>);

impl ToolHandler for SchemaSearch {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let registry = self
                .0
                .upgrade()
                .ok_or_else(|| HarnessError::cancelled("tool registry is closed"))?;
            let arguments: SearchArguments =
                serde_json::from_value(arguments).map_err(|error| {
                    HarnessError::invalid(format!("invalid tool discovery arguments: {error}"))
                })?;
            let tools = registry.tool_specs().await;
            registry.load_schemas(&tools, &arguments)
        })
    }
}

pub(super) fn registration(registry: Weak<ToolRegistry>) -> ToolRegistration {
    ToolRegistration {
        spec: ToolSpec {
            name: SEARCH_TOOL.to_owned(),
            description: "Find available tools and load their current input schemas. Use a descriptive query or exact tool names. Loading a schema does not execute the tool or change its permissions.".to_owned(),
            input_schema: json!({"type":"object","properties":{
                "query":{"type":"string","description":"Words describing the needed capability."},
                "names":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":16},
                "limit":{"type":"integer","minimum":1,"maximum":16,"default":5}
            },"anyOf":[{"required":["query"]},{"required":["names"]}],"additionalProperties":false}),
        },
        effect: ToolEffect::ReadOnly,
        handler: std::sync::Arc::new(SchemaSearch(registry)),
    }
}
