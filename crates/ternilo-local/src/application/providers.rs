use super::{
    AuthorizationAttempt, AuthorizationBeginRequest, AuthorizationCredentialKey,
    AuthorizationPromptAnswer, AuthorizationSnapshot, ExtensionProviderMaterializeRequest,
    HarnessError, LocalApplication, ModelSelection, ProviderModel, ProviderModelDiscoveryRequest,
    ProviderProfile, validate_model,
};
use ternilo_protocol::ProviderModelCatalog as _;

impl LocalApplication {
    pub async fn default_model(&self) -> Result<ModelSelection, HarnessError> {
        self.preferences.default_model().await
    }

    pub async fn set_default_model(
        &self,
        selection: ModelSelection,
    ) -> Result<ModelSelection, HarnessError> {
        validate_model(&selection)?;
        if matches!(
            selection,
            ModelSelection::AccountProvider { .. }
                | ModelSelection::PlatformModel { .. }
                | ModelSelection::ComputerProvider { .. }
        ) {
            return Err(HarnessError::policy(
                "Server models are authorized per session, not as a computer default",
            ));
        }
        if let ModelSelection::NamedProvider {
            provider_id,
            model,
            reasoning_effort,
        } = &selection
        {
            let provider = self.providers.get(provider_id).await.ok_or_else(|| {
                HarnessError::invalid(format!("unknown provider profile {provider_id:?}"))
            })?;
            provider
                .resolved_model(model)?
                .reasoning_value(*reasoning_effort)?;
        }
        let selection = self.preferences.set_default_model(selection).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(selection)
    }

    pub async fn credential_names(&self) -> Vec<String> {
        self.credentials.names().await
    }

    pub async fn credential_inventory(&self) -> ternilo_protocol::CredentialInventory {
        let mut inventory = self.credentials.inventory().await;
        let mut references = std::collections::BTreeSet::new();
        let sessions = self.state.snapshot().await.sessions;
        for plugin in self
            .profile
            .plugins
            .iter()
            .chain(sessions.iter().flat_map(|session| {
                session
                    .preset_plugins
                    .iter()
                    .chain(&session.profile_plugins)
            }))
        {
            if plugin.enabled
                && let Some(reference) = plugin
                    .config
                    .get("api_key_env")
                    .and_then(|value| value.as_str())
            {
                references.insert(reference.to_owned());
            }
        }
        for session in sessions {
            if let ModelSelection::OpenAiCompatible {
                api_key_env: Some(reference),
                ..
            } = session.model
            {
                references.insert(reference);
            }
        }
        for provider in self.providers.list().await {
            if let Some(reference) = provider.api_key_ref {
                references.insert(reference);
            }
        }
        for reference in references {
            if !inventory
                .references
                .iter()
                .any(|entry| entry.reference == reference)
                && let Ok(description) = self.credentials.describe(&reference).await
            {
                inventory.references.push(description);
            }
        }
        inventory
            .references
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        inventory
    }

    pub async fn set_credential(&self, name: String, value: String) -> Result<(), HarnessError> {
        self.credentials.set(name, value).await
    }

    pub async fn remove_credential(&self, name: &str) -> Result<(), HarnessError> {
        self.credentials.remove(name).await
    }

    pub async fn set_credential_record(
        &self,
        key: String,
        kind: String,
        payload: serde_json::Value,
    ) -> Result<ternilo_protocol::CredentialRecordInfo, HarnessError> {
        self.credentials.set_record(key, kind, payload).await
    }

    pub async fn delete_credential_record(&self, key: &str) -> Result<(), HarnessError> {
        self.credentials.delete_record(key).await
    }

    pub async fn authorization_snapshot(
        &self,
        surface_id: &str,
    ) -> Result<AuthorizationSnapshot, HarnessError> {
        self.authorizations.snapshot(surface_id).await
    }

    pub async fn begin_authorization(
        &self,
        request: AuthorizationBeginRequest,
    ) -> Result<AuthorizationAttempt, HarnessError> {
        self.authorizations.begin(request).await
    }

    pub fn answer_authorization_prompt(
        &self,
        answer: AuthorizationPromptAnswer,
    ) -> Result<(), HarnessError> {
        self.authorizations.answer(answer)
    }

    pub async fn cancel_authorization(
        &self,
        key: &AuthorizationCredentialKey,
    ) -> Result<(), HarnessError> {
        self.authorizations.cancel(key).await
    }

    pub async fn provider_profiles(&self) -> Vec<ProviderProfile> {
        self.providers.list().await
    }

    pub async fn materialize_extension_provider(
        &self,
        request: ExtensionProviderMaterializeRequest,
    ) -> Result<ProviderProfile, HarnessError> {
        let provider = self.extension_registry.materialize_provider(&request)?;
        let provider = self.providers.create(provider).await?;
        self.invalidate(None, crate::LocalInvalidationCategory::Workbench, None);
        Ok(provider)
    }

    pub async fn upsert_provider_profile(
        &self,
        provider: ProviderProfile,
    ) -> Result<ProviderProfile, HarnessError> {
        provider.validate()?;
        let sessions = self.state.snapshot().await.sessions;
        for session in &sessions {
            if let ModelSelection::NamedProvider {
                provider_id,
                model,
                reasoning_effort,
            } = &session.model
                && provider_id == &provider.id
            {
                provider
                    .resolved_model(model)?
                    .reasoning_value(*reasoning_effort)?;
            }
        }
        let affected = sessions
            .into_iter()
            .filter_map(|session| match &session.model {
                ModelSelection::NamedProvider { provider_id, .. }
                    if provider_id == &provider.id =>
                {
                    Some(session.identity.session_id.as_str().to_owned())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for session_id in &affected {
            self.stop_live_session(session_id).await?;
        }
        let saved = self.providers.upsert(provider).await?;
        for session_id in affected {
            self.invalidate(
                Some(&session_id),
                crate::LocalInvalidationCategory::Profile,
                None,
            );
        }
        Ok(saved)
    }

    pub async fn delete_provider_profile(&self, id: &str) -> Result<(), HarnessError> {
        if self.state.snapshot().await.sessions.iter().any(|session| {
            matches!(
                &session.model,
                ModelSelection::NamedProvider { provider_id, .. } if provider_id == id
            )
        }) {
            return Err(HarnessError::policy(format!(
                "provider {id:?} is selected by at least one session"
            )));
        }
        self.providers.remove(id).await
    }

    pub async fn discover_provider_models(
        &self,
        request: ProviderModelDiscoveryRequest,
    ) -> Result<Vec<ProviderModel>, HarnessError> {
        request.validate()?;
        let provider = match request.provider_id.as_deref() {
            Some(id) => Some(self.providers.get(id).await.ok_or_else(|| {
                HarnessError::invalid(format!("unknown provider profile {id:?}"))
            })?),
            None => None,
        };
        if provider
            .as_ref()
            .is_some_and(|provider| crate::model_connections::is_connection_provider(&provider.id))
        {
            return Err(HarnessError::policy(
                "refresh models through the Server model connection",
            ));
        }
        let base_url = match request.base_url.as_deref() {
            Some(base_url) => base_url.trim().trim_end_matches('/').to_owned(),
            None => provider
                .as_ref()
                .map(|provider| provider.base_url.trim_end_matches('/').to_owned())
                .ok_or_else(|| HarnessError::invalid("provider discovery requires base_url"))?,
        };
        let timeout_ms = request
            .timeout_ms
            .or_else(|| provider.as_ref().map(|provider| provider.timeout_ms))
            .unwrap_or(120_000);
        let protocol = request
            .protocol
            .or_else(|| provider.as_ref().map(|provider| provider.protocol))
            .unwrap_or_default();
        let api_key = match request.api_key.as_deref().map(str::trim) {
            Some(api_key) if !api_key.is_empty() => Some(api_key.to_owned()),
            _ => match provider
                .as_ref()
                .and_then(|provider| provider.api_key_ref.as_ref())
            {
                Some(reference) => Some(
                    self.credentials
                        .resolve_value(reference)
                        .await?
                        .ok_or_else(|| {
                            HarnessError::policy(format!(
                                "provider credential reference {reference:?} is not configured"
                            ))
                        })?,
                ),
                None => None,
            },
        };
        let mut client = reqwest::Client::builder();
        if timeout_ms > 0 {
            client = client.timeout(std::time::Duration::from_millis(timeout_ms));
        }
        let client = client.build().map_err(|error| {
            HarnessError::execution(format!("build provider discovery client: {error}"))
        })?;
        ternilo_builtins::discover_provider_models(&client, &base_url, protocol, api_key.as_deref())
            .await
    }
}
