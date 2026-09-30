use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::PathBuf,
    pin::Pin,
    process::ExitStatus,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::process_group::Child;
use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::Value;
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunCancellation, RunEnvironment,
    RunEnvironmentClient, SandboxMode, SandboxPolicy, Sandboxes, SandboxesClient, Terminals,
    TerminalsProvider, WorkspaceExecutionLease,
};
use ternilo_protocol::{HarnessError, TerminalId, TerminalRead, TerminalSnapshot, TerminalStatus};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, Command},
    sync::{Mutex, Notify, watch},
};

pub const LOCAL_TERMINALS_KIND: &str = "ternilo.terminals.local";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/local-terminals@1",
        requires: [RunEnvironment, Sandboxes],
        provides: [Terminals],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TerminalConfig {
    #[serde(default = "default_output_bytes")]
    max_retained_output_bytes: usize,
    #[serde(default = "default_max_sessions")]
    max_sessions: usize,
}

const fn default_output_bytes() -> usize {
    1024 * 1024
}

const fn default_max_sessions() -> usize {
    8
}

#[must_use]
pub fn local_terminals_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: LOCAL_TERMINALS_KIND,
            requires: &["ternilo/run-environment@1", "ternilo/sandbox@1"],
            provides: &["ternilo/terminals@1"],
        },
        |value| {
            let config: TerminalConfig = parse_config(value)?;
            if config.max_retained_output_bytes < 4096 || config.max_sessions == 0 {
                return Err(HarnessError::composition(
                    "local terminals require max_retained_output_bytes >= 4096 and max_sessions > 0",
                ));
            }
            Ok(Arc::new(LocalTerminalsPlugin { config }))
        },
    )
    .with_description("管理可跨多次工具调用复用的本地持久终端。")
    .with_config_schema::<TerminalConfig>()
}

struct LocalTerminalsPlugin {
    config: TerminalConfig,
}

impl HarnessPlugin for LocalTerminalsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("local terminals declare RunEnvironment");
        let sandbox = context
            .context()
            .service::<Sandboxes>()
            .expect("local terminals declare Sandboxes");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let config = self.config.clone();
        Activation::Once(Box::pin(async move {
            let workspace = environment.workspace().await.ok_or_else(|| {
                linorun_core::ActivationFailure::user(
                    "local terminals require a workspace-bound session",
                )
            })?;
            let root = tokio::fs::canonicalize(&workspace.path)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "resolve terminal workspace {}: {error}",
                        workspace.path
                    ))
                })?;
            let provider = Arc::new(LocalTerminals {
                root,
                environment,
                sandbox,
                config,
                entries: Mutex::new(BTreeMap::new()),
                next_id: AtomicU64::new(1),
                next_marker: AtomicU64::new(1),
            });
            let service: Arc<dyn TerminalsProvider> = provider.clone();
            scope
                .provide::<Terminals>(&route, service)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide local terminals: {error}"
                    ))
                })?;
            Ok(Some(effect::inverse(move || async move {
                provider.shutdown().await;
                Ok(())
            })))
        }))
    }
}

struct LocalTerminals {
    root: PathBuf,
    environment: RunEnvironmentClient,
    sandbox: SandboxesClient,
    config: TerminalConfig,
    entries: Mutex<BTreeMap<TerminalId, Arc<TerminalEntry>>>,
    next_id: AtomicU64,
    next_marker: AtomicU64,
}

struct TerminalEntry {
    id: TerminalId,
    name: Option<String>,
    process_id: u32,
    process_group: Arc<crate::process_group::ManagedProcessGroup>,
    exit_status: Mutex<Option<Result<ExitStatus, String>>>,
    stop: watch::Sender<bool>,
    process: Mutex<Option<tokio::task::JoinHandle<()>>>,
    stdin: Mutex<ChildStdin>,
    output: Mutex<OutputBuffer>,
    notify: Notify,
    send_gate: Mutex<()>,
    readers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    max_output: usize,
}

#[derive(Default)]
struct OutputBuffer {
    base_offset: u64,
    bytes: Vec<u8>,
}

impl OutputBuffer {
    fn end_offset(&self) -> u64 {
        self.base_offset
            .saturating_add(u64::try_from(self.bytes.len()).unwrap_or(u64::MAX))
    }

    fn append(&mut self, bytes: &[u8], max: usize) {
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() > max {
            let remove = self.bytes.len() - max;
            self.bytes.drain(..remove);
            self.base_offset = self
                .base_offset
                .saturating_add(u64::try_from(remove).unwrap_or(u64::MAX));
        }
    }

    fn read(&self, offset: u64, end: Option<u64>) -> (u64, u64, Vec<u8>, bool) {
        let buffer_end = self.end_offset();
        let actual_start = offset.max(self.base_offset).min(buffer_end);
        let actual_end = end.unwrap_or(buffer_end).max(actual_start).min(buffer_end);
        let start_index = usize::try_from(actual_start.saturating_sub(self.base_offset))
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        let end_index = usize::try_from(actual_end.saturating_sub(self.base_offset))
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        (
            actual_start,
            actual_end,
            self.bytes[start_index..end_index].to_vec(),
            offset < self.base_offset,
        )
    }

    fn find_from(&self, offset: u64, needle: &[u8]) -> Option<u64> {
        let start = usize::try_from(offset.max(self.base_offset) - self.base_offset).ok()?;
        let relative = self
            .bytes
            .get(start..)?
            .windows(needle.len())
            .position(|window| window == needle)?;
        Some(
            self.base_offset
                .saturating_add(u64::try_from(start + relative).ok()?),
        )
    }

    fn line_end_after(&self, offset: u64) -> u64 {
        let start = usize::try_from(offset.saturating_sub(self.base_offset))
            .unwrap_or(self.bytes.len())
            .min(self.bytes.len());
        let relative = self.bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(self.bytes.len() - start, |position| position + 1);
        self.base_offset
            .saturating_add(u64::try_from(start + relative).unwrap_or(u64::MAX))
    }
}

impl LocalTerminals {
    #[expect(
        clippy::too_many_lines,
        reason = "register ownership before spawning and publish the terminal only after its supervisor is installed"
    )]
    async fn open(
        &self,
        run_id: ternilo_protocol::RunId,
        name: Option<String>,
    ) -> Result<TerminalSnapshot, HarnessError> {
        let name = name
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if name
            .as_ref()
            .is_some_and(|value| value.chars().count() > 80)
        {
            return Err(HarnessError::invalid(
                "terminal name must not exceed 80 characters",
            ));
        }
        let lease = self
            .environment
            .acquire_workspace(RunCancellation::new())
            .await?;
        let mut command = self.shell_command().await?;
        self.environment
            .check_run_authorization(run_id.clone())
            .await?;
        let (stop_sender, stop_receiver) = watch::channel(false);
        let (completion, finished) = watch::channel(None);
        let held_lease = Arc::new(std::sync::Mutex::new(Some(lease)));
        let control = Arc::new(TerminalControl {
            stop: stop_sender.clone(),
            finished,
            _lease: held_lease.clone(),
        });
        let mut admission = TerminalAdmission(Some(completion.clone()));
        let resource_id = format!("terminal-{:032x}", rand::random::<u128>());
        self.environment
            .register_execution_resource(run_id, resource_id, control)
            .await?;
        if *stop_receiver.borrow() {
            return Err(HarnessError::cancelled(
                "terminal revoked before process creation",
            ));
        }
        let (mut child, process_group) = crate::process_group::spawn(&mut command)
            .map_err(|error| HarnessError::execution(format!("start terminal shell: {error}")))?;
        admission.0 = None;
        let process_id = child
            .id()
            .ok_or_else(|| HarnessError::execution("terminal process has no ID"))?;
        let process_group = Arc::new(process_group);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::execution("terminal stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::execution("terminal stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarnessError::execution("terminal stderr is unavailable"))?;
        let id = TerminalId::new(format!(
            "terminal-{}-{}",
            now_ms()?,
            self.next_id.fetch_add(1, Ordering::Relaxed)
        ));
        let entry = Arc::new(TerminalEntry {
            id: id.clone(),
            name,
            process_id,
            process_group: Arc::clone(&process_group),
            exit_status: Mutex::new(None),
            stop: stop_sender,
            process: Mutex::new(None),
            stdin: Mutex::new(stdin),
            output: Mutex::new(OutputBuffer::default()),
            notify: Notify::new(),
            send_gate: Mutex::new(()),
            readers: Mutex::new(Vec::new()),
            max_output: self.config.max_retained_output_bytes,
        });
        let stdout_task = tokio::spawn(read_output(stdout, Arc::downgrade(&entry)));
        let stderr_task = tokio::spawn(read_output(stderr, Arc::downgrade(&entry)));
        entry
            .readers
            .lock()
            .await
            .extend([stdout_task, stderr_task]);
        let process = tokio::spawn(supervise_process(
            child,
            process_group,
            stop_receiver,
            Arc::downgrade(&entry),
            held_lease,
            completion,
        ));
        admission.0 = None;
        *entry.process.lock().await = Some(process);
        {
            let mut entries = self.entries.lock().await;
            if entries.len() >= self.config.max_sessions {
                drop(entries);
                terminate(&entry).await;
                return Err(HarnessError::policy(format!(
                    "session already owns the maximum of {} terminals",
                    self.config.max_sessions
                )));
            }
            entries.insert(id, entry.clone());
        }
        entry.snapshot().await
    }

    async fn shell_command(&self) -> Result<Command, HarnessError> {
        let permissions = self.environment.permissions().await;
        let (program, arguments) = native_interactive_shell();
        let mut command = if permissions.allows_full_access() {
            let mut command = Command::new(program);
            command.args(arguments).current_dir(&self.root);
            command
        } else {
            let mode = if permissions.allows_workspace_write() {
                SandboxMode::WorkspaceWrite
            } else {
                SandboxMode::ReadOnly
            };
            let confined = self
                .sandbox
                .confine(
                    program,
                    arguments,
                    SandboxPolicy {
                        mode,
                        workspace_root: self.root.clone(),
                    },
                )
                .await?;
            crate::sandbox::command_from_confined(confined, &self.root)
        };
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::process_group::configure(&mut command);
        Ok(command)
    }

    async fn entry(&self, id: &TerminalId) -> Result<Arc<TerminalEntry>, HarnessError> {
        self.entries
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| HarnessError::invalid(format!("unknown terminal {id}")))
    }

    async fn send(
        &self,
        id: &TerminalId,
        input: String,
        wait_ms: u64,
    ) -> Result<TerminalRead, HarnessError> {
        if input.trim().is_empty() {
            return Err(HarnessError::invalid("terminal input must not be empty"));
        }
        if !(100..=300_000).contains(&wait_ms) {
            return Err(HarnessError::invalid(
                "terminal wait_ms must be between 100 and 300000",
            ));
        }
        let entry = self.entry(id).await?;
        let _send = entry.send_gate.lock().await;
        let marker = format!(
            "__TERNILO_TERMINAL_DONE_{}_{}__",
            now_ms()?,
            self.next_marker.fetch_add(1, Ordering::Relaxed)
        );
        let start = entry.output.lock().await.end_offset();
        let script = terminal_input_script(&input, &marker);
        entry
            .stdin
            .lock()
            .await
            .write_all(script.as_bytes())
            .await
            .map_err(|error| HarnessError::execution(format!("write terminal input: {error}")))?;
        let marker_bytes = marker.as_bytes();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let notified = entry.notify.notified();
            let marker_range = {
                let output = entry.output.lock().await;
                output
                    .find_from(start, marker_bytes)
                    .map(|marker_offset| (marker_offset, output.line_end_after(marker_offset)))
            };
            if let Some((marker_offset, marker_end)) = marker_range {
                return entry
                    .read_range(start, Some(marker_offset), marker_end, false)
                    .await;
            }
            if entry.snapshot().await?.status == TerminalStatus::Exited {
                return entry.read_range(start, None, 0, false).await;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return entry.read_range(start, None, 0, true).await;
            }
        }
    }

    async fn read(&self, id: &TerminalId, offset: u64) -> Result<TerminalRead, HarnessError> {
        self.entry(id)
            .await?
            .read_range(offset, None, 0, false)
            .await
    }

    async fn signal(
        &self,
        id: &TerminalId,
        signal: &str,
    ) -> Result<TerminalSnapshot, HarnessError> {
        let entry = self.entry(id).await?;
        let signal = match signal {
            "interrupt" | "sigint" => TerminalSignal::Interrupt,
            "terminate" | "sigterm" => TerminalSignal::Terminate,
            _ => {
                return Err(HarnessError::invalid(
                    "terminal signal must be interrupt or terminate",
                ));
            }
        };
        if entry.exit_status.lock().await.is_some() {
            return Err(HarnessError::execution(
                "terminal process has already exited",
            ));
        }
        let pid = entry.process_id;
        #[cfg(unix)]
        signal_process(pid, signal)?;
        #[cfg(not(any(unix, windows)))]
        signal_process(pid, signal)?;
        #[cfg(windows)]
        signal_process(pid, signal).await?;
        entry.snapshot().await
    }

    async fn close(&self, id: &TerminalId) -> Result<TerminalSnapshot, HarnessError> {
        let entry = self
            .entries
            .lock()
            .await
            .remove(id)
            .ok_or_else(|| HarnessError::invalid(format!("unknown terminal {id}")))?;
        terminate(&entry).await;
        entry.snapshot().await
    }

    async fn list(&self) -> Vec<TerminalSnapshot> {
        let entries = self
            .entries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(entries.len());
        for entry in entries {
            if let Ok(snapshot) = entry.snapshot().await {
                snapshots.push(snapshot);
            }
        }
        snapshots.sort_by(|left, right| left.terminal_id.cmp(&right.terminal_id));
        snapshots
    }

    async fn shutdown(&self) {
        let entries = std::mem::take(&mut *self.entries.lock().await);
        for entry in entries.into_values() {
            terminate(&entry).await;
        }
    }
}

impl TerminalEntry {
    async fn snapshot(&self) -> Result<TerminalSnapshot, HarnessError> {
        let state = self.exit_status.lock().await;
        let status = match state.as_ref() {
            Some(Ok(status)) => Some(status),
            Some(Err(error)) => {
                return Err(HarnessError::execution(format!(
                    "inspect terminal process: {error}"
                )));
            }
            None => None,
        };
        Ok(TerminalSnapshot {
            terminal_id: self.id.clone(),
            name: self.name.clone(),
            status: if status.is_some() {
                TerminalStatus::Exited
            } else {
                TerminalStatus::Running
            },
            exit_code: status.and_then(ExitStatus::code),
            output_bytes: self.output.lock().await.end_offset(),
        })
    }

    async fn read_range(
        &self,
        offset: u64,
        end: Option<u64>,
        next_offset_override: u64,
        timed_out: bool,
    ) -> Result<TerminalRead, HarnessError> {
        let (actual, end_offset, bytes, truncated) = self.output.lock().await.read(offset, end);
        Ok(TerminalRead {
            terminal: self.snapshot().await?,
            offset: actual,
            next_offset: if next_offset_override == 0 {
                end_offset
            } else {
                next_offset_override
            },
            output: clean_output(&String::from_utf8_lossy(&bytes)),
            truncated,
            timed_out,
        })
    }
}

impl TerminalsProvider for LocalTerminals {
    fn open<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: ternilo_protocol::RunId,
        name: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.open(run_id, name).await })
    }

    fn send<'a>(
        &'a self,
        _: CallContext<()>,
        terminal_id: TerminalId,
        input: String,
        wait_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalRead, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.send(&terminal_id, input, wait_ms).await })
    }

    fn read<'a>(
        &'a self,
        _: CallContext<()>,
        terminal_id: TerminalId,
        offset: u64,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalRead, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.read(&terminal_id, offset).await })
    }

    fn signal<'a>(
        &'a self,
        _: CallContext<()>,
        terminal_id: TerminalId,
        signal: String,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.signal(&terminal_id, &signal).await })
    }

    fn close<'a>(
        &'a self,
        _: CallContext<()>,
        terminal_id: TerminalId,
    ) -> Pin<Box<dyn Future<Output = Result<TerminalSnapshot, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.close(&terminal_id).await })
    }

    fn list<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<TerminalSnapshot>> + Send + 'a>> {
        Box::pin(async move { self.list().await })
    }
}

async fn read_output<R>(mut reader: R, entry: Weak<TerminalEntry>)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = vec![0_u8; 8 * 1024];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                let Some(entry) = entry.upgrade() else {
                    return;
                };
                entry
                    .output
                    .lock()
                    .await
                    .append(&chunk[..size], entry.max_output);
                entry.notify.notify_waiters();
            }
        }
    }
    if let Some(entry) = entry.upgrade() {
        entry.notify.notify_waiters();
    }
}

async fn supervise_process(
    mut child: Child,
    process_group: Arc<crate::process_group::ManagedProcessGroup>,
    mut stop: watch::Receiver<bool>,
    entry: Weak<TerminalEntry>,
    lease: Arc<std::sync::Mutex<Option<WorkspaceExecutionLease>>>,
    completion: watch::Sender<Option<Result<(), String>>>,
) {
    let status = tokio::select! {
        result = child.wait() => result,
        _ = stop.changed() => {
            #[cfg(windows)]
            if let Some(pid) = child.id() { let _ = signal_process(pid, TerminalSignal::Terminate).await; }
            process_group.terminate();
            let _ = child.start_kill();
            child.wait().await
        }
    };
    process_group.terminate();
    let cleanup = process_group.wait_quiescent().await;
    if let Some(entry) = entry.upgrade() {
        let readers = std::mem::take(&mut *entry.readers.lock().await);
        for mut reader in readers {
            if tokio::time::timeout(Duration::from_secs(1), &mut reader)
                .await
                .is_err()
            {
                reader.abort();
                let _ = reader.await;
            }
        }
        *entry.exit_status.lock().await = Some(status.map_err(|error| error.to_string()));
        entry.notify.notify_waiters();
    }
    if cleanup.is_ok() {
        lease.lock().expect("terminal lease lock").take();
    }
    completion.send_replace(Some(cleanup.map_err(|error| error.to_string())));
}

impl Drop for TerminalEntry {
    fn drop(&mut self) {
        self.process_group.terminate();
        // Dropping the sender wakes the independent process supervisor.
        self.stop.send_replace(true);
        for reader in self.readers.get_mut().drain(..) {
            reader.abort();
        }
    }
}

struct TerminalAdmission(Option<watch::Sender<Option<Result<(), String>>>>);
impl Drop for TerminalAdmission {
    fn drop(&mut self) {
        if let Some(completion) = self.0.take() {
            completion.send_replace(Some(Ok(())));
        }
    }
}
struct TerminalControl {
    _lease: Arc<std::sync::Mutex<Option<WorkspaceExecutionLease>>>,
    stop: watch::Sender<bool>,
    finished: watch::Receiver<Option<Result<(), String>>>,
}
impl ternilo_kernel::ExecutionResourceControl for TerminalControl {
    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.stop.send_replace(true);
            let mut finished = self.finished.clone();
            loop {
                if let Some(result) = finished.borrow().clone() {
                    return result.map_err(HarnessError::execution);
                }
                finished.changed().await.map_err(|_| {
                    HarnessError::unavailable("terminal supervisor ended without a cleanup receipt")
                })?;
            }
        })
    }
    fn is_finished(&self) -> bool {
        matches!(&*self.finished.borrow(), Some(Ok(())))
    }
}

async fn terminate(entry: &TerminalEntry) {
    entry.process_group.terminate();
    entry.stop.send_replace(true);
    if let Some(process) = entry.process.lock().await.take() {
        let _ = process.await;
    }
    for reader in entry.readers.lock().await.drain(..) {
        reader.abort();
    }
    entry.notify.notify_waiters();
}

#[derive(Clone, Copy)]
enum TerminalSignal {
    Interrupt,
    Terminate,
}

#[cfg(unix)]
fn signal_process(process_id: u32, signal: TerminalSignal) -> Result<(), HarnessError> {
    let process_id = i32::try_from(process_id)
        .map_err(|_| HarnessError::execution("terminal process id exceeds i32"))?;
    let signal = match signal {
        TerminalSignal::Interrupt => nix::sys::signal::Signal::SIGINT,
        TerminalSignal::Terminate => nix::sys::signal::Signal::SIGTERM,
    };
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(process_id), signal)
        .map_err(|error| HarnessError::execution(format!("signal terminal: {error}")))
}

#[cfg(windows)]
async fn signal_process(process_id: u32, signal: TerminalSignal) -> Result<(), HarnessError> {
    let mut command = Command::new("taskkill.exe");
    command.arg("/PID").arg(process_id.to_string()).arg("/T");
    if matches!(signal, TerminalSignal::Terminate) {
        command.arg("/F");
    }
    let output = command
        .output()
        .await
        .map_err(|error| HarnessError::execution(format!("signal terminal: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(HarnessError::execution(format!(
            "signal terminal failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(not(any(unix, windows)))]
fn signal_process(_: u32, _: TerminalSignal) -> Result<(), HarnessError> {
    Err(HarnessError::policy(
        "terminal signals are unsupported on this operating system",
    ))
}

#[cfg(unix)]
fn native_interactive_shell() -> (OsString, Vec<OsString>) {
    (
        OsString::from("bash"),
        vec![OsString::from("--noprofile"), OsString::from("--norc")],
    )
}

#[cfg(windows)]
fn native_interactive_shell() -> (OsString, Vec<OsString>) {
    (
        OsString::from("powershell.exe"),
        vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NoExit"),
        ],
    )
}

#[cfg(not(any(unix, windows)))]
fn native_interactive_shell() -> (OsString, Vec<OsString>) {
    (OsString::from("sh"), Vec::new())
}

#[cfg(unix)]
fn terminal_input_script(input: &str, marker: &str) -> String {
    format!(
        "{}\nprintf '\\n{}:%s\\n' \"$?\"\n",
        input.trim_end(),
        marker
    )
}

#[cfg(windows)]
fn terminal_input_script(input: &str, marker: &str) -> String {
    format!(
        "{}\n$__ternilo_status = if ($?) {{ 0 }} else {{ 1 }}\n[Console]::Out.Write(\"`n{}:$__ternilo_status`n\")\n",
        input.trim_end(),
        marker
    )
}

#[cfg(not(any(unix, windows)))]
fn terminal_input_script(input: &str, marker: &str) -> String {
    format!(
        "{}\nprintf '\\n{}:%s\\n' \"$?\"\n",
        input.trim_end(),
        marker
    )
}

fn clean_output(output: &str) -> String {
    output
        .lines()
        .filter(|line| !line.starts_with("__TERNILO_TERMINAL_DONE_"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_config<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(if value.is_null() {
        serde_json::json!({})
    } else {
        value
    })
    .map_err(|error| HarnessError::composition(format!("invalid terminal config: {error}")))
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(all(test, unix))]
#[path = "terminal_lifecycle_tests.rs"]
mod lifecycle_tests;
