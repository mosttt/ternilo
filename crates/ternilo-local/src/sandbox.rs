#[cfg(any(target_os = "linux", test))]
use std::ffi::OsStr;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    ConfinedCommand, HarnessPlugin, PluginFactory, PluginManifest, SandboxEnforcement, SandboxMode,
    SandboxPolicy, Sandboxes, SandboxesProvider,
};
use ternilo_protocol::{HarnessError, Profile, SandboxReadiness, SandboxReadinessStatus};
use tokio::process::Command;

pub const LOCAL_SANDBOX_KIND: &str = "ternilo.sandbox.local";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/local-sandbox@1",
        requires: [],
        provides: [Sandboxes],
    }
}

#[derive(Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LocalSandboxConfig {
    #[serde(default)]
    windows_runner: Option<PathBuf>,
}

#[must_use]
pub fn local_sandbox_factory() -> PluginFactory {
    PluginFactory::new(
        PluginManifest {
            kind: LOCAL_SANDBOX_KIND,
            requires: &[],
            provides: &["ternilo/sandbox@1"],
        },
        |value| {
            let config: LocalSandboxConfig = serde_json::from_value(if value.is_null() {
                serde_json::json!({})
            } else {
                value
            })
            .map_err(|error| {
                HarnessError::composition(format!("invalid local sandbox config: {error}"))
            })?;
            Ok(Arc::new(LocalSandboxPlugin { config }))
        },
    )
    .with_description("按平台提供 bubblewrap、Seatbelt 或 Windows 受限进程 sandbox。")
    .with_config_schema::<LocalSandboxConfig>()
}

struct LocalSandboxPlugin {
    config: LocalSandboxConfig,
}

impl HarnessPlugin for LocalSandboxPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        #[cfg(not(target_os = "windows"))]
        let _ = &self.config.windows_runner;
        let provider: Arc<dyn SandboxesProvider> = Arc::new(LocalSandbox {
            #[cfg(target_os = "windows")]
            windows_runner: self.config.windows_runner.clone(),
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Sandboxes>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide local sandbox: {error}"))
                })?;
            Ok(None)
        }))
    }
}

struct LocalSandbox {
    #[cfg(target_os = "windows")]
    windows_runner: Option<PathBuf>,
}

impl SandboxesProvider for LocalSandbox {
    fn confine<'a>(
        &'a self,
        _: CallContext<()>,
        program: OsString,
        arguments: Vec<OsString>,
        policy: SandboxPolicy,
    ) -> Pin<Box<dyn Future<Output = Result<ConfinedCommand, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if !policy.workspace_root.is_absolute() {
                return Err(HarnessError::invalid(
                    "sandbox workspace root must be absolute",
                ));
            }
            if program.is_empty() {
                return Err(HarnessError::invalid(
                    "sandbox command program must not be empty",
                ));
            }
            #[cfg(target_os = "windows")]
            let command = self.platform_command(program, arguments, &policy);
            #[cfg(not(target_os = "windows"))]
            let command = Self::platform_command(program, arguments, &policy);
            command
        })
    }
}

impl LocalSandbox {
    #[cfg(target_os = "linux")]
    fn platform_command(
        program: OsString,
        arguments: Vec<OsString>,
        policy: &SandboxPolicy,
    ) -> Result<ConfinedCommand, HarnessError> {
        let runner = resolve_program(OsStr::new("bwrap"))
            .or_else(|| {
                let path = PathBuf::from("/usr/sbin/bwrap");
                path.is_file().then_some(path)
            })
            .ok_or_else(|| unavailable(policy.mode, "bubblewrap is not installed"))?;
        Ok(bwrap_command(runner, program, arguments, policy))
    }

    #[cfg(target_os = "macos")]
    fn platform_command(
        program: OsString,
        arguments: Vec<OsString>,
        policy: &SandboxPolicy,
    ) -> Result<ConfinedCommand, HarnessError> {
        let runner = PathBuf::from("/usr/bin/sandbox-exec");
        if !runner.is_file() {
            return Err(unavailable(policy.mode, "sandbox-exec is not installed"));
        }
        let user_temp = darwin_user_temp_directory()?;
        Ok(seatbelt_command(
            runner, program, arguments, policy, &user_temp,
        ))
    }

    #[cfg(target_os = "windows")]
    fn platform_command(
        &self,
        program: OsString,
        arguments: Vec<OsString>,
        policy: &SandboxPolicy,
    ) -> Result<ConfinedCommand, HarnessError> {
        let runner = self
            .windows_runner
            .clone()
            .or_else(sibling_windows_runner)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                unavailable(
                    policy.mode,
                    "ternilo-sandbox-windows.exe is not installed beside the Ternilo executable",
                )
            })?;
        Ok(windows_command(
            runner,
            windows_sandbox_state_root(),
            program,
            arguments,
            policy,
        ))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn platform_command(
        _: OsString,
        _: Vec<OsString>,
        policy: &SandboxPolicy,
    ) -> Result<ConfinedCommand, HarnessError> {
        Err(unavailable(
            policy.mode,
            "this operating system has no Ternilo sandbox backend",
        ))
    }
}

#[cfg(any(target_os = "linux", test))]
fn bwrap_command(
    runner: PathBuf,
    program: OsString,
    arguments: Vec<OsString>,
    policy: &SandboxPolicy,
) -> ConfinedCommand {
    let mut args = os_args([
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
    ]);
    args.extend(os_args(["--tmpfs", "/tmp"]));
    if let Some(scratch) = &policy.scratch_root {
        args.push("--bind".into());
        args.push(scratch.as_os_str().to_owned());
        args.push(scratch.as_os_str().to_owned());
    }
    args.push(
        if policy.mode == SandboxMode::WorkspaceWrite {
            "--bind"
        } else {
            "--ro-bind"
        }
        .into(),
    );
    // Rebind after the private /tmp mount, including workspaces located below /tmp.
    args.push(policy.workspace_root.as_os_str().to_owned());
    args.push(policy.workspace_root.as_os_str().to_owned());
    args.extend(os_args(["--chdir"]));
    args.push(policy.workspace_root.as_os_str().to_owned());
    args.push(OsString::from("--"));
    args.push(program);
    args.extend(arguments);
    ConfinedCommand {
        program: runner.into_os_string(),
        arguments: args,
        environment: scratch_environment(policy),
        backend: "bubblewrap".to_owned(),
        enforcement: SandboxEnforcement::Full,
    }
}

#[cfg(any(target_os = "macos", test))]
fn seatbelt_command(
    runner: PathBuf,
    program: OsString,
    arguments: Vec<OsString>,
    policy: &SandboxPolicy,
    user_temp: &Path,
) -> ConfinedCommand {
    let mut writable = vec![PathBuf::from("/dev/null")];
    if policy.mode == SandboxMode::WorkspaceWrite {
        writable.push(policy.workspace_root.clone());
        writable.push(PathBuf::from("/private/tmp"));
        if let Some(temp) = std::env::var_os("TMPDIR").map(PathBuf::from) {
            writable.push(temp);
        }
    }
    if let Some(scratch) = &policy.scratch_root {
        writable.push(scratch.clone());
    }
    // Seatbelt compares canonical paths; /var and /tmp are aliases on macOS.
    for path in &mut writable {
        if let Ok(canonical) = std::fs::canonicalize(&*path) {
            *path = canonical;
        }
    }
    writable.sort();
    writable.dedup();
    let mut profile = String::from("(version 1) (allow default) (deny file-write*)");
    for path in writable {
        profile.push_str(" (allow file-write* (");
        profile.push_str(if path == Path::new("/dev/null") {
            "literal "
        } else {
            "subpath "
        });
        profile.push_str(&seatbelt_string(&path.to_string_lossy()));
        profile.push_str("))");
    }
    // Apple's executable shims cache lookups here, ignoring TMPDIR and xcrun_nocache.
    // Permit only their cache files, not the user-wide temporary directory.
    profile.push_str(" (allow file-write* (regex #");
    profile.push_str(&seatbelt_string(&xcrun_cache_pattern(user_temp)));
    profile.push_str("))");
    let mut args = vec![
        OsString::from("-p"),
        OsString::from(profile),
        OsString::from("--"),
    ];
    args.push(program);
    args.extend(arguments);
    ConfinedCommand {
        program: runner.into_os_string(),
        arguments: args,
        environment: scratch_environment(policy),
        backend: "seatbelt".to_owned(),
        enforcement: SandboxEnforcement::Full,
    }
}

#[cfg(any(target_os = "windows", test))]
fn windows_command(
    runner: PathBuf,
    state_root: PathBuf,
    program: OsString,
    arguments: Vec<OsString>,
    policy: &SandboxPolicy,
) -> ConfinedCommand {
    let mut args = os_args(["--workspace"]);
    args.push(policy.workspace_root.as_os_str().to_owned());
    args.extend(os_args(["--mode"]));
    args.push(OsString::from(match policy.mode {
        SandboxMode::ReadOnly => "read-only",
        SandboxMode::WorkspaceWrite => "workspace-write",
    }));
    if let Some(scratch) = &policy.scratch_root {
        args.push("--scratch".into());
        args.push(scratch.as_os_str().to_owned());
    }
    args.push(OsString::from("--"));
    args.push(program);
    args.extend(arguments);
    let mut environment = scratch_environment(policy);
    environment.insert("ZAGENS_HOME".into(), state_root.into_os_string());
    ConfinedCommand {
        program: runner.into_os_string(),
        arguments: args,
        environment,
        backend: "windows-restricted-token-acl".to_owned(),
        enforcement: SandboxEnforcement::Partial,
    }
}

pub(crate) fn scratch_directory() -> Result<tempfile::TempDir, HarnessError> {
    tempfile::Builder::new()
        .prefix("ternilo-process-")
        .tempdir()
        .map_err(|error| {
            HarnessError::execution(format!("create process temporary directory: {error}"))
        })
}

fn scratch_environment(policy: &SandboxPolicy) -> BTreeMap<OsString, OsString> {
    let Some(root) = &policy.scratch_root else {
        return BTreeMap::new();
    };
    [
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_CACHE_HOME",
        "CLANG_MODULE_CACHE_PATH",
    ]
    .into_iter()
    .map(|name| (OsString::from(name), root.as_os_str().to_owned()))
    .chain([(
        OsString::from("PYTHONDONTWRITEBYTECODE"),
        OsString::from("1"),
    )])
    .collect()
}

fn unavailable(mode: SandboxMode, detail: &str) -> HarnessError {
    let action = if cfg!(target_os = "linux") {
        "Install bubblewrap on the computer running this workspace (Debian/Ubuntu: sudo apt install bubblewrap; Fedora: sudo dnf install bubblewrap; Arch: sudo pacman -S bubblewrap), then retry."
    } else if cfg!(target_os = "windows") {
        "Reinstall the complete Ternilo package on the computer running this workspace, keeping ternilo-sandbox-windows.exe beside the main executable, then retry."
    } else {
        "Repair the sandbox on the computer running this workspace, then retry."
    };
    HarnessError::policy(format!(
        "sandbox mode {mode:?} was requested but no enforcing backend is usable; refusing to run unconfined: {detail}. {action}"
    ))
}

pub(crate) async fn readiness(
    root: &Path,
    profile: &Profile,
    mode: SandboxMode,
) -> Option<SandboxReadiness> {
    let entry = profile
        .plugins
        .iter()
        .find(|entry| entry.enabled && entry.kind == LOCAL_SANDBOX_KIND)?;
    // Custom sandbox plugins own their own diagnostics; do not report the local
    // backend as the execution environment for them.
    if !profile
        .plugins
        .iter()
        .any(|entry| entry.enabled && entry.kind == crate::capabilities::LOCAL_SHELL_KIND)
    {
        return None;
    }
    let backend = if cfg!(target_os = "linux") {
        "bubblewrap"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "seatbelt"
    } else {
        "unsupported"
    };
    let mut result = SandboxReadiness {
        backend: backend.to_owned(),
        status: SandboxReadinessStatus::Missing,
        detail: None,
        install_command: if cfg!(target_os = "linux") {
            linux_install_command(&std::fs::read_to_string("/etc/os-release").unwrap_or_default())
        } else {
            None
        },
    };
    let config = serde_json::from_value::<LocalSandboxConfig>(if entry.config.is_null() {
        serde_json::json!({})
    } else {
        entry.config.clone()
    });
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            result.status = SandboxReadinessStatus::Unusable;
            result.detail = Some(error.to_string());
            return Some(result);
        }
    };
    #[cfg(not(windows))]
    let _ = config;
    #[cfg(windows)]
    let (program, arguments) = crate::capabilities::native_script_command("exit 0");
    #[cfg(not(windows))]
    let (program, arguments) = (OsString::from("/bin/sh"), os_args(["-c", "exit 0"]));
    let policy = SandboxPolicy {
        scratch_root: None,
        mode,
        workspace_root: root.to_owned(),
    };
    #[cfg(windows)]
    let command = LocalSandbox {
        windows_runner: config.windows_runner,
    }
    .platform_command(program, arguments, &policy);
    #[cfg(not(windows))]
    let command = LocalSandbox::platform_command(program, arguments, &policy);
    let command = match command {
        Ok(command) => command,
        Err(error) => {
            result.detail = Some(error.to_string());
            return Some(result);
        }
    };
    let mut command = command_from_confined(command, root);
    let timeout_ms = if cfg!(windows) { 10_000 } else { 5_000 };
    match crate::capabilities::capture_command(&mut command, timeout_ms, 4_096, None).await {
        Ok(crate::capabilities::CaptureResult::Completed(output)) if output.status.success() => {
            result.status = SandboxReadinessStatus::Ready;
            result.install_command = None;
        }
        Ok(crate::capabilities::CaptureResult::Completed(output)) => {
            let (_, stderr) = output.render().await;
            result.status = SandboxReadinessStatus::Unusable;
            result.detail = Some(format!(
                "sandbox check exited with {:?}: {}",
                output.status.code(),
                stderr
            ));
            result.install_command = None;
        }
        outcome => {
            result.status = SandboxReadinessStatus::Unusable;
            result.detail = Some(match outcome {
                Err(error) => error.to_string(),
                _ => format!("sandbox check timed out after {timeout_ms} ms"),
            });
            result.install_command = None;
        }
    }
    Some(result)
}

fn linux_install_command(os_release: &str) -> Option<String> {
    let ids = os_release
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| matches!(*key, "ID" | "ID_LIKE"))
        .flat_map(|(_, value)| value.trim_matches(['\'', '"']).split_whitespace());
    for id in ids {
        let command = match id {
            "debian" | "ubuntu" => "sudo apt install bubblewrap",
            "fedora" | "rhel" | "centos" => "sudo dnf install bubblewrap",
            "arch" | "manjaro" => "sudo pacman -S bubblewrap",
            "alpine" => "sudo apk add bubblewrap",
            "opensuse" | "opensuse-tumbleweed" | "suse" => "sudo zypper install bubblewrap",
            _ => continue,
        };
        return Some(command.to_owned());
    }
    None
}

#[cfg(target_os = "linux")]
fn resolve_program(program: &OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(target_os = "windows")]
fn sibling_windows_runner() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|directory| directory.join("ternilo-sandbox-windows.exe"))
}

#[cfg(target_os = "windows")]
fn windows_sandbox_state_root() -> PathBuf {
    std::env::var_os("TERNILO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|root| root.join("Ternilo"))
        })
        .unwrap_or_else(|| PathBuf::from(".ternilo"))
        .join("windows-sandbox")
}

#[cfg(any(target_os = "macos", test))]
fn seatbelt_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(any(target_os = "macos", test))]
fn xcrun_cache_pattern(user_temp: &Path) -> String {
    format!(
        "^{}/xcrun_db(-[^/]+)?$",
        regex::escape(&user_temp.to_string_lossy())
    )
}

#[cfg(target_os = "macos")]
fn darwin_user_temp_directory() -> Result<PathBuf, HarnessError> {
    let output = std::process::Command::new("/usr/bin/getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .map_err(|error| {
            HarnessError::execution(format!("query Darwin user temporary directory: {error}"))
        })?;
    if !output.status.success() {
        return Err(HarnessError::execution(
            "getconf DARWIN_USER_TEMP_DIR failed",
        ));
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| HarnessError::execution("Darwin user temporary directory is not UTF-8"))?;
    let path = Path::new(path.trim_end_matches(['\r', '\n']));
    if !path.is_absolute() {
        return Err(HarnessError::execution(
            "Darwin user temporary directory is not absolute",
        ));
    }
    path.canonicalize().map_err(|error| {
        HarnessError::execution(format!("resolve Darwin user temporary directory: {error}"))
    })
}

fn os_args<const N: usize>(values: [&str; N]) -> Vec<OsString> {
    values.into_iter().map(OsString::from).collect()
}

pub(crate) fn command_from_confined(spec: ConfinedCommand, root: &Path) -> Command {
    let mut command = Command::new(spec.program);
    command
        .args(spec.arguments)
        .envs(spec.environment)
        .current_dir(root);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: SandboxMode) -> SandboxPolicy {
        SandboxPolicy {
            scratch_root: None,
            mode,
            workspace_root: PathBuf::from("/work tree"),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[tokio::test]
    async fn read_only_process_can_write_private_scratch_but_not_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = scratch_directory().unwrap();
        let policy = SandboxPolicy {
            mode: SandboxMode::ReadOnly,
            workspace_root: workspace.path().canonicalize().unwrap(),
            scratch_root: Some(scratch.path().to_owned()),
        };
        #[cfg(not(windows))]
        let spec = LocalSandbox::platform_command(
            "/bin/sh".into(),
            os_args([
                "-c",
                "printf cache > \"$TMPDIR/check\" && ! touch forbidden.txt",
            ]),
            &policy,
        )
        .unwrap();
        #[cfg(windows)]
        let spec = LocalSandbox { windows_runner: std::env::var_os("TERNILO_TEST_SANDBOX_WINDOWS").map(PathBuf::from) }.platform_command(
            "powershell.exe".into(), os_args(["-NoProfile", "-NonInteractive", "-Command", "Set-Content -Path (Join-Path $env:TMPDIR 'check') -Value cache -NoNewline; try { Set-Content forbidden.txt blocked -ErrorAction Stop; exit 1 } catch { exit 0 }"]), &policy).unwrap();
        let output = command_from_confined(spec, workspace.path())
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(scratch.path().join("check")).unwrap(),
            b"cache"
        );
        assert!(!workspace.path().join("forbidden.txt").exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_system_python_uses_writable_temporary_storage_without_xcrun_noise() {
        let workspace = tempfile::tempdir().unwrap();
        let scratch = scratch_directory().unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let denied_path = unrelated.path().join("not-an-xcrun-cache");
        for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
            let denied_literal = serde_json::to_string(&denied_path.to_string_lossy()).unwrap();
            let deny_check = if mode == SandboxMode::ReadOnly {
                format!(
                    "\ntry:\n pathlib.Path({denied_literal}).write_text('blocked')\nexcept PermissionError:\n pass\nelse:\n raise AssertionError('unrelated user temporary file was writable')\n"
                )
            } else {
                String::new()
            };
            let script = format!(
                "import tempfile, pathlib; p = pathlib.Path(tempfile.gettempdir()) / 'python-cache-check'; p.write_text('ok'); print(p.read_text()){deny_check}"
            );
            let spec = LocalSandbox::platform_command(
                "/usr/bin/python3".into(),
                os_args(["-c", &script]),
                &SandboxPolicy {
                    mode,
                    workspace_root: workspace.path().canonicalize().unwrap(),
                    scratch_root: Some(scratch.path().to_owned()),
                },
            )
            .unwrap();
            let output = command_from_confined(spec, workspace.path())
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
            assert!(
                output.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn bubblewrap_profiles_have_private_pid_and_only_bind_writable_workspaces() {
        let read_only = bwrap_command(
            PathBuf::from("/usr/bin/bwrap"),
            OsString::from("bash"),
            vec![OsString::from("-lc"), OsString::from("true")],
            &policy(SandboxMode::ReadOnly),
        );
        assert!(
            read_only
                .arguments
                .contains(&OsString::from("--unshare-pid"))
        );
        assert!(!read_only.arguments.contains(&OsString::from("--bind")));
        assert!(
            !read_only
                .arguments
                .contains(&OsString::from("--unshare-all"))
        );

        let writable = bwrap_command(
            PathBuf::from("/usr/bin/bwrap"),
            OsString::from("bash"),
            Vec::new(),
            &policy(SandboxMode::WorkspaceWrite),
        );
        assert!(writable.arguments.contains(&OsString::from("--bind")));
        assert_eq!(writable.enforcement, SandboxEnforcement::Full);
    }

    #[test]
    fn seatbelt_profile_escapes_paths_and_grants_workspace_only_in_write_mode() {
        let spec = seatbelt_command(
            PathBuf::from("/usr/bin/sandbox-exec"),
            OsString::from("bash"),
            Vec::new(),
            &SandboxPolicy {
                scratch_root: None,
                mode: SandboxMode::WorkspaceWrite,
                workspace_root: PathBuf::from("/tmp/a \"quoted\" path"),
            },
            Path::new("/private/var/folders/example/T"),
        );
        let profile = spec.arguments[1].to_string_lossy();
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("a \\\"quoted\\\" path"));
        assert_eq!(spec.enforcement, SandboxEnforcement::Full);
    }

    #[test]
    fn xcrun_cache_permission_does_not_allow_other_temporary_files_or_subdirectories() {
        let root = "/private/var/folders/user.with+[chars]/T";
        let allowed = regex::Regex::new(&xcrun_cache_pattern(Path::new(root))).unwrap();
        assert!(allowed.is_match(&format!("{root}/xcrun_db-AbC123")));
        assert!(allowed.is_match(&format!("{root}/xcrun_db")));
        assert!(!allowed.is_match(&format!("{root}/unrelated.txt")));
        assert!(!allowed.is_match(&format!("{root}/xcrun_db-a/subdir")));
        assert!(!allowed.is_match("/private/var/folders/userXwithchars/T/xcrun_db-AbC123"));
    }

    #[test]
    fn windows_runner_receives_exact_mode_workspace_and_partial_enforcement() {
        let spec = windows_command(
            PathBuf::from("ternilo-sandbox-windows.exe"),
            PathBuf::from("C:/state"),
            OsString::from("powershell.exe"),
            vec![OsString::from("-Command"), OsString::from("echo ok")],
            &SandboxPolicy {
                scratch_root: None,
                mode: SandboxMode::ReadOnly,
                workspace_root: PathBuf::from("C:/workspace"),
            },
        );
        assert_eq!(spec.enforcement, SandboxEnforcement::Partial);
        assert_eq!(spec.backend, "windows-restricted-token-acl");
        assert!(spec.arguments.contains(&OsString::from("read-only")));
        assert_eq!(
            spec.environment.get(OsStr::new("ZAGENS_HOME")),
            Some(&OsString::from("C:/state"))
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn bubblewrap_enforces_workspace_write_and_read_only_modes() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let runner = resolve_program(OsStr::new("bwrap")).expect("bubblewrap is installed");
        let writable_policy = SandboxPolicy {
            scratch_root: None,
            mode: SandboxMode::WorkspaceWrite,
            workspace_root: workspace.path().to_path_buf(),
        };
        let inside = bwrap_command(
            runner.clone(),
            OsString::from("/usr/bin/touch"),
            vec![OsString::from("inside.txt")],
            &writable_policy,
        );
        assert!(
            command_from_confined(inside, workspace.path())
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(workspace.path().join("inside.txt").is_file());

        let escaped_path = outside.path().join("escaped.txt");
        let outside_write = bwrap_command(
            runner.clone(),
            OsString::from("/usr/bin/touch"),
            vec![escaped_path.as_os_str().to_owned()],
            &writable_policy,
        );
        assert!(
            !command_from_confined(outside_write, workspace.path())
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(!escaped_path.exists());

        let read_only = bwrap_command(
            runner,
            OsString::from("/usr/bin/touch"),
            vec![OsString::from("blocked.txt")],
            &SandboxPolicy {
                scratch_root: None,
                mode: SandboxMode::ReadOnly,
                workspace_root: workspace.path().to_path_buf(),
            },
        );
        assert!(
            !command_from_confined(read_only, workspace.path())
                .status()
                .await
                .unwrap()
                .success()
        );
        assert!(!workspace.path().join("blocked.txt").exists());
    }
}
