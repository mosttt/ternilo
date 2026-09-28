use std::{collections::BTreeMap, sync::Arc};

use linorun_core::{Activation, Component, ComponentContext, ComponentDescriptor};
use serde_json::Value;
use ternilo_protocol::{
    ApplicationCatalog, HarnessError, PluginCatalogEntry, Profile, SessionEvent,
};

pub trait HarnessPlugin: Send + Sync + 'static {
    fn descriptor(&self) -> &'static ComponentDescriptor;
    fn activate(&self, context: ComponentContext) -> Activation;
}

#[derive(Clone)]
pub struct MountedPlugin {
    inner: Arc<dyn HarnessPlugin>,
}

impl MountedPlugin {
    #[must_use]
    pub fn new(inner: Arc<dyn HarnessPlugin>) -> Self {
        Self { inner }
    }
}

impl Component for MountedPlugin {
    type Config = ();

    fn descriptor(&self) -> &'static ComponentDescriptor {
        self.inner.descriptor()
    }

    fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
        self.inner.activate(context)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginManifest {
    pub kind: &'static str,
    pub requires: &'static [&'static str],
    pub provides: &'static [&'static str],
}

pub type PluginBuilder =
    dyn Fn(Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> + Send + Sync;

#[derive(Clone)]
pub struct PluginFactory {
    pub manifest: PluginManifest,
    pub description: &'static str,
    pub config_schema: Value,
    projection_units: Vec<Arc<dyn SessionProjectionUnit>>,
    build: Arc<PluginBuilder>,
}

impl PluginFactory {
    #[must_use]
    pub fn new(
        manifest: PluginManifest,
        build: impl Fn(Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            manifest,
            description: "",
            config_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": true
            }),
            projection_units: Vec::new(),
            build: Arc::new(build),
        }
    }

    #[must_use]
    pub fn with_description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    #[must_use]
    pub fn with_config_schema<T: schemars::JsonSchema>(mut self) -> Self {
        self.config_schema = serde_json::to_value(schemars::schema_for!(T))
            .expect("schemars schemas are JSON serializable");
        self
    }

    #[must_use]
    pub fn with_projection_unit(mut self, unit: Arc<dyn SessionProjectionUnit>) -> Self {
        self.projection_units.push(unit);
        self
    }

    pub fn build(&self, config: Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
        (self.build)(config)
    }
}

/// One plugin-owned, deterministic fold over the canonical session log.
/// State and views must be plain JSON so a host may checkpoint them without
/// becoming their authority.
pub trait SessionProjectionUnit: Send + Sync + 'static {
    fn key(&self) -> &'static str;
    fn version(&self) -> u32;
    fn initial(&self) -> Value;
    fn valid_state(&self, state: &Value) -> bool;
    fn apply(&self, state: &mut Value, event: &SessionEvent) -> Result<(), HarnessError>;
    fn view(&self, state: &Value) -> Result<Value, HarnessError>;
}

pub struct Catalog {
    revision: String,
    factories: BTreeMap<&'static str, PluginFactory>,
}

impl Catalog {
    #[must_use]
    pub fn new(revision: impl Into<String>) -> Self {
        Self {
            revision: revision.into(),
            factories: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn register(&mut self, factory: PluginFactory) -> Result<(), HarnessError> {
        let kind = factory.manifest.kind;
        if kind.trim().is_empty() {
            return Err(HarnessError::composition("plugin kind must not be empty"));
        }
        if self.factories.insert(kind, factory).is_some() {
            return Err(HarnessError::composition(format!(
                "duplicate plugin factory for {kind}"
            )));
        }
        Ok(())
    }

    pub fn factory(&self, kind: &str) -> Result<&PluginFactory, HarnessError> {
        self.factories.get(kind).ok_or_else(|| {
            HarnessError::composition(format!("plugin kind {kind:?} is not in the catalog"))
        })
    }

    pub fn build(&self, kind: &str, config: Value) -> Result<MountedPlugin, HarnessError> {
        let factory = self.factory(kind)?;
        factory.build(config).map(MountedPlugin::new)
    }

    pub fn kinds(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.factories.keys().copied()
    }

    pub fn projection_units(
        &self,
        profile: &Profile,
    ) -> Result<Vec<Arc<dyn SessionProjectionUnit>>, HarnessError> {
        let mut units =
            BTreeMap::<&'static str, (&'static str, u32, Arc<dyn SessionProjectionUnit>)>::new();
        for entry in profile.plugins.iter().filter(|entry| entry.enabled) {
            let factory = self.factory(&entry.kind)?;
            for unit in &factory.projection_units {
                let key = unit.key();
                if key.is_empty()
                    || key.len() > 128
                    || !key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
                {
                    return Err(HarnessError::composition(format!(
                        "plugin {:?} declares invalid projection key {key:?}",
                        entry.kind
                    )));
                }
                match units.get(key) {
                    None => {
                        units.insert(
                            key,
                            (factory.manifest.kind, unit.version(), Arc::clone(unit)),
                        );
                    }
                    Some((kind, version, _))
                        if *kind == factory.manifest.kind && *version == unit.version() => {}
                    Some((kind, version, _)) => {
                        return Err(HarnessError::composition(format!(
                            "projection key {key:?} conflicts between {kind}@{version} and {}@{}",
                            factory.manifest.kind,
                            unit.version()
                        )));
                    }
                }
            }
        }
        Ok(units.into_values().map(|(_, _, unit)| unit).collect())
    }

    #[must_use]
    pub fn describe(&self) -> ApplicationCatalog {
        let plugins = self
            .factories
            .values()
            .map(|factory| PluginCatalogEntry {
                kind: factory.manifest.kind.to_owned(),
                description: factory.description.to_owned(),
                requires: factory
                    .manifest
                    .requires
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                provides: factory
                    .manifest
                    .provides
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                config_schema: factory.config_schema.clone(),
            })
            .collect::<Vec<_>>();
        ApplicationCatalog {
            revision: self.revision.clone(),
            plugin_kinds: plugins.iter().map(|plugin| plugin.kind.clone()).collect(),
            plugins,
            host_limits: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use ternilo_protocol::{PluginEntry, SessionEvent};

    use super::*;

    struct TestProjection {
        key: &'static str,
        version: u32,
    }

    impl SessionProjectionUnit for TestProjection {
        fn key(&self) -> &'static str {
            self.key
        }

        fn version(&self) -> u32 {
            self.version
        }

        fn initial(&self) -> Value {
            Value::Null
        }

        fn valid_state(&self, state: &Value) -> bool {
            state.is_null()
        }

        fn apply(&self, _: &mut Value, _: &SessionEvent) -> Result<(), HarnessError> {
            Ok(())
        }

        fn view(&self, state: &Value) -> Result<Value, HarnessError> {
            Ok(state.clone())
        }
    }

    fn factory(kind: &'static str, key: &'static str, version: u32) -> PluginFactory {
        PluginFactory::new(
            PluginManifest {
                kind,
                requires: &[],
                provides: &[],
            },
            |_| {
                Err(HarnessError::composition(
                    "factory is not built in this test",
                ))
            },
        )
        .with_projection_unit(Arc::new(TestProjection { key, version }))
    }

    fn entry(id: &str, kind: &str) -> PluginEntry {
        PluginEntry {
            id: id.to_owned(),
            kind: kind.to_owned(),
            enabled: true,
            config: json!({}),
        }
    }

    #[test]
    fn repeated_rows_of_one_factory_deduplicate_projection_units() {
        let mut catalog = Catalog::new("test");
        catalog.register(factory("test.one", "shared", 1)).unwrap();
        let profile = Profile {
            plugins: vec![entry("first", "test.one"), entry("second", "test.one")],
        };

        let units = catalog.projection_units(&profile).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].key(), "shared");
    }

    #[test]
    fn different_factories_cannot_claim_one_projection_key() {
        let mut catalog = Catalog::new("test");
        catalog.register(factory("test.one", "shared", 1)).unwrap();
        catalog.register(factory("test.two", "shared", 2)).unwrap();
        let profile = Profile {
            plugins: vec![entry("first", "test.one"), entry("second", "test.two")],
        };

        let Err(error) = catalog.projection_units(&profile) else {
            panic!("conflicting projection declarations must be rejected");
        };
        assert!(error.to_string().contains("conflicts between"));
    }
}
