use std::sync::Arc;

use ternilo_builtins::{ProviderModelRoute, complete_provider_model};
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_protocol::{
    ComputerModelRequest, HarnessError, ModelResponse, ProviderModelCatalog as _,
};

use super::LocalApplication;

impl LocalApplication {
    /// Called only by the authenticated Server connection after authorizing the
    /// remote session. This does not create a session or execute tools locally.
    pub async fn complete_forwarded_model(
        &self,
        request: ComputerModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Result<ModelResponse, HarnessError> {
        request.validate()?;
        cancellation.check()?;
        self.stopping.check()?;
        if crate::model_connections::is_connection_provider(&request.provider_id) {
            return Err(HarnessError::policy(
                "a connected Server model cannot be forwarded through another computer",
            ));
        }
        let provider = self
            .providers
            .get(&request.provider_id)
            .await
            .ok_or_else(|| {
                HarnessError::unavailable("the source computer Provider is no longer available")
            })?;
        if provider.protocol != request.protocol {
            return Err(HarnessError::policy(
                "the source computer model protocol changed after selection",
            ));
        }
        let current = provider.resolved_model(&request.model)?;
        let reasoning_effort = request
            .resolved_model()
            .reasoning_value(request.reasoning_effort)?
            .map(str::to_owned);
        if let Some(effort) = &reasoning_effort
            && !current.reasoning.as_ref().is_some_and(|reasoning| {
                reasoning
                    .efforts
                    .values()
                    .any(|value| value.as_ref() == Some(effort))
            })
        {
            return Err(HarnessError::policy(
                "the selected reasoning effort is no longer allowed by the source computer",
            ));
        }
        let max_tokens = u32::try_from(
            request
                .defaults
                .max_output_tokens
                .min(current.max_output_tokens),
        )
        .map_err(|_| HarnessError::policy("computer model output limit exceeds u32"))?;
        let api_key = match provider.api_key_ref.as_deref() {
            Some(reference) => Some(
                self.credentials
                    .resolve_value(reference)
                    .await?
                    .ok_or_else(|| {
                        HarnessError::unavailable(
                            "the model credential is missing on the source computer",
                        )
                    })?,
            ),
            None => None,
        };
        let route = ProviderModelRoute {
            provider: provider.id,
            base_url: provider.base_url,
            protocol: provider.protocol,
            model: request.model,
            context_window: Some(request.defaults.context_window.min(current.context_window)),
            timeout_ms: provider.timeout_ms,
            max_tokens: Some(max_tokens),
            temperature: None,
            reasoning_effort,
            max_attempts: provider.max_attempts,
            retry_base_delay_ms: provider.retry_base_delay_ms,
        };
        complete_provider_model(route, api_key, request.request, output, cancellation, None).await
    }
}
