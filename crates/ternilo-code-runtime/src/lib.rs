#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use rhai::{Dynamic, Engine, EvalAltResult, FuncRegistration, Module, Position, Scope};
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    ActivityBranch, CodeBindingHandler, CodeBindingSpec, CodeRunFailure, CodeRunFailureKind,
    CodeRunRequest, CodeRunResult, CodeRuntime, CodeRuntimeClient, CodeRuntimeInfo,
    CodeRuntimeProvider, HarnessPlugin, Hooks, HooksClient, PluginFactory, PluginManifest,
    RunCancellation, RunEnvironment, RunEnvironmentClient, Sessions, SessionsClient, ToolEffect,
    ToolExecutionContext, ToolHandler, ToolPresentation, ToolPresenter, ToolRegistration, Tools,
    ToolsClient,
};
use ternilo_protocol::{
    HarnessError, HookDecision, HookPoint, HookRequest, HookResult, RunId, SessionEventKind,
    ToolCall, ToolOutput, ToolSpec, UserQuestion, UserQuestionOption,
};
use ternilo_rhai::{RhaiSandboxLimits, restricted_engine};

pub const RUNTIME_KIND: &str = "ternilo.code_runtime.rhai";
pub const CODE_MODE_KIND: &str = "ternilo.tools.code_mode";
pub const RUN_CODE_TOOL: &str = "run_code";

component_descriptor! {
    static RUNTIME_DESCRIPTOR: () {
        id: "ternilo/rhai-code-runtime@1",
        requires: [],
        provides: [CodeRuntime],
    }
}

component_descriptor! {
    static CODE_MODE_DESCRIPTOR: () {
        id: "ternilo/code-mode-tool@1",
        requires: [CodeRuntime, Tools, Sessions, Hooks, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuntimeConfig {
    #[serde(flatten)]
    limits: RhaiSandboxLimits,
    #[serde(default = "default_max_wall_ms")]
    max_wall_ms: u64,
    #[serde(default = "default_max_output_bytes")]
    max_output_bytes: usize,
}

const fn default_max_wall_ms() -> u64 {
    60_000
}

const fn default_max_output_bytes() -> usize {
    4 * 1024 * 1024
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CodeModeConfig {
    #[serde(default)]
    mode: CodeMode,
    #[serde(default = "default_max_subcalls")]
    max_subcalls: u64,
}

#[derive(Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CodeMode {
    Native,
    Code,
    #[default]
    Both,
}

const fn default_max_subcalls() -> u64 {
    128
}

#[must_use]
pub fn runtime_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: RUNTIME_KIND,
            requires: &[],
            provides: &["ternilo/code-runtime@1"],
        },
        build_runtime,
    )
    .with_description("提供无文件、网络或进程能力的受限 Rhai 代码运行时。")
    .with_config_schema::<RuntimeConfig>()
}

#[must_use]
pub fn code_mode_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: CODE_MODE_KIND,
            requires: &[
                "ternilo/code-runtime@1",
                "ternilo/tools@1",
                "ternilo/sessions@1",
                "ternilo/hooks@1",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        build_code_mode,
    )
    .with_description("把当前工具动态投影为 Rhai SDK，并让子调用重入共享策略链。")
    .with_config_schema::<CodeModeConfig>()
}

fn build_runtime(config: Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    let config: RuntimeConfig = parse_config(config)?;
    if config.limits.validate().is_err() || config.max_wall_ms == 0 || config.max_output_bytes < 4 {
        return Err(HarnessError::composition(
            "Rhai code runtime limits must be positive and max_output_bytes must be at least 4",
        ));
    }
    Ok(Arc::new(RhaiRuntimePlugin { config }))
}

fn build_code_mode(config: Value) -> Result<Arc<dyn HarnessPlugin>, HarnessError> {
    let config: CodeModeConfig = parse_config(config)?;
    if config.max_subcalls == 0 {
        return Err(HarnessError::composition(
            "Code Mode max_subcalls must be positive",
        ));
    }
    Ok(Arc::new(CodeModePlugin { config }))
}

fn parse_config<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(if value.is_null() { json!({}) } else { value })
        .map_err(|error| HarnessError::composition(format!("invalid plugin config: {error}")))
}

struct RhaiRuntimePlugin {
    config: RuntimeConfig,
}

impl HarnessPlugin for RhaiRuntimePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &RUNTIME_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let provider: Arc<dyn CodeRuntimeProvider> = Arc::new(RhaiRuntime {
            config: self.config.clone(),
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<CodeRuntime>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide Rhai code runtime: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct RhaiRuntime {
    config: RuntimeConfig,
}

impl CodeRuntimeProvider for RhaiRuntime {
    fn info<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = CodeRuntimeInfo> + Send + 'a>> {
        Box::pin(async {
            CodeRuntimeInfo {
                language: "rhai".to_owned(),
                isolation: "embedded-capability-sandbox".to_owned(),
            }
        })
    }

    fn run<'a>(
        &'a self,
        _: CallContext<()>,
        request: CodeRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<CodeRunResult, HarnessError>> + Send + 'a>> {
        Box::pin(self.run_request(request))
    }
}

impl RhaiRuntime {
    async fn run_request(
        &self,
        mut request: CodeRunRequest,
    ) -> Result<CodeRunResult, HarnessError> {
        let config = self.config.clone();
        request.activity.ensure_running().await?;
        if request.cancellation.is_cancelled() {
            return Ok(failed_result(
                Vec::new(),
                CodeRunFailureKind::Cancelled,
                "code run was cancelled before execution",
            ));
        }
        let activity = request.activity.clone();
        let worker = activity.delegate();
        request.activity = worker.branch();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            let result = execute_rhai(&config, &request);
            drop(request);
            runtime.block_on(worker.finish())?;
            Ok(result)
        })
        .await;
        activity.ensure_running().await?;
        match result {
            Ok(result) => result,
            Err(error) => Ok(failed_result(
                Vec::new(),
                CodeRunFailureKind::Substrate,
                format!("Rhai runtime worker failed: {error}"),
            )),
        }
    }
}

struct LogState {
    logs: Vec<String>,
    encoded_bytes: usize,
    overflow: bool,
    max_bytes: usize,
}

impl LogState {
    fn new(max_bytes: usize) -> Self {
        Self {
            logs: Vec::new(),
            encoded_bytes: 2,
            overflow: false,
            max_bytes,
        }
    }

    fn push(&mut self, line: &str) {
        if self.overflow {
            return;
        }
        let encoded = serde_json::to_vec(line).map_or(usize::MAX, |value| value.len());
        let separator = usize::from(!self.logs.is_empty());
        let next = self
            .encoded_bytes
            .saturating_add(separator)
            .saturating_add(encoded);
        if next > self.max_bytes {
            self.overflow = true;
            return;
        }
        self.encoded_bytes = next;
        self.logs.push(line.to_owned());
    }
}

fn execute_rhai(config: &RuntimeConfig, request: &CodeRunRequest) -> CodeRunResult {
    let deadline = Instant::now() + Duration::from_millis(config.max_wall_ms);
    let logs = Arc::new(Mutex::new(LogState::new(config.max_output_bytes)));
    let engine = build_rhai_engine(config, request, &logs, deadline);
    let ast = match engine.compile(&request.program) {
        Ok(ast) => ast,
        Err(error) => {
            return failed_result(
                take_logs(&logs),
                CodeRunFailureKind::Parse,
                rhai_parse_diagnostic(&error),
            );
        }
    };
    let mut scope = Scope::new();
    let evaluated = engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast);
    finish_rhai_execution(config, request, &logs, deadline, evaluated)
}

fn build_rhai_engine(
    config: &RuntimeConfig,
    request: &CodeRunRequest,
    logs: &Arc<Mutex<LogState>>,
    deadline: Instant,
) -> Engine {
    let mut engine = restricted_engine(&config.limits)
        .expect("Rhai runtime limits were validated during plugin composition");

    let print_logs = Arc::clone(logs);
    engine.on_print(move |line| {
        print_logs
            .lock()
            .expect("code log lock poisoned")
            .push(line);
    });
    let debug_logs = Arc::clone(logs);
    engine.on_debug(move |line, _, _| {
        debug_logs
            .lock()
            .expect("code log lock poisoned")
            .push(line);
    });

    let progress_logs = Arc::clone(logs);
    let progress_cancellation = request.cancellation.clone();
    engine.on_progress(move |_| {
        if progress_cancellation.is_cancelled() {
            Some(Dynamic::from("cancelled"))
        } else if progress_logs
            .lock()
            .expect("code log lock poisoned")
            .overflow
        {
            Some(Dynamic::from("output-limit"))
        } else if Instant::now() >= deadline {
            Some(Dynamic::from("wall-time"))
        } else {
            None
        }
    });

    let runtime = tokio::runtime::Handle::current();
    let binding = Arc::clone(&request.binding);
    let binding_cancellation = request.cancellation.clone();
    let binding_activity = request.activity.clone();
    let binding_runtime = runtime.clone();
    engine.register_fn(
        "call_tool",
        move |name: rhai::ImmutableString,
              arguments: Dynamic|
              -> Result<Dynamic, Box<EvalAltResult>> {
            invoke_binding(
                &binding_runtime,
                &binding,
                &binding_cancellation,
                &binding_activity,
                name.as_str(),
                &arguments,
            )
        },
    );
    let mut tools_module = Module::new();
    for spec in request
        .bindings
        .iter()
        .filter(|spec| valid_rhai_identifier(&spec.name))
    {
        let name = spec.name.clone();
        let function_name = name.clone();
        let binding = Arc::clone(&request.binding);
        let cancellation = request.cancellation.clone();
        let activity = request.activity.clone();
        let runtime = runtime.clone();
        FuncRegistration::new(function_name)
            .with_purity(false)
            .with_volatility(true)
            .set_into_module(
                &mut tools_module,
                move |arguments: Dynamic| -> Result<Dynamic, Box<EvalAltResult>> {
                    invoke_binding(
                        &runtime,
                        &binding,
                        &cancellation,
                        &activity,
                        &name,
                        &arguments,
                    )
                },
            );
    }
    engine.register_static_module("tools", tools_module.into());
    engine
}

fn finish_rhai_execution(
    config: &RuntimeConfig,
    request: &CodeRunRequest,
    logs: &Arc<Mutex<LogState>>,
    deadline: Instant,
    evaluated: Result<Dynamic, Box<EvalAltResult>>,
) -> CodeRunResult {
    let (captured, overflow) = {
        let state = logs.lock().expect("code log lock poisoned");
        (state.logs.clone(), state.overflow)
    };
    if request.cancellation.is_cancelled() {
        return failed_result(
            captured,
            CodeRunFailureKind::Cancelled,
            "code run was cancelled",
        );
    }
    if overflow {
        return failed_result(
            captured,
            CodeRunFailureKind::OutputLimit,
            format!("code output exceeded {} bytes", config.max_output_bytes),
        );
    }
    if Instant::now() >= deadline {
        return failed_result(
            captured,
            CodeRunFailureKind::WallTime,
            format!("code run exceeded {} ms", config.max_wall_ms),
        );
    }
    let value = match evaluated {
        Ok(value) if value.is_unit() => None,
        Ok(value) => match rhai::serde::from_dynamic::<Value>(&value) {
            Ok(value) => Some(value),
            Err(error) => {
                return failed_result(
                    captured,
                    CodeRunFailureKind::InvalidOutput,
                    format!("code result must be lossless JSON: {error}"),
                );
            }
        },
        Err(error) => {
            let kind = if matches!(*error, EvalAltResult::ErrorTooManyOperations(_)) {
                CodeRunFailureKind::OperationLimit
            } else {
                CodeRunFailureKind::Exception
            };
            return failed_result(captured, kind, error.to_string());
        }
    };
    let encoded = serde_json::to_vec(&json!({ "logs": captured, "result": value }))
        .map_or(usize::MAX, |encoded| encoded.len());
    if encoded > config.max_output_bytes {
        return failed_result(
            take_logs(logs),
            CodeRunFailureKind::OutputLimit,
            format!("code output exceeded {} bytes", config.max_output_bytes),
        );
    }
    CodeRunResult {
        value,
        logs: take_logs(logs),
        failure: None,
    }
}

fn invoke_binding(
    runtime: &tokio::runtime::Handle,
    binding: &Arc<dyn CodeBindingHandler>,
    cancellation: &RunCancellation,
    activity: &ActivityBranch,
    name: &str,
    arguments: &Dynamic,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let arguments: Value = rhai::serde::from_dynamic(arguments).map_err(|error| {
        Box::new(EvalAltResult::ErrorRuntime(
            format!("tool arguments must be lossless JSON: {error}").into(),
            Position::NONE,
        ))
    })?;
    let result = runtime.block_on(async {
        activity.ensure_running().await?;
        let dispatch = activity.delegate();
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(HarnessError::cancelled("code run was cancelled")),
            result = binding.call(name.to_owned(), arguments, dispatch.branch()) => result,
        };
        // A timeout or cancelled binding may have dropped a dependency wait.
        // Restore admission before an ordinary error can enter a script catch.
        dispatch.finish().await?;
        activity.ensure_running().await?;
        Ok::<_, HarnessError>(result)
    }).map_err(|error| {
        Box::new(EvalAltResult::ErrorTerminated(error.to_string().into(), Position::NONE))
    })?;
    let value = result.map_err(|error| {
        Box::new(EvalAltResult::ErrorRuntime(
            error.to_string().into(),
            Position::NONE,
        ))
    })?;
    rhai::serde::to_dynamic(value).map_err(|error| {
        Box::new(EvalAltResult::ErrorRuntime(
            format!("tool result is not representable in Rhai: {error}").into(),
            Position::NONE,
        ))
    })
}

fn valid_rhai_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
        && Engine::new_raw()
            .compile(format!("let {name} = (); "))
            .is_ok()
}

fn rhai_parse_diagnostic(error: &rhai::ParseError) -> String {
    let mut message =
        format!("{error}. The program was not executed; no tool calls or print statements ran.");
    if matches!(error.err_type(), rhai::ParseErrorType::Reserved(_)) {
        message.push_str(" Use a different variable name. Quote reserved map keys, for example #{\"new\": \"replacement\"}, or assign args[\"new\"] = \"replacement\".");
    }
    message
}

fn take_logs(logs: &Mutex<LogState>) -> Vec<String> {
    logs.lock().expect("code log lock poisoned").logs.clone()
}

fn failed_result(
    logs: Vec<String>,
    kind: CodeRunFailureKind,
    message: impl Into<String>,
) -> CodeRunResult {
    CodeRunResult {
        value: None,
        logs,
        failure: Some(CodeRunFailure {
            kind,
            message: message.into(),
        }),
    }
}

struct CodeModePlugin {
    config: CodeModeConfig,
}

impl HarnessPlugin for CodeModePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &CODE_MODE_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let runtime = context
            .context()
            .service::<CodeRuntime>()
            .expect("Code Mode declares CodeRuntime");
        let tools = context
            .context()
            .service::<Tools>()
            .expect("Code Mode declares Tools");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("Code Mode declares Sessions");
        let hooks = context
            .context()
            .service::<Hooks>()
            .expect("Code Mode declares Hooks");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("Code Mode declares RunEnvironment");
        let max_subcalls = self.config.max_subcalls;
        let mode = self.config.mode;
        Activation::Once(Box::pin(async move {
            let tool = tools
                .register_tool(ToolRegistration {
                    spec: run_code_spec(),
                    effect: ToolEffect::ReadOnly,
                    handler: Arc::new(RunCodeTool {
                        runtime: runtime.clone(),
                        tools: tools.clone(),
                        sessions,
                        hooks,
                        environment,
                        max_subcalls,
                    }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let presenter = match tools
                .register_presenter(Arc::new(CodeModePresenter { runtime, mode }))
                .await
            {
                Ok(presenter) => presenter,
                Err(error) => {
                    let _ = tools.unregister_tool(tool).await;
                    return Err(linorun_core::ActivationFailure::user(error.to_string()));
                }
            };
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_presenter(presenter)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))?;
                tools
                    .unregister_tool(tool)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct CodeModePresenter {
    runtime: CodeRuntimeClient,
    mode: CodeMode,
}

impl ToolPresenter for CodeModePresenter {
    fn present<'a>(
        &'a self,
        tools: Vec<ToolSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolPresentation, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let run_code = tools
                .iter()
                .find(|tool| tool.name == RUN_CODE_TOOL)
                .cloned()
                .ok_or_else(|| {
                    HarnessError::composition("Code Mode transport is not registered")
                })?;
            let native = tools
                .into_iter()
                .filter(|tool| tool.name != RUN_CODE_TOOL)
                .collect::<Vec<_>>();
            if matches!(self.mode, CodeMode::Native) {
                return Ok(ToolPresentation::native(native));
            }
            let info = self.runtime.info().await;
            if info.language != "rhai" {
                return Err(HarnessError::composition(format!(
                    "Code Mode has no SDK renderer for runtime language {:?}",
                    info.language
                )));
            }
            let code_only = matches!(self.mode, CodeMode::Code);
            let presented = if code_only {
                vec![run_code]
            } else {
                let mut presented = native.clone();
                presented.push(run_code);
                presented.sort_by(|left, right| left.name.cmp(&right.name));
                presented
            };
            Ok(ToolPresentation {
                tools: presented,
                system_prompt: Some(render_code_mode_prompt(&native, code_only, &info.isolation)),
                code_only,
            })
        })
    }
}

fn render_code_mode_prompt(tools: &[ToolSpec], code_only: bool, isolation: &str) -> String {
    let mut prompt = String::from("# Code Mode (Rhai)\n\n");
    if code_only {
        prompt.push_str("`run_code` is the only tool callable directly. Call every other tool from inside the Rhai program through the SDK below.\n\n");
    }
    let _ = writeln!(
        prompt,
        "`run_code` starts a fresh `{isolation}` Rhai runtime. It has no filesystem, network, process, environment-variable, credential, module-import, or persistent-state API. Tool calls re-enter the native Ternilo permission, workspace, Hook, timeout, and policy pipeline. Arguments and results must be lossless JSON. Use `print(value)` for ordered logs and leave a JSON value as the final expression. Do not call `run_code` recursively."
    );
    prompt.push_str("\nRhai uses `#{\"key\": value}` maps. Quote keys such as `\"new\"`; do not use reserved words as variable names. Ordinary function arguments are copied: return the changed map or array and assign it at the caller, for example `fn add_result(items, value) { items.push(value); items } let results = []; results = add_result(results, 42);`. Use `try { ... } catch (error) { print(error); }` when independent calls should continue after an error. Uncaught runtime errors preserve earlier logs and completed tool effects; parse errors execute nothing. SDK entries below describe argument shapes, not executable programs: replace type placeholders with values and omit unused optional fields.\n");
    prompt.push_str("\n## Rhai tool SDK\n\n");
    if tools.is_empty() {
        prompt.push_str("No nested tool bindings are available.\n");
        return prompt;
    }
    for tool in tools {
        let arguments = render_rhai_schema(&tool.input_schema, 0);
        let description = tool
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if valid_rhai_identifier(&tool.name) {
            let _ = writeln!(
                prompt,
                "- `tools::{}({arguments}) -> JSON` — {description}",
                tool.name
            );
        } else {
            let encoded =
                serde_json::to_string(&tool.name).unwrap_or_else(|_| "\"tool\"".to_owned());
            let _ = writeln!(
                prompt,
                "- `call_tool({encoded}, {arguments}) -> JSON` — {description}"
            );
        }
    }
    prompt.push_str("\n`call_tool(\"name\", arguments)` is also available as the generic form for every listed binding.\n");
    prompt
}

fn render_rhai_schema(schema: &Value, depth: usize) -> String {
    if depth >= 6 {
        return "JSON".to_owned();
    }
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let rendered = variants
            .iter()
            .map(|variant| render_rhai_schema(variant, depth + 1))
            .collect::<Vec<_>>();
        return rendered.join(" | ");
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "JSON".to_owned()))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>();
            let fields = schema
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|properties| properties.iter())
                .map(|(name, value)| {
                    let marker = if required.contains(name.as_str()) {
                        ""
                    } else {
                        "?"
                    };
                    let key = if valid_rhai_identifier(name) {
                        name.clone()
                    } else {
                        serde_json::to_string(name).expect("schema property name is JSON text")
                    };
                    format!("{key}{marker}: {}", render_rhai_schema(value, depth + 1))
                })
                .collect::<Vec<_>>();
            format!("#{{ {} }}", fields.join(", "))
        }
        Some("array") => format!(
            "[{}]",
            schema.get("items").map_or_else(
                || "JSON".to_owned(),
                |items| render_rhai_schema(items, depth + 1)
            )
        ),
        Some("string") => "string".to_owned(),
        Some("integer") => "int".to_owned(),
        Some("number") => "number".to_owned(),
        Some("boolean") => "bool".to_owned(),
        Some("null") => "null".to_owned(),
        _ => "JSON".to_owned(),
    }
}

fn run_code_spec() -> ToolSpec {
    ToolSpec {
        name: RUN_CODE_TOOL.to_owned(),
        description: "Run one fresh capability-limited Rhai program that can call Ternilo tools through call_tool(name, JSON arguments). Use it for bounded multi-step composition and local JSON transformation; it cannot access the host except through those tool calls.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "minLength": 1 },
                "description": { "type": "string", "minLength": 1, "maxLength": 500 }
            },
            "required": ["code", "description"],
            "additionalProperties": false
        }),
    }
}

struct RunCodeTool {
    runtime: CodeRuntimeClient,
    tools: ToolsClient,
    sessions: SessionsClient,
    hooks: HooksClient,
    environment: RunEnvironmentClient,
    max_subcalls: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCodeArguments {
    code: String,
    description: String,
}

impl ToolHandler for RunCodeTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: RunCodeArguments =
                serde_json::from_value(arguments).map_err(|error| {
                    HarnessError::invalid(format!("invalid run_code arguments: {error}"))
                })?;
            if arguments.code.trim().is_empty() || arguments.description.trim().is_empty() {
                return Err(HarnessError::invalid(
                    "run_code code and description must not be empty",
                ));
            }
            let bindings = self
                .tools
                .list()
                .await
                .into_iter()
                .filter(|tool| tool.name != RUN_CODE_TOOL)
                .map(|tool| CodeBindingSpec {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                })
                .collect::<Vec<_>>();
            let allowed = bindings
                .iter()
                .map(|binding| binding.name.clone())
                .collect::<BTreeSet<_>>();
            let cancellation = RunCancellation::new();
            let mut guard = CancelOnDrop {
                cancellation: cancellation.clone(),
                armed: true,
            };
            let binding: Arc<dyn CodeBindingHandler> = Arc::new(SessionToolBinding {
                tools: self.tools.clone(),
                sessions: self.sessions.clone(),
                hooks: self.hooks.clone(),
                environment: self.environment.clone(),
                run_id: context.run_id,
                parent_call_id: context.call_id,
                cancellation: cancellation.clone(),
                next: AtomicU64::new(1),
                max_subcalls: self.max_subcalls,
                allowed,
            });
            let result = self
                .runtime
                .run(CodeRunRequest {
                    program: arguments.code,
                    bindings,
                    binding,
                    cancellation,
                    activity: context.activity,
                })
                .await?;
            guard.armed = false;
            let is_error = result.failure.is_some();
            let rendered_output = serde_json::to_string_pretty(&json!({
                "runtime": "rhai",
                "description": arguments.description,
                "logs": result.logs,
                "result": result.value,
                "error": result.failure.map(|failure| json!({
                    "kind": failure_kind_name(failure.kind),
                    "message": failure.message,
                })),
            }))
            .map_err(|error| HarnessError::execution(format!("encode run_code output: {error}")))?;
            Ok(ToolOutput {
                content: rendered_output,
                is_error,
            })
        })
    }
}

struct CancelOnDrop {
    cancellation: RunCancellation,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

struct SessionToolBinding {
    tools: ToolsClient,
    sessions: SessionsClient,
    hooks: HooksClient,
    environment: RunEnvironmentClient,
    run_id: RunId,
    parent_call_id: String,
    cancellation: RunCancellation,
    next: AtomicU64,
    max_subcalls: u64,
    allowed: BTreeSet<String>,
}

impl CodeBindingHandler for SessionToolBinding {
    fn call<'a>(
        &'a self,
        name: String,
        arguments: Value,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            activity.ensure_running().await?;
            if !self.allowed.contains(&name) {
                return Err(HarnessError::policy(format!(
                    "tool {name:?} is not available to this code run"
                )));
            }
            let ordinal = self.next.fetch_add(1, Ordering::AcqRel);
            if ordinal > self.max_subcalls {
                return Err(HarnessError::policy(format!(
                    "code run exceeded max_subcalls ({})",
                    self.max_subcalls
                )));
            }
            self.cancellation.check()?;
            let mut call = ToolCall {
                id: format!("{}:code:{ordinal}", self.parent_call_id),
                name: name.clone(),
                arguments,
                presentation: None,
            };
            call.presentation = self.tools.describe(call.name.clone()).await;
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::CodeDispatchStarted {
                        parent_call_id: self.parent_call_id.clone(),
                        call: call.clone(),
                    },
                )
                .await?;
            let mut output = match self
                .run_hooks(HookRequest {
                    point: HookPoint::PreToolUse,
                    run_id: self.run_id.clone(),
                    prompt: None,
                    tool_call: Some(call.clone()),
                    tool_output: None,
                    answer: None,
                })
                .await
            {
                Ok(()) => self.execute_tool(&call, activity.clone()).await,
                Err(error) => ToolOutput {
                    content: error.to_string(),
                    is_error: true,
                },
            };
            activity.ensure_running().await?;
            if !output.is_error
                && let Err(error) = self
                    .run_hooks(HookRequest {
                        point: HookPoint::PostToolUse,
                        run_id: self.run_id.clone(),
                        prompt: None,
                        tool_call: Some(call.clone()),
                        tool_output: Some(output.clone()),
                        answer: None,
                    })
                    .await
            {
                output = ToolOutput {
                    content: error.to_string(),
                    is_error: true,
                };
            }
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::CodeDispatchFinished {
                        parent_call_id: self.parent_call_id.clone(),
                        call_id: call.id,
                        name,
                        output: output.clone(),
                        retained_output: None,
                    },
                )
                .await?;
            if output.is_error {
                return Err(HarnessError::execution(output.content));
            }
            Ok(serde_json::from_str(&output.content)
                .unwrap_or_else(|_| Value::String(output.content)))
        })
    }
}

impl SessionToolBinding {
    async fn execute_tool(&self, call: &ToolCall, activity: ActivityBranch) -> ToolOutput {
        let execution = self.tools.execute(
            self.run_id.clone(),
            call.clone(),
            self.cancellation.clone(),
            activity,
        );
        tokio::pin!(execution);
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => ToolOutput {
                content: "code run was cancelled".to_owned(),
                is_error: true,
            },
            result = &mut execution => match result {
                Ok(output) => output,
                Err(error) => ToolOutput { content: error.to_string(), is_error: true },
            },
        }
    }

    async fn run_hooks(&self, request: HookRequest) -> Result<(), HarnessError> {
        let point = request.point;
        let results = self.hooks.run(request).await;
        for result in &results {
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::HookResult {
                        result: result.clone(),
                    },
                )
                .await?;
            if let Some(content) = result
                .additional_context
                .as_deref()
                .filter(|content| !content.trim().is_empty())
            {
                self.sessions
                    .append(
                        self.run_id.clone(),
                        SessionEventKind::HookContextAdded {
                            handler_id: result.handler_id.clone(),
                            dialect: result.dialect.clone(),
                            content: content.to_owned(),
                            reference: None,
                            completeness: None,
                        },
                    )
                    .await?;
            }
        }
        let denied = results
            .iter()
            .filter(|result| result.stop || result.decision == HookDecision::Deny)
            .collect::<Vec<_>>();
        if !denied.is_empty() {
            return Err(HarnessError::policy(
                joined_hook_reasons(&denied).unwrap_or_else(|| {
                    format!("{} hook blocked the code sub-call", point.wire_name())
                }),
            ));
        }
        let asks = results
            .iter()
            .filter(|result| result.decision == HookDecision::Ask)
            .collect::<Vec<_>>();
        if asks.is_empty() {
            return Ok(());
        }
        let reason = joined_hook_reasons(&asks)
            .unwrap_or_else(|| format!("{} hook requests confirmation", point.wire_name()));
        let answer = self
            .environment
            .ask_user(UserQuestion {
                id: format!(
                    "code-hook-{}-{}-{}",
                    point.wire_name(),
                    self.run_id,
                    self.parent_call_id
                ),
                question: reason,
                detail: None,
                header: None,
                options: vec![
                    UserQuestionOption {
                        label: "Allow once".to_owned(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "Deny".to_owned(),
                        description: None,
                    },
                ],
                multi_select: false,
                presentation: None,
                tool_approval: None,
            })
            .await?;
        if answer.chose("Allow once") {
            Ok(())
        } else {
            Err(HarnessError::policy(format!(
                "user denied the {} hook request",
                point.wire_name()
            )))
        }
    }
}

fn joined_hook_reasons(results: &[&HookResult]) -> Option<String> {
    let reasons = results
        .iter()
        .filter_map(|result| result.reason.as_deref().or(result.stop_reason.as_deref()))
        .filter(|reason| !reason.trim().is_empty())
        .collect::<Vec<_>>();
    (!reasons.is_empty()).then(|| reasons.join("\n\n"))
}

const fn failure_kind_name(kind: CodeRunFailureKind) -> &'static str {
    match kind {
        CodeRunFailureKind::Parse => "parse",
        CodeRunFailureKind::Exception => "exception",
        CodeRunFailureKind::InvalidOutput => "invalid_output",
        CodeRunFailureKind::OutputLimit => "output_limit",
        CodeRunFailureKind::OperationLimit => "operation_limit",
        CodeRunFailureKind::WallTime => "wall_time",
        CodeRunFailureKind::Cancelled => "cancelled",
        CodeRunFailureKind::Substrate => "substrate",
    }
}

#[cfg(test)]
mod activity_tests;

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureBinding;

    impl CodeBindingHandler for FixtureBinding {
        fn call<'a>(
            &'a self,
            name: String,
            arguments: Value,
            _activity: ActivityBranch,
        ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                if name != "double" {
                    return Err(HarnessError::invalid("unknown fixture binding"));
                }
                Ok(json!(arguments["value"].as_i64().unwrap() * 2))
            })
        }
    }

    pub(super) fn runtime(max_operations: u64) -> RhaiRuntime {
        RhaiRuntime {
            config: RuntimeConfig {
                limits: RhaiSandboxLimits {
                    max_operations,
                    max_string_bytes: 8_192,
                    max_collection_items: 1_024,
                    max_call_levels: 32,
                    max_expr_depth: 32,
                    max_variables: 256,
                    max_functions: 32,
                },
                max_wall_ms: 2_000,
                max_output_bytes: 16_384,
            },
        }
    }

    fn fixture_bindings() -> Vec<CodeBindingSpec> {
        vec![CodeBindingSpec {
            name: "double".to_owned(),
            description: "Double one integer.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "integer" } },
                "required": ["value"]
            }),
        }]
    }

    async fn run(runtime: &RhaiRuntime, program: &str) -> CodeRunResult {
        let config = runtime.config.clone();
        let program = program.to_owned();
        tokio::task::spawn_blocking(move || {
            let request = CodeRunRequest {
                program,
                bindings: fixture_bindings(),
                binding: Arc::new(FixtureBinding),
                cancellation: RunCancellation::new(),
                activity: ActivityBranch::default(),
            };
            execute_rhai(&config, &request)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn fresh_rhai_runtime_calls_only_explicit_bindings() {
        let result = run(
            &runtime(10_000),
            r"let answer = tools::double(#{ value: 21 }); print(answer); #{ answer: answer }",
        )
        .await;
        assert_eq!(result.logs, vec!["42"]);
        assert_eq!(result.value, Some(json!({ "answer": 42 })));
        assert_eq!(result.failure, None);
    }

    #[tokio::test]
    async fn runtime_failures_preserve_logs_but_parse_failures_execute_nothing() {
        let result = run(
            &runtime(10_000),
            r#"print("before failure"); call_tool("missing", #{});"#,
        )
        .await;
        assert_eq!(result.logs, vec!["before failure"]);
        assert_eq!(result.failure.unwrap().kind, CodeRunFailureKind::Exception);

        let result = run(&runtime(10_000), r#"print("not executed"); let new = 1;"#).await;
        assert!(result.logs.is_empty());
        let failure = result.failure.unwrap();
        assert_eq!(failure.kind, CodeRunFailureKind::Parse);
        assert!(failure.message.contains("program was not executed"));
        assert!(failure.message.contains("args[\"new\"]"));
    }

    #[tokio::test]
    async fn documented_map_keys_and_returned_collection_updates_execute() {
        let result = run(
            &runtime(10_000),
            r#"
            fn add_result(items, value) { items.push(value); items }
            let original = [];
            let results = add_result(original, #{"new": "replacement"});
            #{ original: original, results: results }
        "#,
        )
        .await;
        assert_eq!(result.failure, None);
        assert_eq!(
            result.value,
            Some(json!({"original": [], "results": [{"new": "replacement"}]}))
        );
        assert!(!valid_rhai_identifier("new"));
        assert!(!valid_rhai_identifier("while"));
        assert!(valid_rhai_identifier("replace_in_file"));
    }

    #[tokio::test]
    async fn operation_budget_stops_hot_programs() {
        let result = run(&runtime(500), "loop { }").await;
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.kind),
            Some(CodeRunFailureKind::OperationLimit)
        );
    }

    #[tokio::test]
    async fn cancellation_stops_a_running_program() {
        let mut runtime = runtime(1_000_000_000_000);
        runtime.config.max_wall_ms = 30_000;
        let cancellation = RunCancellation::new();
        let worker_cancellation = cancellation.clone();
        let config = runtime.config;
        let worker = tokio::task::spawn_blocking(move || {
            let request = CodeRunRequest {
                program: "loop { }".to_owned(),
                bindings: fixture_bindings(),
                binding: Arc::new(FixtureBinding),
                cancellation: worker_cancellation,
                activity: ActivityBranch::default(),
            };
            execute_rhai(&config, &request)
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .expect("cancelled Rhai worker did not quiesce")
            .unwrap();
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.kind),
            Some(CodeRunFailureKind::Cancelled)
        );
    }

    #[test]
    fn runtime_config_remains_flat_and_keeps_legacy_defaults() {
        let config: RuntimeConfig = parse_config(json!({})).unwrap();
        assert_eq!(config.limits, RhaiSandboxLimits::default());
        assert_eq!(config.max_wall_ms, 60_000);
        assert_eq!(config.max_output_bytes, 4 * 1024 * 1024);

        let config: RuntimeConfig = parse_config(json!({ "max_operations": 7 })).unwrap();
        assert_eq!(config.limits.max_operations, 7);
        assert_eq!(
            config.limits.max_string_bytes,
            RhaiSandboxLimits::default().max_string_bytes
        );

        let schema = serde_json::to_value(schemars::schema_for!(RuntimeConfig)).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("max_operations"), "{schema}");
        assert!(properties.contains_key("max_output_bytes"), "{schema}");
        assert!(!properties.contains_key("limits"), "{schema}");

        assert!(parse_config::<RuntimeConfig>(json!({ "unknown": true })).is_err());

        let error = build_runtime(json!({ "max_operations": 0 })).err().unwrap();
        assert_eq!(
            error.message,
            "Rhai code runtime limits must be positive and max_output_bytes must be at least 4"
        );
    }

    #[tokio::test]
    async fn imports_and_host_apis_are_absent() {
        let result = run(&runtime(10_000), r#"import "os" as os; os"#).await;
        assert!(result.failure.is_some());
        assert!(result.value.is_none());
    }

    #[test]
    fn generated_rhai_sdk_names_tools_and_projects_argument_shapes() {
        let prompt = render_code_mode_prompt(
            &[ToolSpec {
                name: "search_files".to_owned(),
                description: "Search files in the workspace.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "limit": { "type": "integer" }
                    },
                    "required": ["pattern"]
                }),
            }],
            true,
            "fixture",
        );
        assert!(prompt.contains("`run_code` is the only tool callable directly"));
        assert!(prompt.contains("tools::search_files(#{"), "{prompt}");
        assert!(prompt.contains("limit?: int"), "{prompt}");
        assert!(prompt.contains("pattern: string"), "{prompt}");
        let shape = render_rhai_schema(
            &json!({
                "type": "object", "properties": { "new": { "type": "string" } }, "required": ["new"]
            }),
            0,
        );
        assert_eq!(shape, r#"#{ "new": string }"#);
    }
}
