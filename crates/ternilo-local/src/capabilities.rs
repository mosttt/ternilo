use std::{
    ffi::OsString,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::ExitStatus,
    sync::Arc,
};

use base64::Engine as _;

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
    FileReplaceResult, FileSearchMatch, FileSearchRequest, FileSearchResult, FileWriteRequest,
    FileWriteResult, HarnessError, ShellRequest, ShellResult,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

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
}

impl LocalFiles {
    async fn read_utf8_bounded(&self, path: &Path) -> Result<String, HarnessError> {
        let metadata = tokio::fs::metadata(path).await.map_err(|error| {
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
        let file = tokio::fs::File::open(path).await.map_err(|error| {
            HarnessError::execution(format!("open {}: {error}", path.display()))
        })?;
        let mut bytes = Vec::new();
        file.take(self.max_read_bytes.saturating_add(1))
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
        String::from_utf8(bytes).map_err(|error| {
            HarnessError::invalid(format!("read UTF-8 file {}: {error}", path.display()))
        })
    }

    async fn existing_path(&self, requested: &str) -> Result<PathBuf, HarnessError> {
        let candidate = self.candidate(requested)?;
        let canonical = tokio::fs::canonicalize(&candidate).await.map_err(|error| {
            HarnessError::invalid(format!("resolve {}: {error}", candidate.display()))
        })?;
        self.ensure_contained(&canonical)?;
        Ok(canonical)
    }

    async fn writable_path(&self, requested: &str) -> Result<PathBuf, HarnessError> {
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
        let canonical_parent = tokio::fs::canonicalize(parent).await.map_err(|error| {
            HarnessError::invalid(format!("resolve parent {}: {error}", parent.display()))
        })?;
        self.ensure_contained(&canonical_parent)?;
        let name = candidate.file_name().ok_or_else(|| {
            HarnessError::invalid(format!("path has no file name: {}", candidate.display()))
        })?;
        Ok(canonical_parent.join(name))
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
) -> Result<(), HarnessError> {
    // A cancelled caller cannot release admission while the blocking file write is running.
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
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
            let path = self.writable_path(&request.path).await?;
            write_owned(path.clone(), request.content.clone(), lease).await?;
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
            let path = self.writable_path(&request.path).await?;
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
            write_owned(path.clone(), updated, lease).await?;
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
        Box::pin(async move {
            if request.pattern.trim().is_empty() {
                return Err(HarnessError::invalid("file glob must not be empty"));
            }
            let limit = usize::try_from(request.limit.clamp(1, 2_000)).unwrap_or(2_000);
            let output = run_bounded(
                Command::new("rg")
                    .env("LC_ALL", "C")
                    .arg("--files")
                    .arg("--null")
                    .arg("--hidden")
                    .arg("--glob")
                    .arg(&request.pattern)
                    .arg(".")
                    .current_dir(&self.root),
                10_000,
            )
            .await?;
            let warnings = search_warnings("rg --files", &output)?;
            let mut files = complete_search_output(&output.stdout, b'\0')
                .split('\0')
                .filter(|path| !path.is_empty())
                .take(limit + 1)
                .map(|line| line.strip_prefix("./").unwrap_or(line).to_owned())
                .collect::<Vec<_>>();
            let truncated = output.stdout.truncated || files.len() > limit;
            files.truncate(limit);
            Ok(FileListResult {
                files,
                warnings,
                truncated,
            })
        })
    }

    fn search_text<'a>(
        &'a self,
        _: CallContext<()>,
        request: FileSearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FileSearchResult, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if request.pattern.trim().is_empty() {
                return Err(HarnessError::invalid("search pattern must not be empty"));
            }
            let limit = usize::try_from(request.limit.clamp(1, 1_000)).unwrap_or(1_000);
            let mut command = Command::new("rg");
            command.env("LC_ALL", "C").arg("--json").arg("--hidden");
            if let Some(file_glob) = request.file_glob.as_deref() {
                command.arg("--glob").arg(file_glob);
            }
            command
                .arg("--")
                .arg(&request.pattern)
                .arg(".")
                .current_dir(&self.root);
            let output = run_bounded(&mut command, 10_000).await?;
            let warnings = search_warnings("rg search", &output)?;
            let mut matches = complete_search_output(&output.stdout, b'\n')
                .lines()
                .filter_map(|line| parse_search_match(line).transpose())
                .take(limit + 1)
                .collect::<Result<Vec<_>, _>>()?;
            let truncated = output.stdout.truncated || matches.len() > limit;
            matches.truncate(limit);
            Ok(FileSearchResult {
                matches,
                warnings,
                truncated,
            })
        })
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
                CaptureResult::Completed(output) => Ok(ShellResult {
                    exit_code: output.status.code(),
                    stdout: output.stdout.render(),
                    stderr: output.stderr.render(),
                    timed_out: false,
                }),
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
fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
    (
        OsString::from("bash"),
        vec![OsString::from("-lc"), OsString::from(script)],
    )
}

#[cfg(windows)]
fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
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
fn native_script_command(script: &str) -> (OsString, Vec<OsString>) {
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

async fn run_bounded(
    command: &mut Command,
    timeout_ms: u64,
) -> Result<CapturedOutput, HarnessError> {
    match capture_command(command, timeout_ms, MAX_INTERNAL_CAPTURE_BYTES, None).await? {
        CaptureResult::Completed(output) => Ok(output),
        CaptureResult::TimedOut => Err(HarnessError::execution(format!(
            "command timed out after {timeout_ms} ms"
        ))),
    }
}

fn command_failure(label: &str, output: &CapturedOutput) -> HarnessError {
    HarnessError::execution(format!(
        "{label} exited with {:?}: {}",
        output.status.code(),
        output.stderr.render()
    ))
}

fn search_warnings(label: &str, output: &CapturedOutput) -> Result<Vec<String>, HarnessError> {
    let diagnostics = String::from_utf8_lossy(&output.stderr.bytes);
    if diagnostics.lines().any(|line| {
        line.starts_with("rg: regex parse error:") || line.starts_with("rg: error parsing glob ")
    }) {
        return Err(command_failure(label, output));
    }
    let permission_only = !diagnostics.trim().is_empty()
        && diagnostics
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| {
                line.trim_end()
                    .ends_with(": Permission denied (os error 13)")
                    || line
                        .trim_end()
                        .ends_with(": Access is denied. (os error 5)")
            });
    match output.status.code() {
        Some(0 | 1) => {}
        Some(2) if !output.stdout.bytes.is_empty() || permission_only => {}
        _ => return Err(command_failure(label, output)),
    }
    let mut warnings = diagnostics
        .lines()
        .take(20)
        .map(|line| line.chars().take(1_000).collect::<String>())
        .collect::<Vec<_>>();
    if output.stderr.truncated || diagnostics.lines().count() > 20 {
        warnings.push("Additional search diagnostics were omitted.".to_owned());
    }
    Ok(warnings)
}

fn complete_search_output(stream: &CapturedStream, delimiter: u8) -> String {
    let bytes = if stream.truncated {
        // A capture limit can cut a path or a match in half. Keep only complete records.
        stream
            .bytes
            .iter()
            .rposition(|byte| *byte == delimiter)
            .map_or(&[][..], |end| &stream.bytes[..=end])
    } else {
        &stream.bytes
    };
    String::from_utf8_lossy(bytes).into_owned()
}

const MAX_SHELL_CAPTURE_BYTES: usize = 256 * 1024;
const MAX_INTERNAL_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

struct CapturedOutput {
    status: ExitStatus,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

enum CaptureResult {
    Completed(CapturedOutput),
    TimedOut,
}

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedStream {
    fn render(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        if self.truncated {
            format!("{text}\n… output truncated …")
        } else {
            text.into_owned()
        }
    }
}

async fn capture_command(
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

fn parse_search_match(line: &str) -> Result<Option<FileSearchMatch>, HarnessError> {
    let record: Value = serde_json::from_str(line)
        .map_err(|error| HarnessError::execution(format!("invalid rg JSON: {error}")))?;
    if record["type"] != "match" {
        return Ok(None);
    }
    let data = &record["data"];
    let path = search_record_text(&data["path"])?;
    let preview = search_record_text(&data["lines"])?;
    Ok(Some(FileSearchMatch {
        path: path.strip_prefix("./").unwrap_or(&path).to_owned(),
        line: data["line_number"]
            .as_u64()
            .ok_or_else(|| HarnessError::execution("rg returned a malformed line number"))?,
        column: data["submatches"][0]["start"]
            .as_u64()
            .and_then(|start| start.checked_add(1))
            .ok_or_else(|| HarnessError::execution("rg returned a malformed column"))?,
        preview: preview.trim_end_matches(['\r', '\n']).to_owned(),
    }))
}

fn search_record_text(value: &Value) -> Result<String, HarnessError> {
    if let Some(text) = value["text"].as_str() {
        return Ok(text.to_owned());
    }
    let encoded = value["bytes"]
        .as_str()
        .ok_or_else(|| HarnessError::execution("rg returned a malformed text field"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| HarnessError::execution(format!("invalid rg encoded text: {error}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn partial_search_keeps_permission_warnings_but_rejects_invalid_queries() {
        use std::os::unix::process::ExitStatusExt;

        let output = |stdout: &str, stderr: &str| CapturedOutput {
            status: ExitStatus::from_raw(2 << 8),
            stdout: CapturedStream {
                bytes: stdout.as_bytes().to_vec(),
                truncated: false,
            },
            stderr: CapturedStream {
                bytes: stderr.as_bytes().to_vec(),
                truncated: false,
            },
        };
        let denied = "rg: ./private: Permission denied (os error 13)\n";
        let record = serde_json::json!({"type": "match", "data": {
            "path": {"text": "./readable:notes.txt"}, "lines": {"text": "needle\n"},
            "line_number": 1, "submatches": [{"start": 0}]
        }})
        .to_string();
        let partial = output(&format!("{record}\n"), denied);
        assert_eq!(
            search_warnings("rg search", &partial).unwrap(),
            vec![denied.trim()]
        );
        let matched = parse_search_match(complete_search_output(&partial.stdout, b'\n').trim())
            .unwrap()
            .unwrap();
        assert_eq!(matched.path, "readable:notes.txt");
        assert_eq!(matched.preview, "needle");
        assert!(
            !search_warnings("rg search", &output("", denied))
                .unwrap()
                .is_empty()
        );
        assert!(
            search_warnings(
                "rg search",
                &output("", "regex parse error: unclosed character class")
            )
            .is_err()
        );

        assert!(search_warnings("rg --files", &output("", "rg: error parsing glob '{Permission denied (os error 13)': unclosed alternate group; missing '}'")).is_err());
        let truncated = CapturedStream {
            bytes: b"./kept.txt\n./incomplete".to_vec(),
            truncated: true,
        };
        assert_eq!(complete_search_output(&truncated, b'\n'), "./kept.txt\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ripgrep_returns_readable_files_when_a_directory_is_inaccessible() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("readable.txt"), "needle\n").unwrap();
        std::fs::write(directory.path().join("colon:line\nname.txt"), "needle\n").unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::write(private.join("hidden.txt"), "needle\n").unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o0)).unwrap();
        let denies_reading = std::fs::read_dir(&private).is_err();
        for (arguments, delimiter) in [
            (vec!["--files", "--null", "--hidden", "."], b'\0'),
            (vec!["--json", "--hidden", "--", "needle", "."], b'\n'),
        ] {
            let result = run_bounded(
                Command::new("rg")
                    .env("LC_ALL", "C")
                    .args(arguments)
                    .current_dir(directory.path()),
                10_000,
            )
            .await;
            let output = result.unwrap();
            let warnings = search_warnings("rg fixture", &output).unwrap();
            let captured = complete_search_output(&output.stdout, delimiter);
            assert!(captured.contains("readable.txt"));
            if delimiter == b'\n' {
                let matches = captured
                    .lines()
                    .filter_map(|line| parse_search_match(line).transpose())
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert!(
                    matches
                        .iter()
                        .any(|entry| entry.path == "colon:line\nname.txt")
                );
            } else {
                assert!(
                    captured
                        .split('\0')
                        .any(|path| path == "./colon:line\nname.txt")
                );
            }
            assert_eq!(!warnings.is_empty(), denies_reading);
        }
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

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
