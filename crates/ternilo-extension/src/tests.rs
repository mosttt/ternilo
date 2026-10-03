use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, HarnessSession, HookHandler, HookRegistration, Hooks, HooksProvider,
    HostEnvironment, HostPolicy, PluginFactory, PluginManifest, Prompts, PromptsProvider,
    RunCancellation, SkillProvider, SkillProviderObservation, SkillProviderRegistration, Skills,
    ToolExecutionContext,
};
use ternilo_protocol::{
    AgentId, ExtensionProviderMaterializeRequest, HarnessError, HookDecision, HookPoint,
    PluginEntry, PromptSection, ProviderModel, ProviderModelDefaults, ProviderModelSettings,
    ProviderProtocol, RunId, RunLimits, SessionEventKind, SessionId, SessionIdentity,
    SkillDefinition, SkillInvocationPolicy, TenantId, ToolSpec, UserId, WorkspaceBinding,
    WorkspaceId,
};

use crate::*;

mod wasm_cancellation;

const SOURCE: &str = r"
fn echo_handler(context, arguments, settings) {
    #{ content: settings.prefix + arguments.text, is_error: false }
}

fn state_handler(context, arguments, settings) {
    #{ content: settings.suffix + arguments.package_id, is_error: false }
}
";

const RHAI_HOOK_SOURCE: &str = r#"
fn echo_handler(context, arguments, settings) {
    #{ content: arguments.text, is_error: false }
}

fn allow_echo(context, request, settings) {
    #{
        decision: "allow",
        reason: settings.reason,
        additional_context: "checked by Rhai"
    }
}

fn deny_echo(context, request, settings) {
    #{ decision: "deny", reason: settings.reason }
}

fn spoof_host_fields(context, request, settings) {
    #{ handler_id: "guest-spoof", dialect: "guest-spoof", duration_ms: 999 }
}
"#;

const RHAI_COMMAND_SOURCE: &str = r#"
fn review(context, arguments, settings) {
    #{ content: arguments.source + ":" + arguments.request, is_error: false }
}

fn fixed_review(context, arguments, settings) {
    #{ content: arguments.mode, is_error: false }
}

fn numeric_review(context, arguments, settings) {
    #{ content: arguments.request.to_string(), is_error: false }
}

fn observe_command(context, request, settings) {
    #{ decision: "allow", reason: settings.reason }
}
"#;

const WASM_COMPONENT: &str = r#"
    (component
      (type $host (instance
        (type (enum "debug" "info" "warn" "error"))
        (export "log-level" (type (eq 0)))
        (type (result (error string)))
        (type (func
          (param "level" 1)
          (param "message" string)
          (result 2)))
        (export "log" (func (type 3)))
        (type (result string (error string)))
        (type (func
          (param "path" string)
          (result 4)))
        (export "read-workspace-text" (func (type 5)))))
      (import "ternilo:extension/host@1.0.0" (instance $host-import (type $host)))

      (core module $guest
        (memory (export "memory") 1)
        (global $heap (mut i32) (i32.const 4096))
        (data (i32.const 2048) "{\"content\":\"alpha\",\"is_error\":false}")
        (data (i32.const 2084) "\"bravo\"")
        (data (i32.const 2100) "{\"decision\":\"deny\",\"reason\":\"blocked by wasm\"}")
        (func (export "cabi_realloc")
          (param $old i32) (param $old-size i32) (param $align i32) (param $new-size i32)
          (result i32)
          (local $result i32)
          (global.get $heap)
          (local.set $result)
          (global.get $heap)
          (local.get $new-size)
          (i32.add)
          (global.set $heap)
          (local.get $result))
        (func (export "cabi_post_invoke") (param i32))
        (func (export "invoke")
          (param $handler-ptr i32) (param $handler-len i32)
          (param i32 i32 i32 i32)
          (result i32)
          (local $output-ptr i32)
          (local $output-len i32)
          (local.get $handler-ptr)
          (i32.load8_u)
          (i32.const 97)
          (i32.eq)
          (if
            (then
              (local.set $output-ptr (i32.const 2048))
              (local.set $output-len (i32.const 36)))
            (else
              (local.get $handler-ptr)
              (i32.load8_u)
              (i32.const 103)
              (i32.eq)
              (if
                (then
                  (local.set $output-ptr (i32.const 2100))
                  (local.set $output-len (i32.const 46)))
                (else
                  (local.set $output-ptr (i32.const 2084))
                  (local.set $output-len (i32.const 7))))))
          (i32.const 1024)
          (i32.const 0)
          (i32.store)
          (i32.const 1028)
          (local.get $output-ptr)
          (i32.store)
          (i32.const 1032)
          (local.get $output-len)
          (i32.store)
          (i32.const 1024)))
      (core instance $guest-instance (instantiate $guest))
      (func $invoke
        (param "handler" string)
        (param "context-json" string)
        (param "input-json" string)
        (result (result string (error string)))
        (canon lift (core func $guest-instance "invoke")
          (memory (core memory $guest-instance "memory"))
          (realloc (core func $guest-instance "cabi_realloc"))
          (post-return (core func $guest-instance "cabi_post_invoke"))))
      (export "invoke" (func $invoke)))
"#;

const RECORDING_PROMPT_KIND: &str = "test.recording-prompt-registry";
const RECORDING_HOOK_KIND: &str = "test.recording-hook-registry";
const PRELOADED_SKILL_PROVIDER_KIND: &str = "test.preloaded-skill-provider";

component_descriptor! {
    static RECORDING_PROMPT_DESCRIPTOR: () {
        id: "test/recording-prompt-registry@1",
        requires: [],
        provides: [Prompts],
    }
}

component_descriptor! {
    static RECORDING_HOOK_DESCRIPTOR: () {
        id: "test/recording-hook-registry@1",
        requires: [],
        provides: [Hooks],
    }
}

#[derive(Default)]
struct RecordingHookState {
    next: u64,
    preloaded_ids: BTreeSet<String>,
    handlers: BTreeMap<u64, HookRegistration>,
    events: Vec<String>,
}

#[derive(Default)]
struct RecordingHooks {
    state: Mutex<RecordingHookState>,
}

impl RecordingHooks {
    fn with_preloaded(handler_id: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RecordingHookState {
                preloaded_ids: [handler_id].into_iter().collect(),
                ..RecordingHookState::default()
            }),
        })
    }

    fn events(&self) -> Vec<String> {
        self.state.lock().unwrap().events.clone()
    }
}

impl HooksProvider for RecordingHooks {
    fn register_hook<'a>(
        &'a self,
        _: CallContext<()>,
        hook: HookRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state.preloaded_ids.contains(&hook.handler_id)
                || state
                    .handlers
                    .values()
                    .any(|current| current.handler_id == hook.handler_id)
            {
                state.events.push(format!("rejected:{}", hook.handler_id));
                return Err(HarnessError::composition(format!(
                    "hook handler {:?} is already registered",
                    hook.handler_id
                )));
            }
            let registration = state.next;
            state.next += 1;
            state.events.push(format!("registered:{}", hook.handler_id));
            state.handlers.insert(registration, hook);
            Ok(registration)
        })
    }

    fn unregister_hook<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let hook = state.handlers.remove(&registration).ok_or_else(|| {
                HarnessError::execution(format!("unknown hook registration {registration}"))
            })?;
            state
                .events
                .push(format!("unregistered:{}", hook.handler_id));
            Ok(())
        })
    }

    fn run<'a>(
        &'a self,
        _: CallContext<()>,
        request: ternilo_protocol::HookRequest,
    ) -> Pin<Box<dyn Future<Output = Vec<ternilo_protocol::HookResult>> + Send + 'a>> {
        Box::pin(async move {
            let handlers = self
                .state
                .lock()
                .unwrap()
                .handlers
                .values()
                .filter(|hook| hook.handler.matches(&request))
                .map(|hook| Arc::clone(&hook.handler))
                .collect::<Vec<Arc<dyn HookHandler>>>();
            let mut results = Vec::with_capacity(handlers.len());
            for handler in handlers {
                results.push(handler.execute(request.clone()).await);
            }
            results
        })
    }
}

struct RecordingHookPlugin {
    hooks: Arc<RecordingHooks>,
}

impl HarnessPlugin for RecordingHookPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &RECORDING_HOOK_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let hooks: Arc<dyn HooksProvider> = self.hooks.clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Hooks>(&route, hooks)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide recording Hook registry: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

fn recording_hook_factory(hooks: Arc<RecordingHooks>) -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: RECORDING_HOOK_KIND,
            requires: &[],
            provides: &["ternilo/hooks@1"],
        },
        move |_| {
            Ok(Arc::new(RecordingHookPlugin {
                hooks: Arc::clone(&hooks),
            }))
        },
    )
}

component_descriptor! {
    static PRELOADED_SKILL_PROVIDER_DESCRIPTOR: () {
        id: "test/preloaded-skill-provider@1",
        requires: [Skills],
        provides: [],
    }
}

struct EmptySkillProvider;

impl SkillProvider for EmptySkillProvider {
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<SkillProviderObservation, HarnessError>> + Send + 'a>>
    {
        Box::pin(async {
            Ok(SkillProviderObservation {
                candidates: Vec::new(),
                complete: true,
            })
        })
    }

    fn load<'a>(
        &'a self,
        _: String,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SkillDefinition>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async { Ok(None) })
    }
}

struct PreloadedSkillProviderPlugin {
    name: String,
}

impl HarnessPlugin for PreloadedSkillProviderPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PRELOADED_SKILL_PROVIDER_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let skills = context
            .context()
            .service::<Skills>()
            .expect("preloaded provider declares Skills");
        let name = self.name.clone();
        Activation::Once(Box::pin(async move {
            let registration = skills
                .register_provider(SkillProviderRegistration {
                    name,
                    provider: Arc::new(EmptySkillProvider),
                })
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "register preloaded skill provider: {error}"
                    ))
                })?;
            Ok(Some(effect::inverse(move || async move {
                skills
                    .unregister_provider(registration)
                    .await
                    .map_err(|error| {
                        linorun_core::CleanupError::user(format!(
                            "unregister preloaded skill provider: {error}"
                        ))
                    })
            })))
        }))
    }
}

fn preloaded_skill_provider_factory(name: String) -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: PRELOADED_SKILL_PROVIDER_KIND,
            requires: &["ternilo/skills@1"],
            provides: &[],
        },
        move |_| {
            Ok(Arc::new(PreloadedSkillProviderPlugin {
                name: name.clone(),
            }))
        },
    )
}

#[derive(Default)]
struct RecordingPromptState {
    next: u64,
    sections: BTreeMap<u64, PromptSection>,
    events: Vec<String>,
}

#[derive(Default)]
struct RecordingPrompts {
    state: Mutex<RecordingPromptState>,
}

impl RecordingPrompts {
    fn with_preloaded(section: PromptSection) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(RecordingPromptState {
                next: 1,
                sections: BTreeMap::from([(0, section)]),
                events: Vec::new(),
            }),
        })
    }

    fn extension_events(&self, prefix: &str) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| event.contains(prefix))
            .cloned()
            .collect()
    }
}

impl PromptsProvider for RecordingPrompts {
    fn register<'a>(
        &'a self,
        _: CallContext<()>,
        section: PromptSection,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            if state
                .sections
                .values()
                .any(|current| current.id == section.id)
            {
                state.events.push(format!("rejected:{}", section.id));
                return Err(HarnessError::composition(format!(
                    "prompt section {:?} is already registered",
                    section.id
                )));
            }
            let registration = state.next;
            state.next += 1;
            state.events.push(format!("registered:{}", section.id));
            state.sections.insert(registration, section);
            Ok(registration)
        })
    }

    fn unregister<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            let section = state.sections.remove(&registration).ok_or_else(|| {
                HarnessError::execution(format!("unknown prompt registration {registration}"))
            })?;
            state.events.push(format!("unregistered:{}", section.id));
            Ok(())
        })
    }

    fn assemble<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            let mut sections = self
                .state
                .lock()
                .unwrap()
                .sections
                .values()
                .cloned()
                .collect::<Vec<_>>();
            sections.sort_by(|left, right| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left.id.cmp(&right.id))
            });
            sections
                .into_iter()
                .map(|section| section.content)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }
}

struct RecordingPromptPlugin {
    prompts: Arc<RecordingPrompts>,
}

impl HarnessPlugin for RecordingPromptPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &RECORDING_PROMPT_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let prompts: Arc<dyn PromptsProvider> = self.prompts.clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Prompts>(&route, prompts)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide recording prompt registry: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

fn recording_prompt_factory(prompts: Arc<RecordingPrompts>) -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: RECORDING_PROMPT_KIND,
            requires: &[],
            provides: &["ternilo/prompts@1"],
        },
        move |_| {
            Ok(Arc::new(RecordingPromptPlugin {
                prompts: Arc::clone(&prompts),
            }))
        },
    )
}

fn object_schema() -> Value {
    json!({ "type": "object", "additionalProperties": true })
}

fn tool_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "content": { "type": "string" },
            "is_error": { "type": "boolean" }
        },
        "required": ["content", "is_error"],
        "additionalProperties": false
    })
}

fn tool(name: &str, handler: &str) -> ExtensionToolContribution {
    ExtensionToolContribution {
        handler: handler.to_owned(),
        spec: ToolSpec {
            name: name.to_owned(),
            description: format!("Fixture tool {name}"),
            input_schema: object_schema(),
        },
        output_schema: tool_output_schema(),
        effect: ExtensionToolEffect::ReadOnly,
        presentation: None,
    }
}

fn prompt(id: &str, order: i32, content: impl Into<String>) -> ExtensionPromptSectionContribution {
    ExtensionPromptSectionContribution {
        id: id.to_owned(),
        order,
        content: content.into(),
    }
}

fn skill(name: &str, content: impl Into<String>) -> ExtensionSkillContribution {
    ExtensionSkillContribution {
        name: name.to_owned(),
        description: format!("Fixture skill {name}"),
        when_to_use: Some(format!("Use when testing {name}.")),
        invocation: SkillInvocationPolicy::default(),
        content: content.into(),
    }
}

fn hook(id: &str, point: HookPoint, handler: &str) -> ExtensionHookContribution {
    ExtensionHookContribution {
        id: id.to_owned(),
        point,
        handler: handler.to_owned(),
        matcher: ExtensionHookMatcher::All {},
    }
}

fn command(name: &str, tool: &str) -> ExtensionCommandContribution {
    ExtensionCommandContribution {
        name: name.to_owned(),
        description: format!("Fixture command {name}"),
        tool: tool.to_owned(),
        input: Some(ExtensionCommandInput {
            hint: "<request>".to_owned(),
            field: "request".to_owned(),
            images: false,
        }),
        fixed_arguments: json!({ "source": "extension" }),
    }
}

fn provider(id: &str) -> ExtensionProviderContribution {
    ExtensionProviderContribution {
        id: id.to_owned(),
        display_name: "Fixture Provider".to_owned(),
        base_url: "https://api.example.test/v1".to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        defaults: ProviderModelDefaults {
            context_window: 128_000,
            max_output_tokens: 16_384,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "fixture-model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 120_000,
        max_attempts: 3,
        retry_base_delay_ms: 250,
        credential: ExtensionProviderCredential {
            required: true,
            suggested_ref: Some("FIXTURE_API_KEY".to_owned()),
        },
    }
}

fn rhai_manifest(package_id: &str, version: &str) -> ExtensionManifest {
    ExtensionManifest {
        schema_version: EXTENSION_PACKAGE_SCHEMA_VERSION,
        package_id: package_id.to_owned(),
        version: version.to_owned(),
        description: Some("Fixture extension".to_owned()),
        source: "https://plugins.ternilo.dev".to_owned(),
        publisher_key_id: "fixture-key".to_owned(),
        payload_sha256: "0".repeat(64),
        runtime: ExtensionRuntime::Rhai {
            limits: RhaiExecutionLimits::default(),
        },
        config_schema: object_schema(),
        contributions: ExtensionContributions {
            tools: vec![
                tool("echo", "echo_handler"),
                tool("extension_set_enabled", "state_handler"),
            ],
            prompt_sections: vec![prompt(
                "fixture-guidance",
                450,
                format!("Extension guidance from {package_id}@{version}."),
            )],
            skills: vec![skill(
                "extension-fixture",
                format!("Static skill from {package_id}@{version}."),
            )],
            hooks: Vec::new(),
            commands: Vec::new(),
            providers: Vec::new(),
        },
        requested_capabilities: BTreeSet::new(),
    }
}

fn with_echo_command(mut manifest: ExtensionManifest) -> ExtensionManifest {
    let mut echo = command("fixture-echo", "echo");
    echo.input.as_mut().unwrap().field = "text".to_owned();
    echo.fixed_arguments = json!({});
    manifest.contributions.commands = vec![echo];
    manifest
}

fn wasm_manifest() -> ExtensionManifest {
    let mut manifest = with_echo_command(rhai_manifest("dev.ternilo.wasm-fixture", "1.0.0"));
    let mut string_tool = tool("extension_set_enabled", "bravo");
    string_tool.output_schema = json!({ "type": "string", "const": "bravo" });
    manifest.contributions.tools = vec![tool("echo", "alpha"), string_tool];
    manifest.runtime = ExtensionRuntime::WasmComponent {
        world: WASM_COMPONENT_RUNTIME_WORLD.to_owned(),
        limits: WasmComponentLimits {
            fuel: 1_000_000,
            max_memory_bytes: 1024 * 1024,
            max_input_bytes: 1024,
            max_output_bytes: 1024,
            max_workspace_read_bytes: 1024,
        },
    };
    manifest
}

fn publisher(signing_key: &SigningKey) -> PublisherTrust {
    PublisherTrust {
        key_id: "fixture-key".to_owned(),
        public_key_base64: STANDARD.encode(signing_key.verifying_key().to_bytes()),
        allowed_sources: ["https://plugins.ternilo.dev".to_owned()]
            .into_iter()
            .collect(),
    }
}

fn install(
    registry: &ExtensionRegistry,
    signing_key: &SigningKey,
    manifest: ExtensionManifest,
    source: &str,
    grants: BTreeSet<Capability>,
    now_ms: u64,
) -> InstalledExtension {
    registry
        .install(
            ExtensionInstallRequest {
                bundle: sign_bundle(
                    manifest,
                    ExtensionPayload::Utf8(source.to_owned()),
                    signing_key,
                )
                .unwrap(),
                granted_capabilities: grants,
            },
            now_ms,
        )
        .unwrap()
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        tenant_id: TenantId::new("tenant"),
        user_id: UserId::new("user"),
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
    }
}

fn execution_context(run_id: &str) -> ToolExecutionContext {
    ToolExecutionContext {
        activity: ternilo_kernel::ActivityBranch::untracked(),
        identity: identity(),
        workspace: None,
        run_id: RunId::new(run_id),
        call_id: format!("call-{run_id}"),
        cancellation: RunCancellation::new(),
    }
}

fn mounted_profile(package_id: &str, version: &str, settings: Value) -> ternilo_protocol::Profile {
    let mut profile = ternilo_builtins::local_profile();
    profile
        .plugins
        .retain(|entry| entry.id != "runtime-extension-tools");
    ensure_skill_registry(&mut profile);
    let mut config = json!({
        "package_id": package_id,
        "version": version,
    });
    config["settings"] = settings;
    profile.plugins.push(PluginEntry {
        id: format!("extension:{package_id}@{version}"),
        kind: EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config,
    });
    profile
}

fn ensure_skill_registry(profile: &mut ternilo_protocol::Profile) {
    if !profile
        .plugins
        .iter()
        .any(|entry| entry.kind == "ternilo.skills.registry")
    {
        profile.plugins.insert(
            4.min(profile.plugins.len()),
            PluginEntry {
                id: "skill-registry".to_owned(),
                kind: "ternilo.skills.registry".to_owned(),
                enabled: true,
                config: json!({}),
            },
        );
    }
}

fn mount_extension(profile: &mut ternilo_protocol::Profile, package_id: &str, version: &str) {
    profile.plugins.push(PluginEntry {
        id: format!("extension:{package_id}@{version}"),
        kind: EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config: json!({
            "package_id": package_id,
            "version": version,
            "settings": {},
        }),
    });
}

fn preload_skill_provider(profile: &mut ternilo_protocol::Profile) {
    let extension_index = profile
        .plugins
        .iter()
        .position(|entry| entry.kind == EXTENSION_PACKAGE_KIND)
        .unwrap_or(profile.plugins.len());
    profile.plugins.insert(
        extension_index,
        PluginEntry {
            id: "preloaded-skill-provider".to_owned(),
            kind: PRELOADED_SKILL_PROVIDER_KIND.to_owned(),
            enabled: true,
            config: json!({}),
        },
    );
}

fn use_recording_prompts(profile: &mut ternilo_protocol::Profile, package_id: &str, version: &str) {
    ensure_skill_registry(profile);
    profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "prompt-registry")
        .expect("fixture profile contains the prompt registry")
        .kind = RECORDING_PROMPT_KIND.to_owned();
    if !profile.plugins.iter().any(|entry| {
        entry.kind == EXTENSION_PACKAGE_KIND
            && entry.config["package_id"] == package_id
            && entry.config["version"] == version
    }) {
        profile.plugins.push(PluginEntry {
            id: format!("extension:{package_id}@{version}"),
            kind: EXTENSION_PACKAGE_KIND.to_owned(),
            enabled: true,
            config: json!({
                "package_id": package_id,
                "version": version,
                "settings": {},
            }),
        });
    }
}

fn use_recording_hooks(profile: &mut ternilo_protocol::Profile) {
    profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "hook-registry")
        .expect("fixture profile contains the Hook registry")
        .kind = RECORDING_HOOK_KIND.to_owned();
}

fn catalog_with_recording_prompts(
    registry: Arc<ExtensionRegistry>,
    prompts: Arc<RecordingPrompts>,
) -> ternilo_kernel::Catalog {
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog.register(recording_prompt_factory(prompts)).unwrap();
    catalog.register(extension_mount_factory(registry)).unwrap();
    catalog
}

fn catalog(registry: Arc<ExtensionRegistry>) -> ternilo_kernel::Catalog {
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog.register(extension_mount_factory(registry)).unwrap();
    catalog
}

fn catalog_with_recording_hooks(
    registry: Arc<ExtensionRegistry>,
    hooks: Arc<RecordingHooks>,
    prompts: Arc<RecordingPrompts>,
) -> ternilo_kernel::Catalog {
    let mut catalog = catalog_with_recording_prompts(registry, prompts);
    catalog.register(recording_hook_factory(hooks)).unwrap();
    catalog
}

#[test]
fn runtime_and_payload_are_strict_single_choice_wire_types() {
    assert_eq!(EXTENSION_PACKAGE_SCHEMA_VERSION, 1);
    let runtime = serde_json::to_value(ExtensionRuntime::Rhai {
        limits: RhaiExecutionLimits::default(),
    })
    .unwrap();
    assert_eq!(runtime["kind"], "rhai");
    assert!(runtime["limits"]["max_operations"].is_number());
    assert!(
        serde_json::from_value::<ExtensionRuntime>(json!({
            "kind": "rhai",
            "limits": serde_json::to_value(RhaiExecutionLimits::default()).unwrap(),
            "world": "not-allowed"
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<ExtensionRuntime>(json!({})).is_err());
    assert!(
        serde_json::from_value::<ExtensionMount>(json!({
            "package_id": "dev.ternilo.fixture",
            "version": "1.0.0"
        }))
        .is_err()
    );

    assert_eq!(
        serde_json::to_value(ExtensionPayload::Utf8("source".to_owned())).unwrap(),
        json!({ "kind": "utf8", "content": "source" })
    );
    assert!(
        serde_json::from_value::<ExtensionPayload>(json!({
            "kind": "hex",
            "content": "00"
        }))
        .is_err()
    );
    let complete = json!({
        "tools": [],
        "prompt_sections": [],
        "skills": [],
        "hooks": [],
        "commands": [],
        "providers": []
    });
    for missing in [
        "tools",
        "prompt_sections",
        "skills",
        "hooks",
        "commands",
        "providers",
    ] {
        let mut missing_array = complete.clone();
        missing_array.as_object_mut().unwrap().remove(missing);
        assert!(serde_json::from_value::<ExtensionContributions>(missing_array).is_err());
    }
    let contributions = serde_json::from_value::<ExtensionContributions>(complete).unwrap();
    assert!(contributions.tools.is_empty());
    assert!(contributions.prompt_sections.is_empty());
    assert!(contributions.skills.is_empty());
    assert!(contributions.hooks.is_empty());
    assert!(contributions.commands.is_empty());
    assert!(contributions.providers.is_empty());
}

#[test]
fn prompt_section_manifest_validation_is_bounded_unique_and_allows_prompt_only_packages() {
    let mut prompt_only = rhai_manifest("dev.ternilo.prompt-only", "1.0.0");
    prompt_only.contributions.tools.clear();
    prompt_only.contributions.skills.clear();
    prompt_only.contributions.prompt_sections = vec![prompt("guidance", 320, "Prompt only")];
    prompt_only.validate().unwrap();

    let mut unsupported_schema = prompt_only.clone();
    unsupported_schema.schema_version = 2;
    let error = unsupported_schema.validate().unwrap_err();
    assert!(error.message.contains("expected 1"));

    let mut empty = prompt_only.clone();
    empty.contributions.prompt_sections.clear();
    assert!(
        empty
            .validate()
            .unwrap_err()
            .message
            .contains("at least one")
    );

    let mut duplicate = prompt_only.clone();
    duplicate
        .contributions
        .prompt_sections
        .push(prompt("guidance", 999, "Duplicate"));
    assert!(
        duplicate
            .validate()
            .unwrap_err()
            .message
            .contains("duplicate prompt section")
    );

    for invalid in [
        prompt("", 1, "content"),
        prompt("bad id", 1, "content"),
        prompt("valid", 1, "   "),
        prompt("valid", 1, "x".repeat(128 * 1024 + 1)),
    ] {
        let mut manifest = prompt_only.clone();
        manifest.contributions.prompt_sections = vec![invalid];
        assert!(manifest.validate().is_err());
    }
}

#[test]
fn skill_manifest_validation_matches_the_builtin_skill_contract() {
    let mut skills_only = rhai_manifest("dev.ternilo.skills-only", "1.0.0");
    skills_only.contributions.tools.clear();
    skills_only.contributions.prompt_sections.clear();
    skills_only.contributions.skills = vec![skill("review-code", "Review carefully.")];
    skills_only.validate().unwrap();

    let decoded = serde_json::from_value::<ExtensionSkillContribution>(json!({
        "name": "default-policy",
        "description": "Default invocation policy",
        "content": "Use the default policy."
    }))
    .unwrap();
    assert_eq!(decoded.invocation, SkillInvocationPolicy::default());

    let mut empty = skills_only.clone();
    empty.contributions.skills.clear();
    assert!(
        empty
            .validate()
            .unwrap_err()
            .message
            .contains("at least one")
    );

    let mut duplicate = skills_only.clone();
    duplicate
        .contributions
        .skills
        .push(skill("review-code", "Duplicate."));
    assert!(
        duplicate
            .validate()
            .unwrap_err()
            .message
            .contains("duplicate skill")
    );

    for invalid in [
        ExtensionSkillContribution {
            name: String::new(),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            name: "x".repeat(129),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            name: "bad--name".to_owned(),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            name: "Bad-Name".to_owned(),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            description: " ".to_owned(),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            description: "x".repeat(2_001),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            when_to_use: Some("x".repeat(4_001)),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            content: " ".to_owned(),
            ..skill("valid", "content")
        },
        ExtensionSkillContribution {
            content: "x".repeat(2 * 1024 * 1024 + 1),
            ..skill("valid", "content")
        },
    ] {
        let mut manifest = skills_only.clone();
        manifest.contributions.skills = vec![invalid];
        assert!(manifest.validate().is_err());
    }

    let mut too_many = skills_only;
    too_many.contributions.skills = (0..2_001)
        .map(|index| skill(&format!("skill-{index}"), "content"))
        .collect();
    assert!(
        too_many
            .validate()
            .unwrap_err()
            .message
            .contains("at most 2000")
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep contribution wire and validation cases together around the same manifest."
)]
fn hook_command_and_provider_manifest_wire_is_strict_and_validated() {
    assert_eq!(
        serde_json::to_value(ExtensionHookMatcher::All {}).unwrap(),
        json!({ "kind": "all" })
    );
    assert_eq!(
        serde_json::from_value::<ExtensionHookMatcher>(json!({ "kind": "all" })).unwrap(),
        ExtensionHookMatcher::All {}
    );
    assert!(
        serde_json::from_value::<ExtensionHookMatcher>(json!({
            "kind": "all",
            "names": ["echo"]
        }))
        .is_err()
    );
    let mut manifest = rhai_manifest("dev.ternilo.new-contributions", "1.0.0");
    manifest.contributions.hooks = vec![ExtensionHookContribution {
        matcher: ExtensionHookMatcher::ToolNames {
            names: vec!["echo".to_owned()],
        },
        ..hook("guard-echo", HookPoint::PreToolUse, "guard_echo")
    }];
    manifest.contributions.commands = vec![command("review", "echo")];
    manifest.contributions.providers = vec![provider("fixture")];
    manifest.validate().unwrap();

    let encoded = serde_json::to_value(&manifest.contributions).unwrap();
    assert_eq!(encoded["hooks"][0]["point"], "pre_tool_use");
    assert_eq!(encoded["hooks"][0]["matcher"]["kind"], "tool_names");
    assert_eq!(encoded["commands"][0]["input"]["images"], false);
    assert_eq!(encoded["providers"][0]["protocol"], "openai-responses");

    let mut hook_only = manifest.clone();
    hook_only.contributions.tools.clear();
    hook_only.contributions.prompt_sections.clear();
    hook_only.contributions.skills.clear();
    hook_only.contributions.commands.clear();
    hook_only.contributions.providers.clear();
    hook_only.validate().unwrap();

    let mut provider_only = manifest.clone();
    provider_only.contributions.tools.clear();
    provider_only.contributions.prompt_sections.clear();
    provider_only.contributions.skills.clear();
    provider_only.contributions.hooks.clear();
    provider_only.contributions.commands.clear();
    provider_only.validate().unwrap();

    let mut duplicate_hook = manifest.clone();
    duplicate_hook
        .contributions
        .hooks
        .push(hook("guard-echo", HookPoint::Stop, "other_handler"));
    assert!(
        duplicate_hook
            .validate()
            .unwrap_err()
            .message
            .contains("duplicate hook")
    );

    let mut invalid_matcher = manifest.clone();
    invalid_matcher.contributions.hooks[0].point = HookPoint::SessionStart;
    assert!(
        invalid_matcher
            .validate()
            .unwrap_err()
            .message
            .contains("tool_names matcher")
    );

    let mut empty_matcher = manifest.clone();
    empty_matcher.contributions.hooks[0].matcher =
        ExtensionHookMatcher::ToolNames { names: Vec::new() };
    assert!(empty_matcher.validate().is_err());

    let mut missing_tool = manifest.clone();
    missing_tool.contributions.commands[0].tool = "outside_package".to_owned();
    assert!(
        missing_tool
            .validate()
            .unwrap_err()
            .message
            .contains("outside its package")
    );

    let mut reserved_command = manifest.clone();
    reserved_command.contributions.commands[0].name = "skill".to_owned();
    assert!(
        reserved_command
            .validate()
            .unwrap_err()
            .message
            .contains("reserved")
    );

    let mut duplicate_command = manifest.clone();
    duplicate_command
        .contributions
        .commands
        .push(command("review", "echo"));
    assert!(
        duplicate_command
            .validate()
            .unwrap_err()
            .message
            .contains("duplicate command")
    );

    let mut image_command = manifest.clone();
    image_command.contributions.commands[0]
        .input
        .as_mut()
        .unwrap()
        .images = true;
    assert!(
        image_command
            .validate()
            .unwrap_err()
            .message
            .contains("does not support images")
    );

    let mut duplicate_provider = manifest.clone();
    duplicate_provider
        .contributions
        .providers
        .push(provider("fixture"));
    assert!(
        duplicate_provider
            .validate()
            .unwrap_err()
            .message
            .contains("duplicate provider template")
    );

    let mut invalid_provider = manifest;
    invalid_provider.contributions.providers[0].base_url = "file:///tmp/model".to_owned();
    assert!(
        invalid_provider
            .validate()
            .unwrap_err()
            .message
            .contains("base_url")
    );
}

#[test]
fn signature_tampering_and_wrong_payload_encoding_are_rejected() {
    let key = SigningKey::from_bytes(&[7; 32]);
    let trust = publisher(&key);
    let mut manifest = rhai_manifest("dev.ternilo.fixture", "1.0.0");
    manifest.contributions.hooks = vec![hook(
        "signed-hook",
        HookPoint::UserPromptSubmit,
        "signed_hook",
    )];
    manifest.contributions.commands = vec![command("signed-command", "echo")];
    manifest.contributions.providers = vec![provider("signed-provider")];
    let bundle = sign_bundle(manifest, ExtensionPayload::Utf8(SOURCE.to_owned()), &key).unwrap();
    let mut prompt_tampered = bundle.clone();
    prompt_tampered.manifest.contributions.prompt_sections[0]
        .content
        .push_str(" tampered");
    assert!(verify_bundle(&prompt_tampered, &trust, 1024 * 1024).is_err());
    let mut skill_tampered = bundle.clone();
    skill_tampered.manifest.contributions.skills[0]
        .content
        .push_str(" tampered");
    assert!(verify_bundle(&skill_tampered, &trust, 1024 * 1024).is_err());
    let mut skill_policy_tampered = bundle.clone();
    skill_policy_tampered.manifest.contributions.skills[0]
        .invocation
        .model_invocable = false;
    assert!(verify_bundle(&skill_policy_tampered, &trust, 1024 * 1024).is_err());
    let mut hook_tampered = bundle.clone();
    hook_tampered.manifest.contributions.hooks[0].handler = "other_hook".to_owned();
    assert!(verify_bundle(&hook_tampered, &trust, 1024 * 1024).is_err());
    let mut command_tampered = bundle.clone();
    command_tampered.manifest.contributions.commands[0]
        .description
        .push_str(" tampered");
    assert!(verify_bundle(&command_tampered, &trust, 1024 * 1024).is_err());
    let mut provider_tampered = bundle.clone();
    provider_tampered.manifest.contributions.providers[0]
        .display_name
        .push_str(" tampered");
    assert!(verify_bundle(&provider_tampered, &trust, 1024 * 1024).is_err());

    let mut bundle = bundle;
    bundle.payload = ExtensionPayload::Utf8(format!("{SOURCE}\n// tampered"));
    assert!(verify_bundle(&bundle, &trust, 1024 * 1024).is_err());
    assert!(
        sign_bundle(
            rhai_manifest("dev.ternilo.bad-encoding", "1.0.0"),
            ExtensionPayload::Base64(STANDARD.encode(SOURCE)),
            &key,
        )
        .is_err()
    );
    assert!(
        sign_bundle(
            wasm_manifest(),
            ExtensionPayload::Utf8("not a component".to_owned()),
            &key,
        )
        .is_err()
    );
}

#[test]
#[expect(
    clippy::unicode_not_nfc,
    reason = "Verify that signatures preserve decomposed Unicode rather than silently normalizing it."
)]
fn signature_is_stable_across_jsonb_object_order_unicode_and_negative_zero() {
    let key = SigningKey::from_bytes(&[35; 32]);
    let trust = publisher(&key);
    let mut manifest = rhai_manifest("dev.ternilo.canonical-signature", "1.0.0");
    manifest.config_schema = json!({
        "type": "object",
        "properties": {
            "é": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "e\u{301}": { "type": "number", "minimum": -0.0 }
                    }
                },
                "examples": [[
                    { "😀": -0.0, "e\u{301}": -0.0, "label": "组合" },
                    "尾"
                ]]
            }
        },
        "required": ["é"],
        "additionalProperties": false
    });
    manifest.contributions.commands = vec![ExtensionCommandContribution {
        fixed_arguments: json!({
            "é": [{ "😀": -0.0, "e\u{301}": -0.0, "label": "组合" }],
            "tail": "尾"
        }),
        input: None,
        ..command("canonical-command", "echo")
    }];
    let mut bundle =
        sign_bundle(manifest, ExtensionPayload::Utf8(SOURCE.to_owned()), &key).unwrap();

    bundle.manifest.config_schema = serde_json::from_str(
        r#"{
            "additionalProperties": false,
            "required": ["é"],
            "properties": {
                "é": {
                    "examples": [[
                        { "label": "组合", "é": 0.0, "😀": 0.0 },
                        "尾"
                    ]],
                    "items": {
                        "properties": {
                            "é": { "minimum": 0.0, "type": "number" }
                        },
                        "type": "object"
                    },
                    "type": "array"
                }
            },
            "type": "object"
        }"#,
    )
    .unwrap();
    bundle.manifest.contributions.commands[0].fixed_arguments = serde_json::from_str(
        r#"{
            "tail": "尾",
            "é": [{ "label": "组合", "é": 0.0, "😀": 0.0 }]
        }"#,
    )
    .unwrap();

    verify_bundle(&bundle, &trust, 1024 * 1024).unwrap();

    let mut unicode_normalized = bundle.clone();
    let properties = unicode_normalized.manifest.config_schema["properties"]
        .as_object_mut()
        .unwrap();
    let schema = properties.remove("é").unwrap();
    properties.insert("e\u{301}".to_owned(), schema);
    unicode_normalized.manifest.config_schema["required"][0] = json!("e\u{301}");
    assert!(verify_bundle(&unicode_normalized, &trust, 1024 * 1024).is_err());

    let mut array_reordered = bundle;
    array_reordered.manifest.config_schema["properties"]["é"]["examples"][0]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    assert!(verify_bundle(&array_reordered, &trust, 1024 * 1024).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_only_package_loads_static_policy_and_unloads_without_running_payload() {
    assert_eq!(crate::plugin::EXTENSION_SKILL_RANK, 250);
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[34; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.skills-only";
    let version = "1.0.0";
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools.clear();
    manifest.contributions.prompt_sections.clear();
    manifest.contributions.skills = vec![ExtensionSkillContribution {
        name: "review-extension".to_owned(),
        description: "Review the mounted extension.".to_owned(),
        when_to_use: Some("Use for extension reviews.".to_owned()),
        invocation: SkillInvocationPolicy {
            model_invocable: false,
            user_invocable: true,
        },
        content: "Review this Extension without invoking its runtime.".to_owned(),
    }];
    install(
        &registry,
        &key,
        manifest,
        "throw \"skills-only runtime must not execute\";",
        BTreeSet::new(),
        2,
    );

    let catalog = catalog(Arc::clone(&registry));
    let profile = mounted_profile(package_id, version, json!({}));
    for _ in 0..2 {
        let harness = HarnessSession::boot(
            &catalog,
            &profile,
            HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
        )
        .await
        .unwrap();
        let summary = harness
            .skill_catalog()
            .await
            .unwrap()
            .skills
            .into_iter()
            .find(|skill| skill.name == "review-extension")
            .expect("static Extension skill appears in the catalog");
        let expected_provider = crate::plugin::extension_skill_provider_name(package_id, version);
        assert_eq!(summary.provider, expected_provider);
        assert_eq!(summary.source, "extension:dev.ternilo.skills-only@1.0.0");
        assert!(!summary.invocation.model_invocable);
        assert!(summary.invocation.user_invocable);
        assert!(summary.provider.len() <= 128);
        assert!(summary.provider.is_ascii());

        let definition = harness.skill("review-extension").await.unwrap().unwrap();
        assert_eq!(
            definition.content,
            "Review this Extension without invoking its runtime."
        );
        assert_eq!(definition.summary, summary);
        assert!(definition.resource_base.is_none());
        harness.shutdown().await.unwrap();
    }
    registry.uninstall(package_id, version).unwrap();
    assert!(registry.inventory().unwrap().extensions.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn same_named_extension_skill_uses_the_first_mounted_provider() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[35; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    for (package_id, content, now_ms) in [
        ("dev.ternilo.skill-alpha", "alpha wins", 2),
        ("dev.ternilo.skill-beta", "beta wins", 3),
    ] {
        let mut manifest = rhai_manifest(package_id, "1.0.0");
        manifest.contributions.tools.clear();
        manifest.contributions.prompt_sections.clear();
        manifest.contributions.skills = vec![skill("extension-order-probe", content)];
        install(
            &registry,
            &key,
            manifest,
            "let static_skill_payload = true;",
            BTreeSet::new(),
            now_ms,
        );
    }
    let catalog = Arc::new(catalog(Arc::clone(&registry)));
    for (first, second, expected) in [
        (
            "dev.ternilo.skill-alpha",
            "dev.ternilo.skill-beta",
            "alpha wins",
        ),
        (
            "dev.ternilo.skill-beta",
            "dev.ternilo.skill-alpha",
            "beta wins",
        ),
    ] {
        let mut profile = mounted_profile(first, "1.0.0", json!({}));
        mount_extension(&mut profile, second, "1.0.0");
        for _ in 0..4 {
            let mut boots = Vec::new();
            for _ in 0..4 {
                let catalog = Arc::clone(&catalog);
                let profile = profile.clone();
                boots.push(tokio::spawn(async move {
                    let harness = HarnessSession::boot(
                        catalog.as_ref(),
                        &profile,
                        HostEnvironment::memory(
                            identity(),
                            None,
                            HostPolicy::local(RunLimits::default()),
                        ),
                    )
                    .await
                    .unwrap();
                    let content = harness
                        .skill("extension-order-probe")
                        .await
                        .unwrap()
                        .unwrap()
                        .content;
                    harness.shutdown().await.unwrap();
                    content
                }));
            }
            for boot in boots {
                assert_eq!(boot.await.unwrap(), expected);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_only_rhai_package_assembles_namespaced_sections_and_unloads_in_reverse() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[30; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = rhai_manifest("dev.ternilo.prompt-only", "1.0.0");
    manifest.contributions.tools.clear();
    manifest.contributions.skills.clear();
    manifest.contributions.prompt_sections = vec![
        prompt("zeta", 325, "Prompt section zeta."),
        prompt("alpha", 325, "Prompt section alpha."),
    ];
    install(
        &registry,
        &key,
        manifest,
        "let prompt_only_payload = true;",
        BTreeSet::new(),
        2,
    );

    let prompts = Arc::new(RecordingPrompts::default());
    let catalog = catalog_with_recording_prompts(Arc::clone(&registry), Arc::clone(&prompts));
    let mut profile = mounted_profile("dev.ternilo.prompt-only", "1.0.0", json!({}));
    use_recording_prompts(&mut profile, "dev.ternilo.prompt-only", "1.0.0");
    let harness = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();

    let outcome = harness
        .run(RunId::new("prompt-only"), "hello")
        .await
        .unwrap();
    let system_prompt = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ModelRequestStarted { system_prompt, .. } => Some(system_prompt),
            _ => None,
        })
        .expect("prompt-only package contributes to the model request");
    assert!(
        system_prompt.find("Prompt section alpha.").unwrap()
            < system_prompt.find("Prompt section zeta.").unwrap()
    );

    harness.shutdown().await.unwrap();
    let prefix = "extension:dev.ternilo.prompt-only@1.0.0:";
    assert_eq!(
        prompts.extension_events(prefix),
        vec![
            format!("registered:{prefix}zeta"),
            format!("registered:{prefix}alpha"),
            format!("unregistered:{prefix}alpha"),
            format!("unregistered:{prefix}zeta"),
        ]
    );
    registry
        .uninstall("dev.ternilo.prompt-only", "1.0.0")
        .unwrap();
    assert!(registry.inventory().unwrap().extensions.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn two_packages_can_contribute_the_same_local_prompt_id() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[33; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();

    for (package_id, content, now_ms) in [
        ("dev.ternilo.prompt-alpha", "Prompt from alpha.", 2),
        ("dev.ternilo.prompt-beta", "Prompt from beta.", 3),
    ] {
        let mut manifest = rhai_manifest(package_id, "1.0.0");
        manifest.contributions.tools.clear();
        manifest.contributions.skills.clear();
        manifest.contributions.prompt_sections = vec![prompt("guidance", 325, content)];
        install(
            &registry,
            &key,
            manifest,
            "let prompt_only_payload = true;",
            BTreeSet::new(),
            now_ms,
        );
    }

    let prompts = Arc::new(RecordingPrompts::default());
    let catalog = catalog_with_recording_prompts(Arc::clone(&registry), Arc::clone(&prompts));
    let mut profile = mounted_profile("dev.ternilo.prompt-alpha", "1.0.0", json!({}));
    use_recording_prompts(&mut profile, "dev.ternilo.prompt-beta", "1.0.0");
    let harness = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();

    let outcome = harness
        .run(RunId::new("same-local-prompt-id"), "hello")
        .await
        .unwrap();
    let system_prompt = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ModelRequestStarted { system_prompt, .. } => Some(system_prompt),
            _ => None,
        })
        .expect("both package-scoped prompt sections contribute to the model request");
    assert!(system_prompt.contains("Prompt from alpha."));
    assert!(system_prompt.contains("Prompt from beta."));
    assert!(
        system_prompt.find("Prompt from alpha.").unwrap()
            < system_prompt.find("Prompt from beta.").unwrap()
    );

    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn prompt_registration_failure_rolls_back_prior_extension_sections() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[31; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.prompt-rollback";
    let version = "1.0.0";
    let prefix = format!("extension:{package_id}@{version}:");
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools.clear();
    manifest.contributions.skills.clear();
    manifest.contributions.prompt_sections = vec![
        prompt("first", 300, "First prompt."),
        prompt("collision", 301, "Colliding prompt."),
    ];
    install(
        &registry,
        &key,
        manifest,
        "let prompt_only_payload = true;",
        BTreeSet::new(),
        2,
    );
    let prompts = RecordingPrompts::with_preloaded(PromptSection {
        id: format!("{prefix}collision"),
        order: 1,
        content: "Existing section".to_owned(),
    });
    let catalog = catalog_with_recording_prompts(Arc::clone(&registry), Arc::clone(&prompts));
    let mut profile = mounted_profile(package_id, version, json!({}));
    use_recording_prompts(&mut profile, package_id, version);

    let error = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("duplicate physical prompt id must fail activation");
    assert!(error.message.contains("prompt section"));
    assert_eq!(
        prompts.extension_events(&prefix),
        vec![
            format!("registered:{prefix}first"),
            format!("rejected:{prefix}collision"),
            format!("unregistered:{prefix}first"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_provider_registration_failure_rolls_back_extension_prompts() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[36; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.skill-registration-rollback";
    let version = "1.0.0";
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools.clear();
    install(
        &registry,
        &key,
        manifest,
        "let static_skill_payload = true;",
        BTreeSet::new(),
        2,
    );

    let prompts = Arc::new(RecordingPrompts::default());
    let mut catalog = catalog_with_recording_prompts(Arc::clone(&registry), Arc::clone(&prompts));
    catalog
        .register(preloaded_skill_provider_factory(
            crate::plugin::extension_skill_provider_name(package_id, version),
        ))
        .unwrap();
    let mut profile = mounted_profile(package_id, version, json!({}));
    use_recording_prompts(&mut profile, package_id, version);
    preload_skill_provider(&mut profile);

    let error = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("duplicate stable provider name must fail activation");
    assert!(error.message.contains("skill provider"));
    let prompt_id = format!("extension:{package_id}@{version}:fixture-guidance");
    assert_eq!(
        prompts.extension_events(&prompt_id),
        vec![
            format!("registered:{prompt_id}"),
            format!("unregistered:{prompt_id}"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_registration_failure_rolls_back_extension_skill_and_prompts() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[32; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.tool-rollback";
    let version = "1.0.0";
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools = vec![tool("extension_set_enabled", "echo_handler")];
    manifest.contributions.prompt_sections = vec![prompt("guidance", 300, "Guidance")];
    install(&registry, &key, manifest, SOURCE, BTreeSet::new(), 2);
    let prompts = Arc::new(RecordingPrompts::default());
    let catalog = catalog_with_recording_prompts(Arc::clone(&registry), Arc::clone(&prompts));
    let mut profile = ternilo_builtins::local_profile();
    use_recording_prompts(&mut profile, package_id, version);

    let error = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("duplicate tool must fail graph activation");
    // Either consumer can register first; graph rollback must clean up both orders.
    assert!(error.message.contains("already registered"), "{error}");
    assert!(error.message.contains("extension_set_enabled"));
    let prefix = format!("extension:{package_id}@{version}:");
    assert_eq!(
        prompts.extension_events(&prefix),
        vec![
            format!("registered:{prefix}guidance"),
            format!("unregistered:{prefix}guidance"),
        ]
    );

    let recovery_profile = mounted_profile(package_id, version, json!({}));
    let recovery = HarnessSession::boot(
        &catalog,
        &recovery_profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .expect("tool failure rollback must unregister the Extension skill provider");
    assert!(recovery.skill("extension-fixture").await.unwrap().is_some());
    recovery.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_tool_rhai_package_mounts_uses_settings_and_observes_state_immediately() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[9; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    install(
        &registry,
        &key,
        with_echo_command(rhai_manifest("dev.ternilo.fixture", "1.0.0")),
        SOURCE,
        BTreeSet::new(),
        2,
    );

    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog
        .register(extension_mount_factory(Arc::clone(&registry)))
        .unwrap();
    let harness = HarnessSession::boot(
        &catalog,
        &mounted_profile(
            "dev.ternilo.fixture",
            "1.0.0",
            json!({ "prefix": "hello ", "suffix": "state:" }),
        ),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();

    let names = harness
        .tool_catalog()
        .await
        .unwrap()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    assert!(names.contains("echo"));
    assert!(names.contains("extension_set_enabled"));
    let echo = harness
        .run(RunId::new("run-1"), "/fixture-echo Ada")
        .await
        .unwrap();
    assert_eq!(echo.answer, "hello Ada");
    let second = harness
        .run(RunId::new("run-2"), "/extension-enable dev.example 1.0.0")
        .await
        .unwrap();
    assert_eq!(second.answer, "state:dev.example");

    registry
        .set_enabled("dev.ternilo.fixture", "1.0.0", false, 3)
        .unwrap();
    let disabled = harness
        .run(RunId::new("run-3"), "/fixture-echo blocked")
        .await
        .unwrap();
    assert!(disabled.answer.contains("disabled"));
    registry
        .set_enabled("dev.ternilo.fixture", "1.0.0", true, 4)
        .unwrap();
    assert_eq!(
        harness
            .run(RunId::new("run-4"), "/fixture-echo back")
            .await
            .unwrap()
            .answer,
        "hello back"
    );
    registry.revoke("dev.ternilo.fixture", "1.0.0", 5).unwrap();
    let uninstall = registry
        .uninstall("dev.ternilo.fixture", "1.0.0")
        .unwrap_err();
    assert_eq!(uninstall.code, ternilo_protocol::ErrorCode::PolicyDenied);
    assert!(uninstall.message.contains("permanent tombstone"));
    let revoked = harness
        .run(RunId::new("run-5"), "/fixture-echo denied")
        .await
        .unwrap();
    assert!(revoked.answer.contains("revoked") || revoked.answer.contains("disabled"));
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_package_tool_conflict_fails_mount_with_the_tool_name() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[10; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    install(
        &registry,
        &key,
        rhai_manifest("dev.ternilo.first", "1.0.0"),
        SOURCE,
        BTreeSet::new(),
        2,
    );
    install(
        &registry,
        &key,
        rhai_manifest("dev.ternilo.second", "1.0.0"),
        SOURCE,
        BTreeSet::new(),
        3,
    );
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog
        .register(extension_mount_factory(Arc::clone(&registry)))
        .unwrap();
    let mut duplicate_mount = mounted_profile("dev.ternilo.first", "1.0.0", json!({}));
    duplicate_mount.plugins.push(PluginEntry {
        id: "extension:first-again".to_owned(),
        kind: EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config: json!({
            "package_id": "dev.ternilo.first",
            "version": "1.0.0",
            "settings": {}
        }),
    });
    let duplicate_mount = registry
        .validate_profile_mounts(&duplicate_mount)
        .unwrap_err();
    assert!(duplicate_mount.message.contains("mounted more than once"));

    let mut profile = mounted_profile("dev.ternilo.first", "1.0.0", json!({}));
    profile.plugins.push(PluginEntry {
        id: "extension:second".to_owned(),
        kind: EXTENSION_PACKAGE_KIND.to_owned(),
        enabled: true,
        config: json!({
            "package_id": "dev.ternilo.second",
            "version": "1.0.0",
            "settings": {}
        }),
    });
    let preflight = registry.validate_profile_mounts(&profile).unwrap_err();
    assert!(preflight.message.contains("echo"));
    assert!(preflight.message.contains("dev.ternilo.first@1.0.0"));
    assert!(preflight.message.contains("dev.ternilo.second@1.0.0"));

    let error = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("duplicate tool names must fail composition");
    assert!(error.message.contains("echo"));
    assert!(error.message.contains("already registered"));
}

#[tokio::test(flavor = "multi_thread")]
async fn mount_settings_are_validated_against_the_signed_config_schema() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[18; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = with_echo_command(rhai_manifest("dev.ternilo.settings", "1.0.0"));
    manifest.contributions.tools = vec![tool("echo", "echo_handler")];
    manifest.config_schema = json!({
        "type": "object",
        "properties": { "prefix": { "type": "string" } },
        "required": ["prefix"],
        "additionalProperties": false
    });
    install(&registry, &key, manifest, SOURCE, BTreeSet::new(), 2);
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog
        .register(extension_mount_factory(Arc::clone(&registry)))
        .unwrap();

    let invalid = HarnessSession::boot(
        &catalog,
        &mounted_profile("dev.ternilo.settings", "1.0.0", json!({ "prefix": 7 })),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("invalid extension settings must fail composition");
    assert!(invalid.message.contains("config_schema"));

    let valid = HarnessSession::boot(
        &catalog,
        &mounted_profile(
            "dev.ternilo.settings",
            "1.0.0",
            json!({ "prefix": "schema: " }),
        ),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();
    assert_eq!(
        valid
            .run(RunId::new("settings"), "/fixture-echo valid")
            .await
            .unwrap()
            .answer,
        "schema: valid"
    );
    valid.shutdown().await.unwrap();
}

#[test]
fn public_extension_settings_validation_enforces_required_type_and_pattern() {
    let mut manifest = rhai_manifest("dev.ternilo.settings-validation", "1.0.0");
    manifest.config_schema = json!({
        "type": "object",
        "properties": {
            "prefix": {
                "type": "string",
                "pattern": "^[a-z]+:$"
            }
        },
        "required": ["prefix"],
        "additionalProperties": false
    });

    validate_extension_settings(&manifest, &json!({ "prefix": "valid:" })).unwrap();

    for invalid in [
        json!({}),
        json!({ "prefix": 7 }),
        json!({ "prefix": "INVALID" }),
    ] {
        let error = validate_extension_settings(&manifest, &invalid).unwrap_err();
        assert!(error.message.contains("config_schema"));
    }
}

#[test]
fn extension_version_identity_survives_uninstall_and_registry_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("extensions");
    let key = SigningKey::from_bytes(&[24; 32]);
    let mut manifest = rhai_manifest("dev.ternilo.identity", "1.0.0");
    manifest.requested_capabilities.insert(Capability::Log);
    let bundle = sign_bundle(
        manifest.clone(),
        ExtensionPayload::Utf8(SOURCE.to_owned()),
        &key,
    )
    .unwrap();
    let request = ExtensionInstallRequest {
        bundle,
        granted_capabilities: BTreeSet::new(),
    };

    let registry = ExtensionRegistry::open(root.clone(), ExtensionHostPolicy::default()).unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    registry.install(request.clone(), 2).unwrap();
    registry.uninstall("dev.ternilo.identity", "1.0.0").unwrap();
    drop(registry);

    let registry = ExtensionRegistry::open(root.clone(), ExtensionHostPolicy::default()).unwrap();
    registry.install(request.clone(), 3).unwrap();
    registry.uninstall("dev.ternilo.identity", "1.0.0").unwrap();

    let mut changed_manifest = manifest;
    changed_manifest.description = Some("Different signed manifest".to_owned());
    let changed_bundle = sign_bundle(
        changed_manifest,
        ExtensionPayload::Utf8(SOURCE.to_owned()),
        &key,
    )
    .unwrap();
    let changed = registry
        .install(
            ExtensionInstallRequest {
                bundle: changed_bundle,
                granted_capabilities: BTreeSet::new(),
            },
            4,
        )
        .unwrap_err();
    assert!(changed.message.contains("permanently bound"));

    let changed_grants = registry
        .install(
            ExtensionInstallRequest {
                bundle: request.bundle,
                granted_capabilities: [Capability::Log].into_iter().collect(),
            },
            5,
        )
        .unwrap_err();
    assert!(changed_grants.message.contains("permanently bound"));

    drop(registry);
    let inventory_path = root.join("inventory.json");
    let inventory_bytes = std::fs::read(&inventory_path).unwrap();
    let mut invalid_identity: Value = serde_json::from_slice(&inventory_bytes).unwrap();
    invalid_identity["version_identities"][0]["signature_base64"] = json!("invalid");
    std::fs::write(
        &inventory_path,
        serde_json::to_vec_pretty(&invalid_identity).unwrap(),
    )
    .unwrap();
    let identity = ExtensionRegistry::open(root.clone(), ExtensionHostPolicy::default())
        .err()
        .expect("invalid persisted version identity must be rejected");
    assert!(identity.message.contains("signature"));

    let mut inventory: Value = serde_json::from_slice(&inventory_bytes).unwrap();
    inventory["schema_version"] = json!(2);
    std::fs::write(
        &inventory_path,
        serde_json::to_vec_pretty(&inventory).unwrap(),
    )
    .unwrap();
    let schema = ExtensionRegistry::open(root, ExtensionHostPolicy::default())
        .err()
        .expect("unsupported inventory schema must be rejected");
    assert!(schema.message.contains("expected 1"));
}

#[test]
fn rhai_output_schema_validates_raw_json_before_tool_output_conversion() {
    const RAW_RHAI: &str = r"
fn raw_handler(context, arguments, settings) {
    arguments.value
}
";
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[20; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = rhai_manifest("dev.ternilo.raw-output", "1.0.0");
    let mut contribution = tool("echo", "raw_handler");
    contribution.output_schema = json!({
        "type": "string",
        "pattern": "^accepted:"
    });
    manifest.contributions.tools = vec![contribution];
    install(&registry, &key, manifest, RAW_RHAI, BTreeSet::new(), 2);
    let resolved = registry.resolve("dev.ternilo.raw-output", "1.0.0").unwrap();
    let output_schema = &resolved.installed.manifest.contributions.tools[0].output_schema;

    let valid = crate::runtime::invoke_extension(
        &resolved.installed,
        &resolved.compiled,
        "raw_handler",
        output_schema,
        &execution_context("rhai-valid"),
        json!({ "value": "accepted:value" }),
        json!({}),
    )
    .unwrap();
    assert_eq!(valid.content, "accepted:value");
    assert!(!valid.is_error);

    let invalid = crate::runtime::invoke_extension(
        &resolved.installed,
        &resolved.compiled,
        "raw_handler",
        output_schema,
        &execution_context("rhai-invalid"),
        json!({ "value": "rejected" }),
        json!({}),
    )
    .unwrap_err();
    assert_eq!(invalid.code, ternilo_protocol::ErrorCode::Execution);
    assert!(invalid.message.contains("signed output_schema contract"));
    assert!(invalid.message.contains("raw_handler"));
}

#[test]
fn wasm_output_schema_rejects_invalid_raw_json_before_tool_output_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[21; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = wasm_manifest();
    manifest.contributions.tools.truncate(1);
    manifest.contributions.tools[0].output_schema = json!({
        "type": "object",
        "properties": {
            "content": { "const": "expected" },
            "is_error": { "const": false }
        },
        "required": ["content", "is_error"],
        "additionalProperties": false
    });
    let request = ExtensionInstallRequest {
        bundle: sign_bundle(
            manifest,
            ExtensionPayload::Base64(STANDARD.encode(WASM_COMPONENT.as_bytes())),
            &key,
        )
        .unwrap(),
        granted_capabilities: BTreeSet::new(),
    };
    registry.install(request, 2).unwrap();
    let resolved = registry
        .resolve("dev.ternilo.wasm-fixture", "1.0.0")
        .unwrap();
    let contribution = &resolved.installed.manifest.contributions.tools[0];
    let error = crate::runtime::invoke_extension(
        &resolved.installed,
        &resolved.compiled,
        &contribution.handler,
        &contribution.output_schema,
        &execution_context("wasm-invalid"),
        json!({}),
        json!({}),
    )
    .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Execution);
    assert!(error.message.contains("signed output_schema contract"));
    assert!(error.message.contains("WASM Component"));
}

#[test]
fn uninstall_only_removes_an_unshared_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[11; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    install(
        &registry,
        &key,
        rhai_manifest("dev.ternilo.first", "1.0.0"),
        SOURCE,
        BTreeSet::new(),
        2,
    );
    install(
        &registry,
        &key,
        rhai_manifest("dev.ternilo.second", "1.0.0"),
        SOURCE,
        BTreeSet::new(),
        3,
    );
    let artifacts = registry.root().join("artifacts");
    assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), 1);
    registry.uninstall("dev.ternilo.first", "1.0.0").unwrap();
    assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), 1);
    registry.uninstall("dev.ternilo.second", "1.0.0").unwrap();
    assert_eq!(std::fs::read_dir(&artifacts).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "Keep granted and denied workspace reads in one end-to-end authorization scenario."
)]
async fn workspace_read_requires_a_grant_rejects_escape_and_hides_absolute_path() {
    const READER: &str = r#"
fn echo_handler(context, arguments, settings) {
    if arguments.text == "context" { return context; }
    read_workspace_text(arguments.text)
}
"#;
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("note.txt"), "workspace text").unwrap();
    std::fs::write(directory.path().join("outside.txt"), "outside").unwrap();
    let key = SigningKey::from_bytes(&[13; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = with_echo_command(rhai_manifest("dev.ternilo.reader", "1.0.0"));
    manifest.contributions.tools = vec![tool("echo", "echo_handler")];
    manifest.contributions.tools[0].output_schema = json!({
        "oneOf": [
            { "type": "string" },
            {
                "type": "object",
                "properties": {
                    "tenant_id": { "type": "string" },
                    "user_id": { "type": "string" },
                    "agent_id": { "type": "string" },
                    "session_id": { "type": "string" },
                    "run_id": { "type": "string" },
                    "workspace": {
                        "type": "object",
                        "properties": { "workspace_id": { "type": "string" } },
                        "required": ["workspace_id"],
                        "additionalProperties": false
                    }
                },
                "required": [
                    "tenant_id",
                    "user_id",
                    "agent_id",
                    "session_id",
                    "run_id",
                    "workspace"
                ],
                "additionalProperties": false
            }
        ]
    });
    manifest.requested_capabilities = [Capability::WorkspaceRead].into_iter().collect();
    install(
        &registry,
        &key,
        manifest,
        READER,
        [Capability::WorkspaceRead].into_iter().collect(),
        2,
    );
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog
        .register(extension_mount_factory(Arc::clone(&registry)))
        .unwrap();
    let harness = HarnessSession::boot(
        &catalog,
        &mounted_profile("dev.ternilo.reader", "1.0.0", json!({})),
        HostEnvironment::memory(
            identity(),
            Some(WorkspaceBinding {
                workspace_id: WorkspaceId::new("workspace"),
                path: workspace.to_string_lossy().into_owned(),
            }),
            HostPolicy::local(RunLimits::default()),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        harness
            .run(RunId::new("read"), "/fixture-echo note.txt")
            .await
            .unwrap()
            .answer,
        "workspace text"
    );
    let context = harness
        .run(RunId::new("context"), "/fixture-echo context")
        .await
        .unwrap()
        .answer;
    assert!(!context.contains(&workspace.to_string_lossy().into_owned()));
    assert!(context.contains("workspace_id"));
    let escaped = harness
        .run(RunId::new("escape"), "/fixture-echo ../outside.txt")
        .await
        .unwrap();
    assert!(escaped.answer.contains("relative") || escaped.answer.contains("escapes"));
    harness.shutdown().await.unwrap();

    let second = tempfile::tempdir().unwrap();
    let denied_registry = ExtensionRegistry::open(
        second.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    denied_registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut denied_manifest =
        with_echo_command(rhai_manifest("dev.ternilo.reader-denied", "1.0.0"));
    denied_manifest.contributions.tools = vec![tool("echo", "echo_handler")];
    denied_manifest.contributions.tools[0].output_schema = json!({ "type": "string" });
    denied_manifest.requested_capabilities = [Capability::WorkspaceRead].into_iter().collect();
    install(
        &denied_registry,
        &key,
        denied_manifest,
        READER,
        BTreeSet::new(),
        2,
    );
    let mut denied_catalog = ternilo_builtins::catalog().unwrap();
    denied_catalog
        .register(extension_mount_factory(Arc::clone(&denied_registry)))
        .unwrap();
    let denied_harness = HarnessSession::boot(
        &denied_catalog,
        &mounted_profile("dev.ternilo.reader-denied", "1.0.0", json!({})),
        HostEnvironment::memory(
            identity(),
            Some(WorkspaceBinding {
                workspace_id: WorkspaceId::new("workspace"),
                path: workspace.to_string_lossy().into_owned(),
            }),
            HostPolicy::local(RunLimits::default()),
        ),
    )
    .await
    .unwrap();
    let denied = denied_harness
        .run(RunId::new("denied"), "/fixture-echo note.txt")
        .await
        .unwrap();
    assert!(denied.answer.contains("workspace_read capability"));
    denied_harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm_component_uses_the_same_registry_mount_and_dispatch_contract() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[17; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let request = ExtensionInstallRequest {
        bundle: sign_bundle(
            wasm_manifest(),
            ExtensionPayload::Base64(STANDARD.encode(WASM_COMPONENT.as_bytes())),
            &key,
        )
        .unwrap(),
        granted_capabilities: BTreeSet::new(),
    };
    registry.install(request, 2).unwrap();
    let mut catalog = ternilo_builtins::catalog().unwrap();
    catalog
        .register(extension_mount_factory(Arc::clone(&registry)))
        .unwrap();
    let harness = HarnessSession::boot(
        &catalog,
        &mounted_profile("dev.ternilo.wasm-fixture", "1.0.0", json!({})),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();
    let skill = harness
        .skill("extension-fixture")
        .await
        .unwrap()
        .expect("WASM Extension exposes the same static Skill contract as Rhai");
    assert_eq!(
        skill.content,
        "Static skill from dev.ternilo.wasm-fixture@1.0.0."
    );
    assert_eq!(skill.summary.invocation, SkillInvocationPolicy::default());
    assert_eq!(
        skill.summary.source,
        "extension:dev.ternilo.wasm-fixture@1.0.0"
    );
    assert_eq!(
        skill.summary.provider,
        crate::plugin::extension_skill_provider_name("dev.ternilo.wasm-fixture", "1.0.0")
    );
    assert!(skill.resource_base.is_none());
    let result = harness
        .run(RunId::new("wasm"), "/fixture-echo ignored")
        .await
        .unwrap();
    assert_eq!(result.answer, "alpha");
    let second = harness
        .run(
            RunId::new("wasm-second"),
            "/extension-enable dev.example 1.0.0",
        )
        .await
        .unwrap();
    assert_eq!(second.answer, "bravo");
    registry
        .set_enabled("dev.ternilo.wasm-fixture", "1.0.0", false, 3)
        .unwrap();
    let disabled = harness
        .run(RunId::new("disabled"), "/fixture-echo ignored")
        .await
        .unwrap();
    assert!(disabled.answer.contains("disabled"));
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rhai_hook_runs_through_shared_registry_with_host_owned_metadata_and_matcher() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[40; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.rhai-hook";
    let mut manifest = with_echo_command(rhai_manifest(package_id, "1.0.0"));
    manifest.contributions.tools = vec![tool("echo", "echo_handler")];
    manifest.contributions.prompt_sections.clear();
    manifest.contributions.skills.clear();
    manifest.contributions.hooks = vec![ExtensionHookContribution {
        matcher: ExtensionHookMatcher::ToolNames {
            names: vec!["echo".to_owned()],
        },
        ..hook("guard-echo", HookPoint::PreToolUse, "allow_echo")
    }];
    install(
        &registry,
        &key,
        manifest,
        RHAI_HOOK_SOURCE,
        BTreeSet::new(),
        2,
    );

    let harness = HarnessSession::boot(
        &catalog(Arc::clone(&registry)),
        &mounted_profile(
            package_id,
            "1.0.0",
            json!({ "reason": "signed settings reason" }),
        ),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();
    let outcome = harness
        .run(RunId::new("rhai-hook"), "/fixture-echo Ada")
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Ada");
    let result = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::HookResult { result }
                if result.handler_id == "extension:dev.ternilo.rhai-hook@1.0.0:hook:guard-echo" =>
            {
                Some(result)
            }
            _ => None,
        })
        .expect("matching Extension Hook emits the shared durable result");
    assert_eq!(result.dialect, "extension-rhai");
    assert_eq!(result.point, HookPoint::PreToolUse);
    assert_eq!(result.decision, HookDecision::Allow);
    assert_eq!(result.reason.as_deref(), Some("signed settings reason"));
    assert_eq!(
        result.additional_context.as_deref(),
        Some("checked by Rhai")
    );
    assert_eq!(result.exit_code, None);
    assert_eq!(result.stderr_summary, None);
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rhai_hook_denies_and_invalid_host_fields_degrade_to_neutral_diagnostics() {
    for (package_id, hook_handler, expected_error) in [
        (
            "dev.ternilo.rhai-hook-deny",
            "deny_echo",
            Some("blocked by Rhai"),
        ),
        ("dev.ternilo.rhai-hook-spoof", "spoof_host_fields", None),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[41; 32]);
        let registry = ExtensionRegistry::open(
            directory.path().join("extensions"),
            ExtensionHostPolicy::default(),
        )
        .unwrap();
        registry.trust_publisher(publisher(&key), 1).unwrap();
        let mut manifest = with_echo_command(rhai_manifest(package_id, "1.0.0"));
        manifest.contributions.tools = vec![tool("echo", "echo_handler")];
        manifest.contributions.prompt_sections.clear();
        manifest.contributions.skills.clear();
        manifest.contributions.hooks = vec![ExtensionHookContribution {
            matcher: ExtensionHookMatcher::ToolNames {
                names: vec!["echo".to_owned()],
            },
            ..hook("guard-echo", HookPoint::PreToolUse, hook_handler)
        }];
        install(
            &registry,
            &key,
            manifest,
            RHAI_HOOK_SOURCE,
            BTreeSet::new(),
            2,
        );
        let harness = HarnessSession::boot(
            &catalog(Arc::clone(&registry)),
            &mounted_profile(
                package_id,
                "1.0.0",
                json!({ "reason": expected_error.unwrap_or("unused") }),
            ),
            HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
        )
        .await
        .unwrap();

        if let Some(expected_error) = expected_error {
            let outcome = harness
                .run(RunId::new("rhai-hook-deny"), "/fixture-echo Ada")
                .await
                .unwrap();
            assert!(outcome.answer.contains(expected_error));
            assert!(outcome.events.iter().any(|event| matches!(
                &event.kind,
                SessionEventKind::HookResult { result }
                    if result.decision == HookDecision::Deny
                        && result.reason.as_deref() == Some(expected_error)
            )));
        } else {
            let outcome = harness
                .run(RunId::new("rhai-hook-neutral"), "/fixture-echo Ada")
                .await
                .unwrap();
            assert_eq!(outcome.answer, "Ada");
            let result = outcome
                .events
                .iter()
                .find_map(|event| match &event.kind {
                    SessionEventKind::HookResult { result }
                        if result.handler_id.ends_with(":hook:guard-echo") =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
                .expect("invalid guest result remains durable as a neutral Hook result");
            assert_eq!(result.decision, HookDecision::None);
            assert!(!result.stop);
            assert!(
                result
                    .stderr_summary
                    .as_deref()
                    .is_some_and(|value| value.contains("unknown field"))
            );
            assert_ne!(result.handler_id, "guest-spoof");
            assert_ne!(result.dialect, "guest-spoof");
        }
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wasm_hook_uses_the_generic_runtime_abi_and_shared_enforcement() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[42; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = wasm_manifest();
    manifest.contributions.hooks = vec![ExtensionHookContribution {
        matcher: ExtensionHookMatcher::ToolNames {
            names: vec!["echo".to_owned()],
        },
        ..hook("guard-echo", HookPoint::PreToolUse, "guard")
    }];
    registry
        .install(
            ExtensionInstallRequest {
                bundle: sign_bundle(
                    manifest,
                    ExtensionPayload::Base64(STANDARD.encode(WASM_COMPONENT.as_bytes())),
                    &key,
                )
                .unwrap(),
                granted_capabilities: BTreeSet::new(),
            },
            2,
        )
        .unwrap();
    let harness = HarnessSession::boot(
        &catalog(Arc::clone(&registry)),
        &mounted_profile("dev.ternilo.wasm-fixture", "1.0.0", json!({})),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();
    let outcome = harness
        .run(RunId::new("wasm-hook"), "/fixture-echo ignored")
        .await
        .unwrap();
    assert!(outcome.answer.contains("blocked by wasm"));
    assert!(outcome.events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::HookResult { result }
            if result.dialect == "extension-wasm-component"
                && result.decision == HookDecision::Deny
                && result.reason.as_deref() == Some("blocked by wasm")
    )));
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "Keep command discovery, argument resolution, execution, and both hook observations together."
)]
async fn extension_commands_resolve_catalog_arguments_and_run_through_both_tool_hooks() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[45; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.command-runtime";
    let mut manifest = rhai_manifest(package_id, "1.0.0");
    let mut review = tool("workspace_review", "review");
    review.spec.input_schema = json!({
        "type": "object",
        "properties": {
            "source": { "type": "string", "const": "extension" },
            "request": { "type": "string" }
        },
        "required": ["source", "request"],
        "additionalProperties": false
    });
    manifest.contributions.tools = vec![review];
    manifest.contributions.prompt_sections.clear();
    manifest.contributions.skills.clear();
    manifest.contributions.hooks = vec![
        ExtensionHookContribution {
            matcher: ExtensionHookMatcher::ToolNames {
                names: vec!["workspace_review".to_owned()],
            },
            ..hook("before-review", HookPoint::PreToolUse, "observe_command")
        },
        ExtensionHookContribution {
            matcher: ExtensionHookMatcher::ToolNames {
                names: vec!["workspace_review".to_owned()],
            },
            ..hook("after-review", HookPoint::PostToolUse, "observe_command")
        },
    ];
    manifest.contributions.commands = vec![command("review", "workspace_review")];
    install(
        &registry,
        &key,
        manifest,
        RHAI_COMMAND_SOURCE,
        BTreeSet::new(),
        2,
    );

    let harness = HarnessSession::boot(
        &catalog(Arc::clone(&registry)),
        &mounted_profile(
            package_id,
            "1.0.0",
            json!({ "reason": "extension command hook" }),
        ),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();
    let entry = harness
        .command_catalog()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.descriptor.name == "review")
        .expect("Extension command appears in the shared Session catalog");
    assert_eq!(entry.tool_name, "workspace_review");
    assert_eq!(entry.descriptor.description, "Fixture command review");
    assert_eq!(
        entry.descriptor.input.unwrap(),
        ternilo_protocol::CommandInputDescriptor {
            hint: "<request>".to_owned(),
            images: false,
        }
    );

    let outcome = harness
        .run(RunId::new("extension-command"), "/review inspect workspace")
        .await
        .unwrap();
    assert_eq!(outcome.answer, "extension:inspect workspace");
    let call = outcome
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolCallStarted { call } if call.name == "workspace_review" => {
                Some(call)
            }
            _ => None,
        })
        .expect("Extension command enters the canonical Tool pipeline");
    assert_eq!(
        call.arguments,
        json!({ "source": "extension", "request": "inspect workspace" })
    );
    let hook_points = outcome
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::HookResult { result }
                if result.handler_id.contains("command-runtime") =>
            {
                Some(result.point)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        hook_points,
        vec![HookPoint::PreToolUse, HookPoint::PostToolUse]
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_commands_reject_unmapped_suffix_and_target_schema_mismatch() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[46; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.command-input";
    let mut manifest = rhai_manifest(package_id, "1.0.0");
    let mut fixed_tool = tool("fixed_review", "fixed_review");
    fixed_tool.spec.input_schema = json!({
        "type": "object",
        "properties": { "mode": { "type": "string", "const": "full" } },
        "required": ["mode"],
        "additionalProperties": false
    });
    let mut numeric_tool = tool("numeric_review", "numeric_review");
    numeric_tool.spec.input_schema = json!({
        "type": "object",
        "properties": { "request": { "type": "integer" } },
        "required": ["request"],
        "additionalProperties": false
    });
    let mut fixed_command = command("fixed-review", "fixed_review");
    fixed_command.input = None;
    fixed_command.fixed_arguments = json!({ "mode": "full" });
    let mut numeric_command = command("numeric-review", "numeric_review");
    numeric_command.fixed_arguments = json!({});
    manifest.contributions.tools = vec![fixed_tool, numeric_tool];
    manifest.contributions.prompt_sections.clear();
    manifest.contributions.skills.clear();
    manifest.contributions.commands = vec![fixed_command, numeric_command];
    install(
        &registry,
        &key,
        manifest,
        RHAI_COMMAND_SOURCE,
        BTreeSet::new(),
        2,
    );
    let harness = HarnessSession::boot(
        &catalog(Arc::clone(&registry)),
        &mounted_profile(package_id, "1.0.0", json!({})),
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .unwrap();

    assert_eq!(
        harness
            .run(RunId::new("fixed-command"), "/fixed-review")
            .await
            .unwrap()
            .answer,
        "full"
    );
    let suffix = harness
        .run(
            RunId::new("fixed-command-extra"),
            "/fixed-review unexpected",
        )
        .await
        .unwrap();
    assert!(
        suffix
            .answer
            .contains("/fixed-review does not accept input")
    );
    let schema = harness
        .run(RunId::new("numeric-command"), "/numeric-review seven")
        .await
        .unwrap();
    assert!(schema.answer.contains("target tool input schema"));
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn builtin_command_collision_fails_at_composed_boot_and_rolls_back_prior_stages() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[47; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.command-rollback";
    let version = "1.0.0";
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools = vec![tool("command_probe", "echo_handler")];
    manifest.contributions.hooks = vec![hook(
        "observe-command",
        HookPoint::UserPromptSubmit,
        "allow_echo",
    )];
    manifest.contributions.commands = vec![command("read", "command_probe")];
    install(
        &registry,
        &key,
        manifest,
        RHAI_HOOK_SOURCE,
        BTreeSet::new(),
        2,
    );

    let prompts = Arc::new(RecordingPrompts::default());
    let hooks = Arc::new(RecordingHooks::default());
    let catalog = catalog_with_recording_hooks(
        Arc::clone(&registry),
        Arc::clone(&hooks),
        Arc::clone(&prompts),
    );
    let mut profile = mounted_profile(package_id, version, json!({}));
    use_recording_prompts(&mut profile, package_id, version);
    use_recording_hooks(&mut profile);
    let error = HarnessSession::boot(
        &catalog,
        &profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("an Extension command must not replace a builtin command");
    assert!(error.message.contains("command \"read\""));
    assert!(error.message.contains("already registered"));
    let hook_id = format!("extension:{package_id}@{version}:hook:observe-command");
    assert_eq!(
        hooks.events(),
        vec![
            format!("registered:{hook_id}"),
            format!("unregistered:{hook_id}"),
        ]
    );
    let prompt_id = format!("extension:{package_id}@{version}:fixture-guidance");
    assert_eq!(
        prompts.extension_events(&prompt_id),
        vec![
            format!("registered:{prompt_id}"),
            format!("unregistered:{prompt_id}"),
        ]
    );
}

#[test]
fn profile_preflight_rejects_cross_package_command_names_before_boot() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[48; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    for (package_id, tool_name, now_ms) in [
        ("dev.ternilo.command-first", "first_probe", 2),
        ("dev.ternilo.command-second", "second_probe", 3),
    ] {
        let mut manifest = rhai_manifest(package_id, "1.0.0");
        manifest.contributions.tools = vec![tool(tool_name, "echo_handler")];
        manifest.contributions.prompt_sections.clear();
        manifest.contributions.skills.clear();
        manifest.contributions.commands = vec![command("shared-review", tool_name)];
        install(&registry, &key, manifest, SOURCE, BTreeSet::new(), now_ms);
    }
    let mut profile = mounted_profile("dev.ternilo.command-first", "1.0.0", json!({}));
    mount_extension(&mut profile, "dev.ternilo.command-second", "1.0.0");
    let error = registry.validate_profile_mounts(&profile).unwrap_err();
    assert!(error.message.contains("shared-review"));
    assert!(error.message.contains("dev.ternilo.command-first@1.0.0"));
    assert!(error.message.contains("dev.ternilo.command-second@1.0.0"));
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_registration_failure_rolls_back_skill_and_prompt_and_unmount_unregisters_hook() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[43; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.hook-lifecycle";
    let version = "1.0.0";
    let physical_id = format!("extension:{package_id}@{version}:hook:guard-echo");
    let mut manifest = rhai_manifest(package_id, version);
    manifest.contributions.tools.clear();
    manifest.contributions.hooks = vec![hook(
        "guard-echo",
        HookPoint::UserPromptSubmit,
        "allow_echo",
    )];
    install(
        &registry,
        &key,
        manifest,
        RHAI_HOOK_SOURCE,
        BTreeSet::new(),
        2,
    );

    let failed_prompts = Arc::new(RecordingPrompts::default());
    let failed_hooks = RecordingHooks::with_preloaded(physical_id.clone());
    let failed_catalog = catalog_with_recording_hooks(
        Arc::clone(&registry),
        Arc::clone(&failed_hooks),
        Arc::clone(&failed_prompts),
    );
    let mut failed_profile = mounted_profile(package_id, version, json!({}));
    use_recording_prompts(&mut failed_profile, package_id, version);
    use_recording_hooks(&mut failed_profile);
    let error = HarnessSession::boot(
        &failed_catalog,
        &failed_profile,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .err()
    .expect("duplicate physical Hook id must fail activation");
    assert!(error.message.contains("hook"));
    assert_eq!(
        failed_hooks.events(),
        vec![format!("rejected:{physical_id}")]
    );
    let prompt_id = format!("extension:{package_id}@{version}:fixture-guidance");
    assert_eq!(
        failed_prompts.extension_events(&prompt_id),
        vec![
            format!("registered:{prompt_id}"),
            format!("unregistered:{prompt_id}"),
        ]
    );

    let mounted_prompts = Arc::new(RecordingPrompts::default());
    let mounted_hooks = Arc::new(RecordingHooks::default());
    let mounted_catalog = catalog_with_recording_hooks(
        Arc::clone(&registry),
        Arc::clone(&mounted_hooks),
        Arc::clone(&mounted_prompts),
    );
    let mut mounted = mounted_profile(package_id, version, json!({ "reason": "ok" }));
    use_recording_prompts(&mut mounted, package_id, version);
    use_recording_hooks(&mut mounted);
    let harness = HarnessSession::boot(
        &mounted_catalog,
        &mounted,
        HostEnvironment::memory(identity(), None, HostPolicy::local(RunLimits::default())),
    )
    .await
    .expect("failed Hook activation must leave the skill provider reusable");
    assert!(harness.skill("extension-fixture").await.unwrap().is_some());
    harness.shutdown().await.unwrap();
    assert_eq!(
        mounted_hooks.events(),
        vec![
            format!("registered:{physical_id}"),
            format!("unregistered:{physical_id}"),
        ]
    );
}

#[test]
fn rhai_tool_invocation_keeps_in_flight_cancellation_after_generic_runtime_refactor() {
    const SPIN: &str = r"
fn spin(context, arguments, settings) {
    while true {}
}
";
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[44; 32]);
    let mut policy = ExtensionHostPolicy::default();
    policy.maximum_rhai_limits.sandbox.max_operations = 1_000_000_000_000;
    policy.maximum_rhai_limits.max_wall_ms = 10_000;
    let registry = ExtensionRegistry::open(directory.path().join("extensions"), policy).unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let mut manifest = rhai_manifest("dev.ternilo.cancel", "1.0.0");
    manifest.contributions.tools = vec![tool("spin", "spin")];
    manifest.contributions.prompt_sections.clear();
    manifest.contributions.skills.clear();
    if let ExtensionRuntime::Rhai { limits } = &mut manifest.runtime {
        limits.sandbox.max_operations = 1_000_000_000_000;
        limits.max_wall_ms = 10_000;
    }
    install(&registry, &key, manifest, SPIN, BTreeSet::new(), 2);
    let resolved = registry.resolve("dev.ternilo.cancel", "1.0.0").unwrap();
    let contribution = resolved.installed.manifest.contributions.tools[0].clone();
    let cancellation = RunCancellation::new();
    let context = ToolExecutionContext {
        cancellation: cancellation.clone(),
        ..execution_context("cancel-in-flight")
    };
    let handle = std::thread::spawn(move || {
        crate::runtime::invoke_extension(
            &resolved.installed,
            &resolved.compiled,
            &contribution.handler,
            &contribution.output_schema,
            &context,
            json!({}),
            json!({}),
        )
    });
    std::thread::sleep(std::time::Duration::from_millis(10));
    cancellation.cancel();
    let error = handle.join().unwrap().unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::Cancelled);
    assert!(error.message.contains("cancelled"));
}

#[test]
fn provider_materialization_uses_only_an_enabled_trusted_signed_template() {
    let directory = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[57; 32]);
    let registry = ExtensionRegistry::open(
        directory.path().join("extensions"),
        ExtensionHostPolicy::default(),
    )
    .unwrap();
    registry.trust_publisher(publisher(&key), 1).unwrap();
    let package_id = "dev.ternilo.provider";
    let version = "1.0.0";
    let mut manifest = rhai_manifest(package_id, version);
    let signed_template = provider("sparkvs");
    manifest.contributions.providers = vec![signed_template.clone()];
    install(&registry, &key, manifest, SOURCE, BTreeSet::new(), 2);
    let mut request = ExtensionProviderMaterializeRequest {
        package_id: package_id.to_owned(),
        version: version.to_owned(),
        template: "sparkvs".to_owned(),
        provider_id: "my-sparkvs".to_owned(),
        api_key_ref: None,
    };

    let missing_credential = registry.materialize_provider(&request).unwrap_err();
    assert_eq!(
        missing_credential.code,
        ternilo_protocol::ErrorCode::InvalidInput
    );
    assert!(missing_credential.message.contains("explicit api_key_ref"));

    request.api_key_ref = Some("USER_SELECTED_KEY".to_owned());
    let materialized = registry.materialize_provider(&request).unwrap();
    assert_eq!(materialized.id, "my-sparkvs");
    assert_eq!(materialized.display_name, signed_template.display_name);
    assert_eq!(materialized.base_url, signed_template.base_url);
    assert_eq!(materialized.protocol, signed_template.protocol);
    assert_eq!(materialized.defaults, signed_template.defaults);
    assert_eq!(materialized.models, signed_template.models);
    assert_eq!(materialized.timeout_ms, signed_template.timeout_ms);
    assert_eq!(materialized.max_attempts, signed_template.max_attempts);
    assert_eq!(
        materialized.retry_base_delay_ms,
        signed_template.retry_base_delay_ms
    );
    assert_eq!(
        materialized.api_key_ref.as_deref(),
        Some("USER_SELECTED_KEY")
    );
    materialized.validate().unwrap();

    let unknown_template = ExtensionProviderMaterializeRequest {
        template: "unknown".to_owned(),
        ..request.clone()
    };
    assert_eq!(
        registry
            .materialize_provider(&unknown_template)
            .unwrap_err()
            .code,
        ternilo_protocol::ErrorCode::InvalidInput
    );

    registry.set_enabled(package_id, version, false, 3).unwrap();
    assert_eq!(
        registry.materialize_provider(&request).unwrap_err().code,
        ternilo_protocol::ErrorCode::PolicyDenied
    );
    registry.set_enabled(package_id, version, true, 4).unwrap();
    let mut revoked_publisher = registry.inventory().unwrap();
    revoked_publisher.publishers[0].revoked = true;
    assert_eq!(
        materialize_provider_from_inventory(&revoked_publisher, &request)
            .unwrap_err()
            .code,
        ternilo_protocol::ErrorCode::PolicyDenied
    );

    registry.uninstall(package_id, version).unwrap();
    assert!(registry.materialize_provider(&request).is_err());
    assert_eq!(materialized.id, "my-sparkvs");
    materialized.validate().unwrap();

    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 5);
    assert!(
        serde_json::from_value::<ExtensionProviderMaterializeRequest>(json!({
            "package_id": package_id,
            "version": version,
            "template": "sparkvs",
            "provider_id": "my-sparkvs",
            "api_key_ref": "USER_SELECTED_KEY",
            "base_url": "https://attacker.invalid/v1"
        }))
        .is_err()
    );
}
