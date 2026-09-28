use std::{
    collections::BTreeSet,
    io::Read as _,
    path::{Component as PathComponent, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rhai::{AST, Dynamic, Engine, EvalAltResult, Scope};
use serde::Serialize;
use serde_json::Value;
use ternilo_kernel::{RunCancellation, ToolExecutionContext};
use ternilo_protocol::{HarnessError, HookRequest, SessionIdentity, ToolOutput, WorkspaceBinding};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{
    Config, Engine as WasmtimeEngine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};

mod wasm_cancellation;
use wasm_cancellation::WasmCancellation;

use crate::{
    Capability, ExtensionManifest, ExtensionRuntime, InstalledExtension, RhaiExecutionLimits,
    Runtime, WasmComponentLimits, ternilo::extension::host,
};

#[derive(Clone)]
pub(crate) enum CompiledExtension {
    Rhai(CompiledRhaiPackage),
    WasmComponent(CompiledWasmPackage),
}

#[derive(Clone)]
pub(crate) struct CompiledRhaiPackage {
    pub ast: AST,
}

pub(crate) fn compile_extension(
    manifest: &ExtensionManifest,
    bytes: &[u8],
) -> Result<CompiledExtension, HarnessError> {
    match &manifest.runtime {
        ExtensionRuntime::Rhai { limits } => {
            let source = std::str::from_utf8(bytes)
                .map_err(|_| HarnessError::invalid("Rhai extension payload is not valid UTF-8"))?;
            compile_rhai(
                source,
                limits,
                manifest
                    .contributions
                    .tools
                    .iter()
                    .map(|tool| (format!("tool {:?}", tool.spec.name), tool.handler.clone()))
                    .chain(
                        manifest
                            .contributions
                            .hooks
                            .iter()
                            .map(|hook| (format!("hook {:?}", hook.id), hook.handler.clone())),
                    ),
            )
            .map(CompiledExtension::Rhai)
        }
        ExtensionRuntime::WasmComponent { world, limits } => {
            compile_wasm_component(bytes, &manifest.package_id, world, limits)
                .map(CompiledExtension::WasmComponent)
        }
    }
}

pub(crate) fn invoke_extension(
    installed: &InstalledExtension,
    compiled: &CompiledExtension,
    handler: &str,
    output_schema: &Value,
    context: &ToolExecutionContext,
    arguments: Value,
    settings: Value,
) -> Result<ToolOutput, HarnessError> {
    context.cancellation.check()?;
    let invocation = ExtensionInvocationContext {
        identity: context.identity.clone(),
        workspace: context.workspace.clone(),
        run_id: context.run_id.clone(),
        cancellation: Some(context.cancellation.clone()),
    };
    let result = invoke_json(
        installed,
        compiled,
        handler,
        &invocation,
        arguments,
        settings,
    )?;
    let runtime = runtime_name(compiled);
    validate_handler_output(output_schema, &result, runtime, handler)?;
    render_output(result, output_limit(installed), runtime)
}

pub(crate) fn invoke_extension_hook(
    installed: &InstalledExtension,
    compiled: &CompiledExtension,
    handler: &str,
    identity: SessionIdentity,
    workspace: Option<WorkspaceBinding>,
    request: HookRequest,
    settings: Value,
) -> Result<Value, HarnessError> {
    let invocation = ExtensionInvocationContext {
        identity,
        workspace,
        run_id: request.run_id.clone(),
        cancellation: None,
    };
    invoke_json(
        installed,
        compiled,
        handler,
        &invocation,
        serde_json::to_value(request)
            .map_err(|error| HarnessError::execution(format!("encode Hook request: {error}")))?,
        settings,
    )
}

struct ExtensionInvocationContext {
    identity: SessionIdentity,
    workspace: Option<WorkspaceBinding>,
    run_id: ternilo_protocol::RunId,
    cancellation: Option<RunCancellation>,
}

fn invoke_json(
    installed: &InstalledExtension,
    compiled: &CompiledExtension,
    handler: &str,
    context: &ExtensionInvocationContext,
    input: Value,
    settings: Value,
) -> Result<Value, HarnessError> {
    match compiled {
        CompiledExtension::Rhai(compiled) => {
            invoke_rhai(installed, compiled, handler, context, input, settings)
        }
        CompiledExtension::WasmComponent(compiled) => {
            invoke_wasm_component(installed, compiled, handler, context, &input, &settings)
        }
    }
}

fn runtime_name(compiled: &CompiledExtension) -> &'static str {
    match compiled {
        CompiledExtension::Rhai(_) => "Rhai",
        CompiledExtension::WasmComponent(_) => "WASM Component",
    }
}

fn output_limit(installed: &InstalledExtension) -> usize {
    match &installed.manifest.runtime {
        ExtensionRuntime::Rhai { limits } => limits.max_output_bytes,
        ExtensionRuntime::WasmComponent { limits, .. } => {
            usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX)
        }
    }
}

pub(crate) fn compile_rhai(
    source: &str,
    limits: &RhaiExecutionLimits,
    handlers: impl Iterator<Item = (String, String)>,
) -> Result<CompiledRhaiPackage, HarnessError> {
    let engine = ternilo_rhai::restricted_engine(&limits.sandbox)?;
    let ast = engine
        .compile(source)
        .map_err(|error| HarnessError::invalid(format!("compile Rhai extension: {error}")))?;
    let functions = ast
        .iter_functions()
        .map(|function| (function.name.to_owned(), function.params.len()))
        .collect::<Vec<_>>();
    for (contribution, handler) in handlers {
        if !functions
            .iter()
            .any(|(name, parameters)| name == &handler && *parameters == 3)
        {
            return Err(HarnessError::invalid(format!(
                "Rhai handler {handler:?} for {contribution} must be declared with exactly three parameters"
            )));
        }
    }
    Ok(CompiledRhaiPackage { ast })
}

#[derive(Serialize)]
struct SafeInvocationContext<'a> {
    tenant_id: &'a ternilo_protocol::TenantId,
    user_id: &'a ternilo_protocol::UserId,
    agent_id: &'a ternilo_protocol::AgentId,
    session_id: &'a ternilo_protocol::SessionId,
    run_id: &'a ternilo_protocol::RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<SafeWorkspaceContext<'a>>,
}

#[derive(Serialize)]
struct SafeWorkspaceContext<'a> {
    workspace_id: &'a ternilo_protocol::WorkspaceId,
}

fn rhai_context(context: &ExtensionInvocationContext) -> Result<Value, HarnessError> {
    let safe_context = SafeInvocationContext {
        tenant_id: &context.identity.tenant_id,
        user_id: &context.identity.user_id,
        agent_id: &context.identity.agent_id,
        session_id: &context.identity.session_id,
        run_id: &context.run_id,
        workspace: context
            .workspace
            .as_ref()
            .map(|workspace| SafeWorkspaceContext {
                workspace_id: &workspace.workspace_id,
            }),
    };
    serde_json::to_value(safe_context)
        .map_err(|error| HarnessError::execution(format!("encode Rhai context: {error}")))
}

fn invoke_rhai(
    installed: &InstalledExtension,
    compiled: &CompiledRhaiPackage,
    handler: &str,
    context: &ExtensionInvocationContext,
    input: Value,
    settings: Value,
) -> Result<Value, HarnessError> {
    let crate::ExtensionRuntime::Rhai { limits } = &installed.manifest.runtime else {
        return Err(HarnessError::execution(
            "compiled Rhai package does not match the installed runtime",
        ));
    };
    let context_json = rhai_context(context)?;
    let input_bytes = serde_json::to_vec(&context_json)
        .and_then(|mut bytes| {
            bytes.extend(serde_json::to_vec(&input)?);
            bytes.extend(serde_json::to_vec(&settings)?);
            Ok(bytes)
        })
        .map_err(|error| HarnessError::execution(format!("encode Rhai input: {error}")))?;
    if input_bytes.len() > limits.max_input_bytes {
        return Err(HarnessError::policy(
            "Rhai extension input exceeds its byte limit",
        ));
    }

    let deadline = Instant::now() + Duration::from_millis(limits.max_wall_ms);
    let mut engine = ternilo_rhai::restricted_engine(&limits.sandbox)?;
    install_host_functions(
        &mut engine,
        installed,
        context
            .workspace
            .as_ref()
            .map(|workspace| workspace.path.as_str()),
        limits.max_workspace_read_bytes,
    );
    let stop = Arc::new(Mutex::new(None::<StopReason>));
    let progress_stop = Arc::clone(&stop);
    let cancellation = context.cancellation.clone();
    engine.on_progress(move |_| {
        let reason = if cancellation
            .as_ref()
            .is_some_and(RunCancellation::is_cancelled)
        {
            Some(StopReason::Cancelled)
        } else if Instant::now() >= deadline {
            Some(StopReason::WallTime)
        } else {
            None
        };
        if let Some(reason) = reason {
            *progress_stop.lock().expect("Rhai stop lock poisoned") = Some(reason);
            Some(Dynamic::from(reason.message()))
        } else {
            None
        }
    });

    let dynamic_context = rhai::serde::to_dynamic(context_json)
        .map_err(|error| HarnessError::execution(format!("bind Rhai context: {error}")))?;
    let dynamic_input = rhai::serde::to_dynamic(input)
        .map_err(|error| HarnessError::execution(format!("bind Rhai input: {error}")))?;
    let dynamic_settings = rhai::serde::to_dynamic(settings)
        .map_err(|error| HarnessError::execution(format!("bind Rhai settings: {error}")))?;
    let result = engine.call_fn::<Dynamic>(
        &mut Scope::new(),
        &compiled.ast,
        handler,
        (dynamic_context, dynamic_input, dynamic_settings),
    );
    let result = match result {
        Ok(value) => value,
        Err(error) => match *stop.lock().expect("Rhai stop lock poisoned") {
            Some(StopReason::Cancelled) => {
                return Err(HarnessError::cancelled(
                    "Rhai extension invocation was cancelled",
                ));
            }
            Some(StopReason::WallTime) => {
                return Err(HarnessError::policy(
                    "Rhai extension invocation exceeded its wall-time limit",
                ));
            }
            None => {
                return Err(HarnessError::execution(format!(
                    "Rhai extension handler {handler:?} failed: {error}"
                )));
            }
        },
    };
    let result: Value = rhai::serde::from_dynamic(&result)
        .map_err(|error| HarnessError::execution(format!("decode Rhai result: {error}")))?;
    let output_bytes = serde_json::to_vec(&result)
        .map_err(|error| HarnessError::execution(format!("encode Rhai result: {error}")))?;
    if output_bytes.len() > limits.max_output_bytes {
        return Err(HarnessError::policy(
            "Rhai extension output exceeds its byte limit",
        ));
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum StopReason {
    Cancelled,
    WallTime,
}

impl StopReason {
    const fn message(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::WallTime => "wall-time",
        }
    }
}

fn install_host_functions(
    engine: &mut Engine,
    installed: &InstalledExtension,
    workspace_root: Option<&str>,
    max_workspace_read_bytes: usize,
) {
    let can_log = installed.granted_capabilities.contains(&Capability::Log);
    engine.register_fn(
        "log",
        move |level: &str, message: &str| -> Result<(), Box<EvalAltResult>> {
            if !can_log {
                return Err("extension does not have the log capability".into());
            }
            if message.len() > 4 * 1024 {
                return Err("extension log message exceeds 4096 bytes".into());
            }
            eprintln!("[extension:{level}] {message}");
            Ok(())
        },
    );

    let can_read = installed
        .granted_capabilities
        .contains(&Capability::WorkspaceRead);
    let workspace_root = workspace_root.map(PathBuf::from);
    let bytes_read = Arc::new(Mutex::new(0_usize));
    engine.register_fn(
        "read_workspace_text",
        move |path: &str| -> Result<String, Box<EvalAltResult>> {
            if !can_read {
                return Err("extension does not have the workspace_read capability".into());
            }
            let root = workspace_root
                .as_ref()
                .ok_or_else(|| -> Box<EvalAltResult> { "session has no workspace".into() })?;
            let path = Path::new(path);
            if path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        PathComponent::ParentDir
                            | PathComponent::RootDir
                            | PathComponent::Prefix(_)
                    )
                })
            {
                return Err("workspace read path must stay relative to the workspace".into());
            }
            let root = std::fs::canonicalize(root).map_err(|error| -> Box<EvalAltResult> {
                format!("open workspace: {error}").into()
            })?;
            let target =
                std::fs::canonicalize(root.join(path)).map_err(|error| -> Box<EvalAltResult> {
                    format!("open workspace file: {error}").into()
                })?;
            if !target.starts_with(&root) {
                return Err("workspace read path escapes the workspace".into());
            }
            let metadata = std::fs::metadata(&target).map_err(|error| -> Box<EvalAltResult> {
                format!("inspect workspace file: {error}").into()
            })?;
            if !metadata.is_file() {
                return Err("workspace read target must be a regular file".into());
            }
            let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            let mut total = bytes_read
                .lock()
                .expect("workspace read counter lock poisoned");
            if total.saturating_add(size) > max_workspace_read_bytes {
                return Err("workspace read exceeds the extension byte limit".into());
            }
            let bytes = std::fs::read(&target).map_err(|error| -> Box<EvalAltResult> {
                format!("read workspace file: {error}").into()
            })?;
            let text = String::from_utf8(bytes)
                .map_err(|_| -> Box<EvalAltResult> { "workspace file is not UTF-8".into() })?;
            *total += text.len();
            Ok(text)
        },
    );
}

#[derive(Clone)]
pub(crate) struct CompiledWasmPackage {
    engine: WasmtimeEngine,
    component: Arc<Component>,
}

struct WasmHostState {
    package_id: String,
    grants: BTreeSet<Capability>,
    workspace: Option<WorkspaceBinding>,
    max_workspace_read_bytes: u64,
    workspace_read_bytes: u64,
    limits: StoreLimits,
    cancellation: Option<RunCancellation>,
}

fn compile_wasm_component(
    bytes: &[u8],
    package_id: &str,
    world: &str,
    limits: &WasmComponentLimits,
) -> Result<CompiledWasmPackage, HarnessError> {
    let mut config = Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = WasmtimeEngine::new(&config)
        .map_err(|error| HarnessError::execution(format!("create Wasmtime engine: {error}")))?;
    let component = Component::new(&engine, bytes).map_err(|error| {
        HarnessError::invalid(format!("compile WASM Component extension: {error:#}"))
    })?;
    let compiled = CompiledWasmPackage {
        engine,
        component: Arc::new(component),
    };
    let mut store = wasm_store(
        &compiled.engine,
        package_id,
        limits,
        &BTreeSet::new(),
        None,
        None,
    )?;
    let linker = wasm_linker(&compiled.engine)?;
    Runtime::instantiate(&mut store, &compiled.component, &linker).map_err(|error| {
        HarnessError::invalid(format!(
            "WASM Component does not implement {world}: {error:#}"
        ))
    })?;
    Ok(compiled)
}

#[derive(Serialize)]
struct WasmInvocationContext<'a> {
    tenant_id: &'a ternilo_protocol::TenantId,
    user_id: &'a ternilo_protocol::UserId,
    agent_id: &'a ternilo_protocol::AgentId,
    session_id: &'a ternilo_protocol::SessionId,
    run_id: &'a ternilo_protocol::RunId,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<SafeWorkspaceContext<'a>>,
    settings: &'a Value,
}

fn invoke_wasm_component(
    installed: &InstalledExtension,
    compiled: &CompiledWasmPackage,
    handler: &str,
    context: &ExtensionInvocationContext,
    input: &Value,
    settings: &Value,
) -> Result<Value, HarnessError> {
    let ExtensionRuntime::WasmComponent { limits, .. } = &installed.manifest.runtime else {
        return Err(HarnessError::execution(
            "compiled WASM Component does not match the installed runtime",
        ));
    };
    let context_json = serde_json::to_string(&WasmInvocationContext {
        tenant_id: &context.identity.tenant_id,
        user_id: &context.identity.user_id,
        agent_id: &context.identity.agent_id,
        session_id: &context.identity.session_id,
        run_id: &context.run_id,
        workspace: context
            .workspace
            .as_ref()
            .map(|workspace| SafeWorkspaceContext {
                workspace_id: &workspace.workspace_id,
            }),
        settings,
    })
    .map_err(|error| HarnessError::execution(format!("encode WASM context: {error}")))?;
    let input_json = serde_json::to_string(input)
        .map_err(|error| HarnessError::execution(format!("encode WASM input: {error}")))?;
    let input_bytes = handler
        .len()
        .saturating_add(context_json.len())
        .saturating_add(input_json.len());
    if input_bytes > usize::try_from(limits.max_input_bytes).unwrap_or(usize::MAX) {
        return Err(HarnessError::policy(
            "WASM Component invocation input exceeds its byte limit",
        ));
    }
    let mut store = wasm_store(
        &compiled.engine,
        &installed.manifest.package_id,
        limits,
        &installed.granted_capabilities,
        context.workspace.clone(),
        context.cancellation.clone(),
    )?;
    let linker = wasm_linker(&compiled.engine)?;
    let _cancellation = context
        .cancellation
        .as_ref()
        .map(|cancellation| WasmCancellation::watch(compiled.engine.clone(), cancellation.clone()))
        .transpose()?;
    let bindings = Runtime::instantiate(&mut store, &compiled.component, &linker)
        .map_err(|error| wasm_execution_error(context, "instantiate", &error))?;
    let result = bindings
        .call_invoke(&mut store, handler, &context_json, &input_json)
        .map_err(|error| wasm_execution_error(context, "execute", &error))?
        .map_err(|message| {
            HarnessError::execution(format!("WASM Component extension failed: {message}"))
        })?;
    if let Some(cancellation) = &context.cancellation {
        cancellation.check()?;
    }
    if result.len() > usize::try_from(limits.max_output_bytes).unwrap_or(usize::MAX) {
        return Err(HarnessError::policy(
            "WASM Component output exceeds its byte limit",
        ));
    }
    serde_json::from_str(&result).map_err(|error| {
        HarnessError::execution(format!(
            "decode WASM Component handler {handler:?} JSON result: {error}"
        ))
    })
}

fn wasm_execution_error(
    context: &ExtensionInvocationContext,
    operation: &str,
    error: &wasmtime::Error,
) -> HarnessError {
    if context
        .cancellation
        .as_ref()
        .is_some_and(RunCancellation::is_cancelled)
    {
        HarnessError::cancelled("WASM Component invocation was cancelled")
    } else {
        HarnessError::execution(format!("{operation} WASM Component extension: {error}"))
    }
}

fn wasm_linker(engine: &WasmtimeEngine) -> Result<Linker<WasmHostState>, HarnessError> {
    let mut linker = Linker::new(engine);
    Runtime::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .map_err(|error| HarnessError::execution(format!("link WASM host functions: {error}")))?;
    Ok(linker)
}

fn wasm_store(
    engine: &WasmtimeEngine,
    package_id: &str,
    limits: &WasmComponentLimits,
    grants: &BTreeSet<Capability>,
    workspace: Option<WorkspaceBinding>,
    cancellation: Option<RunCancellation>,
) -> Result<Store<WasmHostState>, HarnessError> {
    let memory_limit = usize::try_from(limits.max_memory_bytes)
        .map_err(|_| HarnessError::policy("WASM Component memory limit exceeds this host"))?;
    let store_limits = StoreLimitsBuilder::new()
        .memory_size(memory_limit)
        .instances(16)
        .tables(16)
        .memories(16)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(
        engine,
        WasmHostState {
            package_id: package_id.to_owned(),
            grants: grants.clone(),
            workspace,
            max_workspace_read_bytes: limits.max_workspace_read_bytes,
            workspace_read_bytes: 0,
            limits: store_limits,
            cancellation,
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_epoch_deadline(1);
    store.epoch_deadline_callback(|context| {
        if let Some(cancellation) = &context.data().cancellation {
            cancellation
                .check()
                .map_err(|error| wasmtime::format_err!("{error}"))?;
        }
        Ok(UpdateDeadline::Continue(1))
    });
    store
        .set_fuel(limits.fuel)
        .map_err(|error| HarnessError::execution(format!("set WASM Component fuel: {error}")))?;
    Ok(store)
}

impl host::Host for WasmHostState {
    fn log(&mut self, level: host::LogLevel, message: String) -> Result<(), String> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check().map_err(|error| error.to_string())?;
        }
        if !self.grants.contains(&Capability::Log) {
            return Err("extension does not have the log capability".to_owned());
        }
        if message.len() > 4 * 1024 {
            return Err("extension log message exceeds 4096 bytes".to_owned());
        }
        eprintln!("[extension:{}:{level:?}] {message}", self.package_id);
        Ok(())
    }

    fn read_workspace_text(&mut self, requested: String) -> Result<String, String> {
        self.read_workspace_text_inner(&requested)
            .map_err(|error| error.to_string())
    }
}

impl WasmHostState {
    fn read_workspace_text_inner(&mut self, requested: &str) -> Result<String, HarnessError> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        if !self.grants.contains(&Capability::WorkspaceRead) {
            return Err(HarnessError::policy(
                "extension does not have the workspace_read capability",
            ));
        }
        let workspace = self.workspace.as_ref().ok_or_else(|| {
            HarnessError::policy("this WASM Component invocation has no workspace")
        })?;
        let relative = Path::new(requested);
        if relative.as_os_str().is_empty()
            || relative.is_absolute()
            || relative.components().any(|component| {
                !matches!(component, PathComponent::Normal(_) | PathComponent::CurDir)
            })
        {
            return Err(HarnessError::policy(
                "workspace read path must stay relative to the workspace",
            ));
        }
        let root = canonicalize(&workspace.path, "workspace root")?;
        let candidate = canonicalize(root.join(relative), "workspace file")?;
        if !candidate.starts_with(&root) {
            return Err(HarnessError::policy(
                "workspace read path escapes the bound workspace",
            ));
        }
        let file = std::fs::File::open(&candidate)
            .map_err(|error| HarnessError::execution(format!("open workspace file: {error}")))?;
        let metadata = file
            .metadata()
            .map_err(|error| HarnessError::execution(format!("inspect workspace file: {error}")))?;
        let remaining = self
            .max_workspace_read_bytes
            .saturating_sub(self.workspace_read_bytes);
        if !metadata.is_file() || metadata.len() > remaining {
            return Err(HarnessError::policy(
                "workspace target is not a regular file or exceeds the read limit",
            ));
        }
        let mut take = file.take(remaining.saturating_add(1));
        let mut bytes = Vec::new();
        take.read_to_end(&mut bytes)
            .map_err(|error| HarnessError::execution(format!("read workspace file: {error}")))?;
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if length > remaining {
            return Err(HarnessError::policy(
                "workspace file grew beyond the extension read limit",
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| HarnessError::invalid("workspace file is not UTF-8 text"))?;
        self.workspace_read_bytes = self.workspace_read_bytes.saturating_add(length);
        Ok(text)
    }
}

fn canonicalize(path: impl Into<PathBuf>, label: &str) -> Result<PathBuf, HarnessError> {
    let path = path.into();
    std::fs::canonicalize(&path).map_err(|error| {
        HarnessError::invalid(format!("resolve {label} {}: {error}", path.display()))
    })
}

fn validate_handler_output(
    schema: &Value,
    output: &Value,
    runtime: &str,
    handler: &str,
) -> Result<(), HarnessError> {
    let validator = jsonschema::validator_for(schema).map_err(|error| {
        HarnessError::execution(format!(
            "compile signed output_schema for {runtime} handler {handler:?}: {error}"
        ))
    })?;
    if let Err(error) = validator.validate(output) {
        return Err(HarnessError::execution(format!(
            "{runtime} handler {handler:?} returned JSON that violates its signed output_schema contract: {error}"
        )));
    }
    Ok(())
}

fn render_output(value: Value, maximum: usize, runtime: &str) -> Result<ToolOutput, HarnessError> {
    let output = match value {
        Value::String(content) => ToolOutput {
            content,
            is_error: false,
        },
        Value::Object(object)
            if object.len() == 2
                && object.get("content").is_some_and(Value::is_string)
                && object.get("is_error").is_some_and(Value::is_boolean) =>
        {
            ToolOutput {
                content: object["content"]
                    .as_str()
                    .expect("checked string")
                    .to_owned(),
                is_error: object["is_error"].as_bool().expect("checked boolean"),
            }
        }
        value => ToolOutput {
            content: serde_json::to_string(&value).map_err(|error| {
                HarnessError::execution(format!("encode {runtime} handler result: {error}"))
            })?,
            is_error: false,
        },
    };
    if output.content.len() > maximum {
        return Err(HarnessError::policy(format!(
            "{runtime} extension output exceeds its byte limit"
        )));
    }
    Ok(output)
}
