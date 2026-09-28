#![forbid(unsafe_code)]

mod package;
mod plugin;
mod registry;
mod runtime;

wasmtime::component::bindgen!({
    path: "wit",
    world: "runtime",
});

pub use package::{
    Capability, ExtensionCommandContribution, ExtensionCommandInput, ExtensionContributions,
    ExtensionDistribution, ExtensionHookContribution, ExtensionHookMatcher, ExtensionHostPolicy,
    ExtensionInstallRequest, ExtensionManifest, ExtensionPayload,
    ExtensionPromptSectionContribution, ExtensionProviderContribution, ExtensionProviderCredential,
    ExtensionRuntime, ExtensionSkillContribution, ExtensionToolContribution, ExtensionToolEffect,
    InstalledExtension, PublisherTrust, RhaiExecutionLimits, SignedExtensionBundle,
    TrustedPublisher, WasmComponentLimits, extension_payload_digest, sign_bundle,
    validate_extension_command_name_uniqueness, validate_extension_settings,
    validate_extension_tool_name_uniqueness, verify_bundle,
};
pub use plugin::{
    ExtensionMount, extension_mount_factory, extension_mounts, unique_extension_mounts,
};
pub use registry::{
    ExtensionInventory, ExtensionRegistry, ResolvedExtension, materialize_provider_from_inventory,
};

pub const EXTENSION_PACKAGE_KIND: &str = "ternilo.extension.package";
pub const EXTENSION_PACKAGE_SCHEMA_VERSION: u32 = 1;
pub const WASM_COMPONENT_RUNTIME_WORLD: &str = "ternilo:extension/runtime@1.0.0";

#[cfg(test)]
mod tests;
