use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use ternilo_protocol::{HarnessError, ProviderProfile};
use tokio::sync::Mutex;

use crate::persistence::atomic_replace;

pub struct LocalProviders {
    path: PathBuf,
    pub(crate) connections: Arc<crate::model_connections::LocalModelConnections>,
    entries: Mutex<BTreeMap<String, ProviderProfile>>,
}

impl LocalProviders {
    pub async fn open(data_root: PathBuf) -> Result<Arc<Self>, HarnessError> {
        let connections =
            crate::model_connections::LocalModelConnections::open(data_root.clone()).await?;
        let path = data_root.join("config/providers.json");
        let entries = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<BTreeMap<String, ProviderProfile>>(&bytes)
                .map_err(|error| {
                    HarnessError::execution(format!("parse {}: {error}", path.display()))
                })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read {}: {error}",
                    path.display()
                )));
            }
        };
        for (id, provider) in &entries {
            provider.validate()?;
            if id != &provider.id {
                return Err(HarnessError::execution(format!(
                    "provider map key {id:?} does not match profile id {:?}",
                    provider.id
                )));
            }
        }
        Ok(Arc::new(Self {
            path,
            connections,
            entries: Mutex::new(entries),
        }))
    }

    pub async fn list(&self) -> Vec<ProviderProfile> {
        let mut profiles: Vec<_> = self.entries.lock().await.values().cloned().collect();
        profiles.extend(
            self.connections
                .list()
                .await
                .into_iter()
                .flat_map(|connection| connection.providers()),
        );
        profiles
    }

    pub async fn get(&self, id: &str) -> Option<ProviderProfile> {
        if crate::model_connections::is_connection_provider(id) {
            return self
                .connections
                .stored()
                .await
                .into_iter()
                .flat_map(|connection| connection.known_providers)
                .find(|profile| profile.id == id);
        }
        self.entries.lock().await.get(id).cloned()
    }

    pub async fn upsert(&self, provider: ProviderProfile) -> Result<ProviderProfile, HarnessError> {
        provider.validate()?;
        require_manual_provider(&provider.id)?;
        let mut guard = self.entries.lock().await;
        let mut next = guard.clone();
        next.insert(provider.id.clone(), provider.clone());
        self.persist(&next).await?;
        *guard = next;
        Ok(provider)
    }

    pub async fn create(&self, provider: ProviderProfile) -> Result<ProviderProfile, HarnessError> {
        provider.validate()?;
        require_manual_provider(&provider.id)?;
        let mut guard = self.entries.lock().await;
        if guard.contains_key(&provider.id) {
            return Err(HarnessError::conflict(format!(
                "provider profile {:?} already exists",
                provider.id
            )));
        }
        let mut next = guard.clone();
        next.insert(provider.id.clone(), provider.clone());
        self.persist(&next).await?;
        *guard = next;
        Ok(provider)
    }

    pub async fn remove(&self, id: &str) -> Result<(), HarnessError> {
        validate_provider_id(id)?;
        require_manual_provider(id)?;
        let mut guard = self.entries.lock().await;
        let mut next = guard.clone();
        if next.remove(id).is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown provider profile {id:?}"
            )));
        }
        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    async fn persist(
        &self,
        entries: &BTreeMap<String, ProviderProfile>,
    ) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(entries).map_err(|error| {
            HarnessError::execution(format!("serialize provider profiles: {error}"))
        })?;
        atomic_replace(&self.path, &bytes, true).await
    }
}

fn require_manual_provider(id: &str) -> Result<(), HarnessError> {
    if crate::model_connections::is_connection_provider(id) {
        return Err(HarnessError::policy(
            "manage this Provider through its Server model connection",
        ));
    }
    Ok(())
}

fn validate_provider_id(id: &str) -> Result<(), HarnessError> {
    let mut bytes = id.bytes();
    if id.len() > 64
        || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(HarnessError::invalid(
            "provider id must start with a lowercase letter and use lowercase letters, digits, dash, or underscore",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{
        ProviderModel, ProviderModelDefaults, ProviderModelSettings, ProviderProtocol,
    };

    fn provider() -> ProviderProfile {
        ProviderProfile {
            id: "local-openai".to_owned(),
            display_name: "Local OpenAI".to_owned(),
            base_url: "http://127.0.0.1:8080/v1".to_owned(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: Some("LOCAL_OPENAI_KEY".to_owned()),
            defaults: ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 8_192,
                reasoning: None,
            },
            models: vec![ProviderModel {
                id: "model-a".to_owned(),
                display_name: Some("Model A".to_owned()),
                settings: ProviderModelSettings::Inherit,
            }],
            timeout_ms: 120_000,
            max_attempts: 3,
            retry_base_delay_ms: 250,
        }
    }

    #[tokio::test]
    async fn provider_library_is_atomic_and_persistent() {
        let root = tempfile::tempdir().unwrap();
        let library = LocalProviders::open(root.path().to_path_buf())
            .await
            .unwrap();
        library.upsert(provider()).await.unwrap();
        drop(library);
        let reopened = LocalProviders::open(root.path().to_path_buf())
            .await
            .unwrap();
        assert_eq!(reopened.list().await, vec![provider()]);
        reopened.remove("local-openai").await.unwrap();
        assert!(reopened.list().await.is_empty());
    }

    #[tokio::test]
    async fn provider_create_is_persistent_and_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let library = LocalProviders::open(root.path().to_path_buf())
            .await
            .unwrap();
        let original = library.create(provider()).await.unwrap();
        let mut replacement = original.clone();
        replacement.display_name = "Replacement".to_owned();
        let error = library.create(replacement).await.unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::Conflict);
        assert_eq!(library.get(&original.id).await, Some(original));

        drop(library);
        let reopened = LocalProviders::open(root.path().to_path_buf())
            .await
            .unwrap();
        assert_eq!(reopened.get("local-openai").await, Some(provider()));
    }
}
