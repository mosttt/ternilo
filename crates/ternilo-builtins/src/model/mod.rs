use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    Attachments, AttachmentsClient, HarnessPlugin, ModelOutput, Models, ModelsProvider,
    PluginFactory, PluginManifest, RunCancellation, RunEnvironment, RunEnvironmentClient,
};
use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse, ProviderProtocol};

use crate::{factory as make_factory, parse_config};

mod attempt;
mod completion;
mod protocols;
mod transport;
use completion::{StreamCompletion, StreamEvent};
#[cfg(test)]
mod attempt_tests;
#[cfg(test)]
mod deepseek_tests;
#[cfg(test)]
mod native_tests;

pub use attempt::{ModelAttemptObserver, ModelAttemptReport};

pub const KIND: &str = "ternilo.model.openai_compatible";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-openai-compatible-model@1",
        requires: [RunEnvironment, Attachments],
        provides: [Models],
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum ProtocolConfig {
    #[default]
    #[serde(rename = "openai-chat-completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "deepseek-responses")]
    DeepSeekResponses,
    #[serde(rename = "google-gemini")]
    GoogleGemini,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

impl From<ProtocolConfig> for ProviderProtocol {
    fn from(value: ProtocolConfig) -> Self {
        match value {
            ProtocolConfig::OpenAiChatCompletions => Self::OpenAiChatCompletions,
            ProtocolConfig::OpenAiResponses => Self::OpenAiResponses,
            ProtocolConfig::DeepSeekResponses => Self::DeepSeekResponses,
            ProtocolConfig::GoogleGemini => Self::GoogleGemini,
            ProtocolConfig::AnthropicMessages => Self::AnthropicMessages,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProviderModelConfig {
    provider: String,
    base_url: String,
    #[serde(default)]
    protocol: ProtocolConfig,
    model: String,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    context_window: Option<u64>,
    /// Total request limit in milliseconds, including reasoning and output. Zero disables it.
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default = "default_retry_base_delay_ms")]
    retry_base_delay_ms: u64,
}

const fn default_timeout_ms() -> u64 {
    600_000
}

const fn default_max_attempts() -> u32 {
    3
}

const fn default_retry_base_delay_ms() -> u64 {
    250
}

#[derive(Clone, Debug)]
pub struct ProviderModelRoute {
    pub provider: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    pub model: String,
    pub context_window: Option<u64>,
    pub timeout_ms: u64,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub max_attempts: u32,
    pub retry_base_delay_ms: u64,
}

impl ProviderModelRoute {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.provider.trim().is_empty()
            || !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://"))
            || self.model.trim().is_empty()
            || !(1..=8).contains(&self.max_attempts)
            || self.retry_base_delay_ms == 0
            || self
                .reasoning_effort
                .as_ref()
                .is_some_and(|value| value.trim().is_empty() || value.chars().any(char::is_control))
        {
            return Err(HarnessError::composition(
                "OpenAI-compatible model requires an HTTP(S) base_url, model, non-negative timeout, positive retry delay, and max_attempts between 1 and 8",
            ));
        }
        crate::provider_model_endpoint(&self.base_url, self.protocol, &self.model, true)?;
        crate::apply_native_reasoning(
            &mut serde_json::json!({}),
            self.protocol,
            self.reasoning_effort.as_deref(),
            u64::from(self.max_tokens.unwrap_or(4096)),
        )?;
        Ok(())
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/run-environment@1", "ternilo/attachments@1"],
            provides: &["ternilo/models@3"],
        },
        |value| {
            let config: ProviderModelConfig = parse_config(value)?;
            let route = ProviderModelRoute::from(&config);
            route.validate()?;
            let mut client = reqwest::Client::builder();
            if config.timeout_ms > 0 {
                client = client.timeout(std::time::Duration::from_millis(config.timeout_ms));
            }
            let client = client.build().map_err(|error| {
                HarnessError::composition(format!("build model HTTP client: {error}"))
            })?;
            Ok(Arc::new(ProviderModelPlugin { config, client }))
        },
    )
    .with_config_schema::<ProviderModelConfig>()
}

struct ProviderModelPlugin {
    config: ProviderModelConfig,
    client: reqwest::Client,
}

impl HarnessPlugin for ProviderModelPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("OpenAI-compatible model declares RunEnvironment");
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("OpenAI-compatible model declares Attachments");
        let provider: Arc<dyn ModelsProvider> = Arc::new(ProviderModel {
            provider: self.config.provider.clone(),
            endpoint: crate::provider_model_endpoint(
                &self.config.base_url,
                self.config.protocol.into(),
                &self.config.model,
                true,
            )
            .expect("validated provider URL"),
            protocol: self.config.protocol.into(),
            model: self.config.model.clone(),
            context_window: self.config.context_window,
            api_key_env: self.config.api_key_env.clone(),
            api_key_override: None,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            reasoning_effort: self.config.reasoning_effort.clone(),
            max_attempts: self.config.max_attempts,
            retry_base_delay_ms: self.config.retry_base_delay_ms,
            client: self.client.clone(),
            environment: Some(environment),
            attachments: Some(attachments),
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Models>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide OpenAI-compatible model: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct ProviderModel {
    provider: String,
    endpoint: String,
    protocol: ProviderProtocol,
    model: String,
    context_window: Option<u64>,
    api_key_env: Option<String>,
    api_key_override: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    reasoning_effort: Option<String>,
    max_attempts: u32,
    retry_base_delay_ms: u64,
    client: reqwest::Client,
    environment: Option<RunEnvironmentClient>,
    attachments: Option<AttachmentsClient>,
}

impl ModelsProvider for ProviderModel {
    fn context_window<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async move { self.context_window })
    }

    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(self.complete_request(request, output, cancellation, None))
    }
}

impl From<&ProviderModelConfig> for ProviderModelRoute {
    fn from(config: &ProviderModelConfig) -> Self {
        Self {
            provider: config.provider.clone(),
            base_url: config.base_url.clone(),
            protocol: config.protocol.into(),
            model: config.model.clone(),
            context_window: config.context_window,
            timeout_ms: config.timeout_ms,
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            reasoning_effort: config.reasoning_effort.clone(),
            max_attempts: config.max_attempts,
            retry_base_delay_ms: config.retry_base_delay_ms,
        }
    }
}

pub async fn complete_provider_model(
    route: ProviderModelRoute,
    api_key: Option<String>,
    request: ModelRequest,
    output: Arc<dyn ModelOutput>,
    cancellation: RunCancellation,
    observer: Option<Arc<dyn ModelAttemptObserver>>,
) -> Result<ModelResponse, HarnessError> {
    route.validate()?;
    let mut client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if route.timeout_ms > 0 {
        client = client.timeout(std::time::Duration::from_millis(route.timeout_ms));
    }
    let client = client
        .build()
        .map_err(|error| HarnessError::execution(format!("build model HTTP client: {error}")))?;
    ProviderModel {
        provider: route.provider,
        endpoint: crate::provider_model_endpoint(
            &route.base_url,
            route.protocol,
            &route.model,
            true,
        )?,
        protocol: route.protocol,
        model: route.model,
        context_window: route.context_window,
        api_key_env: None,
        api_key_override: api_key,
        max_tokens: route.max_tokens,
        temperature: route.temperature,
        reasoning_effort: route.reasoning_effort,
        max_attempts: route.max_attempts,
        retry_base_delay_ms: route.retry_base_delay_ms,
        client,
        environment: None,
        attachments: None,
    }
    .complete_request(request, output, cancellation, observer)
    .await
}

#[cfg(test)]
mod tests;
