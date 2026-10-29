use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment, RunEnvironmentClient,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};

use crate::{factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.web.fetch.deepseek";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-deepseek-fetch@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FetchConfig {
    #[serde(default)]
    provider_web_access: Option<ternilo_protocol::ProviderWebAccess>,
    #[serde(default = "default_base_url")]
    base_url: String,
    /// Credential name on the execution host, never the API key itself.
    #[serde(default)]
    api_key_env: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_max_bytes")]
    max_bytes: usize,
}

fn default_base_url() -> String {
    "https://api.deepseek.com".to_owned()
}

const fn default_timeout() -> u64 {
    30_000
}
const fn default_max_bytes() -> usize {
    2 * 1024 * 1024
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest { kind: KIND, requires: &["ternilo/tools@1", "ternilo/run-environment@1"], provides: &[] },
        |value| {
            let config: FetchConfig = parse_config(value)?;
            if let Some(web) = &config.provider_web_access
                && (web.fetch.mode != ternilo_protocol::WebFetchMode::Deepseek
                    || web.fetch.base_url.as_deref().unwrap_or("https://api.deepseek.com") != config.base_url
                    || web.fetch.timeout_ms != config.timeout_ms
                    || web.fetch.max_bytes != config.max_bytes as u64
                    || !config.api_key_env.is_empty())
            {
                return Err(HarnessError::policy("fetch plugin must use its selected Provider web configuration"));
            }
            let url = reqwest::Url::parse(&config.base_url)
                .map_err(|_| HarnessError::composition("DeepSeek fetch base_url must be an HTTP(S) URL"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
                || !url.username().is_empty() || url.password().is_some()
                || url.query().is_some() || url.fragment().is_some() {
                return Err(HarnessError::composition("DeepSeek fetch base_url requires HTTP(S) without embedded credentials, query or fragment"));
            }
            if (config.api_key_env.trim().is_empty() && config.provider_web_access.is_none()) || config.timeout_ms == 0 || config.max_bytes == 0 {
                return Err(HarnessError::composition("DeepSeek fetch requires a credential name, positive timeout_ms and max_bytes"));
            }
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(config.timeout_ms))
                .user_agent(concat!("Ternilo/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::none())
                .build().map_err(|_| HarnessError::composition("could not build DeepSeek fetch client"))?;
            Ok(Arc::new(FetchPlugin { config, client }))
        },
    ).with_config_schema::<FetchConfig>()
}

struct FetchPlugin {
    config: FetchConfig,
    client: reqwest::Client,
}

impl HarnessPlugin for FetchPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("DeepSeek fetch declares Tools");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("DeepSeek fetch declares RunEnvironment");
        let handler = Arc::new(FetchTool {
            config: self.config.clone(),
            client: self.client.clone(),
            environment,
        });
        Activation::Once(Box::pin(async move {
            let registration = tools.register_tool(ToolRegistration {
                spec: ToolSpec {
                    name: "web_fetch".to_owned(),
                    description: "Fetch a public HTTP(S) page through DeepSeek and return its title and Markdown content. The target URL is sent to DeepSeek.".to_owned(),
                    input_schema: json!({"type":"object","properties":{"url":{"type":"string","maxLength":8192}},"required":["url"],"additionalProperties":false}),
                },
                effect: ternilo_kernel::ToolEffect::ReadOnly,
                handler,
            }).await.map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct FetchTool {
    config: FetchConfig,
    client: reqwest::Client,
    environment: RunEnvironmentClient,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchArguments {
    url: String,
}

#[derive(Deserialize)]
struct FetchResponse {
    title: String,
    content: String,
}

impl ToolHandler for FetchTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            context.cancellation.check()?;
            tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(HarnessError::cancelled("DeepSeek fetch cancelled")),
                result = self.fetch(context.run_id, arguments) => result,
            }
        })
    }
}

impl FetchTool {
    async fn fetch(
        &self,
        run_id: ternilo_protocol::RunId,
        arguments: Value,
    ) -> Result<ToolOutput, HarnessError> {
        let arguments: FetchArguments = serde_json::from_value(arguments)
            .map_err(|_| HarnessError::invalid("web_fetch requires a URL"))?;
        validate_target(&arguments.url)?;
        let key = if let Some(expected) = &self.config.provider_web_access {
            self.environment
                .resolve_provider_web_credential(
                    run_id,
                    ternilo_protocol::WebCapability::Fetch,
                    expected.clone(),
                )
                .await?
        } else {
            self.environment
                .resolve_secret(self.config.api_key_env.clone())
                .await?
        }
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HarnessError::execution("DeepSeek fetch credential is not configured"))?;
        let request = self
            .client
            .post(format!(
                "{}/v1/tools/fetch",
                self.config.base_url.trim_end_matches('/')
            ))
            .bearer_auth(key)
            .json(&json!({"url":arguments.url}));
        let bytes = read_response(request, "fetch", self.config.max_bytes).await?;
        let page: FetchResponse = serde_json::from_slice(&bytes)
            .map_err(|_| HarnessError::execution("DeepSeek returned an invalid fetch response"))?;
        Ok(ToolOutput {
            content: json!({"url":arguments.url, "title":page.title, "content":page.content})
                .to_string(),
            is_error: false,
        })
    }
}

fn validate_target(value: &str) -> Result<(), HarnessError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| HarnessError::invalid("DeepSeek fetch requires an absolute HTTP(S) URL"))?;
    if value.chars().count() > 8192
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(HarnessError::invalid(
            "DeepSeek fetch requires an HTTP(S) URL without embedded credentials, at most 8192 characters",
        ));
    }
    Ok(())
}

pub(super) async fn read_response(
    request: reqwest::RequestBuilder,
    operation: &str,
    limit: usize,
) -> Result<Vec<u8>, HarnessError> {
    let mut response = request.send().await.map_err(|error| {
        let reason = if error.is_timeout() {
            "timed out"
        } else if error.is_connect() {
            "could not connect"
        } else {
            "request failed"
        };
        HarnessError::execution(format!("DeepSeek {operation} {reason}"))
    })?;
    if !response.status().is_success() {
        return Err(http_error(response.status().as_u16()));
    }
    let too_large = || {
        HarnessError::execution(format!(
            "DeepSeek {operation} response exceeds {limit} bytes"
        ))
    };
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        HarnessError::execution(format!("DeepSeek {operation} response could not be read"))
    })? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn http_error(status: u16) -> HarnessError {
    let reason = match status {
        400 | 413 => "check the search query, result count or page URL before retrying",
        401 => {
            "check the API key stored under this plugin's credential name on the execution computer"
        }
        402 => {
            "top up the DeepSeek account that owns this API key at https://platform.deepseek.com/top_up; increasing Ternilo's token budget will not help"
        }
        403 => {
            "the page was blocked by the fetch service's safety policy; use a different public URL"
        }
        404 => "no readable page content was found; check the URL or use a different page",
        408 | 504 => "the request timed out; try again later",
        429 => {
            "the account or target domain request limit was reached; reduce request frequency and try again later"
        }
        500..=599 => "the search service is unavailable; try again later",
        _ => "the search service rejected the request",
    };
    HarnessError::execution(format!(
        "DeepSeek web tools error (HTTP {status}): {reason}"
    ))
}

#[cfg(test)]
mod tests;
