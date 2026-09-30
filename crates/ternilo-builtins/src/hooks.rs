use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, FiberId, effect,
};
use linorun_macros::component_descriptor;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ternilo_kernel::{
    HarnessPlugin, HookHandler, HookRegistration, Hooks, HooksProvider, PluginFactory,
    PluginManifest, RunEnvironment, RunEnvironmentClient, Shell, ShellClient,
};
use ternilo_protocol::{
    HarnessError, HookDecision, HookPoint, HookRequest, HookResult, ShellRequest,
};

use crate::{factory as make_factory, parse_config};

pub const REGISTRY_KIND: &str = "ternilo.hooks.registry";
pub const CLAUDE_CODE_KIND: &str = "ternilo.hooks.claude_code";
pub const CODEX_KIND: &str = "ternilo.hooks.codex";

const DEFAULT_TIMEOUT_MS: u64 = 600_000;
const MAX_HOOKS: usize = 128;
const MAX_CONTEXT_CHARS: usize = 64 * 1024;

component_descriptor! {
    static REGISTRY_DESCRIPTOR: () {
        id: "ternilo/builtin-hook-registry@1",
        requires: [],
        provides: [Hooks],
    }
}

component_descriptor! {
    static CLAUDE_CODE_DESCRIPTOR: () {
        id: "ternilo/builtin-claude-code-hooks@1",
        requires: [Hooks, Shell, RunEnvironment],
        provides: [],
    }
}

component_descriptor! {
    static CODEX_DESCRIPTOR: () {
        id: "ternilo/builtin-codex-hooks@1",
        requires: [Hooks, Shell, RunEnvironment],
        provides: [],
    }
}

pub fn registry_factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: REGISTRY_KIND,
            requires: &[],
            provides: &["ternilo/hooks@1"],
        },
        |value| {
            let _: crate::EmptyConfig = parse_config(value)?;
            Ok(Arc::new(HookRegistryPlugin))
        },
    )
    .with_config_schema::<crate::EmptyConfig>()
}

pub fn claude_code_factory() -> PluginFactory {
    bridge_factory(CLAUDE_CODE_KIND, build_claude_code_bridge)
}

pub fn codex_factory() -> PluginFactory {
    bridge_factory(CODEX_KIND, build_codex_bridge)
}

fn bridge_factory(
    kind: &'static str,
    build: fn(Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError>,
) -> PluginFactory {
    make_factory(
        PluginManifest {
            kind,
            requires: &[
                "ternilo/hooks@1",
                "ternilo/shell@1",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        build,
    )
    .with_config_schema::<BridgeConfig>()
}

fn build_claude_code_bridge(value: Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    build_bridge(value, HookDialect::ClaudeCode)
}

fn build_codex_bridge(value: Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    build_bridge(value, HookDialect::Codex)
}

fn build_bridge(
    value: Value,
    dialect: HookDialect,
) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    let config: BridgeConfig = parse_config(value)?;
    if config.config_path.as_os_str().is_empty()
        || config.default_timeout_ms == 0
        || config.default_timeout_ms > DEFAULT_TIMEOUT_MS
        || config.stderr_summary_max_chars == 0
    {
        return Err(HarnessError::composition(
            "hook bridge requires config_path, a positive stderr summary limit, and a default timeout from 1 ms to 10 minutes",
        ));
    }
    Ok(Arc::new(CommandBridgePlugin { dialect, config }))
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BridgeConfig {
    #[serde(alias = "configPath")]
    config_path: PathBuf,
    #[serde(default, alias = "pluginRoot")]
    plugin_root: Option<String>,
    #[serde(default, alias = "projectDir")]
    project_dir: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_timeout_ms", alias = "defaultTimeoutMs")]
    default_timeout_ms: u64,
    #[serde(
        default = "default_stderr_summary_max_chars",
        alias = "stderrSummaryMaxChars"
    )]
    stderr_summary_max_chars: usize,
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

const fn default_stderr_summary_max_chars() -> usize {
    500
}

#[derive(Clone, Copy)]
enum HookDialect {
    ClaudeCode,
    Codex,
}

impl HookDialect {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }
}

struct HookRegistryPlugin;

impl HarnessPlugin for HookRegistryPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let provider: Arc<dyn HooksProvider> = Arc::new(HookRegistry::default());
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Hooks>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide hook registry: {error}"))
                })?;
            Ok(None)
        }))
    }
}

#[derive(Default)]
struct HookRegistryState {
    next: u64,
    handlers: BTreeMap<u64, RegisteredHook>,
}

struct RegisteredHook {
    caller: FiberId,
    hook: HookRegistration,
}

#[derive(Default)]
struct HookRegistry {
    state: Mutex<HookRegistryState>,
}

impl HooksProvider for HookRegistry {
    fn register_hook<'a>(
        &'a self,
        context: CallContext<()>,
        hook: HookRegistration,
    ) -> Pin<Box<dyn Future<Output = Result<u64, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if hook.handler_id.trim().is_empty() {
                return Err(HarnessError::invalid("hook handler id must not be empty"));
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| HarnessError::execution("hook registry lock poisoned"))?;
            if state
                .handlers
                .values()
                .any(|registered| registered.hook.handler_id == hook.handler_id)
            {
                return Err(HarnessError::composition(format!(
                    "hook handler {:?} is already registered",
                    hook.handler_id
                )));
            }
            let registration = state.next;
            state.next = state
                .next
                .checked_add(1)
                .ok_or_else(|| HarnessError::execution("hook registration id exhausted"))?;
            state.handlers.insert(
                registration,
                RegisteredHook {
                    caller: context.caller,
                    hook,
                },
            );
            Ok(registration)
        })
    }

    fn unregister_hook<'a>(
        &'a self,
        _: CallContext<()>,
        registration: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.state
                .lock()
                .map_err(|_| HarnessError::execution("hook registry lock poisoned"))?
                .handlers
                .remove(&registration)
                .map(|_| ())
                .ok_or_else(|| {
                    HarnessError::execution(format!("unknown hook registration {registration}"))
                })
        })
    }

    fn run<'a>(
        &'a self,
        _: CallContext<()>,
        request: HookRequest,
    ) -> Pin<Box<dyn Future<Output = Vec<HookResult>> + Send + 'a>> {
        Box::pin(async move {
            let handlers = self
                .state
                .lock()
                .expect("hook registry lock poisoned")
                .handlers
                .iter()
                .filter(|(_, registration)| registration.hook.handler.matches(&request))
                .map(|(registration, registered)| {
                    (
                        registered.caller,
                        *registration,
                        Arc::clone(&registered.hook.handler),
                    )
                })
                .collect::<Vec<_>>();
            let mut handlers = handlers;
            handlers.sort_by_key(|(caller, registration, _)| (*caller, *registration));
            let mut results = Vec::with_capacity(handlers.len());
            for (_, _, handler) in handlers {
                results.push(handler.execute(request.clone()).await);
            }
            results
        })
    }
}

struct CommandBridgePlugin {
    dialect: HookDialect,
    config: BridgeConfig,
}

impl HarnessPlugin for CommandBridgePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        match self.dialect {
            HookDialect::ClaudeCode => &CLAUDE_CODE_DESCRIPTOR,
            HookDialect::Codex => &CODEX_DESCRIPTOR,
        }
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let hooks = context
            .context()
            .service::<Hooks>()
            .expect("hook bridge declares Hooks");
        let shell = context
            .context()
            .service::<Shell>()
            .expect("hook bridge declares Shell");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("hook bridge declares RunEnvironment");
        let dialect = self.dialect;
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let definitions = load_definitions(&config, dialect)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let mut registrations = Vec::with_capacity(definitions.len());
            for definition in definitions {
                let handler_id = definition.handler_id.clone();
                let handler: Arc<dyn HookHandler> = Arc::new(CommandHookHandler {
                    definition,
                    shell: shell.clone(),
                    environment: environment.clone(),
                });
                registrations.push(
                    hooks
                        .register_hook(HookRegistration {
                            handler_id,
                            handler,
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            Ok(Some(effect::inverse(move || async move {
                for registration in registrations.into_iter().rev() {
                    hooks
                        .unregister_hook(registration)
                        .await
                        .map_err(|error| CleanupError::user(error.to_string()))?;
                }
                Ok(())
            })))
        }))
    }
}

#[derive(Clone)]
struct CommandHookDefinition {
    handler_id: String,
    dialect: HookDialect,
    point: HookPoint,
    matcher: Option<String>,
    command: String,
    timeout_ms: u64,
    project_dir: Option<String>,
    model: Option<String>,
    stderr_summary_max_chars: usize,
}

struct CommandHookHandler {
    definition: CommandHookDefinition,
    shell: ShellClient,
    environment: RunEnvironmentClient,
}

impl HookHandler for CommandHookHandler {
    fn matches(&self, request: &HookRequest) -> bool {
        if request.point != self.definition.point {
            return false;
        }
        if !matches!(
            request.point,
            HookPoint::SessionStart | HookPoint::PreToolUse | HookPoint::PostToolUse
        ) {
            return true;
        }
        let subject = match request.point {
            HookPoint::SessionStart => Some("startup"),
            HookPoint::PreToolUse | HookPoint::PostToolUse => {
                request.tool_call.as_ref().map(|call| call.name.as_str())
            }
            HookPoint::UserPromptSubmit | HookPoint::Stop => None,
        };
        subject.is_some_and(|subject| {
            matcher_matches(
                self.definition.matcher.as_deref(),
                subject,
                self.definition.dialect,
            )
        })
    }

    fn execute<'a>(
        &'a self,
        request: HookRequest,
    ) -> Pin<Box<dyn Future<Output = HookResult> + Send + 'a>> {
        Box::pin(async move {
            let started = now_ms();
            let identity = self.environment.identity().await;
            let workspace = self.environment.workspace().await;
            let cwd = workspace
                .as_ref()
                .map(|workspace| workspace.path.clone())
                .unwrap_or_default();
            let payload = hook_payload(&request, &identity, &cwd, &self.definition);
            let mut env = BTreeMap::new();
            if matches!(self.definition.dialect, HookDialect::ClaudeCode) {
                env.insert(
                    "CLAUDE_PROJECT_DIR".to_owned(),
                    self.definition.project_dir.clone().unwrap_or(cwd),
                );
            }
            let mut stdin = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
            if matches!(self.definition.dialect, HookDialect::ClaudeCode) {
                stdin.push('\n');
            }
            match self
                .shell
                .execute(
                    request.run_id.clone(),
                    ShellRequest {
                        command: self.definition.command.clone(),
                        timeout_ms: self.definition.timeout_ms,
                        full_access: false,
                        stdin: Some(stdin),
                        env,
                    },
                )
                .await
            {
                Ok(output) => parse_hook_output(
                    &self.definition,
                    output.exit_code,
                    &output.stdout,
                    &output.stderr,
                    now_ms().saturating_sub(started),
                ),
                Err(error) => neutral_failure(
                    &self.definition,
                    &error.to_string(),
                    now_ms().saturating_sub(started),
                ),
            }
        })
    }
}

async fn load_definitions(
    config: &BridgeConfig,
    dialect: HookDialect,
) -> Result<Vec<CommandHookDefinition>, HarnessError> {
    let document = read_hook_document(&config.config_path).await?;
    let root = document
        .get("hooks")
        .unwrap_or(&document)
        .as_object()
        .ok_or_else(|| HarnessError::composition("hook config must contain a hooks object"))?;
    let mut definitions = Vec::new();
    let mut ids = HashSet::new();
    for (event, groups) in root {
        let Some(point) = parse_point(event) else {
            continue;
        };
        let groups = groups.as_array().ok_or_else(|| {
            HarnessError::composition(format!("hook event {event} must contain an array"))
        })?;
        for (group_index, group) in groups.iter().enumerate() {
            let group = group.as_object().ok_or_else(|| {
                HarnessError::composition(format!("hook event {event} group must be an object"))
            })?;
            let matcher = if matches!(point, HookPoint::UserPromptSubmit | HookPoint::Stop) {
                None
            } else {
                string_field(group, "matcher")
            };
            validate_matcher(matcher.as_deref(), dialect)?;
            let commands = group
                .get("hooks")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    HarnessError::composition(format!(
                        "hook event {event} group has no hooks array"
                    ))
                })?;
            for (hook_index, command) in commands.iter().enumerate() {
                let Some(command) = command.as_object() else {
                    continue;
                };
                if string_field(command, "type")
                    .as_deref()
                    .unwrap_or("command")
                    != "command"
                    || matches!(dialect, HookDialect::Codex)
                        && command.get("async").and_then(Value::as_bool) == Some(true)
                {
                    continue;
                }
                let mut shell_command = string_field(command, "command")
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        HarnessError::composition(format!("hook event {event} command is empty"))
                    })?;
                if matches!(dialect, HookDialect::ClaudeCode) {
                    if let Some(plugin_root) = &config.plugin_root {
                        shell_command = shell_command.replace("${CLAUDE_PLUGIN_ROOT}", plugin_root);
                    }
                    if let Some(project_dir) = &config.project_dir {
                        shell_command = shell_command.replace("${CLAUDE_PROJECT_DIR}", project_dir);
                    }
                }
                let timeout_ms = command
                    .get("timeout")
                    .or_else(|| command.get("timeoutSec"))
                    .and_then(Value::as_u64)
                    .map_or(config.default_timeout_ms, |seconds| {
                        seconds.saturating_mul(1_000)
                    });
                if timeout_ms == 0 || timeout_ms > DEFAULT_TIMEOUT_MS {
                    return Err(HarnessError::composition(format!(
                        "hook event {event} timeout must be from 1 second to 10 minutes"
                    )));
                }
                let handler_id =
                    format!("{}:{event}:{group_index}:{hook_index}", dialect.wire_name());
                if !ids.insert(handler_id.clone()) || definitions.len() == MAX_HOOKS {
                    return Err(HarnessError::composition(format!(
                        "hook config exceeds {MAX_HOOKS} unique command hooks"
                    )));
                }
                definitions.push(CommandHookDefinition {
                    handler_id,
                    dialect,
                    point,
                    matcher: matcher.clone(),
                    command: shell_command,
                    timeout_ms,
                    project_dir: config.project_dir.clone(),
                    model: config.model.clone(),
                    stderr_summary_max_chars: config.stderr_summary_max_chars,
                });
            }
        }
    }
    Ok(definitions)
}

async fn read_hook_document(path: &PathBuf) -> Result<Value, HarnessError> {
    let bytes = tokio::fs::read(path).await.map_err(|error| {
        HarnessError::composition(format!("read hook config {}: {error}", path.display()))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        HarnessError::composition(format!("parse hook config {}: {error}", path.display()))
    })
}

fn parse_point(value: &str) -> Option<HookPoint> {
    match value {
        "SessionStart" => Some(HookPoint::SessionStart),
        "UserPromptSubmit" => Some(HookPoint::UserPromptSubmit),
        "PreToolUse" => Some(HookPoint::PreToolUse),
        "PostToolUse" => Some(HookPoint::PostToolUse),
        "Stop" => Some(HookPoint::Stop),
        _ => None,
    }
}

fn string_field(object: &Map<String, Value>, name: &str) -> Option<String> {
    object.get(name).and_then(Value::as_str).map(str::to_owned)
}

fn validate_matcher(matcher: Option<&str>, dialect: HookDialect) -> Result<(), HarnessError> {
    if matcher.is_none_or(|matcher| matcher.is_empty() || matcher == "*") {
        return Ok(());
    }
    let matcher = matcher.expect("checked above");
    if matches!(dialect, HookDialect::ClaudeCode) && claude_literal(matcher) {
        return Ok(());
    }
    Regex::new(matcher).map(|_| ()).map_err(|error| {
        HarnessError::composition(format!(
            "invalid {} hook matcher {matcher:?}: {error}",
            dialect.wire_name()
        ))
    })
}

fn matcher_matches(matcher: Option<&str>, query: &str, dialect: HookDialect) -> bool {
    let Some(matcher) = matcher.filter(|matcher| !matcher.is_empty() && *matcher != "*") else {
        return true;
    };
    if matches!(dialect, HookDialect::ClaudeCode) && claude_literal(matcher) {
        matcher.split('|').any(|candidate| candidate == query)
    } else {
        Regex::new(matcher).is_ok_and(|pattern| pattern.is_match(query))
    }
}

fn claude_literal(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'|'))
}

fn hook_payload(
    request: &HookRequest,
    identity: &ternilo_protocol::SessionIdentity,
    cwd: &str,
    definition: &CommandHookDefinition,
) -> Value {
    let mut payload = json!({
        "session_id": identity.session_id,
        "cwd": cwd,
        "hook_event_name": request.point.wire_name(),
    });
    match definition.dialect {
        HookDialect::ClaudeCode => {
            payload["transcript_path"] = json!("");
        }
        HookDialect::Codex => {
            payload["transcript_path"] = Value::Null;
            payload["model"] = json!(definition.model.as_deref().unwrap_or_default());
            payload["permission_mode"] = json!("default");
            if request.point != HookPoint::SessionStart {
                payload["turn_id"] = json!(request.run_id.as_str());
            }
        }
    }
    if request.point == HookPoint::SessionStart {
        payload["source"] = json!("startup");
    }
    if let Some(prompt) = &request.prompt {
        payload["prompt"] = json!(prompt);
    }
    if let Some(call) = &request.tool_call {
        payload["tool_name"] = json!(call.name);
        payload["tool_input"] = match definition.dialect {
            HookDialect::ClaudeCode => call.arguments.clone(),
            HookDialect::Codex => json!({
                "command": call.arguments.get("command").and_then(Value::as_str).unwrap_or_default()
            }),
        };
        payload["tool_use_id"] = json!(call.id);
    }
    if let Some(output) = &request.tool_output {
        payload["tool_response"] = json!(output.content);
    }
    if let Some(answer) = &request.answer {
        if matches!(definition.dialect, HookDialect::Codex) {
            payload["last_assistant_message"] = Value::Null;
        } else {
            payload["last_assistant_message"] = json!(answer);
        }
    }
    if request.point == HookPoint::Stop {
        payload["stop_hook_active"] = json!(false);
    }
    payload
}

fn parse_hook_output(
    definition: &CommandHookDefinition,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    duration_ms: u64,
) -> HookResult {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    let mut result = HookResult {
        handler_id: definition.handler_id.clone(),
        dialect: definition.dialect.wire_name().to_owned(),
        point: definition.point,
        decision: if exit_code == Some(2) {
            HookDecision::Deny
        } else {
            HookDecision::None
        },
        reason: (exit_code == Some(2) && !stderr.is_empty())
            .then(|| truncate(stderr, definition.stderr_summary_max_chars)),
        stop: false,
        stop_reason: None,
        additional_context: None,
        system_message: None,
        exit_code,
        stderr_summary: (!stderr.is_empty())
            .then(|| truncate(stderr, definition.stderr_summary_max_chars)),
        duration_ms,
    };
    if exit_code != Some(0) {
        return result;
    }
    let parsed = stdout
        .starts_with('{')
        .then(|| serde_json::from_str::<Value>(stdout).ok())
        .flatten()
        .and_then(|value| value.as_object().cloned());
    let Some(parsed) = parsed else {
        if !stdout.is_empty()
            && matches!(
                definition.point,
                HookPoint::SessionStart | HookPoint::UserPromptSubmit
            )
        {
            result.additional_context = Some(truncate(stdout, MAX_CONTEXT_CHARS));
        }
        return result;
    };
    if parsed.get("continue").and_then(Value::as_bool) == Some(false) {
        result.stop = true;
        result.stop_reason = string_field(&parsed, "stopReason");
    }
    result.system_message =
        string_field(&parsed, "systemMessage").map(|value| truncate(&value, MAX_CONTEXT_CHARS));
    if let Some(decision) = string_field(&parsed, "decision") {
        result.decision = match decision.as_str() {
            "approve" => HookDecision::Allow,
            "block" => HookDecision::Deny,
            _ => result.decision,
        };
    }
    result.reason = string_field(&parsed, "reason").or(result.reason);
    if let Some(specific) = parsed.get("hookSpecificOutput").and_then(Value::as_object)
        && string_field(specific, "hookEventName").as_deref() == Some(definition.point.wire_name())
    {
        if let Some(decision) = string_field(specific, "permissionDecision") {
            result.decision = match decision.as_str() {
                "allow" => HookDecision::Allow,
                "ask" => HookDecision::Ask,
                "deny" => HookDecision::Deny,
                _ => result.decision,
            };
        }
        result.reason = string_field(specific, "permissionDecisionReason").or(result.reason);
        result.additional_context = string_field(specific, "additionalContext")
            .map(|value| truncate(&value, MAX_CONTEXT_CHARS));
    }
    result
}

fn neutral_failure(
    definition: &CommandHookDefinition,
    error: &str,
    duration_ms: u64,
) -> HookResult {
    HookResult {
        handler_id: definition.handler_id.clone(),
        dialect: definition.dialect.wire_name().to_owned(),
        point: definition.point,
        decision: HookDecision::None,
        reason: None,
        stop: false,
        stop_reason: None,
        additional_context: None,
        system_message: None,
        exit_code: None,
        stderr_summary: Some(truncate(error, definition.stderr_summary_max_chars)),
        duration_ms,
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_owned()
    } else {
        value.chars().take(max_chars).collect::<String>() + "…"
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use linorun_core::{Component, FiberState, Runtime};
    use ternilo_kernel::{HooksClient, TokioSpawner};
    use tokio::{sync::oneshot, time::Duration};

    component_descriptor! {
        static TEST_REGISTRY_DESCRIPTOR: () {
            id: "test/hook-registry@1",
            requires: [],
            provides: [Hooks],
        }
    }

    component_descriptor! {
        static TEST_REGISTRAR_DESCRIPTOR: () {
            id: "test/hook-registrar@1",
            requires: [Hooks],
            provides: [],
        }
    }

    component_descriptor! {
        static TEST_CLIENT_DESCRIPTOR: () {
            id: "test/hook-client@1",
            requires: [Hooks],
            provides: [],
        }
    }

    struct TestRegistry;

    impl Component for TestRegistry {
        type Config = ();

        fn descriptor(&self) -> &'static ComponentDescriptor {
            &TEST_REGISTRY_DESCRIPTOR
        }

        fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
            let route = context.context().clone();
            let scope = context.scope().clone();
            let provider: Arc<dyn HooksProvider> = Arc::new(HookRegistry::default());
            Activation::Once(Box::pin(async move {
                scope
                    .provide::<Hooks>(&route, provider)
                    .await
                    .map_err(|error| {
                        linorun_core::ActivationFailure::user(format!(
                            "provide test hook registry: {error}"
                        ))
                    })?;
                Ok(None)
            }))
        }
    }

    struct OrderedHook {
        handler_id: String,
    }

    impl HookHandler for OrderedHook {
        fn matches(&self, _: &HookRequest) -> bool {
            true
        }

        fn execute<'a>(
            &'a self,
            request: HookRequest,
        ) -> Pin<Box<dyn Future<Output = HookResult> + Send + 'a>> {
            Box::pin(async move {
                HookResult {
                    handler_id: self.handler_id.clone(),
                    dialect: "test".to_owned(),
                    point: request.point,
                    decision: HookDecision::None,
                    reason: None,
                    stop: false,
                    stop_reason: None,
                    additional_context: None,
                    system_message: None,
                    exit_code: None,
                    stderr_summary: None,
                    duration_ms: 0,
                }
            })
        }
    }

    struct TestRegistrar {
        prefix: &'static str,
        delay: Duration,
    }

    impl Component for TestRegistrar {
        type Config = ();

        fn descriptor(&self) -> &'static ComponentDescriptor {
            &TEST_REGISTRAR_DESCRIPTOR
        }

        fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
            let hooks = context
                .context()
                .service::<Hooks>()
                .expect("test registrar declares Hooks");
            let prefix = self.prefix;
            let delay = self.delay;
            Activation::Once(Box::pin(async move {
                tokio::time::sleep(delay).await;
                let mut registrations = Vec::new();
                for suffix in ["a", "b"] {
                    let handler_id = format!("{prefix}-{suffix}");
                    let registration = hooks
                        .register_hook(HookRegistration {
                            handler_id: handler_id.clone(),
                            handler: Arc::new(OrderedHook { handler_id }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?;
                    registrations.push(registration);
                }
                Ok(Some(effect::inverse(move || async move {
                    for registration in registrations.into_iter().rev() {
                        hooks
                            .unregister_hook(registration)
                            .await
                            .map_err(|error| CleanupError::user(error.to_string()))?;
                    }
                    Ok(())
                })))
            }))
        }
    }

    struct TestClient {
        sender: Mutex<Option<oneshot::Sender<HooksClient>>>,
    }

    impl Component for TestClient {
        type Config = ();

        fn descriptor(&self) -> &'static ComponentDescriptor {
            &TEST_CLIENT_DESCRIPTOR
        }

        fn activate(&self, context: ComponentContext, _: Arc<Self::Config>) -> Activation {
            let hooks = context
                .context()
                .service::<Hooks>()
                .expect("test client declares Hooks");
            let sender = self.sender.lock().unwrap().take();
            Activation::Once(Box::pin(async move {
                sender
                    .expect("test client activates once")
                    .send(hooks)
                    .map_err(|_| {
                        linorun_core::ActivationFailure::user("test hook client dropped")
                    })?;
                Ok(None)
            }))
        }
    }

    #[test]
    fn matcher_dialects_follow_claude_literal_and_codex_regex_rules() {
        assert!(matcher_matches(
            Some("read_file|shell"),
            "shell",
            HookDialect::ClaudeCode
        ));
        assert!(!matcher_matches(
            Some("read_file|shell"),
            "shell-extra",
            HookDialect::ClaudeCode
        ));
        assert!(matcher_matches(
            Some("shell"),
            "shell-extra",
            HookDialect::Codex
        ));
    }

    #[test]
    fn codec_applies_event_scoped_permission_and_context() {
        let definition = CommandHookDefinition {
            handler_id: "test".to_owned(),
            dialect: HookDialect::ClaudeCode,
            point: HookPoint::PreToolUse,
            matcher: None,
            command: "true".to_owned(),
            timeout_ms: 100,
            project_dir: None,
            model: None,
            stderr_summary_max_chars: 500,
        };
        let output = parse_hook_output(
            &definition,
            Some(0),
            r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"review","additionalContext":"checked"}}"#,
            "",
            3,
        );
        assert_eq!(output.decision, HookDecision::Ask);
        assert_eq!(output.reason.as_deref(), Some("review"));
        assert_eq!(output.additional_context.as_deref(), Some("checked"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registry_runs_hooks_by_mount_order_then_caller_registration_order() {
        let runtime = Runtime::builder(TokioSpawner).build();
        let root = runtime.root();
        root.mount(TestRegistry, ()).await.unwrap();
        let first = root
            .mount(
                TestRegistrar {
                    prefix: "first",
                    delay: Duration::from_millis(40),
                },
                (),
            )
            .await
            .unwrap();
        let second = root
            .mount(
                TestRegistrar {
                    prefix: "second",
                    delay: Duration::ZERO,
                },
                (),
            )
            .await
            .unwrap();
        let (sender, receiver) = oneshot::channel();
        let client = root
            .mount(
                TestClient {
                    sender: Mutex::new(Some(sender)),
                },
                (),
            )
            .await
            .unwrap();

        assert!(runtime.wait_quiescent().await.quiescent);
        assert_eq!(first.state().await, FiberState::Active);
        assert_eq!(second.state().await, FiberState::Active);
        assert_eq!(client.state().await, FiberState::Active);
        let hooks = receiver.await.unwrap();
        let results = hooks
            .run(HookRequest {
                point: HookPoint::UserPromptSubmit,
                run_id: ternilo_protocol::RunId::new("mount-order"),
                prompt: Some("test".to_owned()),
                tool_call: None,
                tool_output: None,
                answer: None,
            })
            .await;
        assert_eq!(
            results
                .into_iter()
                .map(|result| result.handler_id)
                .collect::<Vec<_>>(),
            ["first-a", "first-b", "second-a", "second-b"]
        );
        runtime.shutdown().await;
    }
}
