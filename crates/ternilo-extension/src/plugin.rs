use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    future::Future,
    pin::Pin,
    sync::Arc,
};

use linorun_core::{
    Activation, ActivationFailure, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use ternilo_kernel::{
    CommandRegistration, CommandResolver, Commands, CommandsClient, HarnessPlugin, HookHandler,
    HookRegistration, Hooks, HooksClient, PluginFactory, PluginManifest, Prompts, PromptsClient,
    RunEnvironment, RunEnvironmentClient, SkillCandidate, SkillProvider, SkillProviderObservation,
    SkillProviderRegistration, Skills, SkillsClient, ToolEffect, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{
    CommandDescriptor, CommandInputDescriptor, HarnessError, HookDecision, HookPoint, HookRequest,
    HookResult, Profile, PromptSection, SkillDefinition, SkillSummary, ToolOutput,
    ToolPresentationDescriptor,
};

use crate::{
    EXTENSION_PACKAGE_KIND, ExtensionCommandContribution, ExtensionHookContribution,
    ExtensionHookMatcher, ExtensionPromptSectionContribution, ExtensionRegistry, ExtensionRuntime,
    ExtensionSkillContribution, ExtensionToolContribution, ExtensionToolEffect,
    package::validate_extension_settings,
    runtime::{invoke_extension, invoke_extension_hook},
};

pub(crate) const EXTENSION_SKILL_RANK: u32 = 250;

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/extension-package@1",
        requires: [Prompts, Skills, Hooks, Tools, Commands, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMount {
    pub package_id: String,
    pub version: String,
    pub settings: Value,
}

pub fn extension_mounts(profile: &Profile) -> Result<Vec<ExtensionMount>, HarnessError> {
    profile
        .plugins
        .iter()
        .filter(|entry| entry.enabled && entry.kind == EXTENSION_PACKAGE_KIND)
        .map(|entry| {
            serde_json::from_value(entry.config.clone()).map_err(|error| {
                HarnessError::composition(format!("invalid extension mount config: {error}"))
            })
        })
        .collect()
}

pub fn unique_extension_mounts(profile: &Profile) -> Result<Vec<ExtensionMount>, HarnessError> {
    let mounts = extension_mounts(profile)?;
    let mut references = BTreeSet::new();
    for mount in &mounts {
        if !references.insert((&mount.package_id, &mount.version)) {
            return Err(HarnessError::composition(format!(
                "extension package {}@{} is mounted more than once",
                mount.package_id, mount.version
            )));
        }
    }
    Ok(mounts)
}

#[must_use]
pub fn extension_mount_factory(registry: Arc<ExtensionRegistry>) -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: EXTENSION_PACKAGE_KIND,
            requires: &[
                "ternilo/prompts@1",
                "ternilo/skills@1",
                "ternilo/hooks@1",
                "ternilo/tools@1",
                "ternilo/commands@1",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        move |value| {
            let mount: ExtensionMount = serde_json::from_value(if value.is_null() {
                serde_json::json!({})
            } else {
                value
            })
            .map_err(|error| {
                HarnessError::composition(format!("invalid extension mount config: {error}"))
            })?;
            let installed = registry.describe(&mount.package_id, &mount.version)?;
            validate_extension_settings(&installed.manifest, &mount.settings)?;
            let contributions = installed.manifest.contributions;
            Ok(Arc::new(ExtensionPackagePlugin {
                registry: Arc::clone(&registry),
                mount,
                tools: contributions.tools,
                prompt_sections: contributions.prompt_sections,
                skills: contributions.skills,
                hooks: contributions.hooks,
                commands: contributions.commands,
            }))
        },
    )
    .with_description("挂载已验签并获显式 capability grant 的 Extension Package。")
    .with_config_schema::<ExtensionMount>()
}

struct ExtensionPackagePlugin {
    registry: Arc<ExtensionRegistry>,
    mount: ExtensionMount,
    tools: Vec<ExtensionToolContribution>,
    prompt_sections: Vec<ExtensionPromptSectionContribution>,
    skills: Vec<ExtensionSkillContribution>,
    hooks: Vec<ExtensionHookContribution>,
    commands: Vec<ExtensionCommandContribution>,
}

impl HarnessPlugin for ExtensionPackagePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep ordered contribution activation and reverse-order rollback together."
    )]
    fn activate(&self, context: ComponentContext) -> Activation {
        let prompts = context
            .context()
            .service::<Prompts>()
            .expect("extension package declares Prompts");
        let tools = context
            .context()
            .service::<Tools>()
            .expect("extension package declares Tools");
        let skills = context
            .context()
            .service::<Skills>()
            .expect("extension package declares Skills");
        let hooks = context
            .context()
            .service::<Hooks>()
            .expect("extension package declares Hooks");
        let commands = context
            .context()
            .service::<Commands>()
            .expect("extension package declares Commands");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("extension package declares RunEnvironment");
        let prompt_sections = self
            .prompt_sections
            .iter()
            .map(|section| PromptSection {
                id: format!(
                    "extension:{}@{}:{}",
                    self.mount.package_id, self.mount.version, section.id
                ),
                order: section.order,
                content: section.content.clone(),
            })
            .collect::<Vec<_>>();
        let skill_provider =
            extension_skill_provider(&self.mount.package_id, &self.mount.version, &self.skills);
        let tool_registrations = self
            .tools
            .iter()
            .map(|tool| ToolRegistration {
                spec: tool.spec.clone(),
                effect: tool_effect(tool.effect),
                handler: Arc::new(ExtensionToolHandler {
                    registry: Arc::clone(&self.registry),
                    package_id: self.mount.package_id.clone(),
                    version: self.mount.version.clone(),
                    handler: tool.handler.clone(),
                    output_schema: tool.output_schema.clone(),
                    settings: self.mount.settings.clone(),
                    presentation: tool.presentation.clone(),
                }),
            })
            .collect::<Vec<_>>();
        let hook_registrations = self
            .hooks
            .iter()
            .map(|hook| {
                let handler_id = format!(
                    "extension:{}@{}:hook:{}",
                    self.mount.package_id, self.mount.version, hook.id
                );
                HookRegistration {
                    handler_id: handler_id.clone(),
                    handler: Arc::new(ExtensionHookHandler {
                        registry: Arc::clone(&self.registry),
                        package_id: self.mount.package_id.clone(),
                        version: self.mount.version.clone(),
                        handler_id,
                        handler: hook.handler.clone(),
                        point: hook.point,
                        matcher: hook.matcher.clone(),
                        settings: self.mount.settings.clone(),
                        environment: environment.clone(),
                    }),
                }
            })
            .collect::<Vec<_>>();
        let command_registrations = self
            .commands
            .iter()
            .map(extension_command_registration)
            .collect::<Vec<_>>();
        let package = format!("{}@{}", self.mount.package_id, self.mount.version);
        Activation::Once(Box::pin(async move {
            let mut mounted_prompts = Vec::with_capacity(prompt_sections.len());
            for section in prompt_sections {
                let id = section.id.clone();
                match prompts.register(section).await {
                    Ok(registration) => mounted_prompts.push((registration, id)),
                    Err(error) => {
                        let rollback = unregister_prompts(&prompts, &mut mounted_prompts).await;
                        return Err(activation_failure(
                            format!("register extension {package} prompt section {id:?}: {error}"),
                            &rollback,
                        ));
                    }
                }
            }

            let mut mounted_skill_provider = None;
            if let Some((name, provider)) = skill_provider {
                match skills
                    .register_provider(SkillProviderRegistration {
                        name: name.clone(),
                        provider,
                    })
                    .await
                {
                    Ok(registration) => mounted_skill_provider = Some((registration, name)),
                    Err(error) => {
                        let rollback = unregister_prompts(&prompts, &mut mounted_prompts).await;
                        return Err(activation_failure(
                            format!(
                                "register extension {package} skill provider {name:?}: {error}"
                            ),
                            &rollback,
                        ));
                    }
                }
            }

            let mut mounted_hooks = Vec::with_capacity(hook_registrations.len());
            for registration in hook_registrations {
                let handler_id = registration.handler_id.clone();
                match hooks.register_hook(registration).await {
                    Ok(id) => mounted_hooks.push((id, handler_id)),
                    Err(error) => {
                        let mut rollback = unregister_hooks(&hooks, &mut mounted_hooks).await;
                        rollback.extend(
                            unregister_skill_provider(&skills, &mut mounted_skill_provider).await,
                        );
                        rollback.extend(unregister_prompts(&prompts, &mut mounted_prompts).await);
                        return Err(activation_failure(
                            format!("register extension {package} hook {handler_id:?}: {error}"),
                            &rollback,
                        ));
                    }
                }
            }

            let mut mounted_tools = Vec::with_capacity(tool_registrations.len());
            for registration in tool_registrations {
                let name = registration.spec.name.clone();
                match tools.register_tool(registration).await {
                    Ok(id) => mounted_tools.push((id, name.clone())),
                    Err(error) => {
                        let mut rollback = unregister_tools(&tools, &mut mounted_tools).await;
                        rollback.extend(unregister_hooks(&hooks, &mut mounted_hooks).await);
                        rollback.extend(
                            unregister_skill_provider(&skills, &mut mounted_skill_provider).await,
                        );
                        rollback.extend(unregister_prompts(&prompts, &mut mounted_prompts).await);
                        return Err(activation_failure(
                            format!("register extension {package} tool {name:?}: {error}"),
                            &rollback,
                        ));
                    }
                }
            }
            let mut mounted_commands = Vec::with_capacity(command_registrations.len());
            for registration in command_registrations {
                let name = registration.descriptor.name.clone();
                match commands.register_command(registration).await {
                    Ok(id) => mounted_commands.push((id, name.clone())),
                    Err(error) => {
                        let mut rollback =
                            unregister_commands(&commands, &mut mounted_commands).await;
                        rollback.extend(unregister_tools(&tools, &mut mounted_tools).await);
                        rollback.extend(unregister_hooks(&hooks, &mut mounted_hooks).await);
                        rollback.extend(
                            unregister_skill_provider(&skills, &mut mounted_skill_provider).await,
                        );
                        rollback.extend(unregister_prompts(&prompts, &mut mounted_prompts).await);
                        return Err(activation_failure(
                            format!("register extension {package} command {name:?}: {error}"),
                            &rollback,
                        ));
                    }
                }
            }
            Ok(Some(effect::inverse(move || async move {
                let mut errors = unregister_commands(&commands, &mut mounted_commands).await;
                errors.extend(unregister_tools(&tools, &mut mounted_tools).await);
                errors.extend(unregister_hooks(&hooks, &mut mounted_hooks).await);
                errors
                    .extend(unregister_skill_provider(&skills, &mut mounted_skill_provider).await);
                errors.extend(unregister_prompts(&prompts, &mut mounted_prompts).await);
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(CleanupError::user(format!(
                        "cleanup extension {package}: {}",
                        errors.join("; ")
                    )))
                }
            })))
        }))
    }
}

struct ExtensionCommandResolver {
    command_name: String,
    fixed_arguments: serde_json::Map<String, Value>,
    input_field: Option<String>,
}

impl CommandResolver for ExtensionCommandResolver {
    fn resolve(&self, input: &str) -> Result<Value, HarnessError> {
        let mut arguments = self.fixed_arguments.clone();
        match &self.input_field {
            Some(field) => {
                arguments.insert(field.clone(), Value::String(input.to_owned()));
            }
            None if !input.is_empty() => {
                return Err(HarnessError::invalid(format!(
                    "/{} does not accept input",
                    self.command_name
                )));
            }
            None => {}
        }
        Ok(Value::Object(arguments))
    }
}

fn extension_command_registration(command: &ExtensionCommandContribution) -> CommandRegistration {
    let fixed_arguments = command
        .fixed_arguments
        .as_object()
        .expect("validated Extension command fixed_arguments")
        .clone();
    CommandRegistration {
        descriptor: CommandDescriptor {
            name: command.name.clone(),
            description: command.description.clone(),
            input: command.input.as_ref().map(|input| CommandInputDescriptor {
                hint: input.hint.clone(),
                images: input.images,
            }),
        },
        tool_name: command.tool.clone(),
        resolver: Arc::new(ExtensionCommandResolver {
            command_name: command.name.clone(),
            fixed_arguments,
            input_field: command.input.as_ref().map(|input| input.field.clone()),
        }),
    }
}

#[derive(Clone)]
struct ExtensionSkillProvider {
    candidates: Vec<SkillCandidate>,
    definitions: BTreeMap<String, SkillDefinition>,
}

impl SkillProvider for ExtensionSkillProvider {
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<SkillProviderObservation, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            Ok(SkillProviderObservation {
                candidates: self.candidates.clone(),
                complete: true,
            })
        })
    }

    fn load<'a>(
        &'a self,
        locator: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(self.definitions.get(&locator).cloned()) })
    }
}

fn extension_skill_provider(
    package_id: &str,
    version: &str,
    skills: &[ExtensionSkillContribution],
) -> Option<(String, Arc<dyn SkillProvider>)> {
    if skills.is_empty() {
        return None;
    }
    let provider_name = extension_skill_provider_name(package_id, version);
    let source = format!("extension:{package_id}@{version}");
    let mut candidates = Vec::with_capacity(skills.len());
    let mut definitions = BTreeMap::new();
    for skill in skills {
        let summary = SkillSummary {
            name: skill.name.clone(),
            description: skill.description.clone(),
            when_to_use: skill.when_to_use.clone(),
            invocation: skill.invocation,
            source: source.clone(),
            provider: provider_name.clone(),
        };
        candidates.push(SkillCandidate {
            summary: summary.clone(),
            rank: EXTENSION_SKILL_RANK,
            locator: skill.name.clone(),
        });
        definitions.insert(
            skill.name.clone(),
            SkillDefinition {
                summary,
                content: skill.content.clone(),
                resource_base: None,
            },
        );
    }
    Some((
        provider_name,
        Arc::new(ExtensionSkillProvider {
            candidates,
            definitions,
        }),
    ))
}

pub(crate) fn extension_skill_provider_name(package_id: &str, version: &str) -> String {
    let mut identity = Vec::with_capacity(package_id.len() + version.len() + 1);
    identity.extend_from_slice(package_id.as_bytes());
    identity.push(0);
    identity.extend_from_slice(version.as_bytes());
    let mut name = String::with_capacity("extension.skill.".len() + 64);
    name.push_str("extension.skill.");
    for byte in Sha256::digest(identity) {
        write!(&mut name, "{byte:02x}").expect("writing to a String cannot fail");
    }
    name
}

async fn unregister_tools(
    tools: &ToolsClient,
    registrations: &mut Vec<(u64, String)>,
) -> Vec<String> {
    let mut errors = Vec::new();
    while let Some((registration, name)) = registrations.pop() {
        if let Err(error) = tools.unregister_tool(registration).await {
            errors.push(format!("unregister tool {name:?}: {error}"));
        }
    }
    errors
}

async fn unregister_commands(
    commands: &CommandsClient,
    registrations: &mut Vec<(u64, String)>,
) -> Vec<String> {
    let mut errors = Vec::new();
    while let Some((registration, name)) = registrations.pop() {
        if let Err(error) = commands.unregister_command(registration).await {
            errors.push(format!("unregister command {name:?}: {error}"));
        }
    }
    errors
}

async fn unregister_prompts(
    prompts: &PromptsClient,
    registrations: &mut Vec<(u64, String)>,
) -> Vec<String> {
    let mut errors = Vec::new();
    while let Some((registration, id)) = registrations.pop() {
        if let Err(error) = prompts.unregister(registration).await {
            errors.push(format!("unregister prompt section {id:?}: {error}"));
        }
    }
    errors
}

async fn unregister_hooks(
    hooks: &HooksClient,
    registrations: &mut Vec<(u64, String)>,
) -> Vec<String> {
    let mut errors = Vec::new();
    while let Some((registration, handler_id)) = registrations.pop() {
        if let Err(error) = hooks.unregister_hook(registration).await {
            errors.push(format!("unregister hook {handler_id:?}: {error}"));
        }
    }
    errors
}

async fn unregister_skill_provider(
    skills: &SkillsClient,
    registration: &mut Option<(u64, String)>,
) -> Vec<String> {
    let Some((registration, name)) = registration.take() else {
        return Vec::new();
    };
    match skills.unregister_provider(registration).await {
        Ok(()) => Vec::new(),
        Err(error) => vec![format!("unregister skill provider {name:?}: {error}")],
    }
}

fn activation_failure(message: String, rollback_errors: &[String]) -> ActivationFailure {
    if rollback_errors.is_empty() {
        ActivationFailure::user(message)
    } else {
        ActivationFailure::user(format!(
            "{message}; rollback failed: {}",
            rollback_errors.join("; ")
        ))
    }
}

struct ExtensionToolHandler {
    registry: Arc<ExtensionRegistry>,
    package_id: String,
    version: String,
    handler: String,
    output_schema: Value,
    settings: Value,
    presentation: Option<ToolPresentationDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtensionHookOutput {
    #[serde(default)]
    decision: HookDecision,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    additional_context: Option<String>,
    #[serde(default)]
    system_message: Option<String>,
}

struct ExtensionHookHandler {
    registry: Arc<ExtensionRegistry>,
    package_id: String,
    version: String,
    handler_id: String,
    handler: String,
    point: HookPoint,
    matcher: ExtensionHookMatcher,
    settings: Value,
    environment: RunEnvironmentClient,
}

impl HookHandler for ExtensionHookHandler {
    fn matches(&self, request: &HookRequest) -> bool {
        if request.point != self.point {
            return false;
        }
        match &self.matcher {
            ExtensionHookMatcher::All {} => true,
            ExtensionHookMatcher::ToolNames { names } => request
                .tool_call
                .as_ref()
                .is_some_and(|call| names.iter().any(|name| name == &call.name)),
        }
    }

    fn execute<'a>(
        &'a self,
        request: HookRequest,
    ) -> Pin<Box<dyn Future<Output = HookResult> + Send + 'a>> {
        let registry = Arc::clone(&self.registry);
        let package_id = self.package_id.clone();
        let version = self.version.clone();
        let handler = self.handler.clone();
        let settings = self.settings.clone();
        let environment = self.environment.clone();
        let handler_id = self.handler_id.clone();
        let point = self.point;
        Box::pin(async move {
            let started = std::time::Instant::now();
            let identity = environment.identity().await;
            let workspace = environment.workspace().await;
            let runtime =
                registry
                    .describe(&package_id, &version)
                    .map_or("extension", |installed| match installed.manifest.runtime {
                        ExtensionRuntime::Rhai { .. } => "extension-rhai",
                        ExtensionRuntime::WasmComponent { .. } => "extension-wasm-component",
                    });
            let result = tokio::task::spawn_blocking(move || {
                let resolved = registry.resolve(&package_id, &version)?;
                let value = invoke_extension_hook(
                    &resolved.installed,
                    &resolved.compiled,
                    &handler,
                    identity,
                    workspace,
                    request,
                    settings,
                )?;
                serde_json::from_value::<ExtensionHookOutput>(value).map_err(|error| {
                    HarnessError::execution(format!(
                        "decode extension Hook handler {handler:?} result: {error}"
                    ))
                })
            })
            .await
            .map_err(|error| HarnessError::execution(format!("join extension Hook: {error}")))
            .and_then(|result| result);
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            match result {
                Ok(output) => HookResult {
                    handler_id,
                    dialect: runtime.to_owned(),
                    point,
                    decision: output.decision,
                    reason: output.reason,
                    stop: output.stop,
                    stop_reason: output.stop_reason,
                    additional_context: output.additional_context,
                    system_message: output.system_message,
                    exit_code: None,
                    stderr_summary: None,
                    duration_ms,
                },
                Err(error) => HookResult {
                    handler_id,
                    dialect: runtime.to_owned(),
                    point,
                    decision: HookDecision::None,
                    reason: None,
                    stop: false,
                    stop_reason: None,
                    additional_context: None,
                    system_message: None,
                    exit_code: None,
                    stderr_summary: Some(truncate(&error.to_string(), 500)),
                    duration_ms,
                },
            }
        })
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_owned()
    } else {
        value.chars().take(max_chars).collect::<String>() + "…"
    }
}

impl ToolHandler for ExtensionToolHandler {
    fn presentation(&self) -> Option<ToolPresentationDescriptor> {
        self.presentation.clone()
    }

    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        let registry = Arc::clone(&self.registry);
        let package_id = self.package_id.clone();
        let version = self.version.clone();
        let handler = self.handler.clone();
        let output_schema = self.output_schema.clone();
        let settings = self.settings.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let resolved = registry.resolve(&package_id, &version)?;
                invoke_extension(
                    &resolved.installed,
                    &resolved.compiled,
                    &handler,
                    &output_schema,
                    &context,
                    arguments,
                    settings,
                )
            })
            .await
            .map_err(|error| {
                HarnessError::execution(format!("join extension tool execution: {error}"))
            })?
        })
    }
}

const fn tool_effect(effect: ExtensionToolEffect) -> ToolEffect {
    match effect {
        ExtensionToolEffect::ReadOnly => ToolEffect::ReadOnly,
        ExtensionToolEffect::Mutating => ToolEffect::Mutating,
        ExtensionToolEffect::Dangerous => ToolEffect::Dangerous,
    }
}
