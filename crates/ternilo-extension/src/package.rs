use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ternilo_protocol::{
    ExtensionProviderMaterializeRequest, HarnessError, HookPoint, ProviderModel,
    ProviderModelDefaults, ProviderProfile, ProviderProtocol, SkillInvocationPolicy,
    ToolPresentationDescriptor, ToolSpec,
};
use ternilo_rhai::RhaiSandboxLimits;

use crate::EXTENSION_PACKAGE_SCHEMA_VERSION;

pub const MAX_EXTENSION_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const SIGNATURE_DOMAIN: &[u8] = b"TERNILO-EXTENSION-PACKAGE-V1\0";
const MAX_PROMPT_SECTION_CONTENT_BYTES: usize = 128 * 1024;
const MAX_SKILL_CONTENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SKILLS_PER_PACKAGE: usize = 2_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Log,
    WorkspaceRead,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionToolEffect {
    ReadOnly,
    Mutating,
    Dangerous,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RhaiExecutionLimits {
    #[serde(flatten)]
    pub sandbox: RhaiSandboxLimits,
    pub max_wall_ms: u64,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_workspace_read_bytes: usize,
}

impl Default for RhaiExecutionLimits {
    fn default() -> Self {
        Self {
            sandbox: RhaiSandboxLimits::default(),
            max_wall_ms: 60_000,
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 4 * 1024 * 1024,
            max_workspace_read_bytes: 2 * 1024 * 1024,
        }
    }
}

impl RhaiExecutionLimits {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.sandbox.validate()?;
        if self.max_wall_ms == 0
            || self.max_input_bytes == 0
            || self.max_output_bytes == 0
            || self.max_workspace_read_bytes == 0
        {
            return Err(HarnessError::invalid(
                "Rhai wall, input, output, and workspace-read limits must be positive",
            ));
        }
        Ok(())
    }

    pub fn validate_within(&self, maximum: &Self) -> Result<(), HarnessError> {
        self.validate()?;
        maximum.validate()?;
        self.sandbox.validate_within(&maximum.sandbox)?;
        if self.max_wall_ms > maximum.max_wall_ms
            || self.max_input_bytes > maximum.max_input_bytes
            || self.max_output_bytes > maximum.max_output_bytes
            || self.max_workspace_read_bytes > maximum.max_workspace_read_bytes
        {
            return Err(HarnessError::policy(
                "Rhai extension limits exceed the host policy ceiling",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WasmComponentLimits {
    pub fuel: u64,
    pub max_memory_bytes: u64,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_workspace_read_bytes: u64,
}

impl WasmComponentLimits {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.fuel == 0
            || self.max_memory_bytes < 64 * 1024
            || self.max_input_bytes == 0
            || self.max_output_bytes == 0
            || self.max_workspace_read_bytes == 0
        {
            return Err(HarnessError::invalid(
                "WASM Component limits require positive values and at least 64 KiB memory",
            ));
        }
        Ok(())
    }

    pub fn validate_within(&self, maximum: &Self) -> Result<(), HarnessError> {
        self.validate()?;
        maximum.validate()?;
        if self.fuel > maximum.fuel
            || self.max_memory_bytes > maximum.max_memory_bytes
            || self.max_input_bytes > maximum.max_input_bytes
            || self.max_output_bytes > maximum.max_output_bytes
            || self.max_workspace_read_bytes > maximum.max_workspace_read_bytes
        {
            return Err(HarnessError::policy(
                "WASM Component limits exceed the host policy ceiling",
            ));
        }
        Ok(())
    }
}

impl Default for WasmComponentLimits {
    fn default() -> Self {
        Self {
            fuel: 20_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024,
            max_workspace_read_bytes: 2 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ExtensionRuntime {
    Rhai {
        limits: RhaiExecutionLimits,
    },
    WasmComponent {
        world: String,
        limits: WasmComponentLimits,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "content", rename_all = "lowercase")]
pub enum ExtensionPayload {
    Utf8(String),
    Base64(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionToolContribution {
    pub handler: String,
    pub spec: ToolSpec,
    pub output_schema: Value,
    pub effect: ExtensionToolEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ToolPresentationDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionPromptSectionContribution {
    pub id: String,
    pub order: i32,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionSkillContribution {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub invocation: SkillInvocationPolicy,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionHookMatcher {
    All {},
    ToolNames { names: Vec<String> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHookContribution {
    pub id: String,
    pub point: HookPoint,
    pub handler: String,
    pub matcher: ExtensionHookMatcher,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionCommandInput {
    pub hint: String,
    pub field: String,
    #[serde(default)]
    pub images: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionCommandContribution {
    pub name: String,
    pub description: String,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ExtensionCommandInput>,
    pub fixed_arguments: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProviderCredential {
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProviderContribution {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    pub defaults: ProviderModelDefaults,
    pub models: Vec<ProviderModel>,
    pub timeout_ms: u64,
    pub max_attempts: u32,
    pub retry_base_delay_ms: u64,
    pub credential: ExtensionProviderCredential,
}

impl ExtensionProviderContribution {
    fn validate(&self) -> Result<(), HarnessError> {
        ProviderProfile {
            hosted_tools: None,
            id: self.id.clone(),
            display_name: self.display_name.clone(),
            base_url: self.base_url.clone(),
            protocol: self.protocol,
            api_key_ref: self.credential.suggested_ref.clone(),
            defaults: self.defaults.clone(),
            models: self.models.clone(),
            timeout_ms: self.timeout_ms,
            max_attempts: self.max_attempts,
            retry_base_delay_ms: self.retry_base_delay_ms,
        }
        .validate()
    }

    pub(crate) fn materialize(
        &self,
        request: &ExtensionProviderMaterializeRequest,
    ) -> Result<ProviderProfile, HarnessError> {
        if self.credential.required && request.api_key_ref.is_none() {
            return Err(HarnessError::invalid(format!(
                "extension Provider template {:?} requires an explicit api_key_ref",
                self.id
            )));
        }
        let provider = ProviderProfile {
            hosted_tools: None,
            id: request.provider_id.clone(),
            display_name: self.display_name.clone(),
            base_url: self.base_url.clone(),
            protocol: self.protocol,
            api_key_ref: request.api_key_ref.clone(),
            defaults: self.defaults.clone(),
            models: self.models.clone(),
            timeout_ms: self.timeout_ms,
            max_attempts: self.max_attempts,
            retry_base_delay_ms: self.retry_base_delay_ms,
        };
        provider.validate()?;
        Ok(provider)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionContributions {
    pub tools: Vec<ExtensionToolContribution>,
    pub prompt_sections: Vec<ExtensionPromptSectionContribution>,
    pub skills: Vec<ExtensionSkillContribution>,
    pub hooks: Vec<ExtensionHookContribution>,
    pub commands: Vec<ExtensionCommandContribution>,
    pub providers: Vec<ExtensionProviderContribution>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub schema_version: u32,
    pub package_id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: String,
    pub publisher_key_id: String,
    pub payload_sha256: String,
    pub runtime: ExtensionRuntime,
    pub config_schema: Value,
    pub contributions: ExtensionContributions,
    #[serde(default)]
    pub requested_capabilities: BTreeSet<Capability>,
}

impl ExtensionManifest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.schema_version != EXTENSION_PACKAGE_SCHEMA_VERSION {
            return Err(HarnessError::invalid(format!(
                "unsupported extension package schema {}; expected {EXTENSION_PACKAGE_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        validate_identifier(&self.package_id, "extension package id")?;
        Version::parse(&self.version).map_err(|error| {
            HarnessError::invalid(format!("invalid extension version: {error}"))
        })?;
        if self
            .description
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > 1_000)
        {
            return Err(HarnessError::invalid(
                "extension description must contain 1 to 1000 bytes when present",
            ));
        }
        validate_identifier(&self.publisher_key_id, "publisher key id")?;
        if self.source.trim().is_empty() || self.source.len() > 2_048 {
            return Err(HarnessError::invalid(
                "extension source must contain 1 to 2048 bytes",
            ));
        }
        validate_digest(&self.payload_sha256)?;
        validate_object_schema(&self.config_schema, "extension config schema")?;
        match &self.runtime {
            ExtensionRuntime::Rhai { limits } => limits.validate()?,
            ExtensionRuntime::WasmComponent { world, limits } => {
                if world != crate::WASM_COMPONENT_RUNTIME_WORLD {
                    return Err(HarnessError::invalid(format!(
                        "unsupported WASM Component world {world:?}; expected {:?}",
                        crate::WASM_COMPONENT_RUNTIME_WORLD
                    )));
                }
                limits.validate()?;
            }
        }
        self.contributions.validate()
    }
}

impl ExtensionContributions {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.tools.is_empty()
            && self.prompt_sections.is_empty()
            && self.skills.is_empty()
            && self.hooks.is_empty()
            && self.commands.is_empty()
            && self.providers.is_empty()
        {
            return Err(HarnessError::invalid(
                "extension package must contribute at least one tool, prompt section, skill, hook, command, or provider",
            ));
        }
        self.validate_tools()?;
        self.validate_prompt_sections()?;
        self.validate_skills()?;
        self.validate_hooks()?;
        self.validate_commands()?;
        self.validate_providers()?;
        Ok(())
    }

    fn validate_tools(&self) -> Result<(), HarnessError> {
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            validate_identifier(&tool.handler, "extension tool handler")?;
            if tool.spec.name.trim().is_empty()
                || tool.spec.name.len() > 128
                || tool.spec.description.trim().is_empty()
            {
                return Err(HarnessError::invalid(
                    "extension tools require a bounded name and non-empty description",
                ));
            }
            if !names.insert(tool.spec.name.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate tool {:?}",
                    tool.spec.name
                )));
            }
            validate_object_schema(&tool.spec.input_schema, "tool input schema")?;
            validate_json_schema(&tool.output_schema, "tool output schema")?;
        }
        Ok(())
    }

    fn validate_prompt_sections(&self) -> Result<(), HarnessError> {
        let mut prompt_ids = BTreeSet::new();
        for section in &self.prompt_sections {
            validate_identifier(&section.id, "extension prompt section id")?;
            if section.content.trim().is_empty()
                || section.content.len() > MAX_PROMPT_SECTION_CONTENT_BYTES
            {
                return Err(HarnessError::invalid(format!(
                    "extension prompt section content must contain 1 to {MAX_PROMPT_SECTION_CONTENT_BYTES} bytes"
                )));
            }
            if !prompt_ids.insert(section.id.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate prompt section {:?}",
                    section.id
                )));
            }
        }
        Ok(())
    }

    fn validate_skills(&self) -> Result<(), HarnessError> {
        if self.skills.len() > MAX_SKILLS_PER_PACKAGE {
            return Err(HarnessError::invalid(format!(
                "extension package may contribute at most {MAX_SKILLS_PER_PACKAGE} skills"
            )));
        }
        let mut skill_names = BTreeSet::new();
        for skill in &self.skills {
            validate_skill_name(&skill.name)?;
            if skill.description.trim().is_empty()
                || skill.description.chars().count() > 2_000
                || skill
                    .when_to_use
                    .as_ref()
                    .is_some_and(|value| value.chars().count() > 4_000)
            {
                return Err(HarnessError::invalid(
                    "extension skill contains an invalid description or routing hint",
                ));
            }
            if skill.content.trim().is_empty() || skill.content.len() > MAX_SKILL_CONTENT_BYTES {
                return Err(HarnessError::invalid(
                    "extension skill content must contain 1 to 2097152 bytes",
                ));
            }
            if !skill_names.insert(skill.name.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate skill {:?}",
                    skill.name
                )));
            }
        }
        Ok(())
    }

    fn validate_hooks(&self) -> Result<(), HarnessError> {
        let mut hook_ids = BTreeSet::new();
        for hook in &self.hooks {
            validate_identifier(&hook.id, "extension hook id")?;
            validate_identifier(&hook.handler, "extension hook handler")?;
            if !hook_ids.insert(hook.id.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate hook {:?}",
                    hook.id
                )));
            }
            if let ExtensionHookMatcher::ToolNames { names } = &hook.matcher {
                if !matches!(hook.point, HookPoint::PreToolUse | HookPoint::PostToolUse) {
                    return Err(HarnessError::invalid(
                        "extension hook tool_names matcher is only valid for pre_tool_use or post_tool_use",
                    ));
                }
                if names.is_empty() {
                    return Err(HarnessError::invalid(
                        "extension hook tool_names matcher requires at least one tool name",
                    ));
                }
                let mut matched_names = BTreeSet::new();
                for name in names {
                    if name.trim().is_empty()
                        || name.len() > 128
                        || !matched_names.insert(name.as_str())
                    {
                        return Err(HarnessError::invalid(
                            "extension hook tool_names must contain unique bounded names",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_commands(&self) -> Result<(), HarnessError> {
        let tool_names = self
            .tools
            .iter()
            .map(|tool| tool.spec.name.as_str())
            .collect::<BTreeSet<_>>();
        let mut command_names = BTreeSet::new();
        for command in &self.commands {
            validate_command_name(&command.name)?;
            if matches!(command.name.as_str(), "feedback" | "plan" | "skill") {
                return Err(HarnessError::invalid(format!(
                    "extension command {:?} is reserved",
                    command.name
                )));
            }
            if !command_names.insert(command.name.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate command {:?}",
                    command.name
                )));
            }
            if command.description.trim().is_empty() || command.description.chars().count() > 1_000
            {
                return Err(HarnessError::invalid(
                    "extension command description must contain 1 to 1000 characters",
                ));
            }
            if !tool_names.contains(command.tool.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension command {:?} references tool {:?} outside its package",
                    command.name, command.tool
                )));
            }
            let Some(fixed_arguments) = command.fixed_arguments.as_object() else {
                return Err(HarnessError::invalid(
                    "extension command fixed_arguments must be an object",
                ));
            };
            if let Some(input) = &command.input {
                if input.hint.trim().is_empty()
                    || input.hint.chars().count() > 200
                    || input.field.trim().is_empty()
                    || input.field.chars().count() > 128
                    || input.field.contains('.')
                    || fixed_arguments.contains_key(&input.field)
                {
                    return Err(HarnessError::invalid(
                        "extension command input requires a bounded top-level field that is not fixed",
                    ));
                }
                if input.images {
                    return Err(HarnessError::invalid(
                        "extension command input does not support images",
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_providers(&self) -> Result<(), HarnessError> {
        let mut provider_ids = BTreeSet::new();
        for provider in &self.providers {
            provider.validate()?;
            if !provider_ids.insert(provider.id.as_str()) {
                return Err(HarnessError::invalid(format!(
                    "extension package contains duplicate provider template {:?}",
                    provider.id
                )));
            }
        }
        Ok(())
    }
}

fn validate_command_name(name: &str) -> Result<(), HarnessError> {
    let mut bytes = name.bytes();
    if name.len() > 64
        || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Err(HarnessError::invalid(
            "extension command name must start with a lowercase letter and use lowercase letters, digits, or dash",
        ))
    } else {
        Ok(())
    }
}

fn validate_skill_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty()
        || name.len() > 128
        || name.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        Err(HarnessError::invalid(
            "extension skill name must be a 1 to 128 byte lowercase kebab-case identifier",
        ))
    } else {
        Ok(())
    }
}

fn validate_object_schema(value: &Value, label: &str) -> Result<(), HarnessError> {
    let Some(schema) = value.as_object() else {
        return Err(HarnessError::invalid(format!("{label} must be an object")));
    };
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(HarnessError::invalid(format!(
            "{label} must declare type object"
        )));
    }
    jsonschema::validator_for(value).map_err(|error| {
        HarnessError::invalid(format!("{label} is not valid JSON Schema: {error}"))
    })?;
    Ok(())
}

fn validate_json_schema(value: &Value, label: &str) -> Result<(), HarnessError> {
    if !value.is_object() {
        return Err(HarnessError::invalid(format!(
            "{label} must be a JSON Schema object"
        )));
    }
    jsonschema::validator_for(value).map_err(|error| {
        HarnessError::invalid(format!("{label} is not valid JSON Schema: {error}"))
    })?;
    Ok(())
}

pub fn validate_extension_settings(
    manifest: &ExtensionManifest,
    settings: &Value,
) -> Result<(), HarnessError> {
    validate_settings(&manifest.config_schema, settings)
}

pub fn validate_extension_tool_name_uniqueness<'a>(
    manifests: impl IntoIterator<Item = &'a ExtensionManifest>,
) -> Result<(), HarnessError> {
    let mut owners = BTreeMap::<&str, String>::new();
    for manifest in manifests {
        let package = format!("{}@{}", manifest.package_id, manifest.version);
        for tool in &manifest.contributions.tools {
            let name = tool.spec.name.as_str();
            if let Some(previous) = owners.insert(name, package.clone()) {
                return Err(HarnessError::composition(format!(
                    "extension tool {name:?} is contributed by both {previous} and {package}"
                )));
            }
        }
    }
    Ok(())
}

pub fn validate_extension_command_name_uniqueness<'a>(
    manifests: impl IntoIterator<Item = &'a ExtensionManifest>,
) -> Result<(), HarnessError> {
    let mut owners = BTreeMap::<&str, String>::new();
    for manifest in manifests {
        let package = format!("{}@{}", manifest.package_id, manifest.version);
        for command in &manifest.contributions.commands {
            let name = command.name.as_str();
            if let Some(previous) = owners.insert(name, package.clone()) {
                return Err(HarnessError::composition(format!(
                    "extension command {name:?} is contributed by both {previous} and {package}"
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_settings(schema: &Value, settings: &Value) -> Result<(), HarnessError> {
    if !settings.is_object() {
        return Err(HarnessError::composition(
            "extension mount settings must be an object",
        ));
    }
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        HarnessError::composition(format!("compile extension config schema: {error}"))
    })?;
    if let Err(error) = validator.validate(settings) {
        return Err(HarnessError::composition(format!(
            "extension mount settings do not match config_schema: {error}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedExtensionBundle {
    pub manifest: ExtensionManifest,
    pub payload: ExtensionPayload,
    pub signature_base64: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherTrust {
    pub key_id: String,
    pub public_key_base64: String,
    pub allowed_sources: BTreeSet<String>,
}

impl PublisherTrust {
    pub fn validate(&self) -> Result<VerifyingKey, HarnessError> {
        validate_identifier(&self.key_id, "publisher key id")?;
        if self.allowed_sources.is_empty()
            || self
                .allowed_sources
                .iter()
                .any(|source| source.trim().is_empty() || source.len() > 2_048)
        {
            return Err(HarnessError::invalid(
                "publisher trust requires at least one valid exact source",
            ));
        }
        let bytes = decode_base64(&self.public_key_base64, 32, "publisher public key")?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| HarnessError::invalid("publisher public key must contain 32 bytes"))?;
        VerifyingKey::from_bytes(&bytes)
            .map_err(|_| HarnessError::invalid("publisher public key is invalid"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInstallRequest {
    pub bundle: SignedExtensionBundle,
    #[serde(default)]
    pub granted_capabilities: BTreeSet<Capability>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledExtension {
    pub manifest: ExtensionManifest,
    pub granted_capabilities: BTreeSet<Capability>,
    pub enabled: bool,
    pub revoked: bool,
    pub installed_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPublisher {
    pub trust: PublisherTrust,
    pub revoked: bool,
    pub added_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHostPolicy {
    pub allowed_capabilities: BTreeSet<Capability>,
    pub maximum_rhai_limits: RhaiExecutionLimits,
    pub maximum_wasm_component_limits: WasmComponentLimits,
    pub max_payload_bytes: usize,
}

impl Default for ExtensionHostPolicy {
    fn default() -> Self {
        Self {
            allowed_capabilities: [Capability::Log, Capability::WorkspaceRead]
                .into_iter()
                .collect(),
            maximum_rhai_limits: RhaiExecutionLimits::default(),
            maximum_wasm_component_limits: WasmComponentLimits::default(),
            max_payload_bytes: MAX_EXTENSION_PAYLOAD_BYTES,
        }
    }
}

impl ExtensionHostPolicy {
    pub fn validate_install(
        &self,
        manifest: &ExtensionManifest,
        grants: &BTreeSet<Capability>,
    ) -> Result<(), HarnessError> {
        manifest.validate()?;
        if self.max_payload_bytes == 0 || self.max_payload_bytes > MAX_EXTENSION_PAYLOAD_BYTES {
            return Err(HarnessError::invalid(
                "extension host payload limit must be within the supported bound",
            ));
        }
        if !grants.is_subset(&manifest.requested_capabilities)
            || !grants.is_subset(&self.allowed_capabilities)
        {
            return Err(HarnessError::policy(
                "extension capability grants must be requested and allowed by the host",
            ));
        }
        match &manifest.runtime {
            ExtensionRuntime::Rhai { limits } => {
                limits.validate_within(&self.maximum_rhai_limits)?;
            }
            ExtensionRuntime::WasmComponent { limits, .. } => {
                limits.validate_within(&self.maximum_wasm_component_limits)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDistribution {
    pub publisher: PublisherTrust,
    pub install: ExtensionInstallRequest,
}

pub fn sign_bundle(
    mut manifest: ExtensionManifest,
    payload: ExtensionPayload,
    signing_key: &SigningKey,
) -> Result<SignedExtensionBundle, HarnessError> {
    let bytes = payload_bytes(&payload, MAX_EXTENSION_PAYLOAD_BYTES)?;
    validate_runtime_payload(&manifest.runtime, &payload)?;
    manifest.payload_sha256 = extension_payload_digest(&bytes);
    manifest.validate()?;
    let signature = signing_key.sign(&signing_payload(&manifest)?);
    Ok(SignedExtensionBundle {
        manifest,
        payload,
        signature_base64: STANDARD.encode(signature.to_bytes()),
    })
}

pub fn verify_bundle(
    bundle: &SignedExtensionBundle,
    publisher: &PublisherTrust,
    max_payload_bytes: usize,
) -> Result<Vec<u8>, HarnessError> {
    bundle.manifest.validate()?;
    if bundle.manifest.publisher_key_id != publisher.key_id {
        return Err(HarnessError::policy(
            "extension publisher does not match the selected trust root",
        ));
    }
    if !publisher.allowed_sources.contains(&bundle.manifest.source) {
        return Err(HarnessError::policy(
            "extension source is not allowed by its publisher trust root",
        ));
    }
    validate_runtime_payload(&bundle.manifest.runtime, &bundle.payload)?;
    let bytes = payload_bytes(&bundle.payload, max_payload_bytes)?;
    if extension_payload_digest(&bytes) != bundle.manifest.payload_sha256 {
        return Err(HarnessError::policy(
            "extension payload digest does not match its signed manifest",
        ));
    }
    verify_manifest_signature(&bundle.manifest, &bundle.signature_base64, publisher)?;
    Ok(bytes)
}

pub(crate) fn verify_manifest_signature(
    manifest: &ExtensionManifest,
    signature_base64: &str,
    publisher: &PublisherTrust,
) -> Result<(), HarnessError> {
    let signature =
        Signature::from_slice(&decode_base64(signature_base64, 64, "extension signature")?)
            .map_err(|_| HarnessError::invalid("extension signature must contain 64 bytes"))?;
    publisher
        .validate()?
        .verify_strict(&signing_payload(manifest)?, &signature)
        .map_err(|_| HarnessError::policy("extension bundle signature verification failed"))
}

fn validate_runtime_payload(
    runtime: &ExtensionRuntime,
    payload: &ExtensionPayload,
) -> Result<(), HarnessError> {
    if matches!(runtime, ExtensionRuntime::Rhai { .. })
        != matches!(payload, ExtensionPayload::Utf8(_))
    {
        return Err(HarnessError::invalid(
            "Rhai extensions require utf8 payloads and WASM Component extensions require base64 payloads",
        ));
    }
    Ok(())
}

pub(crate) fn payload_bytes(
    payload: &ExtensionPayload,
    maximum: usize,
) -> Result<Vec<u8>, HarnessError> {
    let bytes = match payload {
        ExtensionPayload::Utf8(source) => source.as_bytes().to_vec(),
        ExtensionPayload::Base64(encoded) => decode_base64(encoded, maximum, "extension payload")?,
    };
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(HarnessError::invalid(format!(
            "extension payload must contain 1 to {maximum} bytes"
        )));
    }
    Ok(bytes)
}

#[must_use]
pub fn extension_payload_digest(payload: &[u8]) -> String {
    let digest = Sha256::digest(payload);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn signing_payload(manifest: &ExtensionManifest) -> Result<Vec<u8>, HarnessError> {
    let mut manifest = serde_json::to_value(manifest)
        .map_err(|error| HarnessError::execution(format!("encode extension manifest: {error}")))?;
    canonicalize_json_value(&mut manifest);
    let manifest = serde_json::to_vec(&manifest)
        .map_err(|error| HarnessError::execution(format!("encode extension manifest: {error}")))?;
    let mut payload = SIGNATURE_DOMAIN.to_vec();
    let length = u64::try_from(manifest.len())
        .map_err(|_| HarnessError::execution("extension manifest length exceeds u64"))?;
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(&manifest);
    Ok(payload)
}

fn canonicalize_json_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (_, value) in &mut entries {
                canonicalize_json_value(value);
            }
            object.extend(entries);
        }
        Value::Array(values) => values.iter_mut().for_each(canonicalize_json_value),
        Value::Number(number)
            if number
                .as_f64()
                .is_some_and(|value| value == 0.0 && value.is_sign_negative()) =>
        {
            *number =
                serde_json::Number::from_f64(0.0).expect("positive zero is a finite JSON number");
        }
        _ => {}
    }
}

fn decode_base64(value: &str, maximum: usize, label: &str) -> Result<Vec<u8>, HarnessError> {
    let maximum_encoded = maximum.saturating_add(2) / 3 * 4;
    if value.len() > maximum_encoded {
        return Err(HarnessError::invalid(format!(
            "{label} exceeds its size limit"
        )));
    }
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| HarnessError::invalid(format!("{label} is not valid base64")))?;
    if bytes.len() > maximum {
        return Err(HarnessError::invalid(format!(
            "{label} exceeds its size limit"
        )));
    }
    Ok(bytes)
}

fn validate_identifier(value: &str, label: &str) -> Result<(), HarnessError> {
    if value.is_empty()
        || value.len() > 160
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(HarnessError::invalid(format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), HarnessError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(HarnessError::invalid(
            "payload_sha256 must be 64 lowercase hexadecimal characters",
        ));
    }
    Ok(())
}
