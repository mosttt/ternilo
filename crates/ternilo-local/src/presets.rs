use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    AgentPresetCopyRequest, AgentPresetDocument, AgentPresetRoster, AgentPresetSummary,
    AgentPresetTrust, AgentPresetUpdateRequest, DEFAULT_AGENT_PRESET_ID, HarnessError, PluginEntry,
    Profile, system_agent_preset, system_agent_presets, validate_agent_preset_id,
};
use tokio::sync::Mutex;

use crate::persistence::atomic_replace;

const DOCUMENT_VERSION: u32 = 1;
pub const DEFAULT_AGENT_PRESET: &str = DEFAULT_AGENT_PRESET_ID;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPreset {
    display_name: String,
    description: String,
    profile: Profile,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetDocument {
    schema_version: u32,
    default_id: String,
    presets: BTreeMap<String, StoredPreset>,
}

impl Default for PresetDocument {
    fn default() -> Self {
        Self {
            schema_version: DOCUMENT_VERSION,
            default_id: DEFAULT_AGENT_PRESET.to_owned(),
            presets: BTreeMap::new(),
        }
    }
}

pub struct LocalAgentPresets {
    path: PathBuf,
    document: Mutex<PresetDocument>,
}

impl LocalAgentPresets {
    pub async fn open(data_root: PathBuf) -> Result<Arc<Self>, HarnessError> {
        let path = data_root.join("agent-presets.json");
        let document = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<PresetDocument>(&bytes).map_err(|error| {
                HarnessError::execution(format!("parse {}: {error}", path.display()))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PresetDocument::default(),
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read {}: {error}",
                    path.display()
                )));
            }
        };
        if document.schema_version != DOCUMENT_VERSION {
            return Err(HarnessError::execution(format!(
                "unsupported agent preset document version {}; expected {DOCUMENT_VERSION}",
                document.schema_version
            )));
        }
        validate_agent_preset_id(&document.default_id)?;
        for (id, preset) in &document.presets {
            validate_agent_preset_id(id)?;
            validate_metadata(&preset.display_name, &preset.description)?;
            if system_agent_preset(id).is_some() {
                return Err(HarnessError::execution(format!(
                    "user agent preset {id:?} shadows a system preset"
                )));
            }
        }
        let library = Arc::new(Self {
            path,
            document: Mutex::new(document),
        });
        let default_id = library.default_id().await;
        if library.resolve(&default_id).await.is_none() {
            return Err(HarnessError::execution(
                "agent preset default does not resolve to a system or user preset",
            ));
        }
        Ok(library)
    }

    pub async fn roster(&self) -> AgentPresetRoster {
        let document = self.document.lock().await;
        let presets = system_agent_presets()
            .into_iter()
            .map(|preset| preset.summary)
            .chain(
                document
                    .presets
                    .iter()
                    .map(|(id, preset)| AgentPresetSummary {
                        id: id.clone(),
                        display_name: preset.display_name.clone(),
                        description: preset.description.clone(),
                        trust: AgentPresetTrust::User,
                    }),
            )
            .collect::<Vec<_>>();
        AgentPresetRoster {
            presets,
            default_id: document.default_id.clone(),
            authorable: true,
        }
    }

    pub async fn documents(&self) -> Vec<AgentPresetDocument> {
        let document = self.document.lock().await;
        let mut presets = system_agent_presets();
        presets.extend(
            document
                .presets
                .iter()
                .map(|(id, preset)| user_document(id, preset)),
        );
        presets
    }

    pub async fn default_id(&self) -> String {
        self.document.lock().await.default_id.clone()
    }

    pub async fn resolve(&self, id: &str) -> Option<AgentPresetDocument> {
        if let Some(preset) = system_agent_preset(id) {
            return Some(preset);
        }
        self.document
            .lock()
            .await
            .presets
            .get(id)
            .map(|preset| user_document(id, preset))
    }

    pub async fn copy(
        &self,
        request: AgentPresetCopyRequest,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(&request.from)?;
        validate_agent_preset_id(&request.id)?;
        let source = self.resolve(&request.from).await.ok_or_else(|| {
            HarnessError::invalid(format!("unknown agent preset {:?}", request.from))
        })?;
        let display_name = request
            .display_name
            .unwrap_or_else(|| request.id.clone())
            .trim()
            .to_owned();
        validate_metadata(&display_name, &source.summary.description)?;
        let mut guard = self.document.lock().await;
        if system_agent_preset(&request.id).is_some() || guard.presets.contains_key(&request.id) {
            return Err(HarnessError::policy(format!(
                "agent preset {:?} already exists",
                request.id
            )));
        }
        let stored = StoredPreset {
            display_name,
            description: source.summary.description,
            profile: source.profile,
        };
        let mut next = guard.clone();
        next.presets.insert(request.id.clone(), stored.clone());
        self.persist(&next).await?;
        *guard = next;
        Ok(user_document(&request.id, &stored))
    }

    pub async fn update(
        &self,
        id: &str,
        request: AgentPresetUpdateRequest,
    ) -> Result<AgentPresetDocument, HarnessError> {
        validate_agent_preset_id(id)?;
        validate_metadata(&request.display_name, &request.description)?;
        let mut guard = self.document.lock().await;
        if !guard.presets.contains_key(id) {
            return if system_agent_preset(id).is_some() {
                Err(HarnessError::policy("system agent presets are read-only"))
            } else {
                Err(HarnessError::invalid(format!(
                    "unknown agent preset {id:?}"
                )))
            };
        }
        let stored = StoredPreset {
            display_name: request.display_name.trim().to_owned(),
            description: request.description.trim().to_owned(),
            profile: request.profile,
        };
        let mut next = guard.clone();
        next.presets.insert(id.to_owned(), stored.clone());
        self.persist(&next).await?;
        *guard = next;
        Ok(user_document(id, &stored))
    }

    pub async fn remove(&self, id: &str) -> Result<(), HarnessError> {
        validate_agent_preset_id(id)?;
        if system_agent_preset(id).is_some() {
            return Err(HarnessError::policy("system agent presets are read-only"));
        }
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        if next.presets.remove(id).is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown agent preset {id:?}"
            )));
        }
        if next.default_id == id {
            DEFAULT_AGENT_PRESET.clone_into(&mut next.default_id);
        }
        self.persist(&next).await?;
        *guard = next;
        Ok(())
    }

    pub async fn set_default(&self, id: &str) -> Result<AgentPresetRoster, HarnessError> {
        validate_agent_preset_id(id)?;
        if self.resolve(id).await.is_none() {
            return Err(HarnessError::invalid(format!(
                "unknown agent preset {id:?}"
            )));
        }
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        id.clone_into(&mut next.default_id);
        self.persist(&next).await?;
        *guard = next;
        drop(guard);
        Ok(self.roster().await)
    }

    pub async fn remove_extension_mount(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<(), HarnessError> {
        self.remove_extension_mounts(&[(package_id.to_owned(), version.to_owned())])
            .await
    }

    pub async fn remove_extension_mounts(
        &self,
        packages: &[(String, String)],
    ) -> Result<(), HarnessError> {
        let mut guard = self.document.lock().await;
        let mut next = guard.clone();
        let mut changed = false;
        for preset in next.presets.values_mut() {
            let before = preset.profile.plugins.len();
            preset.profile.plugins.retain(|entry| {
                !packages
                    .iter()
                    .any(|(package_id, version)| is_extension_mount(entry, package_id, version))
            });
            changed |= before != preset.profile.plugins.len();
        }
        if changed {
            self.persist(&next).await?;
            *guard = next;
        }
        Ok(())
    }

    async fn persist(&self, document: &PresetDocument) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(document).map_err(|error| {
            HarnessError::execution(format!("serialize agent presets: {error}"))
        })?;
        atomic_replace(&self.path, &bytes, true).await
    }
}

fn is_extension_mount(entry: &PluginEntry, package_id: &str, version: &str) -> bool {
    entry.kind == ternilo_extension::EXTENSION_PACKAGE_KIND
        && entry
            .config
            .get("package_id")
            .and_then(serde_json::Value::as_str)
            == Some(package_id)
        && entry
            .config
            .get("version")
            .and_then(serde_json::Value::as_str)
            == Some(version)
}

fn validate_metadata(display_name: &str, description: &str) -> Result<(), HarnessError> {
    if display_name.trim().is_empty() || display_name.chars().count() > 120 {
        return Err(HarnessError::invalid(
            "agent preset display name must contain 1 to 120 characters",
        ));
    }
    if description.chars().count() > 2_000 {
        return Err(HarnessError::invalid(
            "agent preset description must not exceed 2000 characters",
        ));
    }
    Ok(())
}

fn user_document(id: &str, preset: &StoredPreset) -> AgentPresetDocument {
    AgentPresetDocument {
        base_profile: None,
        summary: AgentPresetSummary {
            id: id.to_owned(),
            display_name: preset.display_name.clone(),
            description: preset.description.clone(),
            trust: AgentPresetTrust::User,
        },
        profile: preset.profile.clone(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn system_presets_are_read_only_and_user_copies_are_atomic() {
        let root = tempdir().unwrap();
        let presets = LocalAgentPresets::open(root.path().to_path_buf())
            .await
            .unwrap();
        let roster = presets.roster().await;
        assert_eq!(
            roster
                .presets
                .iter()
                .map(|preset| preset.id.as_str())
                .collect::<Vec<_>>(),
            ["standard", "ptc", "minimal", "creative"],
        );
        assert_eq!(presets.documents().await, system_agent_presets());
        assert_eq!(
            presets.resolve("ptc").await.unwrap().profile.plugins[0].config["mode"],
            "code",
        );
        let copied = presets
            .copy(AgentPresetCopyRequest {
                from: "creative".to_owned(),
                id: "my-creative".to_owned(),
                display_name: Some("My Creative".to_owned()),
            })
            .await
            .unwrap();
        assert_eq!(copied.summary.trust, AgentPresetTrust::User);
        assert_eq!(
            copied.profile,
            system_agent_preset("creative").unwrap().profile,
        );
        assert!(
            presets
                .copy(AgentPresetCopyRequest {
                    from: "standard".to_owned(),
                    id: "ptc".to_owned(),
                    display_name: None,
                })
                .await
                .is_err()
        );
        presets.set_default("my-creative").await.unwrap();
        drop(presets);

        let reopened = LocalAgentPresets::open(root.path().to_path_buf())
            .await
            .unwrap();
        assert_eq!(reopened.default_id().await, "my-creative");
        assert!(reopened.remove("standard").await.is_err());
        assert!(reopened.remove("creative").await.is_err());
        reopened.remove("my-creative").await.unwrap();
        assert_eq!(reopened.default_id().await, DEFAULT_AGENT_PRESET);
    }
}
