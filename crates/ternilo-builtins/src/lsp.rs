use std::{
    collections::BTreeMap,
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    DeferredToolSource, HarnessPlugin, PluginFactory, PluginManifest, RunCancellation,
    RunEnvironment, RunEnvironmentClient, ToolExecutionContext, ToolHandler, ToolRegistration,
    Tools, WorkspaceExecutionLease,
};
use ternilo_protocol::{
    HarnessError, SessionServiceKind, SessionServiceSnapshot, SessionServiceStatus, ToolOutput,
    ToolSpec,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use crate::{
    factory as make_factory, parse_config,
    process_group::{ManagedProcess, OwnedProcessGroup, ProcessControl},
};

pub const KIND: &str = "ternilo.lsp.stdio";

#[cfg(all(test, unix))]
#[path = "lsp_lifetime_tests.rs"]
mod lifetime_tests;

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-lsp-stdio@1",
        requires: [Tools, RunEnvironment],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LspConfig {
    server_name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_refs: BTreeMap<String, String>,
    #[serde(default)]
    initialization_options: Option<Value>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_max_message_bytes")]
    max_message_bytes: usize,
}

const fn default_timeout_ms() -> u64 {
    30_000
}

const fn default_max_message_bytes() -> usize {
    8 * 1024 * 1024
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/run-environment@1"],
            provides: &[],
        },
        |value| {
            let config: LspConfig = parse_config(value)?;
            if config.server_name.trim().is_empty()
                || config.command.trim().is_empty()
                || config.timeout_ms == 0
                || config.max_message_bytes < 1024
            {
                return Err(HarnessError::composition(
                    "LSP config requires server_name, command, positive timeout, and max_message_bytes >= 1024",
                ));
            }
            Ok(Arc::new(LspPlugin { config }))
        },
    )
    .with_config_schema::<LspConfig>()
}

struct LspPlugin {
    config: LspConfig,
}

impl HarnessPlugin for LspPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("LSP declares Tools");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("LSP declares RunEnvironment");
        let source = Arc::new(LspSource {
            handler: Arc::new(LspTool {
                config: self.config.clone(),
                environment,
                process: Mutex::new(None),
                state: StdMutex::new(LspState::default()),
            }),
        });
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_source(source)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_source(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct LspSource {
    handler: Arc<LspTool>,
}

impl Drop for LspSource {
    fn drop(&mut self) {
        let mut state = self.handler.state.lock().expect("LSP state lock poisoned");
        state.closed = true;
        state.manually_stopped = true;
        state.cancellation.cancel();
        if let Some(control) = &state.control {
            control.request_stop();
        }
    }
}

impl DeferredToolSource for LspSource {
    fn snapshot(&self) -> SessionServiceSnapshot {
        let mut state = self.handler.state.lock().expect("LSP state lock poisoned");
        state.refresh();
        SessionServiceSnapshot {
            id: format!("lsp:{}", self.handler.config.server_name),
            name: self.handler.config.server_name.clone(),
            kind: SessionServiceKind::Lsp,
            status: state.status,
            active_calls: state.active_calls,
            error: state.error.clone(),
        }
    }

    fn initial_tools(&self) -> Vec<ToolRegistration> {
        vec![ToolRegistration {
            spec: ToolSpec {
                name: format!("lsp__{}", normalize_name(&self.handler.config.server_name)),
                description: format!(
                    "Send one JSON-RPC request to the configured {} language server. Prefer filesystem search for broad discovery and LSP for semantic navigation.",
                    self.handler.config.server_name
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "method": { "type": "string" },
                        "params": {},
                        "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 300_000 }
                    },
                    "required": ["method"],
                    "additionalProperties": false
                }),
            },
            effect: ternilo_kernel::ToolEffect::Dangerous,
            handler: self.handler.clone(),
        }]
    }

    fn prepare<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            cancellation.check()?;
            Ok(self.initial_tools())
        })
    }

    fn start<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolRegistration>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            {
                let mut state = self.handler.state.lock().expect("LSP state lock poisoned");
                state.refresh();
                if matches!(
                    state.status,
                    SessionServiceStatus::Starting | SessionServiceStatus::Stopping
                ) {
                    return Err(HarnessError::conflict("LSP service is changing state"));
                }
            }
            let mut stored = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(HarnessError::cancelled("LSP start was cancelled")),
                stored = self.handler.process.lock() => stored,
            };
            {
                let mut state = self.handler.state.lock().expect("LSP state lock poisoned");
                state.refresh();
                if state.closed {
                    return Err(HarnessError::unavailable("LSP source was unmounted"));
                }
                if matches!(
                    state.status,
                    SessionServiceStatus::Starting | SessionServiceStatus::Stopping
                ) {
                    return Err(HarnessError::conflict("LSP service is changing state"));
                }
                if state.status != SessionServiceStatus::Running {
                    state.manually_stopped = false;
                    state.status = SessionServiceStatus::Idle;
                    state.error = None;
                }
            }
            self.handler
                .ensure_started(&mut stored, &cancellation)
                .await?;
            Ok(self.initial_tools())
        })
    }

    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.handler.stop(false))
    }

    fn shutdown<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.handler.stop(true))
    }
}

struct LspState {
    status: SessionServiceStatus,
    manually_stopped: bool,
    closed: bool,
    active_calls: u32,
    cancellation: RunCancellation,
    control: Option<ProcessControl>,
    error: Option<String>,
}

impl Default for LspState {
    fn default() -> Self {
        Self {
            status: SessionServiceStatus::Idle,
            manually_stopped: false,
            closed: false,
            active_calls: 0,
            cancellation: RunCancellation::new(),
            control: None,
            error: None,
        }
    }
}

impl LspState {
    fn refresh(&mut self) {
        if let Some(control) = &self.control
            && control.is_finished()
        {
            if self.status == SessionServiceStatus::Stopping {
                self.status = SessionServiceStatus::Stopped;
            } else if self.status == SessionServiceStatus::Running {
                self.status = SessionServiceStatus::Failed;
                self.error = Some(format!(
                    "LSP server exited{}; start it again from session services",
                    control
                        .status()
                        .map_or_else(String::new, |status| format!(" with {status}"))
                ));
            }
        }
    }

    fn check_available(&mut self) -> Result<(), HarnessError> {
        self.refresh();
        if self.closed {
            return Err(HarnessError::unavailable("LSP source was unmounted"));
        }
        if self.manually_stopped || self.status == SessionServiceStatus::Failed {
            return Err(HarnessError::unavailable(
                "LSP service is stopped or failed; start it from session services before using it",
            ));
        }
        Ok(())
    }
}

/// A dropped initialization or RPC must stop its process even if the caller disappears.
struct LspOperation<'a> {
    state: &'a StdMutex<LspState>,
    completed: bool,
}

impl Drop for LspOperation<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = self.state.lock().expect("LSP state lock poisoned");
            if let Some(control) = &state.control {
                control.request_stop();
            }
            state.active_calls = 0;
            if state.manually_stopped {
                state.status = SessionServiceStatus::Stopping;
                if state.control.is_none() {
                    state.status = SessionServiceStatus::Stopped;
                }
                state.refresh();
            } else {
                state.status = SessionServiceStatus::Failed;
                state.error.get_or_insert_with(|| {
                    "LSP operation was interrupted; start the service again".to_owned()
                });
            }
        }
    }
}

struct LspTool {
    config: LspConfig,
    environment: RunEnvironmentClient,
    process: Mutex<Option<LspProcess>>,
    state: StdMutex<LspState>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LspArguments {
    method: String,
    #[serde(default)]
    params: Value,
    timeout_ms: Option<u64>,
}

impl ToolHandler for LspTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            context.cancellation.check()?;
            let arguments: LspArguments = serde_json::from_value(arguments).map_err(|error| {
                HarnessError::invalid(format!("invalid LSP arguments: {error}"))
            })?;
            if arguments.method.trim().is_empty()
                || matches!(
                    arguments.method.as_str(),
                    "initialize" | "initialized" | "shutdown" | "exit"
                )
                || arguments.method.starts_with("$/")
            {
                return Err(HarnessError::invalid(
                    "LSP method must be a non-lifecycle request method",
                ));
            }
            if context.workspace.is_none() {
                return Err(HarnessError::execution("LSP requires a bound workspace"));
            }
            let timeout_ms = arguments.timeout_ms.unwrap_or(self.config.timeout_ms);
            if timeout_ms == 0 || timeout_ms > 300_000 {
                return Err(HarnessError::invalid(
                    "LSP timeout_ms must be between 1 and 300000",
                ));
            }
            let mut stored = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => return Err(HarnessError::cancelled("LSP request was cancelled")),
                stored = self.process.lock() => stored,
            };
            self.ensure_started(&mut stored, &context.cancellation)
                .await?;
            let service_cancellation = {
                let mut state = self.state.lock().expect("LSP state lock poisoned");
                state.check_available()?;
                state.active_calls = 1;
                state.cancellation.clone()
            };
            let mut operation = LspOperation {
                state: &self.state,
                completed: false,
            };
            // The in-flight request owns the process so cancellation also tears down its writers.
            let process = stored.take().expect("initialized LSP process is available");
            let result = tokio::select! {
                biased;
                () = context.cancellation.cancelled() => Err(HarnessError::cancelled("LSP request was cancelled")),
                () = service_cancellation.cancelled() => Err(HarnessError::cancelled("LSP service was stopped")),
                result = process.call(&arguments.method, arguments.params, timeout_ms) => result,
            };
            let (process, response) = match result {
                Ok(result) => result,
                Err(error) => {
                    self.record_failure(&error).await;
                    return Err(error);
                }
            };
            *stored = Some(process);
            self.state
                .lock()
                .expect("LSP state lock poisoned")
                .active_calls = 0;
            operation.completed = true;
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&response).map_err(|error| {
                    HarnessError::execution(format!("serialize LSP response: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

impl LspTool {
    async fn ensure_started(
        &self,
        stored: &mut Option<LspProcess>,
        cancellation: &RunCancellation,
    ) -> Result<(), HarnessError> {
        cancellation.check()?;
        let service_cancellation = {
            let mut state = self.state.lock().expect("LSP state lock poisoned");
            state.check_available()?;
            if state.status == SessionServiceStatus::Running && stored.is_some() {
                return Ok(());
            }
            state.status = SessionServiceStatus::Starting;
            state.error = None;
            state.cancellation = RunCancellation::new();
            state.cancellation.clone()
        };
        let mut operation = LspOperation {
            state: &self.state,
            completed: false,
        };
        if let Some(previous) = stored.take() {
            previous.shutdown().await?;
        }
        let startup = async {
            let lease = self
                .environment
                .acquire_workspace(service_cancellation.clone())
                .await?;
            let workspace = self
                .environment
                .workspace()
                .await
                .ok_or_else(|| HarnessError::execution("LSP requires a bound workspace"))?;
            let mut process = LspProcess::launch(
                &self.config,
                Path::new(&workspace.path),
                &self.environment,
                Some(lease),
            )
            .await?;
            self.state.lock().expect("LSP state lock poisoned").control =
                Some(process.child.control());
            let root_uri = reqwest::Url::from_directory_path(&workspace.path)
                .map_err(|()| {
                    HarnessError::execution("workspace path cannot be represented as a URI")
                })?
                .to_string();
            let identity = self.environment.identity().await;
            process
                .initialize(&self.config, root_uri, identity.session_id.as_str())
                .await?;
            Ok::<_, HarnessError>(process)
        };
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(HarnessError::cancelled("LSP initialization was cancelled")),
            () = service_cancellation.cancelled() => Err(HarnessError::cancelled("LSP initialization was stopped")),
            result = startup => result,
        };
        let process = match result {
            Ok(process) => process,
            Err(error) => {
                self.record_failure(&error).await;
                return Err(error);
            }
        };
        {
            let mut state = self.state.lock().expect("LSP state lock poisoned");
            state.check_available()?;
            service_cancellation.check()?;
            state.status = SessionServiceStatus::Running;
        }
        *stored = Some(process);
        operation.completed = true;
        Ok(())
    }

    async fn record_failure(&self, error: &HarnessError) {
        let control = {
            let mut state = self.state.lock().expect("LSP state lock poisoned");
            state.error = Some(error.to_string());
            state.control.clone()
        };
        if let Some(control) = control {
            let _ = control.stop().await;
        }
    }

    async fn stop(&self, force: bool) -> Result<(), HarnessError> {
        {
            let mut state = self.state.lock().expect("LSP state lock poisoned");
            state.refresh();
            if !force && state.active_calls > 0 {
                return Err(HarnessError::conflict(
                    "LSP is processing a request; stop the active run before stopping the service",
                ));
            }
            state.manually_stopped = true;
            state.closed |= force;
            state.cancellation.cancel();
            if (force || state.status == SessionServiceStatus::Starting)
                && let Some(control) = &state.control
            {
                control.request_stop();
            }
            state.status = SessionServiceStatus::Stopping;
            state.error = None;
        }
        let mut operation = LspOperation {
            state: &self.state,
            completed: false,
        };
        let mut stored = self.process.lock().await;
        if let Some(process) = stored.take() {
            process.shutdown().await?;
        }
        let control = self
            .state
            .lock()
            .expect("LSP state lock poisoned")
            .control
            .clone();
        if let Some(control) = control {
            control.stop().await.map_err(|error| {
                HarnessError::execution(format!("stop {} LSP: {error}", self.config.server_name))
            })?;
        }
        let mut state = self.state.lock().expect("LSP state lock poisoned");
        state.status = SessionServiceStatus::Stopped;
        state.active_calls = 0;
        state.control = None;
        state.error = None;
        operation.completed = true;
        Ok(())
    }
}

struct LspProcess {
    child: ManagedProcess,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: tokio::task::JoinHandle<()>,
    stderr_tail: Arc<StdMutex<String>>,
    next_id: u64,
    max_message_bytes: usize,
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}

impl LspProcess {
    async fn shutdown(mut self) -> Result<(), HarnessError> {
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            self.request("shutdown", Value::Null).await?;
            self.notify("exit", Value::Null).await
        })
        .await;
        self.child
            .stop()
            .await
            .map_err(|error| HarnessError::execution(format!("stop LSP process: {error}")))?;
        self.stderr.abort();
        Ok(())
    }
    async fn launch(
        config: &LspConfig,
        workspace: &Path,
        environment: &RunEnvironmentClient,
        lease: Option<WorkspaceExecutionLease>,
    ) -> Result<Self, HarnessError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(workspace)
            .env_clear()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for key in ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command.envs(&config.env);
        for (target, reference) in &config.env_refs {
            let value = environment
                .resolve_secret(reference.clone())
                .await?
                .ok_or_else(|| {
                    HarnessError::execution(format!(
                        "LSP credential {reference:?} is not configured"
                    ))
                })?;
            command.env(target, value);
        }
        Self::spawn(&mut command, config.max_message_bytes, lease).map_err(|error| {
            HarnessError::execution(format!("start {} LSP: {error}", config.server_name))
        })
    }

    async fn initialize(
        &mut self,
        config: &LspConfig,
        root_uri: String,
        session_id: &str,
    ) -> Result<(), HarnessError> {
        tokio::time::timeout(Duration::from_millis(config.timeout_ms), async {
            self.request(
                "initialize",
                json!({
                    "processId": null,
                    "clientInfo": { "name": "Ternilo", "version": env!("CARGO_PKG_VERSION") },
                    "rootUri": root_uri,
                    "workspaceFolders": [{ "uri": root_uri, "name": session_id }],
                    "capabilities": {},
                    "initializationOptions": config.initialization_options,
                }),
            )
            .await?;
            self.notify("initialized", json!({})).await
        })
        .await
        .map_err(|_| {
            HarnessError::execution(format!(
                "LSP server {} initialization exceeded {} ms",
                config.server_name, config.timeout_ms
            ))
        })?
    }

    fn spawn(
        command: &mut Command,
        max_message_bytes: usize,
        lease: Option<WorkspaceExecutionLease>,
    ) -> Result<Self, HarnessError> {
        crate::process_group::configure(command);
        let mut child = command
            .spawn()
            .map_err(|error| HarnessError::execution(format!("start LSP process: {error}")))?;
        let group = OwnedProcessGroup::new(child.id());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::execution("LSP stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::execution("LSP stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::execution("LSP stderr is unavailable"))?;
        let child = ManagedProcess::new(child, group, lease);
        let stderr_tail = Arc::new(StdMutex::new(String::new()));
        let stderr_capture = stderr_tail.clone();
        let stderr = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Ok(mut tail) = stderr_capture.lock() {
                            tail.push_str(&line);
                            if tail.len() > 8_192 {
                                let split = tail
                                    .char_indices()
                                    .find_map(|(index, _)| (index >= 4_096).then_some(index))
                                    .unwrap_or(0);
                                tail.drain(..split);
                            }
                        }
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr,
            stderr_tail,
            next_id: 1,
            max_message_bytes,
        })
    }

    async fn call(
        mut self,
        method: &str,
        params: Value,
        timeout_ms: u64,
    ) -> Result<(Self, Value), HarnessError> {
        let response = tokio::time::timeout(
            Duration::from_millis(timeout_ms),
            self.request(method, params),
        )
        .await
        .map_err(|_| {
            HarnessError::execution(format!("LSP request {method:?} exceeded {timeout_ms} ms"))
        })??;
        Ok((self, response))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, HarnessError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| HarnessError::execution("LSP request id exhausted"))?;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        loop {
            let message = self.read_message().await?;
            if message.get("id") == Some(&json!(id)) {
                if let Some(error) = message.get("error") {
                    return Err(HarnessError::execution(format!(
                        "LSP {method:?} returned an error: {error}"
                    )));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                let response_id = message["id"].clone();
                self.write_message(&json!({
                    "jsonrpc": "2.0",
                    "id": response_id,
                    "error": { "code": -32601, "message": "Ternilo does not implement server-to-client requests" }
                }))
                .await?;
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), HarnessError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_message(&mut self, message: &Value) -> Result<(), HarnessError> {
        let body = serde_json::to_vec(message)
            .map_err(|error| HarnessError::execution(format!("serialize LSP message: {error}")))?;
        if body.len() > self.max_message_bytes {
            return Err(HarnessError::policy(
                "outgoing LSP message exceeds configured cap",
            ));
        }
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|error| HarnessError::execution(format!("write LSP header: {error}")))?;
        self.stdin
            .write_all(&body)
            .await
            .map_err(|error| HarnessError::execution(format!("write LSP body: {error}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| HarnessError::execution(format!("flush LSP request: {error}")))
    }

    async fn read_message(&mut self) -> Result<Value, HarnessError> {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let bytes = self.stdout.read_line(&mut line).await.map_err(|error| {
                HarnessError::execution(format!("read LSP response header: {error}"))
            })?;
            if bytes == 0 {
                let status = self.child.status();
                let stderr = self
                    .stderr_tail
                    .lock()
                    .map(|tail| tail.trim().to_owned())
                    .unwrap_or_default();
                return Err(HarnessError::execution(format!(
                    "LSP server closed stdout{}{}",
                    status.map_or_else(String::new, |status| format!(" with {status}")),
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    }
                )));
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.trim().strip_prefix("Content-Length:").map(str::trim) {
                content_length = Some(value.parse::<usize>().map_err(|error| {
                    HarnessError::execution(format!("invalid LSP Content-Length: {error}"))
                })?);
            }
        }
        let content_length = content_length
            .ok_or_else(|| HarnessError::execution("LSP response omitted Content-Length"))?;
        if content_length > self.max_message_bytes {
            return Err(HarnessError::policy(format!(
                "LSP response exceeds {} bytes",
                self.max_message_bytes
            )));
        }
        let mut body = vec![0_u8; content_length];
        self.stdout
            .read_exact(&mut body)
            .await
            .map_err(|error| HarnessError::execution(format!("read LSP response body: {error}")))?;
        serde_json::from_slice(&body)
            .map_err(|error| HarnessError::execution(format!("parse LSP response: {error}")))
    }
}

fn normalize_name(value: &str) -> String {
    let mut normalized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    normalized.truncate(48);
    if normalized.is_empty() {
        "server".to_owned()
    } else {
        normalized
    }
}
