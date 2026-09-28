mod stream;
use std::{future::Future, pin::Pin, sync::Arc};
pub use stream::read_model_gateway_response;

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    Attachments, AttachmentsClient, HarnessPlugin, ModelGateway, ModelGatewayClient, ModelOutput,
    Models, ModelsProvider, PluginFactory, PluginManifest, RunCancellation,
};
use ternilo_protocol::{HarnessError, ModelRequest, ModelResponse, Profile, RunModelSnapshot};

pub const BROKERED_MODEL_KIND: &str = "ternilo.model.host_gateway";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-model-gateway-consumer@1",
        requires: [ModelGateway, Attachments],
        provides: [Models],
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrokeredModelConfig {
    #[schemars(with = "serde_json::Value")]
    pub snapshot: Option<RunModelSnapshot>,
}

impl BrokeredModelConfig {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Some(snapshot) = &self.snapshot {
            snapshot.validate()?;
        }
        Ok(())
    }
}

pub fn profile_model_snapshot(profile: &Profile) -> Result<Option<RunModelSnapshot>, HarnessError> {
    let mut models = profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled && entry.kind == BROKERED_MODEL_KIND);
    let snapshot = models
        .next()
        .map(|entry| {
            let config: BrokeredModelConfig = serde_json::from_value(entry.config.clone())
                .map_err(|error| {
                    HarnessError::composition(format!("invalid host-gateway model: {error}"))
                })?;
            config.validate()?;
            Ok::<_, HarnessError>(config.snapshot)
        })
        .transpose()?
        .flatten();
    if models.next().is_some() {
        return Err(HarnessError::composition(
            "a model profile must have at most one model binding",
        ));
    }
    Ok(snapshot)
}

#[must_use]
pub fn model_gateway_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: BROKERED_MODEL_KIND,
            requires: &["ternilo/model-gateway@3", "ternilo/attachments@1"],
            provides: &["ternilo/models@3"],
        },
        |value| {
            let config: BrokeredModelConfig = serde_json::from_value(value).map_err(|error| {
                HarnessError::composition(format!("invalid host-gateway model config: {error}"))
            })?;
            config.validate()?;
            Ok(Arc::new(BrokeredModelPlugin { config }))
        },
    )
    .with_description("Call an explicitly authorized model through the trusted Server gateway.")
    .with_config_schema::<BrokeredModelConfig>()
}

struct BrokeredModelPlugin {
    config: BrokeredModelConfig,
}

impl HarnessPlugin for BrokeredModelPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let gateway = context
            .context()
            .service::<ModelGateway>()
            .expect("host-gateway model declares ModelGateway");
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("host-gateway model declares Attachments");
        let provider: Arc<dyn ModelsProvider> = Arc::new(BrokeredModel {
            config: self.config.clone(),
            gateway,
            attachments,
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Models>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide host-gateway model: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct BrokeredModel {
    config: BrokeredModelConfig,
    gateway: ModelGatewayClient,
    attachments: AttachmentsClient,
}

impl ModelsProvider for BrokeredModel {
    fn context_window<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async move {
            self.config
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.defaults.context_window)
        })
    }

    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(self.complete_request(request, output, cancellation))
    }
}

impl BrokeredModel {
    async fn complete_request(
        &self,
        mut request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Result<ModelResponse, HarnessError> {
        let snapshot = self.config.snapshot.as_ref().ok_or_else(|| {
            HarnessError::policy("Choose an available model before submitting a task")
        })?;
        for message in &mut request.messages {
            for attachment in &mut message.attachments {
                *attachment = self.attachments.resolve(attachment.clone()).await?;
            }
        }
        self.gateway
            .complete(snapshot.binding.clone(), request, output, cancellation)
            .await
    }
}
