use sha2::{Digest, Sha256};
use ternilo_protocol::{ProviderModel, ProviderModelSettings, ProviderProfile, ProviderProtocol};

use super::{ModelConnection, credential_reference};

impl ModelConnection {
    #[must_use]
    pub fn providers(&self) -> Vec<ProviderProfile> {
        self.session
            .grants
            .iter()
            .map(|grant| (false, &grant.grant_id, &grant.grant_name, &grant.models))
            .chain(self.session.providers.iter().map(|provider| {
                (
                    true,
                    &provider.provider_id,
                    &provider.provider_name,
                    &provider.models,
                )
            }))
            .flat_map(|(account, source_id, source_name, catalog)| {
                [
                    ProviderProtocol::OpenAiChatCompletions,
                    ProviderProtocol::OpenAiResponses,
                    ProviderProtocol::DeepSeekResponses,
                    ProviderProtocol::GoogleGemini,
                    ProviderProtocol::AnthropicMessages,
                ]
                .into_iter()
                .filter_map(|protocol| {
                    let models: Vec<_> = catalog
                        .iter()
                        .filter(|model| model.protocol == protocol)
                        .collect();
                    let first = models.first()?;
                    let identity = if account {
                        format!("{}:account:{source_id}:{protocol:?}", self.connection_id)
                    } else {
                        format!("{}:{source_id}:{protocol:?}", self.connection_id)
                    };
                    let digest: String = Sha256::digest(identity.as_bytes())
                        .iter()
                        .flat_map(|byte| {
                            const HEX: &[u8; 16] = b"0123456789abcdef";
                            [
                                char::from(HEX[usize::from(byte >> 4)]),
                                char::from(HEX[usize::from(byte & 15)]),
                            ]
                        })
                        .collect();
                    let mut url = reqwest::Url::parse(&self.server_url).ok()?;
                    url.path_segments_mut().ok()?.pop_if_empty().extend([
                        "v1",
                        if account { "device-account" } else { "device" },
                        source_id,
                    ]);
                    Some(ProviderProfile {
                        id: if account {
                            format!("server_a_{}", &digest[..54])
                        } else {
                            format!("server_{}", &digest[..56])
                        },
                        display_name: format!(
                            "{} · {} · {}",
                            self.name, self.session.identity.username, source_name
                        ),
                        base_url: url.to_string(),
                        protocol,
                        api_key_ref: Some(credential_reference(&self.connection_id)),
                        defaults: first.defaults.clone(),
                        models: models
                            .iter()
                            .map(|model| ProviderModel {
                                id: model.model_id.clone(),
                                display_name: Some(model.display_name.clone()),
                                settings: ProviderModelSettings::Override {
                                    context_window: model.defaults.context_window,
                                    max_output_tokens: model.defaults.max_output_tokens,
                                    reasoning: model.defaults.reasoning.clone(),
                                },
                            })
                            .collect(),
                        timeout_ms: 600_000,
                        max_attempts: 3,
                        retry_base_delay_ms: 250,
                    })
                })
                .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn remember_providers(&mut self) {
        for mut profile in self.providers() {
            if let Some(previous) = self
                .known_providers
                .iter_mut()
                .find(|entry| entry.id == profile.id)
            {
                for model in &previous.models {
                    if !profile.models.iter().any(|current| current.id == model.id) {
                        profile.models.push(model.clone());
                    }
                }
                *previous = profile;
            } else {
                self.known_providers.push(profile);
            }
        }
    }
}

pub fn is_connection_provider(id: &str) -> bool {
    id.strip_prefix("server_")
        .is_some_and(|id| id.len() == 56 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || id
            .strip_prefix("server_a_")
            .is_some_and(|id| id.len() == 54 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
