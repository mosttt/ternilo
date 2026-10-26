use std::{
    ffi::OsString,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitStatus,
    sync::Arc,
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::Value;
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, RunCancellation, RunEnvironment,
    RunEnvironmentClient, SandboxMode, SandboxPolicy, Sandboxes, SandboxesClient, Shell,
    ShellProvider, WorkspaceExecutionLease, WorkspaceFiles, WorkspaceFilesProvider,
};
use ternilo_protocol::{
    FileContent, FileListRequest, FileListResult, FileReadRequest, FileReplaceRequest,
    FileReplaceResult, FileSearchRequest, FileSearchResult, FileWriteRequest, FileWriteResult,
    HarnessError, ShellRequest, ShellResult,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use crate::file_read_cache::{FileReadCache, FileVersion};

mod search;
#[cfg(any(windows, test))]
mod shell_encoding;

pub const LOCAL_FILES_KIND: &str = "ternilo.files.local";
pub const LOCAL_SHELL_KIND: &str = "ternilo.shell.local";

component_descriptor! {
    static FILES_DESCRIPTOR: () {
        id: "ternilo/local-files@1",
        requires: [RunEnvironment],
        provides: [WorkspaceFiles],
    }
}

component_descriptor! {
    static SHELL_DESCRIPTOR: () {
        id: "ternilo/local-shell@1",
        requires: [RunEnvironment, Sandboxes],
        provides: [Shell],
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FilesConfig {
    #[serde(default = "default_max_read_bytes")]
    max_read_bytes: u64,
    #[serde(default)]
    read_cache_bytes: usize,
}

const fn default_max_read_bytes() -> u64 {
    2 * 1024 * 1024
}

#[derive(Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShellConfig {}

#[must_use]
pub fn local_files_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: LOCAL_FILES_KIND,
            requires: &["ternilo/run-environment@1"],
            provides: &["ternilo/workspace-files@2"],
        },
        |value| {
            let config: FilesConfig = parse_config(value)?;
            if config.max_read_bytes == 0 {
                return Err(HarnessError::composition(
                    "local files max_read_bytes must be greater than zero",
                ));
            }
            Ok(Arc::new(LocalFilesPlugin { config }))
        },
    )
    .with_description("提供受工作区边界约束的本地文件 capability。")
    .with_config_schema::<FilesConfig>()
}

#[must_use]
pub fn local_shell_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: LOCAL_SHELL_KIND,
            requires: &["ternilo/run-environment@1", "ternilo/sandbox@1"],
            provides: &["ternilo/shell@1"],
        },
        |value| {
            let _: ShellConfig = parse_config(value)?;
            Ok(Arc::new(LocalShellPlugin))
        },
    )
    .with_description("提供清理环境并经可信 sandbox 执行的本地 shell capability。")
    .with_config_schema::<ShellConfig>()
}

fn parse_config<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    let value = if value.is_null() {
        serde_json::json!({})
    } else {
        value
    };
    serde_json::from_value(value)
        .map_err(|error| HarnessError::composition(format!("invalid plugin config: {error}")))
}

struct LocalFilesPlugin {
    config: FilesConfig,
}

impl HarnessPlugin for LocalFilesPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &FILES_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("local files declares RunEnvironment");
        let max_read_bytes = self.config.max_read_bytes;
        let read_cache_bytes = self.config.read_cache_bytes;
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            let workspace = environment.workspace().await.ok_or_else(|| {
                linorun_core::ActivationFailure::user(
                    "local files requires a workspace-bound session",
                )
            })?;
            let root = tokio::fs::canonicalize(&workspace.path)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "resolve workspace {}: {error}",
                        workspace.path
                    ))
                })?;
            let provider: Arc<dyn WorkspaceFilesProvider> = Arc::new(LocalFiles {
                root,
                environment,
                max_read_bytes,
                read_cache_bytes,
                read_cache: tokio::sync::Mutex::new(FileReadCache::default()),
            });
            scope
                .provide::<WorkspaceFiles>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide local workspace files: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct LocalFiles {
    root: PathBuf,
    environment: RunEnvironmentClient,
    max_read_bytes: u64,
    read_cache_bytes: usize,
    read_cache: tokio::sync::Mutex<FileReadCache>,
}

impl LocalFiles {
    async fn read_utf8_bounded(&self, path: &Path) -> Result<String, HarnessError> {
        let mut file = tokio::fs::File::open(path).await.map_err(|error| {
            HarnessError::execution(format!("open {}: {error}", path.display()))
        })?;
        let metadata = file.metadata().await.map_err(|error| {
            HarnessError::execution(format!("inspect {}: {error}", path.display()))
        })?;
        if !metadata.is_file() {
            return Err(HarnessError::invalid(format!(
                "path is not a file: {}",
                path.display()
            )));
        }
        if metadata.len() > self.max_read_bytes {
            return Err(HarnessError::policy(format!(
                "file is {} bytes; read limit is {} bytes",
                metadata.len(),
                self.max_read_bytes
            )));
        }
        let version = FileVersion::from_metadata(&metadata);
        if self.read_cache_bytes > 0
            && let Some(version) = &version
            && let Some(content) = self.read_cache.lock().await.get(path, version)
        {
            return Ok(content);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(self.max_read_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| {
                HarnessError::execution(format!("read {}: {error}", path.display()))
            })?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.max_read_bytes {
            return Err(HarnessError::policy(format!(
                "file grew beyond the {} byte read limit",
                self.max_read_bytes
            )));
        }
        let content = String::from_utf8(bytes).map_err(|error| {
            HarnessError::invalid(format!("read UTF-8 file {}: {error}", path.display()))
        })?;
        if self.read_cache_bytes > 0
            && let Some(version) = version
            && let Ok(after) = file.metadata().await
            && FileVersion::from_metadata(&after).as_ref() == Some(&version)
        {
            self.read_cache.lock().await.insert(
                path.to_path_buf(),
                version,
                content.clone(),
                self.read_cache_bytes,
            );
        }
        Ok(content)
    }

    async fn existing_path(&self, requested: &str) -> Result<PathBuf, HarnessError> {
        let candidate = self.candidate(requested)?;
        let canonical = tokio::fs::canonicalize(&candidate).await.map_err(|error| {
            HarnessError::invalid(format!("resolve {}: {error}", candidate.display()))
        })?;
        self.ensure_contained(&canonical)?;
        Ok(canonical)
    }

    async fn writable_path(
        &self,
        requested: &str,
        create_parents: bool,
    ) -> Result<PathBuf, HarnessError> {
        if !self
            .environment
            .permissions()
            .await
            .allows_workspace_write()
        {
            return Err(HarnessError::policy(
                "current permission preset does not allow workspace writes",
            ));
        }
        let candidate = self.candidate(requested)?;
        if tokio::fs::try_exists(&candidate).await.map_err(|error| {
            HarnessError::execution(format!("inspect {}: {error}", candidate.display()))
        })? {
            let canonical = self.existing_path(requested).await?;
            if tokio::fs::metadata(&canonical)
                .await
                .map_err(|error| {
                    HarnessError::execution(format!("inspect {}: {error}", canonical.display()))
                })?
                .is_dir()
            {
                return Err(HarnessError::invalid(format!(
                    "path is a directory: {}",
                    canonical.display()
                )));
            }
            return Ok(canonical);
        }
        let parent = candidate.parent().ok_or_else(|| {
            HarnessError::invalid(format!("path has no parent: {}", candidate.display()))
        })?;
        let canonical_parent = if create_parents {
            self.parent_for_creation(parent).await?
        } else {
            tokio::fs::canonicalize(parent).await.map_err(|error| {
                HarnessError::invalid(format!("resolve parent {}: {error}", parent.display()))
            })?
        };
        self.ensure_contained(&canonical_parent)?;
        let name = candidate.file_name().ok_or_else(|| {
            HarnessError::invalid(format!("path has no file name: {}", candidate.display()))
        })?;
        Ok(canonical_parent.join(name))
    }

    // Resolve the existing ancestor before creating anything; missing components stay literal.
    async fn parent_for_creation(&self, mut parent: &Path) -> Result<PathBuf, HarnessError> {
        let mut missing = Vec::new();
        loop {
            match tokio::fs::canonicalize(parent).await {
                Ok(mut resolved) => {
                    self.ensure_contained(&resolved)?;
                    for name in missing.into_iter().rev() {
                        resolved.push(name);
                    }
                    return Ok(resolved);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let name = parent.file_name().ok_or_else(|| HarnessError::invalid("a new parent directory must have a literal name, without unresolved parent traversal"))?;
                    missing.push(name.to_owned());
                    parent = parent
                        .parent()
                        .ok_or_else(|| HarnessError::invalid("file path has no existing parent"))?;
                }
                Err(error) => {
                    return Err(HarnessError::invalid(format!(
                        "resolve parent {}: {error}",
                        parent.display()
                    )));
                }
            }
        }
    }

    fn candidate(&self, requested: &str) -> Result<PathBuf, HarnessError> {
        if requested.trim().is_empty() {
            return Err(HarnessError::invalid("file path must not be empty"));
        }
        let path = Path::new(requested);
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        })
    }

    fn ensure_contained(&self, path: &Path) -> Result<(), HarnessError> {
        if path.starts_with(&self.root) {
            Ok(())
        } else {
            Err(HarnessError::policy(format!(
                "path escapes workspace {}: {}",
                self.root.display(),
                path.display()
            )))
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map_or_else(|| ".".to_owned(), |relative| relative.display().to_string())
    }
}

async fn write_owned(
    path: PathBuf,
    content: String,
    lease: WorkspaceExecutionLease,
    create_parents_within: Option<PathBuf>,
) -> Result<(), HarnessError> {
    // A cancelled caller cannot release admission while the blocking file write is running.
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
        if let Some(root) = create_parents_within {
            let parent = path
                .parent()
                .ok_or_else(|| HarnessError::invalid("file path has no parent"))?;
            std::fs::create_dir_all(parent).map_err(|error| {
                HarnessError::execution(format!("create parent {}: {error}", parent.display()))
            })?;
            let parent = std::fs::canonicalize(parent).map_err(|error| {
                HarnessError::execution(format!("resolve created parent: {error}"))
            })?;
            if !parent.starts_with(root) {
                return Err(HarnessError::policy("created parent escapes the workspace"));
            }
        }
        std::fs::write(&path, content)
            .map_err(|error| HarnessError::execution(format!("write {}: {error}", path.display())))
    })
    .await
    .map_err(|error| HarnessError::execution(format!("join file write: {error}")))?
}

impl WorkspaceFilesProvider for LocalFiles {
    fn read_text<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileReadRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileContent, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let path = self.existing_path(&request.path).await?;
            let content = self.read_utf8_bounded(&path).await?;
            let lines = content.lines().collect::<Vec<_>>();
            let total_lines = u64::try_from(lines.len())
                .map_err(|_| HarnessError::execution("line count exceeds u64"))?;
            let start = request.start_line.unwrap_or(1).max(1);
            let count = request.line_count.unwrap_or(400).clamp(1, 2_000);
            let start_index = usize::try_from(start.saturating_sub(1))
                .unwrap_or(usize::MAX)
                .min(lines.len());
            let count = usize::try_from(count).unwrap_or(2_000);
            let end_index = start_index.saturating_add(count).min(lines.len());
            let selected = lines[start_index..end_index].join("\n");
            Ok(FileContent {
                path: self.display_path(&path),
                content: selected,
                start_line: u64::try_from(start_index).unwrap_or(u64::MAX) + 1,
                end_line: u64::try_from(end_index).unwrap_or(u64::MAX),
                total_lines,
            })
        })
    }

    fn write_text<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileWriteRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileWriteResult, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let lease = self
                .environment
                .acquire_workspace(RunCancellation::new())
                .await?;
            let path = self.writable_path(&request.path, true).await?;
            self.read_cache.lock().await.invalidate(&path);
            write_owned(
                path.clone(),
                request.content.clone(),
                lease,
                Some(self.root.clone()),
            )
            .await?;
            Ok(FileWriteResult {
                path: self.display_path(&path),
                bytes_written: u64::try_from(request.content.len())
                    .map_err(|_| HarnessError::execution("written content exceeds u64"))?,
            })
        })
    }

    fn replace_text<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileReplaceRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileReplaceResult, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if request.old.is_empty() {
                return Err(HarnessError::invalid(
                    "replacement old text must not be empty",
                ));
            }
            let lease = self
                .environment
                .acquire_workspace(RunCancellation::new())
                .await?;
            let path = self.writable_path(&request.path, false).await?;
            let content = self.read_utf8_bounded(&path).await?;
            let matches = content.matches(&request.old).count();
            if matches == 0 {
                return Err(HarnessError::invalid("old text was not found in the file"));
            }
            if !request.replace_all && matches != 1 {
                return Err(HarnessError::invalid(format!(
                    "old text occurs {matches} times; make it unique or set replace_all"
                )));
            }
            let updated = if request.replace_all {
                content.replace(&request.old, &request.new)
            } else {
                content.replacen(&request.old, &request.new, 1)
            };
            self.read_cache.lock().await.invalidate(&path);
            write_owned(path.clone(), updated, lease, None).await?;
            Ok(FileReplaceResult {
                path: self.display_path(&path),
                replacements: u64::try_from(matches)
                    .map_err(|_| HarnessError::execution("replacement count exceeds u64"))?,
            })
        })
    }

    fn list_files<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileListRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileListResult, HarnessError>> + Send + 'a>> {
        Box::pin(search::list_files(self.root.clone(), request))
    }

    fn search_text<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileSearchResult, HarnessError>> + Send + 'a>> {
        Box::pin(search::search_text(self.root.clone(), request))
    }
}

struct LocalShellPlugin;

impl HarnessPlugin for LocalShellPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SHELL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("local shell declares RunEnvironment");
        let sandbox = context
            .context()
            .service::<Sandboxes>()
            .expect("local shell declares Sandboxes");
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            let workspace = environment.workspace().await.ok_or_else(|| {
                linorun_core::ActivationFailure::user(
                    "local shell requires a workspace-bound session",
                )
            })?;
            let root = tokio::fs::canonicalize(&workspace.path)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "resolve workspace {}: {error}",
                        workspace.path
                    ))
                })?;
            let provider: Arc<dyn ShellProvider> = Arc::new(LocalShell {
                root,
                environment,
                sandbox,
            });
            scope
                .provide::<Shell>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide local shell: {error}"))
                })?;
            Ok(None)
        }))
    }
}

struct LocalShell {
    root: PathBuf,
    environment: RunEnvironmentClient,
    sandbox: SandboxesClient,
}

impl ShellProvider for LocalShell {
    fn execute<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: ternilo_protocol::RunId,
        request: ShellRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ShellResult, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if request.command.trim().is_empty() {
                return Err(HarnessError::invalid("shell command must not be empty"));
            }
            self.environment
                .check_run_authorization(run_id.clone())
                .await?;
            let timeout_ms = request.timeout_ms.clamp(100, 600_000);
            let permissions = self.environment.permissions().await;
            let direct = request.full_access || permissions.allows_full_access();
            let (program, arguments) = native_script_command(&request.command);
            let mut command = if direct {
                command_from_program(program, arguments, &self.root)
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
            for (name, _) in std::env::vars_os() {
                if sensitive_environment_name(&name) {
                    command.env_remove(name);
                }
            }
            for (name, value) in &request.env {
                if name.is_empty()
                    || name.contains('=')
                    || name.contains('\0')
                    || value.contains('\0')
                {
                    return Err(HarnessError::invalid(
                        "shell environment contains an invalid name or value",
                    ));
                }
            }
            command.envs(&request.env);
            let lease = self
                .environment
                .acquire_workspace(RunCancellation::new())
                .await?;
            match capture_command_owned(
                &mut command,
                timeout_ms,
                MAX_SHELL_CAPTURE_BYTES,
                request.stdin.as_deref(),
                Some(lease),
                Some((self.environment.clone(), run_id)),
            )
            .await?
            {
                CaptureResult::Completed(output) => {
                    let (stdout, stderr) = output.render().await;
                    Ok(ShellResult {
                        exit_code: output.status.code(),
                        stdout,
                        stderr,
                        timed_out: false,
                    })
                }
                CaptureResult::TimedOut => Ok(ShellResult {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("command timed out after {timeout_ms} ms"),
                    timed_out: true,
                }),
            }
        })
    }
}

fn command_from_program(program: OsString, arguments: Vec<OsString>, root: &Path) -> Command {
    let mut command = Command::new(program);
    command.args(arguments).current_dir(root);
    command
}

#[cfg(unix)]
pub(super) fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
    (
        OsString::from("bash"),
        vec![OsString::from("-lc"), OsString::from(script)],
    )
}

#[cfg(windows)]
pub(super) fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
    (
        OsString::from("powershell.exe"),
        vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
        ],
    )
}

#[cfg(not(any(unix, windows)))]
pub(super) fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
    (
        OsString::from("sh"),
        vec![OsString::from("-c"), OsString::from(script)],
    )
}

fn sensitive_environment_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("TERNILO_")
        || ["KEY", "SECRET", "TOKEN", "PASSWORD", "CREDENTIAL"]
            .iter()
            .any(|marker| name.contains(marker))
}

const MAX_SHELL_CAPTURE_BYTES: usize = 256 * 1024;

pub(super) struct CapturedOutput {
    pub(super) status: ExitStatus,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

impl CapturedOutput {
    #[cfg(windows)]
    pub(super) async fn render(&self) -> (String, String) {
        shell_encoding::render_output(self).await
    }

    #[cfg(not(windows))]
    pub(super) fn render(&self) -> std::future::Ready<(String, String)> {
        std::future::ready((self.stdout.render(), self.stderr.render()))
    }
}

pub(super) enum CaptureResult {
    Completed(CapturedOutput),
    TimedOut,
}

pub(super) struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedStream {
    pub(super) fn render(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        if self.truncated {
            format!("{text}\n… output truncated …")
        } else {
            text.into_owned()
        }
    }
}

pub(super) async fn capture_command(
    command: &mut Command,
    timeout_ms: u64,
    max_capture_bytes: usize,
    stdin: Option<&str>,
) -> Result<CaptureResult, HarnessError> {
    capture_command_owned(command, timeout_ms, max_capture_bytes, stdin, None, None).await
}

async fn capture_command_owned(
    command: &mut Command,
    timeout_ms: u64,
    max_capture_bytes: usize,
    stdin: Option<&str>,
    lease: Option<WorkspaceExecutionLease>,
    ownership: Option<(RunEnvironmentClient, ternilo_protocol::RunId)>,
) -> Result<CaptureResult, HarnessError> {
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let (completed, completion) = tokio::sync::watch::channel(None);
    let held_lease = Arc::new(std::sync::Mutex::new(lease));
    let control = Arc::new(CaptureControl {
        stop: stop.clone(),
        completion,
        _lease: held_lease.clone(),
    });
    let _stop_on_drop = CaptureStopOnDrop(stop);
    let mut admission = CaptureAdmission(Some(completed.clone()));
    if let Some((environment, run_id)) = ownership {
        environment.check_run_authorization(run_id.clone()).await?;
        let id = format!("process-{:032x}", rand::random::<u128>());
        environment
            .register_execution_resource(run_id, id, control)
            .await?;
    }
    if *stopped.borrow() {
        return Err(HarnessError::cancelled(
            "command was revoked before process creation",
        ));
    }
    command
        .kill_on_drop(true)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, process_group) = crate::process_group::spawn(command)
        .map_err(|error| HarnessError::execution(format!("start command: {error}")))?;
    admission.0 = None;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| HarnessError::execution("command stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| HarnessError::execution("command stderr is unavailable"))?;
    let mut child_stdin = child.stdin.take();
    let stdin = stdin.map(str::to_owned);
    // The independent supervisor retains ownership while cancellation reaps the process.
    let supervisor = tokio::spawn(async move {
        let capture = async {
            let write_stdin = async {
                if let (Some(mut child_stdin), Some(stdin)) = (child_stdin.take(), stdin) {
                    child_stdin.write_all(stdin.as_bytes()).await?;
                    child_stdin.shutdown().await?;
                }
                Ok::<(), io::Error>(())
            };
            let wait = async {
                let status = child.wait().await;
                process_group.terminate();
                status
            };
            let ((), status, stdout, stderr) = tokio::try_join!(
                write_stdin,
                wait,
                capture_stream(stdout, max_capture_bytes),
                capture_stream(stderr, max_capture_bytes)
            )?;
            Ok::<_, io::Error>(CapturedOutput {
                status,
                stdout,
                stderr,
            })
        };
        let outcome = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), capture) => match result {
                Ok(Ok(output)) => Ok(CaptureResult::Completed(output)),
                Ok(Err(error)) => Err(HarnessError::execution(format!("capture command output: {error}"))),
                Err(_) => Ok(CaptureResult::TimedOut),
            },
            _ = stopped.changed() => Err(HarnessError::cancelled("command capture was cancelled")),
        };
        process_group.terminate();
        let _ = child.start_kill();
        let _ = child.wait().await;
        let cleanup = process_group.wait_quiescent().await;
        if cleanup.is_ok() {
            held_lease.lock().expect("process lease lock").take();
        }
        completed.send_replace(Some(cleanup.map_err(|error| error.to_string())));
        outcome
    });
    admission.0 = None;
    supervisor
        .await
        .map_err(|error| HarnessError::execution(format!("supervise command: {error}")))?
}

struct CaptureStopOnDrop(tokio::sync::watch::Sender<bool>);
impl Drop for CaptureStopOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}
struct CaptureAdmission(Option<tokio::sync::watch::Sender<Option<Result<(), String>>>>);
impl Drop for CaptureAdmission {
    fn drop(&mut self) {
        if let Some(completion) = self.0.take() {
            completion.send_replace(Some(Ok(())));
        }
    }
}
struct CaptureControl {
    _lease: Arc<std::sync::Mutex<Option<WorkspaceExecutionLease>>>,
    stop: tokio::sync::watch::Sender<bool>,
    completion: tokio::sync::watch::Receiver<Option<Result<(), String>>>,
}
impl ternilo_kernel::ExecutionResourceControl for CaptureControl {
    fn stop<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.stop.send_replace(true);
            let mut completion = self.completion.clone();
            loop {
                if let Some(result) = completion.borrow().clone() {
                    return result.map_err(HarnessError::execution);
                }
                completion.changed().await.map_err(|_| {
                    HarnessError::unavailable("process supervisor ended without a cleanup receipt")
                })?;
            }
        })
    }
    fn is_finished(&self) -> bool {
        matches!(&*self.completion.borrow(), Some(Ok(())))
    }
}

async fn capture_stream<R>(mut stream: R, max_capture_bytes: usize) -> io::Result<CapturedStream>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = vec![0_u8; 8192];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let retained = read.min(max_capture_bytes.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;
    }
    Ok(CapturedStream { bytes, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn command_capture_drains_but_does_not_retain_unbounded_output() {
        let mut command = Command::new("bash");
        command.args(["-c", "head -c 300000 /dev/zero"]);
        let CaptureResult::Completed(output) =
            capture_command(&mut command, 5_000, MAX_SHELL_CAPTURE_BYTES, None)
                .await
                .unwrap()
        else {
            panic!("bounded command unexpectedly timed out");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes.len(), MAX_SHELL_CAPTURE_BYTES);
        assert!(output.stdout.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_timeout_returns_after_killing_its_process_group() {
        let mut command = Command::new("bash");
        command.args(["-c", "sleep 5 & wait"]);
        let started = tokio::time::Instant::now();
        let result = capture_command(&mut command, 50, MAX_SHELL_CAPTURE_BYTES, None)
            .await
            .unwrap();
        assert!(matches!(result, CaptureResult::TimedOut));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_capture_writes_stdin_and_closes_the_pipe() {
        let mut command = Command::new("bash");
        command.args(["-c", "payload=$(cat); printf 'received:%s' \"$payload\""]);
        let CaptureResult::Completed(output) = capture_command(
            &mut command,
            5_000,
            MAX_SHELL_CAPTURE_BYTES,
            Some("hook-input"),
        )
        .await
        .unwrap() else {
            panic!("stdin command unexpectedly timed out");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout.render(), "received:hook-input");
    }

    #[test]
    fn shell_scrubs_ambient_credentials_but_keeps_ordinary_variables() {
        assert!(sensitive_environment_name(std::ffi::OsStr::new(
            "DEEPSEEK_API_KEY"
        )));
        assert!(sensitive_environment_name(std::ffi::OsStr::new(
            "TERNILO_INTERNAL"
        )));
        assert!(!sensitive_environment_name(std::ffi::OsStr::new("PATH")));
    }
}

#[cfg(all(test, unix))]
#[path = "process_lifecycle_tests.rs"]
mod process_lifecycle_tests;
