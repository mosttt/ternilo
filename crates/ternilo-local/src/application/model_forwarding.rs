use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use ternilo_builtins::{
    ModelAttemptObserver, ModelAttemptReport, ProviderModelRoute, complete_provider_model,
};
use ternilo_kernel::{ModelOutput, RunCancellation};
use ternilo_protocol::{
    ComputerModelRequest, HarnessError, ModelResponse, ProviderModelCatalog as _, ProviderProfile,
};

use super::LocalApplication;

impl LocalApplication {
    /// Called only by the authenticated Server connection after authorizing the
    /// remote session. This does not create a session or execute tools locally.
    #[expect(
        clippy::too_many_lines,
        reason = "Keep source resolution, revocation and final attempt reporting in one invocation lifetime."
    )]
    pub async fn complete_forwarded_model(
        &self,
        request: ComputerModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
        observer: Arc<dyn ternilo_builtins::ModelAttemptObserver>,
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
        let policy = Arc::new(SourcePolicy {
            providers: Arc::clone(&self.providers),
            credentials: Arc::clone(&self.credentials),
            provider: provider.clone(),
            model: request.model.clone(),
            api_key: api_key.clone(),
            observer,
        });
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
            max_attempts: provider.max_attempts.min(request.max_attempts),
            retry_base_delay_ms: provider.retry_base_delay_ms,
        };
        let completion = complete_provider_model(
            route,
            api_key,
            request.request,
            output,
            cancellation.clone(),
            Some(policy.clone()),
        );
        tokio::pin!(completion);
        let period = Duration::from_secs(2);
        let mut check = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let invalid = tokio::select! {
                result = &mut completion => return result,
                () = self.stopping.cancelled() => Some(HarnessError::cancelled("source computer is stopping")),
                _ = check.tick() => policy.check().await.err(),
            };
            if let Some(error) = invalid {
                cancellation.cancel();
                let _ = completion.await;
                return Err(error);
            }
        }
    }
}

struct SourcePolicy {
    providers: Arc<crate::LocalProviders>,
    credentials: Arc<crate::LocalCredentials>,
    provider: ProviderProfile,
    model: String,
    api_key: Option<String>,
    observer: Arc<dyn ModelAttemptObserver>,
}

impl SourcePolicy {
    async fn check(&self) -> Result<(), HarnessError> {
        let invalid =
            || HarnessError::policy("source computer model configuration or credential changed");
        let current = self
            .providers
            .get(&self.provider.id)
            .await
            .ok_or_else(invalid)?;
        let mut selected = self.provider.resolved_model(&self.model)?;
        let mut available = current.resolved_model(&self.model).map_err(|_| invalid())?;
        selected.display_name = None;
        available.display_name = None;
        if current.base_url != self.provider.base_url
            || current.protocol != self.provider.protocol
            || current.api_key_ref != self.provider.api_key_ref
            || current.timeout_ms != self.provider.timeout_ms
            || current.max_attempts != self.provider.max_attempts
            || current.retry_base_delay_ms != self.provider.retry_base_delay_ms
            || selected != available
        {
            return Err(invalid());
        }
        let key = match current.api_key_ref.as_deref() {
            Some(reference) => self.credentials.resolve_value(reference).await?,
            None => None,
        };
        if key != self.api_key {
            return Err(invalid());
        }
        Ok(())
    }
}

impl ModelAttemptObserver for SourcePolicy {
    fn before_attempt<'a>(
        &'a self,
        attempt: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.check().await?;
            self.observer.before_attempt(attempt).await?;
            // Authorization may wait for Server; local revocation still wins.
            self.check().await
        })
    }

    fn after_attempt<'a>(
        &'a self,
        report: ModelAttemptReport,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        self.observer.after_attempt(report)
    }
}
