use std::collections::{BTreeMap, BTreeSet};

use linorun_core::ServiceContract;
use ternilo_protocol::{HarnessError, Profile};

use crate::{
    AgentTeam, Agents, Attachments, Catalog, ModelGateway, RunEnvironment, RuntimeExtensions,
    SessionQueries, SessionTelemetry, Sessions,
};

pub fn compose_profiles(layers: impl IntoIterator<Item = Profile>) -> Profile {
    let mut composed = Vec::new();
    let mut positions = BTreeMap::<String, usize>::new();

    for layer in layers {
        for entry in layer.plugins {
            if let Some(position) = positions.get(&entry.id).copied() {
                composed[position] = entry;
            } else {
                positions.insert(entry.id.clone(), composed.len());
                composed.push(entry);
            }
        }
    }

    Profile { plugins: composed }
}

pub fn validate_profile(profile: &Profile, catalog: &Catalog) -> Result<(), HarnessError> {
    catalog.projection_units(profile)?;
    let mut row_ids = BTreeSet::new();
    let mut providers = BTreeMap::<&str, &str>::new();
    providers.insert(RunEnvironment::ID, "host");
    providers.insert(Attachments::ID, "host");
    providers.insert(SessionQueries::ID, "host");
    providers.insert(RuntimeExtensions::ID, "host");
    providers.insert(SessionTelemetry::ID, "host");
    providers.insert(ModelGateway::ID, "host");
    providers.insert(AgentTeam::ID, "host");

    for entry in &profile.plugins {
        if entry.id.trim().is_empty() {
            return Err(HarnessError::composition("plugin row id must not be empty"));
        }
        if !row_ids.insert(entry.id.as_str()) {
            return Err(HarnessError::composition(format!(
                "duplicate plugin row id {:?}; compose layers before validation",
                entry.id
            )));
        }
        if !entry.enabled {
            continue;
        }
        let factory = catalog.factory(&entry.kind)?;
        factory.build(entry.config.clone())?;
        let manifest = &factory.manifest;
        for service in manifest.provides {
            if let Some(previous) = providers.insert(service, entry.id.as_str()) {
                return Err(HarnessError::composition(format!(
                    "service {service} is provided by both {previous} and {}",
                    entry.id
                )));
            }
        }
    }

    for entry in profile.plugins.iter().filter(|entry| entry.enabled) {
        let manifest = &catalog.factory(&entry.kind)?.manifest;
        for service in manifest.requires {
            if !providers.contains_key(service) {
                return Err(HarnessError::composition(format!(
                    "plugin {} requires missing service {service}",
                    entry.id
                )));
            }
        }
    }

    for service in [Agents::ID, Sessions::ID] {
        if !providers.contains_key(service) {
            return Err(HarnessError::composition(format!(
                "profile does not provide required harness service {service}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use ternilo_protocol::PluginEntry;

    use super::*;

    #[test]
    fn later_layers_replace_whole_rows_without_reordering() {
        let base = Profile {
            plugins: vec![
                PluginEntry {
                    id: "model".to_owned(),
                    kind: "model.a".to_owned(),
                    enabled: true,
                    config: json!({ "old": true }),
                },
                PluginEntry {
                    id: "tool".to_owned(),
                    kind: "tool.a".to_owned(),
                    enabled: true,
                    config: json!({}),
                },
            ],
        };
        let overlay = Profile {
            plugins: vec![PluginEntry {
                id: "model".to_owned(),
                kind: "model.b".to_owned(),
                enabled: true,
                config: json!({ "new": true }),
            }],
        };

        let profile = compose_profiles([base, overlay]);
        assert_eq!(profile.plugins[0].kind, "model.b");
        assert_eq!(profile.plugins[0].config, json!({ "new": true }));
        assert_eq!(profile.plugins[1].id, "tool");
    }
}
