use super::{LocalApplication, TurnInput};
use std::{future::Future, pin::Pin, sync::Arc};
use ternilo_kernel::ProviderWebCredentials;
use ternilo_protocol::{HarnessError, ProviderWebAccess, RunId, RunModelBinding, WebCapability};

pub(super) struct WebCredentials {
    pub providers: Arc<crate::LocalProviders>,
    pub credentials: Arc<crate::LocalCredentials>,
    pub gateways: Arc<crate::server_models::ServerModelGateways>,
    pub session_id: String,
    pub provider_id: Option<String>,
    pub binding: Option<RunModelBinding>,
}

pub(super) async fn local_credential(
    providers: &crate::LocalProviders,
    credentials: &crate::LocalCredentials,
    provider_id: &str,
    capability: WebCapability,
    expected: &ProviderWebAccess,
) -> Result<Option<String>, HarnessError> {
    let provider = providers
        .get(provider_id)
        .await
        .ok_or_else(|| HarnessError::unavailable("the selected web Provider no longer exists"))?;
    if provider.web_access.as_deref() != Some(expected) {
        return Err(HarnessError::conflict(
            "Provider web settings changed; start a new task to use the current settings",
        ));
    }
    match expected.credential(capability) {
        Some(reference) => credentials.resolve_value(reference).await?.filter(|value| !value.trim().is_empty()).map(Some)
            .ok_or_else(|| HarnessError::unavailable("Provider web credential is missing; configure it in the Provider's web access settings")),
        None => Ok(None),
    }
}

impl LocalApplication {
    pub(super) async fn refresh_web_for_input_locked(
        &self,
        session_id: &str,
        input: &TurnInput,
    ) -> Result<bool, HarnessError> {
        if let TurnInput::Prompt(text) = input
            && text.trim_start().starts_with('/')
        {
            let cached = self.ensure_session_locked(session_id).await?;
            return Ok(cached
                .harness
                .resolve_command(text.clone())
                .await
                .is_none_or(|command| {
                    command.result.is_ok_and(|call| {
                        matches!(call.tool_name.as_str(), "web_search" | "web_fetch")
                    })
                }));
        }
        Ok(true)
    }

    pub(super) async fn validate_provider_web_submission(
        &self,
        session_id: &str,
        input: &TurnInput,
    ) -> Result<(), HarnessError> {
        let lifecycle = self.session_lifecycle(session_id).await;
        let _lifecycle = lifecycle.lock().await;
        let snapshot = self
            .state
            .session(session_id)
            .await
            .and_then(|session| session.server_model);
        if let Some(snapshot) = snapshot
            && self.refresh_web_for_input_locked(session_id, input).await?
        {
            // Reject known source/configuration failures before accepting a queued task.
            // Preparation still refreshes when that task actually starts.
            self.server_models
                .web_access(session_id, snapshot.binding, None)
                .await?;
        }
        Ok(())
    }

    /// Internal Server-to-source operation; only the separate web credential can be returned.
    pub async fn provider_web_credential(
        &self,
        provider_id: &str,
        capability: WebCapability,
        expected: &ProviderWebAccess,
    ) -> Result<Option<String>, HarnessError> {
        local_credential(
            &self.providers,
            &self.credentials,
            provider_id,
            capability,
            expected,
        )
        .await
    }
}

impl ProviderWebCredentials for WebCredentials {
    fn resolve<'a>(
        &'a self,
        run_id: RunId,
        capability: WebCapability,
        expected: ProviderWebAccess,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(binding) = &self.binding {
                return self
                    .gateways
                    .web_access(
                        &self.session_id,
                        binding.clone(),
                        Some((run_id, capability, expected)),
                    )
                    .await
                    .map(|reply| reply.api_key);
            }
            let id = self
                .provider_id
                .as_deref()
                .ok_or_else(|| HarnessError::policy("no Provider is selected for web access"))?;
            local_credential(
                &self.providers,
                &self.credentials,
                id,
                capability,
                &expected,
            )
            .await
        })
    }
}
