use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use ternilo_protocol::{ExtensionProviderMaterializeRequest, HarnessError, ProviderProfile};

use crate::{
    ExtensionHostPolicy, ExtensionInstallRequest, InstalledExtension, PublisherTrust,
    TrustedPublisher, extension_payload_digest,
    package::{verify_bundle, verify_manifest_signature},
    runtime::{CompiledExtension, compile_extension},
};

const INVENTORY_SCHEMA_VERSION: u32 = 1;
const MAX_INSTALLED_EXTENSIONS: usize = 1_000;
const MAX_TRUSTED_PUBLISHERS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ExtensionInventory {
    pub publishers: Vec<TrustedPublisher>,
    pub extensions: Vec<InstalledExtension>,
}

#[derive(Clone)]
pub struct ResolvedExtension {
    pub installed: InstalledExtension,
    pub(crate) compiled: Arc<CompiledExtension>,
}

pub struct ExtensionRegistry {
    root: PathBuf,
    inventory_path: PathBuf,
    artifacts_dir: PathBuf,
    policy: ExtensionHostPolicy,
    document: Mutex<InventoryDocument>,
    compiled: Mutex<BTreeMap<String, Arc<CompiledExtension>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryDocument {
    schema_version: u32,
    publishers: Vec<TrustedPublisher>,
    extensions: Vec<InstalledExtension>,
    version_identities: Vec<ExtensionVersionIdentity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtensionVersionIdentity {
    manifest: crate::ExtensionManifest,
    signature_base64: String,
    granted_capabilities: BTreeSet<crate::Capability>,
}

impl Default for InventoryDocument {
    fn default() -> Self {
        Self {
            schema_version: INVENTORY_SCHEMA_VERSION,
            publishers: Vec::new(),
            extensions: Vec::new(),
            version_identities: Vec::new(),
        }
    }
}

impl ExtensionRegistry {
    pub fn open(root: PathBuf, policy: ExtensionHostPolicy) -> Result<Arc<Self>, HarnessError> {
        std::fs::create_dir_all(&root).map_err(|error| {
            HarnessError::execution(format!(
                "create extension registry directory {}: {error}",
                root.display()
            ))
        })?;
        let artifacts_dir = root.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).map_err(|error| {
            HarnessError::execution(format!(
                "create extension artifact directory {}: {error}",
                artifacts_dir.display()
            ))
        })?;
        let inventory_path = root.join("inventory.json");
        let document = match std::fs::read(&inventory_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                HarnessError::execution(format!(
                    "parse extension inventory {}: {error}",
                    inventory_path.display()
                ))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                InventoryDocument::default()
            }
            Err(error) => {
                return Err(HarnessError::execution(format!(
                    "read extension inventory {}: {error}",
                    inventory_path.display()
                )));
            }
        };
        validate_document(&document, &policy, &artifacts_dir)?;
        let registry = Arc::new(Self {
            root,
            inventory_path,
            artifacts_dir,
            policy,
            document: Mutex::new(document),
            compiled: Mutex::new(BTreeMap::new()),
        });
        if !registry.inventory_path.exists() {
            registry.persist(&InventoryDocument::default())?;
        }
        Ok(registry)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn inventory(&self) -> Result<ExtensionInventory, HarnessError> {
        let document = self.lock_document()?;
        Ok(ExtensionInventory {
            publishers: document.publishers.clone(),
            extensions: document.extensions.clone(),
        })
    }

    pub fn materialize_provider(
        &self,
        request: &ExtensionProviderMaterializeRequest,
    ) -> Result<ProviderProfile, HarnessError> {
        materialize_provider_from_inventory(&self.inventory()?, request)
    }

    pub fn trust_publisher(
        &self,
        trust: PublisherTrust,
        now_ms: u64,
    ) -> Result<TrustedPublisher, HarnessError> {
        trust.validate()?;
        let mut document = self.lock_document()?;
        if let Some(existing) = document
            .publishers
            .iter()
            .find(|publisher| publisher.trust.key_id == trust.key_id)
        {
            if existing.trust == trust && !existing.revoked {
                return Ok(existing.clone());
            }
            return Err(HarnessError::policy(
                "publisher key ids are immutable and revoked ids cannot be reused",
            ));
        }
        if document.publishers.len() >= MAX_TRUSTED_PUBLISHERS {
            return Err(HarnessError::policy(
                "trusted extension publisher inventory is full",
            ));
        }
        let publisher = TrustedPublisher {
            trust,
            revoked: false,
            added_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        let mut next = document.clone();
        next.publishers.push(publisher.clone());
        sort_document(&mut next);
        self.persist(&next)?;
        *document = next;
        Ok(publisher)
    }

    pub fn revoke_publisher(&self, key_id: &str, now_ms: u64) -> Result<(), HarnessError> {
        let mut document = self.lock_document()?;
        let mut next = document.clone();
        let publisher = next
            .publishers
            .iter_mut()
            .find(|publisher| publisher.trust.key_id == key_id)
            .ok_or_else(|| HarnessError::invalid("extension publisher does not exist"))?;
        publisher.revoked = true;
        publisher.updated_at_ms = now_ms;
        for extension in next
            .extensions
            .iter_mut()
            .filter(|extension| extension.manifest.publisher_key_id == key_id)
        {
            extension.enabled = false;
            extension.revoked = true;
            extension.updated_at_ms = now_ms;
        }
        self.persist(&next)?;
        *document = next;
        self.lock_compiled()?.clear();
        Ok(())
    }

    pub fn install(
        &self,
        request: ExtensionInstallRequest,
        now_ms: u64,
    ) -> Result<InstalledExtension, HarnessError> {
        self.policy
            .validate_install(&request.bundle.manifest, &request.granted_capabilities)?;
        let mut document = self.lock_document()?;
        let publisher = document
            .publishers
            .iter()
            .find(|publisher| {
                publisher.trust.key_id == request.bundle.manifest.publisher_key_id
                    && !publisher.revoked
            })
            .ok_or_else(|| {
                HarnessError::policy("extension publisher is not trusted or was revoked")
            })?;
        let bytes = verify_bundle(
            &request.bundle,
            &publisher.trust,
            self.policy.max_payload_bytes,
        )?;
        let version_identity = ExtensionVersionIdentity {
            manifest: request.bundle.manifest.clone(),
            signature_base64: request.bundle.signature_base64.clone(),
            granted_capabilities: request.granted_capabilities.clone(),
        };
        let manifest = request.bundle.manifest;
        if let Some(existing) = document.version_identities.iter().find(|identity| {
            identity.manifest.package_id == manifest.package_id
                && identity.manifest.version == manifest.version
        }) && existing != &version_identity
        {
            return Err(HarnessError::policy(
                "extension package id and version are permanently bound to their first signed bundle and capability grants",
            ));
        }
        let compiled = compile_manifest(&manifest, &bytes)?;
        let key = package_key(&manifest.package_id, &manifest.version);
        if let Some(existing) = document.extensions.iter().find(|extension| {
            extension.manifest.package_id == manifest.package_id
                && extension.manifest.version == manifest.version
        }) {
            if existing.manifest == manifest
                && existing.granted_capabilities == request.granted_capabilities
                && !existing.revoked
            {
                return Ok(existing.clone());
            }
            return Err(HarnessError::policy(
                "a different or revoked extension already owns this package id and version",
            ));
        }
        if document.extensions.len() >= MAX_INSTALLED_EXTENSIONS {
            return Err(HarnessError::policy("extension inventory is full"));
        }
        self.persist_artifact(&manifest.payload_sha256, &bytes)?;
        let installed = InstalledExtension {
            manifest,
            granted_capabilities: request.granted_capabilities,
            enabled: true,
            revoked: false,
            installed_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        let mut next = document.clone();
        if !next.version_identities.iter().any(|identity| {
            identity.manifest.package_id == version_identity.manifest.package_id
                && identity.manifest.version == version_identity.manifest.version
        }) {
            next.version_identities.push(version_identity);
        }
        next.extensions.push(installed.clone());
        sort_document(&mut next);
        self.persist(&next)?;
        *document = next;
        self.lock_compiled()?.insert(key, compiled);
        Ok(installed)
    }

    pub fn describe(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<InstalledExtension, HarnessError> {
        self.lock_document()?
            .extensions
            .iter()
            .find(|extension| {
                extension.manifest.package_id == package_id && extension.manifest.version == version
            })
            .cloned()
            .ok_or_else(|| HarnessError::composition("extension package is not installed"))
    }

    pub fn validate_mount_settings(
        &self,
        package_id: &str,
        version: &str,
        settings: &serde_json::Value,
    ) -> Result<(), HarnessError> {
        let installed = self.resolve(package_id, version)?;
        crate::package::validate_settings(&installed.installed.manifest.config_schema, settings)
    }

    pub fn validate_profile_mounts(
        &self,
        profile: &ternilo_protocol::Profile,
    ) -> Result<(), HarnessError> {
        let mounts = crate::unique_extension_mounts(profile)?;
        let mut installed = Vec::with_capacity(mounts.len());
        for mount in mounts {
            let resolved = self.resolve(&mount.package_id, &mount.version)?;
            crate::validate_extension_settings(&resolved.installed.manifest, &mount.settings)?;
            installed.push(resolved.installed);
        }
        crate::validate_extension_tool_name_uniqueness(
            installed.iter().map(|extension| &extension.manifest),
        )?;
        crate::validate_extension_command_name_uniqueness(
            installed.iter().map(|extension| &extension.manifest),
        )
    }

    pub fn resolve(
        &self,
        package_id: &str,
        version: &str,
    ) -> Result<ResolvedExtension, HarnessError> {
        let installed = {
            let document = self.lock_document()?;
            let extension = document
                .extensions
                .iter()
                .find(|extension| {
                    extension.manifest.package_id == package_id
                        && extension.manifest.version == version
                })
                .ok_or_else(|| HarnessError::composition("extension package is not installed"))?;
            if !extension.enabled || extension.revoked {
                return Err(HarnessError::policy(
                    "extension package is disabled or has been revoked",
                ));
            }
            if !document.publishers.iter().any(|publisher| {
                publisher.trust.key_id == extension.manifest.publisher_key_id && !publisher.revoked
            }) {
                return Err(HarnessError::policy(
                    "extension publisher is no longer trusted",
                ));
            }
            extension.clone()
        };
        let key = package_key(package_id, version);
        if let Some(compiled) = self.lock_compiled()?.get(&key).cloned() {
            return Ok(ResolvedExtension {
                installed,
                compiled,
            });
        }
        let bytes = self.read_artifact(&installed.manifest.payload_sha256)?;
        let compiled = compile_manifest(&installed.manifest, &bytes)?;
        self.lock_compiled()?.insert(key, Arc::clone(&compiled));
        Ok(ResolvedExtension {
            installed,
            compiled,
        })
    }

    pub fn set_enabled(
        &self,
        package_id: &str,
        version: &str,
        enabled: bool,
        now_ms: u64,
    ) -> Result<InstalledExtension, HarnessError> {
        let mut document = self.lock_document()?;
        let mut next = document.clone();
        let index = next
            .extensions
            .iter()
            .position(|extension| {
                extension.manifest.package_id == package_id && extension.manifest.version == version
            })
            .ok_or_else(|| HarnessError::invalid("extension package does not exist"))?;
        if enabled {
            let extension = &next.extensions[index];
            if extension.revoked
                || !next.publishers.iter().any(|publisher| {
                    publisher.trust.key_id == extension.manifest.publisher_key_id
                        && !publisher.revoked
                })
            {
                return Err(HarnessError::policy(
                    "revoked extensions or publishers cannot be re-enabled",
                ));
            }
            let bytes = self.read_artifact(&extension.manifest.payload_sha256)?;
            let compiled = compile_manifest(&extension.manifest, &bytes)?;
            self.lock_compiled()?
                .insert(package_key(package_id, version), compiled);
        }
        let extension = &mut next.extensions[index];
        extension.enabled = enabled;
        extension.updated_at_ms = now_ms;
        let result = extension.clone();
        self.persist(&next)?;
        *document = next;
        Ok(result)
    }

    pub fn revoke(&self, package_id: &str, version: &str, now_ms: u64) -> Result<(), HarnessError> {
        let mut document = self.lock_document()?;
        let mut next = document.clone();
        let extension = find_extension_mut(&mut next, package_id, version)?;
        extension.enabled = false;
        extension.revoked = true;
        extension.updated_at_ms = now_ms;
        self.persist(&next)?;
        *document = next;
        self.lock_compiled()?
            .remove(&package_key(package_id, version));
        Ok(())
    }

    pub fn uninstall(&self, package_id: &str, version: &str) -> Result<(), HarnessError> {
        let mut document = self.lock_document()?;
        let index = document
            .extensions
            .iter()
            .position(|extension| {
                extension.manifest.package_id == package_id && extension.manifest.version == version
            })
            .ok_or_else(|| HarnessError::invalid("extension package does not exist"))?;
        if document.extensions[index].revoked {
            return Err(HarnessError::policy(
                "revoked extension packages are permanent tombstones and cannot be uninstalled",
            ));
        }
        let digest = document.extensions[index].manifest.payload_sha256.clone();
        let mut next = document.clone();
        next.extensions.remove(index);
        self.persist(&next)?;
        let shared = next
            .extensions
            .iter()
            .any(|extension| extension.manifest.payload_sha256 == digest);
        *document = next;
        self.lock_compiled()?
            .remove(&package_key(package_id, version));
        if !shared {
            let path = self.artifacts_dir.join(format!("{digest}.payload"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(HarnessError::execution(format!(
                        "remove extension artifact {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    fn persist_artifact(&self, digest: &str, bytes: &[u8]) -> Result<(), HarnessError> {
        let path = self.artifacts_dir.join(format!("{digest}.payload"));
        if path.exists() {
            let current = std::fs::read(&path).map_err(|error| {
                HarnessError::execution(format!("read existing extension artifact: {error}"))
            })?;
            if extension_payload_digest(&current) != digest {
                return Err(HarnessError::policy(
                    "content-addressed extension artifact contains unexpected bytes",
                ));
            }
            return Ok(());
        }
        atomic_write(&path, bytes)
    }

    fn read_artifact(&self, digest: &str) -> Result<Vec<u8>, HarnessError> {
        let path = self.artifacts_dir.join(format!("{digest}.payload"));
        let bytes = std::fs::read(&path).map_err(|error| {
            HarnessError::execution(format!(
                "read extension artifact {}: {error}",
                path.display()
            ))
        })?;
        if extension_payload_digest(&bytes) != digest {
            return Err(HarnessError::policy(
                "content-addressed extension artifact failed digest verification",
            ));
        }
        Ok(bytes)
    }

    fn persist(&self, document: &InventoryDocument) -> Result<(), HarnessError> {
        let bytes = serde_json::to_vec_pretty(document).map_err(|error| {
            HarnessError::execution(format!("encode extension inventory: {error}"))
        })?;
        atomic_write(&self.inventory_path, &bytes)
    }

    fn lock_document(&self) -> Result<std::sync::MutexGuard<'_, InventoryDocument>, HarnessError> {
        self.document
            .lock()
            .map_err(|_| HarnessError::execution("extension inventory lock poisoned"))
    }

    fn lock_compiled(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, Arc<CompiledExtension>>>, HarnessError>
    {
        self.compiled
            .lock()
            .map_err(|_| HarnessError::execution("extension compile cache lock poisoned"))
    }
}

pub fn materialize_provider_from_inventory(
    inventory: &ExtensionInventory,
    request: &ExtensionProviderMaterializeRequest,
) -> Result<ProviderProfile, HarnessError> {
    request.validate()?;
    let installed = inventory
        .extensions
        .iter()
        .find(|extension| {
            extension.manifest.package_id == request.package_id
                && extension.manifest.version == request.version
        })
        .ok_or_else(|| HarnessError::invalid("extension package is not installed"))?;
    if !installed.enabled || installed.revoked {
        return Err(HarnessError::policy(
            "extension package is disabled or has been revoked",
        ));
    }
    if !inventory.publishers.iter().any(|publisher| {
        publisher.trust.key_id == installed.manifest.publisher_key_id && !publisher.revoked
    }) {
        return Err(HarnessError::policy(
            "extension publisher is no longer trusted",
        ));
    }
    installed
        .manifest
        .contributions
        .providers
        .iter()
        .find(|provider| provider.id == request.template)
        .ok_or_else(|| {
            HarnessError::invalid(format!(
                "extension package has no Provider template {:?}",
                request.template
            ))
        })?
        .materialize(request)
}

fn compile_manifest(
    manifest: &crate::ExtensionManifest,
    bytes: &[u8],
) -> Result<Arc<CompiledExtension>, HarnessError> {
    compile_extension(manifest, bytes).map(Arc::new)
}

fn validate_document(
    document: &InventoryDocument,
    policy: &ExtensionHostPolicy,
    artifacts_dir: &Path,
) -> Result<(), HarnessError> {
    if document.schema_version != INVENTORY_SCHEMA_VERSION {
        return Err(HarnessError::invalid(format!(
            "unsupported extension inventory schema {}; expected {INVENTORY_SCHEMA_VERSION}",
            document.schema_version
        )));
    }
    if document.publishers.len() > MAX_TRUSTED_PUBLISHERS
        || document.extensions.len() > MAX_INSTALLED_EXTENSIONS
    {
        return Err(HarnessError::policy(
            "extension inventory exceeds host limits",
        ));
    }
    let mut publishers = BTreeMap::new();
    for publisher in &document.publishers {
        publisher.trust.validate()?;
        if publishers
            .insert(publisher.trust.key_id.as_str(), publisher)
            .is_some()
        {
            return Err(HarnessError::invalid(
                "extension inventory contains duplicate publisher key ids",
            ));
        }
    }
    let mut packages = BTreeSet::new();
    let mut version_identities = BTreeMap::new();
    for identity in &document.version_identities {
        validate_version_identity(identity, &publishers)?;
        let key = package_key(&identity.manifest.package_id, &identity.manifest.version);
        if version_identities.insert(key, identity).is_some() {
            return Err(HarnessError::invalid(
                "extension inventory contains duplicate version identities",
            ));
        }
    }
    for extension in &document.extensions {
        policy.validate_install(&extension.manifest, &extension.granted_capabilities)?;
        let publisher = publishers
            .get(extension.manifest.publisher_key_id.as_str())
            .ok_or_else(|| HarnessError::policy("extension publisher is not in inventory"))?;
        if (publisher.revoked || extension.revoked) && extension.enabled {
            return Err(HarnessError::policy(
                "revoked extension inventory entries cannot be enabled",
            ));
        }
        if !publisher
            .trust
            .allowed_sources
            .contains(&extension.manifest.source)
        {
            return Err(HarnessError::policy(
                "installed extension source no longer matches its trust root",
            ));
        }
        if !packages.insert(package_key(
            &extension.manifest.package_id,
            &extension.manifest.version,
        )) {
            return Err(HarnessError::invalid(
                "extension inventory contains duplicate package versions",
            ));
        }
        let identity = version_identities
            .get(&package_key(
                &extension.manifest.package_id,
                &extension.manifest.version,
            ))
            .ok_or_else(|| {
                HarnessError::invalid("installed extension is missing its version identity")
            })?;
        if identity.manifest != extension.manifest
            || identity.granted_capabilities != extension.granted_capabilities
        {
            return Err(HarnessError::policy(
                "installed extension does not match its permanent version identity",
            ));
        }
        let path = artifacts_dir.join(format!("{}.payload", extension.manifest.payload_sha256));
        let bytes = std::fs::read(&path).map_err(|error| {
            HarnessError::execution(format!(
                "read installed extension artifact {}: {error}",
                path.display()
            ))
        })?;
        if extension_payload_digest(&bytes) != extension.manifest.payload_sha256 {
            return Err(HarnessError::policy(
                "installed extension artifact failed digest verification",
            ));
        }
    }
    Ok(())
}

fn validate_version_identity(
    identity: &ExtensionVersionIdentity,
    publishers: &BTreeMap<&str, &TrustedPublisher>,
) -> Result<(), HarnessError> {
    identity.manifest.validate()?;
    if !identity
        .granted_capabilities
        .is_subset(&identity.manifest.requested_capabilities)
    {
        return Err(HarnessError::policy(
            "extension version identity contains capabilities not requested by its manifest",
        ));
    }
    let publisher = publishers
        .get(identity.manifest.publisher_key_id.as_str())
        .ok_or_else(|| HarnessError::policy("extension publisher is not in inventory"))?;
    if !publisher
        .trust
        .allowed_sources
        .contains(&identity.manifest.source)
    {
        return Err(HarnessError::policy(
            "extension version identity source no longer matches its trust root",
        ));
    }
    verify_manifest_signature(
        &identity.manifest,
        &identity.signature_base64,
        &publisher.trust,
    )?;
    Ok(())
}

fn find_extension_mut<'a>(
    document: &'a mut InventoryDocument,
    package_id: &str,
    version: &str,
) -> Result<&'a mut InstalledExtension, HarnessError> {
    document
        .extensions
        .iter_mut()
        .find(|extension| {
            extension.manifest.package_id == package_id && extension.manifest.version == version
        })
        .ok_or_else(|| HarnessError::invalid("extension package does not exist"))
}

fn package_key(package_id: &str, version: &str) -> String {
    format!("{package_id}@{version}")
}

fn sort_document(document: &mut InventoryDocument) {
    document.publishers.sort_by(|left, right| {
        left.trust
            .key_id
            .cmp(&right.trust.key_id)
            .then_with(|| left.added_at_ms.cmp(&right.added_at_ms))
    });
    document.extensions.sort_by(|left, right| {
        left.manifest
            .package_id
            .cmp(&right.manifest.package_id)
            .then_with(|| left.manifest.version.cmp(&right.manifest.version))
    });
    document.version_identities.sort_by(|left, right| {
        left.manifest
            .package_id
            .cmp(&right.manifest.package_id)
            .then_with(|| left.manifest.version.cmp(&right.manifest.version))
    });
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), HarnessError> {
    let parent = path
        .parent()
        .ok_or_else(|| HarnessError::execution("extension persistence path has no parent"))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| {
        HarnessError::execution(format!("create temporary extension file: {error}"))
    })?;
    temporary.write_all(bytes).map_err(|error| {
        HarnessError::execution(format!("write temporary extension file: {error}"))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        HarnessError::execution(format!("sync temporary extension file: {error}"))
    })?;
    temporary.persist(path).map_err(|error| {
        HarnessError::execution(format!("commit extension file {}: {error}", path.display()))
    })?;
    OpenOptions::new()
        .read(true)
        .open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| HarnessError::execution(format!("sync extension directory: {error}")))
}
