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

use crate::{factory as make_factory, parse_config};

mod providers;
#[cfg(test)]
mod tests;

use providers::SearchProvider;

pub const KIND: &str = "ternilo.web.search.searxng";
pub const BRAVE_KIND: &str = "ternilo.web.search.brave";
pub const TAVILY_KIND: &str = "ternilo.web.search.tavily";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-searxng-search@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

component_descriptor! {
    static BRAVE_DESCRIPTOR: () {
        id: "ternilo/builtin-brave-search@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

component_descriptor! {
    static TAVILY_DESCRIPTOR: () {
        id: "ternilo/builtin-tavily-search@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchConfig {
    /// Service base URL. Brave and Tavily default to their official endpoints.
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    /// Credential reference resolved on the execution host, never a literal API key.
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
    provider_factory(SearchProvider::Searxng)
}

pub fn brave_factory() -> PluginFactory {
    provider_factory(SearchProvider::Brave)
}

pub fn tavily_factory() -> PluginFactory {
    provider_factory(SearchProvider::Tavily)
}

fn provider_factory(provider: SearchProvider) -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: provider.kind(),
            requires: &["ternilo/tools@1", "ternilo/run-environment@1"],
            provides: &[],
        },
        match provider {
            SearchProvider::Searxng => |value| build_plugin(value, SearchProvider::Searxng),
            SearchProvider::Brave => |value| build_plugin(value, SearchProvider::Brave),
            SearchProvider::Tavily => |value| build_plugin(value, SearchProvider::Tavily),
        },
    )
    .with_config_schema::<SearchConfig>()
}

fn build_plugin(
    value: Value,
    provider: SearchProvider,
) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    let config: SearchConfig = parse_config(value)?;
    config.validate(provider)?;
    Ok(Arc::new(SearchPlugin { config, provider }))
}

impl SearchConfig {
    fn validate(&self, provider: SearchProvider) -> Result<(), HarnessError> {
        let url = reqwest::Url::parse(self.base_url(provider)?)
            .map_err(|_| HarnessError::composition("search base_url must be an HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(HarnessError::composition(
                "search base_url requires HTTP(S) without embedded credentials, query or fragment",
            ));
        }
        if self.timeout_ms == 0 || !(1..=provider.max_results()).contains(&self.max_results) {
            return Err(HarnessError::composition(format!(
                "{} requires a positive timeout and max_results between 1 and {}",
                provider.name(),
                provider.max_results()
            )));
        }
        if self
            .api_key_env
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
            || (provider != SearchProvider::Searxng && self.api_key_env.is_none())
        {
            return Err(HarnessError::composition(
                "search api_key_env must name a configured credential",
            ));
        }
        Ok(())
    }

    fn base_url(&self, provider: SearchProvider) -> Result<&str, HarnessError> {
        self.base_url
            .as_deref()
            .or(provider.default_base_url())
            .ok_or_else(|| HarnessError::composition("SearXNG requires base_url"))
    }
}

struct SearchPlugin {
    config: SearchConfig,
    provider: SearchProvider,
}

impl HarnessPlugin for SearchPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        match self.provider {
            SearchProvider::Searxng => &DESCRIPTOR,
            SearchProvider::Brave => &BRAVE_DESCRIPTOR,
            SearchProvider::Tavily => &TAVILY_DESCRIPTOR,
        }
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
            .user_agent(concat!("Ternilo/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .build();
        let config = self.config.clone();
        let provider = self.provider;
        Activation::Once(Box::pin(async move {
            let client = client.map_err(|error| {
                linorun_core::ActivationFailure::user(format!("build search client: {error}"))
            })?;
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "web_search".to_owned(),
                        description: format!("Search the web through the configured {} service and return bounded results with source URLs.", provider.name()),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "query": { "type": "string" },
                                "limit": { "type": "integer", "minimum": 1, "maximum": config.max_results },
                                "language": { "type": "string" }
                            },
                            "required": ["query"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: Arc::new(SearchTool {
                        config,
                        provider,
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
    provider: SearchProvider,
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
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            context.cancellation.check()?;
            tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(HarnessError::cancelled("web search cancelled")),
                result = self.search(arguments) => result,
            }
        })
    }
}

impl SearchTool {
    async fn search(&self, arguments: Value) -> Result<ToolOutput, HarnessError> {
        let arguments: SearchArguments = serde_json::from_value(arguments).map_err(|error| {
            HarnessError::invalid(format!("invalid web_search arguments: {error}"))
        })?;
        let query = arguments.query.trim();
        if query.is_empty() {
            return Err(HarnessError::invalid("web_search query must not be empty"));
        }
        let limit = arguments.limit.unwrap_or(self.config.max_results);
        if !(1..=self.config.max_results).contains(&limit) {
            return Err(HarnessError::invalid(format!(
                "web_search limit must be between 1 and {}",
                self.config.max_results
            )));
        }
        let key = if let Some(variable) = self.config.api_key_env.as_deref() {
            Some(
                self.environment
                    .resolve_secret(variable.to_owned())
                    .await?
                    .filter(|key| !key.trim().is_empty())
                    .ok_or_else(|| {
                        HarnessError::execution(format!(
                            "search credential {variable:?} is not configured"
                        ))
                    })?,
            )
        } else {
            None
        };
        let request = self.provider.request(
            &self.client,
            self.config.base_url(self.provider)?,
            key.as_deref(),
            query,
            limit,
            arguments.language.as_deref(),
        );
        let results = self.provider.search(request, limit).await?;
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&results).map_err(|error| {
                HarnessError::execution(format!("serialize web search results: {error}"))
            })?,
            is_error: false,
        })
    }
}

#[derive(Debug, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    engine: String,
}
