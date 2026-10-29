use super::LocalApplication;
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
