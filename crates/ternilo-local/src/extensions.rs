use std::{
    collections::BTreeSet,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use linorun_core::CallContext;
use serde_json::{Value, json};
use ternilo_kernel::{Catalog, RuntimeExtensionsProvider, compose_profiles};
use ternilo_protocol::{HarnessError, PluginEntry, Profile};

pub struct LocalRuntimeExtensions {
    registry: Arc<ternilo_extension::ExtensionRegistry>,
    state: Arc<crate::state::LocalState>,
    presets: Arc<crate::LocalAgentPresets>,
    base_profile: Profile,
    restart_requests: tokio::sync::Mutex<BTreeSet<String>>,
    catalog_revision: String,
    catalog: Vec<Value>,
}

impl LocalRuntimeExtensions {
    #[must_use]
    pub fn new(
        catalog: &Catalog,
        registry: Arc<ternilo_extension::ExtensionRegistry>,
        state: Arc<crate::state::LocalState>,
        presets: Arc<crate::LocalAgentPresets>,
        base_profile: Profile,
    ) -> Self {
        let catalog_entries = catalog
            .kinds()
            .map(|kind| {
                let manifest = catalog
                    .factory(kind)
                    .expect("catalog kind must resolve to its factory")
                    .manifest;
                json!({
                    "kind": manifest.kind,
                    "requires": manifest.requires,
                    "provides": manifest.provides,
                })
            })
            .collect();
        Self {
            registry,
            state,
            presets,
            base_profile,
            restart_requests: tokio::sync::Mutex::new(BTreeSet::new()),
            catalog_revision: catalog.revision().to_owned(),
            catalog: catalog_entries,
        }
    }

    pub async fn take_restart_request(&self, session_id: &str) -> bool {
        self.restart_requests.lock().await.remove(session_id)
    }

    async fn remove_extension_mounts(
        &self,
        packages: &[(String, String)],
    ) -> Result<(), HarnessError> {
        let mut affected = self
            .state
            .snapshot()
            .await
            .sessions
            .into_iter()
            .filter_map(|mut session| {
                match remove_extension_mounts_from_session(
                    &self.base_profile,
                    &mut session.preset_plugins,
                    &mut session.profile_plugins,
                    packages,
                ) {
                    Ok(true) => Some(Ok(session)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut restart_requests = self.restart_requests.lock().await;
        for session in &mut affected {
            session.updated_at_ms = now_ms()?;
            self.state
                .replace_session(session.identity.session_id.as_str(), session.clone())
                .await?;
            restart_requests.insert(session.identity.session_id.as_str().to_owned());
        }
        drop(restart_requests);
        self.presets.remove_extension_mounts(packages).await
    }
}

impl RuntimeExtensionsProvider for LocalRuntimeExtensions {
    fn inspect<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        let registry = Arc::clone(&self.registry);
        let catalog_revision = self.catalog_revision.clone();
        let catalog = self.catalog.clone();
        Box::pin(async move {
            let inventory = tokio::task::spawn_blocking(move || registry.inventory())
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "join runtime extension inventory read: {error}"
                    ))
                })??;
            Ok(json!({
                "supported": true,
                "catalog_revision": catalog_revision,
                "catalog": catalog,
                "publishers": inventory.publishers,
                "extensions": inventory.extensions,
            }))
        })
    }

    fn set_enabled<'a>(
        &'a self,
        _: CallContext<()>,
        package_id: String,
        version: String,
        enabled: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        let registry = Arc::clone(&self.registry);
        Box::pin(async move {
            if !enabled {
                let validation_registry = Arc::clone(&registry);
                let validation_package_id = package_id.clone();
                let validation_version = version.clone();
                tokio::task::spawn_blocking(move || {
                    validation_registry
                        .describe(&validation_package_id, &validation_version)
                        .map(|_| ())
                })
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "join runtime extension state validation: {error}"
                    ))
                })??;
                self.remove_extension_mounts(&[(package_id.clone(), version.clone())])
                    .await?;
            }
            let plugin = tokio::task::spawn_blocking(move || {
                registry.set_enabled(&package_id, &version, enabled, now_ms()?)
            })
            .await
            .map_err(|error| {
                HarnessError::execution(format!("join runtime extension state change: {error}"))
            })??;
            serde_json::to_value(plugin).map_err(|error| {
                HarnessError::execution(format!("encode runtime extension state: {error}"))
            })
        })
    }

    fn revoke<'a>(
        &'a self,
        _: CallContext<()>,
        package_id: String,
        version: String,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        let registry = Arc::clone(&self.registry);
        Box::pin(async move {
            let result_package_id = package_id.clone();
            let result_version = version.clone();
            let validation_registry = Arc::clone(&registry);
            let validation_package_id = package_id.clone();
            let validation_version = version.clone();
            tokio::task::spawn_blocking(move || {
                validation_registry
                    .describe(&validation_package_id, &validation_version)
                    .map(|_| ())
            })
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "join runtime extension revocation validation: {error}"
                ))
            })??;
            self.remove_extension_mounts(&[(package_id.clone(), version.clone())])
                .await?;
            tokio::task::spawn_blocking(move || registry.revoke(&package_id, &version, now_ms()?))
                .await
                .map_err(|error| {
                    HarnessError::execution(format!("join runtime extension revocation: {error}"))
                })??;
            Ok(json!({
                "package_id": result_package_id,
                "version": result_version,
                "revoked": true,
            }))
        })
    }

    fn set_mounted<'a>(
        &'a self,
        _: CallContext<()>,
        session_id: String,
        package_id: String,
        version: String,
        mounted: bool,
        settings: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if mounted {
                let registry = Arc::clone(&self.registry);
                let check_package_id = package_id.clone();
                let check_version = version.clone();
                let check_settings = settings.clone();
                tokio::task::spawn_blocking(move || {
                    registry.validate_mount_settings(
                        &check_package_id,
                        &check_version,
                        &check_settings,
                    )
                })
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "join runtime extension mount validation: {error}"
                    ))
                })??;
            }
            let mut session =
                self.state.session(&session_id).await.ok_or_else(|| {
                    HarnessError::invalid(format!("unknown session {session_id:?}"))
                })?;
            update_extension_mount(
                &self.base_profile,
                &session.preset_plugins,
                &mut session.profile_plugins,
                &package_id,
                &version,
                mounted,
                &settings,
            )?;
            let candidate = compose_profiles([
                self.base_profile.clone(),
                Profile {
                    plugins: session.preset_plugins.clone(),
                },
                Profile {
                    plugins: session.profile_plugins.clone(),
                },
            ]);
            let registry = Arc::clone(&self.registry);
            tokio::task::spawn_blocking(move || registry.validate_profile_mounts(&candidate))
                .await
                .map_err(|error| {
                    HarnessError::execution(format!(
                        "join runtime extension profile validation: {error}"
                    ))
                })??;
            session.updated_at_ms = now_ms()?;
            self.state.replace_session(&session_id, session).await?;
            self.restart_requests
                .lock()
                .await
                .insert(session_id.clone());
            Ok(json!({
                "session_id": session_id,
                "package_id": package_id,
                "version": version,
                "mounted": mounted,
                "effective": "next_turn"
            }))
        })
    }
}

fn matches_extension_mount(entry: &PluginEntry, packages: &[(String, String)]) -> bool {
    packages
        .iter()
        .any(|(package_id, version)| extension_mount_matches(entry, package_id, version))
}

pub(crate) fn remove_extension_mounts_from_session(
    base_profile: &Profile,
    preset_plugins: &mut Vec<PluginEntry>,
    session_plugins: &mut Vec<PluginEntry>,
    packages: &[(String, String)],
) -> Result<bool, HarnessError> {
    let matched = base_profile
        .plugins
        .iter()
        .chain(preset_plugins.iter())
        .chain(session_plugins.iter())
        .any(|entry| matches_extension_mount(entry, packages));
    if !matched {
        return Ok(false);
    }
    for (package_id, version) in packages {
        update_extension_mount(
            base_profile,
            preset_plugins,
            session_plugins,
            package_id,
            version,
            false,
            &Value::Null,
        )?;
    }
    let inherited_shadows = session_plugins
        .iter()
        .filter(|entry| !entry.enabled && matches_extension_mount(entry, packages))
        .cloned()
        .collect::<Vec<_>>();
    session_plugins.retain(|entry| !matches_extension_mount(entry, packages));
    for shadow in inherited_shadows {
        upsert_profile_entry(session_plugins, shadow);
    }
    preset_plugins.retain(|entry| !matches_extension_mount(entry, packages));
    let lower_after_cleanup = compose_profiles([
        base_profile.clone(),
        Profile {
            plugins: preset_plugins.clone(),
        },
    ]);
    let effective_after_cleanup = compose_profiles([
        lower_after_cleanup.clone(),
        Profile {
            plugins: session_plugins.clone(),
        },
    ]);
    for entry in effective_after_cleanup
        .plugins
        .iter()
        .filter(|entry| entry.enabled && matches_extension_mount(entry, packages))
    {
        if lower_after_cleanup.plugins.iter().any(|lower| {
            lower.id == entry.id && lower.enabled && matches_extension_mount(lower, packages)
        }) {
            let mut shadow = entry.clone();
            shadow.enabled = false;
            upsert_profile_entry(session_plugins, shadow);
        }
    }
    Ok(true)
}

pub(crate) fn unavailable_inherited_extension_shadows(
    inherited_profile: &Profile,
    registry: &ternilo_extension::ExtensionRegistry,
) -> Result<Vec<PluginEntry>, HarnessError> {
    let mut shadows = Vec::new();
    for entry in inherited_profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled && entry.kind == ternilo_extension::EXTENSION_PACKAGE_KIND)
    {
        let mount =
            serde_json::from_value::<ternilo_extension::ExtensionMount>(entry.config.clone())
                .map_err(|error| {
                    HarnessError::composition(format!("invalid extension mount config: {error}"))
                })?;
        if registry.resolve(&mount.package_id, &mount.version).is_err() {
            let mut shadow = entry.clone();
            shadow.enabled = false;
            shadows.push(shadow);
        }
    }
    Ok(shadows)
}

fn extension_mount_matches(entry: &PluginEntry, package_id: &str, version: &str) -> bool {
    entry.kind == ternilo_extension::EXTENSION_PACKAGE_KIND
        && serde_json::from_value::<ternilo_extension::ExtensionMount>(entry.config.clone())
            .is_ok_and(|mount| mount.package_id == package_id && mount.version == version)
}

fn update_extension_mount(
    base_profile: &Profile,
    preset_plugins: &[PluginEntry],
    session_plugins: &mut Vec<PluginEntry>,
    package_id: &str,
    version: &str,
    mounted: bool,
    settings: &Value,
) -> Result<(), HarnessError> {
    let lower_profile = compose_profiles([
        base_profile.clone(),
        Profile {
            plugins: preset_plugins.to_vec(),
        },
    ]);
    let effective_profile = compose_profiles([
        lower_profile.clone(),
        Profile {
            plugins: session_plugins.clone(),
        },
    ]);

    if mounted {
        let existing = session_plugins
            .iter()
            .find(|entry| extension_mount_matches(entry, package_id, version))
            .cloned()
            .or_else(|| {
                effective_profile
                    .plugins
                    .iter()
                    .find(|entry| {
                        entry.enabled && extension_mount_matches(entry, package_id, version)
                    })
                    .cloned()
            });
        let row_id = existing.as_ref().map_or_else(
            || format!("extension:{package_id}@{version}"),
            |entry| entry.id.clone(),
        );
        if existing.is_none()
            && effective_profile
                .plugins
                .iter()
                .any(|entry| entry.id == row_id)
        {
            return Err(HarnessError::policy(format!(
                "profile row {row_id:?} is already owned by another extension"
            )));
        }
        let mut entry = existing.unwrap_or_else(|| PluginEntry {
            id: row_id,
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: Value::Null,
        });
        entry.enabled = true;
        entry.config = json!({
            "package_id": package_id,
            "version": version,
            "settings": settings,
        });
        upsert_profile_entry(session_plugins, entry);
        return Ok(());
    }

    for entry in effective_profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled && extension_mount_matches(entry, package_id, version))
    {
        let inherited = lower_profile.plugins.iter().any(|lower| {
            lower.id == entry.id
                && lower.enabled
                && extension_mount_matches(lower, package_id, version)
        });
        if inherited {
            let mut shadow = entry.clone();
            shadow.enabled = false;
            upsert_profile_entry(session_plugins, shadow);
        } else {
            session_plugins.retain(|candidate| candidate.id != entry.id);
        }
    }
    session_plugins.retain(|entry| {
        if entry.enabled || !extension_mount_matches(entry, package_id, version) {
            return true;
        }
        lower_profile.plugins.iter().any(|lower| {
            lower.id == entry.id
                && lower.enabled
                && extension_mount_matches(lower, package_id, version)
        })
    });
    Ok(())
}

fn upsert_profile_entry(entries: &mut Vec<PluginEntry>, entry: PluginEntry) {
    if let Some(position) = entries
        .iter()
        .position(|candidate| candidate.id == entry.id)
    {
        entries[position] = entry;
    } else {
        entries.push(entry);
    }
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabling_an_existing_extension_mount_applies_the_validated_settings() {
        let old_settings = json!({"endpoint": "https://old.example.test"});
        let settings = json!({"endpoint": "https://example.test", "nested": {"mode": "fast"}});
        let entry = PluginEntry {
            id: "extension:dev.example@1.0.0".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: false,
            config: json!({
                "package_id": "dev.example",
                "version": "1.0.0",
                "settings": old_settings,
            }),
        };
        let mut entries = vec![entry];

        update_extension_mount(
            &Profile { plugins: vec![] },
            &[],
            &mut entries,
            "dev.example",
            "1.0.0",
            true,
            &settings,
        )
        .unwrap();

        let entry = &entries[0];
        assert!(entry.enabled);
        assert_eq!(entry.config["settings"], settings);
    }

    #[test]
    fn inherited_extension_mounts_use_disabled_shadows_and_keep_their_row_identity() {
        let inherited = PluginEntry {
            id: "custom-extension-row".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.example",
                "version": "1.0.0",
                "settings": {"endpoint": "https://preset.example.test"},
            }),
        };
        let unrelated = PluginEntry {
            id: "session-only".to_owned(),
            kind: "ternilo.other".to_owned(),
            enabled: true,
            config: json!({}),
        };
        let base = Profile {
            plugins: vec![inherited.clone()],
        };
        let mut session_plugins = vec![unrelated.clone()];

        update_extension_mount(
            &base,
            &[],
            &mut session_plugins,
            "dev.example",
            "1.0.0",
            false,
            &json!({}),
        )
        .unwrap();
        assert_eq!(session_plugins[0], unrelated);
        assert_eq!(session_plugins[1].id, inherited.id);
        assert!(!session_plugins[1].enabled);
        let effective = compose_profiles([
            base.clone(),
            Profile {
                plugins: session_plugins.clone(),
            },
        ]);
        assert!(!effective.plugins[0].enabled);

        let next_settings = json!({"endpoint": "https://session.example.test"});
        update_extension_mount(
            &base,
            &[],
            &mut session_plugins,
            "dev.example",
            "1.0.0",
            true,
            &next_settings,
        )
        .unwrap();
        assert_eq!(session_plugins[0], unrelated);
        assert_eq!(session_plugins[1].id, inherited.id);
        assert!(session_plugins[1].enabled);
        assert_eq!(session_plugins[1].config["settings"], next_settings);
    }

    #[test]
    fn lifecycle_cleanup_shadows_inherited_mounts_and_removes_session_only_mounts() {
        let mount = |id: &str, scope: &str| PluginEntry {
            id: id.to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.example",
                "version": "1.0.0",
                "settings": {"scope": scope},
            }),
        };
        let base = Profile {
            plugins: vec![mount("base-extension-row", "base")],
        };
        let mut preset_plugins = vec![mount("preset-extension-row", "preset")];
        let unrelated = PluginEntry {
            id: "unrelated".to_owned(),
            kind: "ternilo.other".to_owned(),
            enabled: true,
            config: json!({}),
        };
        let mut session_plugins =
            vec![mount("session-extension-row", "session"), unrelated.clone()];

        assert!(
            remove_extension_mounts_from_session(
                &base,
                &mut preset_plugins,
                &mut session_plugins,
                &[("dev.example".to_owned(), "1.0.0".to_owned())],
            )
            .unwrap()
        );

        assert!(preset_plugins.is_empty());
        assert_eq!(session_plugins[0], unrelated);
        assert_eq!(session_plugins.len(), 3);
        for id in ["base-extension-row", "preset-extension-row"] {
            let shadow = session_plugins
                .iter()
                .find(|entry| entry.id == id)
                .expect("inherited extension row receives a session shadow");
            assert!(!shadow.enabled);
        }
        assert!(
            session_plugins
                .iter()
                .all(|entry| entry.id != "session-extension-row")
        );
    }

    #[test]
    fn lifecycle_cleanup_shadows_base_mount_revealed_by_removed_preset_override() {
        let base_mount = PluginEntry {
            id: "shared-extension-row".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.example",
                "version": "1.0.0",
                "settings": {"scope": "base"},
            }),
        };
        let mut preset_plugins = vec![PluginEntry {
            enabled: false,
            config: json!({
                "package_id": "dev.example",
                "version": "1.0.0",
                "settings": {"scope": "preset"},
            }),
            ..base_mount.clone()
        }];
        let mut session_plugins = Vec::new();

        assert!(
            remove_extension_mounts_from_session(
                &Profile {
                    plugins: vec![base_mount.clone()],
                },
                &mut preset_plugins,
                &mut session_plugins,
                &[("dev.example".to_owned(), "1.0.0".to_owned())],
            )
            .unwrap()
        );

        assert!(preset_plugins.is_empty());
        assert_eq!(session_plugins.len(), 1);
        assert_eq!(session_plugins[0].id, base_mount.id);
        assert!(!session_plugins[0].enabled);
        assert_eq!(session_plugins[0].config, base_mount.config);
    }

    #[test]
    fn unavailable_inherited_extensions_receive_new_session_shadows() {
        let directory = tempfile::tempdir().unwrap();
        let registry = ternilo_extension::ExtensionRegistry::open(
            directory.path().join("extensions"),
            ternilo_extension::ExtensionHostPolicy::default(),
        )
        .unwrap();
        let unavailable = PluginEntry {
            id: "base-extension-row".to_owned(),
            kind: ternilo_extension::EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": "dev.unavailable",
                "version": "1.0.0",
                "settings": {"scope": "base"},
            }),
        };
        let inherited = Profile {
            plugins: vec![
                unavailable.clone(),
                PluginEntry {
                    id: "disabled-extension-row".to_owned(),
                    enabled: false,
                    ..unavailable.clone()
                },
                PluginEntry {
                    id: "unrelated".to_owned(),
                    kind: "ternilo.other".to_owned(),
                    enabled: true,
                    config: json!({}),
                },
            ],
        };

        let shadows = unavailable_inherited_extension_shadows(&inherited, &registry).unwrap();
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].id, unavailable.id);
        assert!(!shadows[0].enabled);
        assert_eq!(shadows[0].config, unavailable.config);
    }
}
