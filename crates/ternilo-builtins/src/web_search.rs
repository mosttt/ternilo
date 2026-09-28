use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, RunEnvironmentClient,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};

use crate::{factory as make_factory, parse_config, read_provider_response};

pub const KIND: &str = "ternilo.web.search.searxng";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-searxng-search@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchConfig {
    base_url: String,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_max_results")]
    max_results: usize,
}

const fn default_timeout_ms() -> u64 {
    30_000
}

const fn default_max_results() -> usize {
    10
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/run-environment@1"],
            provides: &[],
        },
        |value| {
            let config: SearchConfig = parse_config(value)?;
            if !(config.base_url.starts_with("http://") || config.base_url.starts_with("https://"))
                || config.timeout_ms == 0
                || config.max_results == 0
                || config.max_results > 50
            {
                return Err(HarnessError::composition(
                    "SearXNG search requires an HTTP(S) base_url, positive timeout, and max_results between 1 and 50",
                ));
            }
            Ok(Arc::new(SearchPlugin { config }))
        },
    )
    .with_config_schema::<SearchConfig>()
}

struct SearchPlugin {
    config: SearchConfig,
}

impl HarnessPlugin for SearchPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("web search declares Tools");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("web search declares RunEnvironment");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .user_agent("Ternilo/0.1")
            .build();
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let client = client.map_err(|error| {
                linorun_core::ActivationFailure::user(format!("build search client: {error}"))
            })?;
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "web_search".to_owned(),
                        description: "Search the web through the user-configured SearXNG instance and return bounded provider-neutral results.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "limit": { "type": "integer", "minimum": 1, "maximum": 50 },
                                "language": { "type": "string" }
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: Arc::new(SearchTool {
                        config,
                        client,
                        environment,
                    }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct SearchTool {
    config: SearchConfig,
    client: reqwest::Client,
    environment: RunEnvironmentClient,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    limit: Option<usize>,
    language: Option<String>,
}

impl ToolHandler for SearchTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: SearchArguments =
                serde_json::from_value(arguments).map_err(|error| {
                    HarnessError::invalid(format!("invalid web_search arguments: {error}"))
                })?;
            let query = arguments.query.trim();
            if query.is_empty() {
                return Err(HarnessError::invalid("web_search query must not be empty"));
            }
            let limit = arguments.limit.unwrap_or(self.config.max_results);
            if limit == 0 || limit > self.config.max_results {
                return Err(HarnessError::invalid(format!(
                    "web_search limit must be between 1 and {}",
                    self.config.max_results
                )));
            }
            let endpoint = format!("{}/search", self.config.base_url.trim_end_matches('/'));
            let mut request = self.client.get(endpoint).query(&[
                ("q", query),
                ("format", "json"),
                ("language", arguments.language.as_deref().unwrap_or("all")),
            ]);
            if let Some(variable) = self.config.api_key_env.as_deref() {
                let key = self
                    .environment
                    .resolve_secret(variable.to_owned())
                    .await?
                    .ok_or_else(|| {
                        HarnessError::execution(format!(
                            "search credential {variable:?} is not configured"
                        ))
                    })?;
                request = request.bearer_auth(key);
            }
            let response = request.send().await.map_err(|error| {
                HarnessError::execution(format!("SearXNG request failed: {error}"))
            })?;
            let status = response.status();
            let bytes = read_provider_response(response, "SearXNG").await?;
            if !status.is_success() {
                return Err(HarnessError::execution(format!(
                    "SearXNG returned {status}: {}",
                    String::from_utf8_lossy(&bytes)
                        .chars()
                        .take(2_000)
                        .collect::<String>()
                )));
            }
            let response: SearxResponse = serde_json::from_slice(&bytes).map_err(|error| {
                HarnessError::execution(format!("parse SearXNG response: {error}"))
            })?;
            let results = response
                .results
                .into_iter()
                .take(limit)
                .map(|result| SearchResult {
                    title: result.title,
                    url: result.url,
                    snippet: result.content,
                    engine: result.engine,
                })
                .collect::<Vec<_>>();
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&results).map_err(|error| {
                    HarnessError::execution(format!("serialize web search results: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

#[derive(Deserialize)]
struct SearxResponse {
    #[serde(default)]
    results: Vec<SearxResult>,
}

#[derive(Deserialize)]
struct SearxResult {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    engine: String,
}

#[derive(Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    engine: String,
}
