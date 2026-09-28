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
use ternilo_protocol::HarnessError;
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
        Ok(seatbelt_command(runner, program, arguments, policy))
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
    if policy.mode == SandboxMode::WorkspaceWrite {
        args.extend(os_args(["--tmpfs", "/tmp", "--bind"]));
        args.push(policy.workspace_root.as_os_str().to_owned());
        args.push(policy.workspace_root.as_os_str().to_owned());
    }
    args.extend(os_args(["--chdir"]));
    args.push(policy.workspace_root.as_os_str().to_owned());
    args.push(OsString::from("--"));
    args.push(program);
    args.extend(arguments);
    ConfinedCommand {
        program: runner.into_os_string(),
        arguments: args,
        environment: BTreeMap::new(),
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
) -> ConfinedCommand {
    let mut writable = vec![PathBuf::from("/dev/null")];
    if policy.mode == SandboxMode::WorkspaceWrite {
        writable.push(policy.workspace_root.clone());
        writable.push(PathBuf::from("/private/tmp"));
        if let Some(temp) = std::env::var_os("TMPDIR").map(PathBuf::from) {
            writable.push(temp);
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
        profile.push_str(&seatbelt_string(&path));
        profile.push_str("))");
    }
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
        environment: BTreeMap::new(),
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
    args.push(OsString::from("--"));
    args.push(program);
    args.extend(arguments);
    ConfinedCommand {
        program: runner.into_os_string(),
        arguments: args,
        environment: BTreeMap::from([(OsString::from("ZAGENS_HOME"), state_root.into_os_string())]),
        backend: "windows-restricted-token-acl".to_owned(),
        enforcement: SandboxEnforcement::Partial,
    }
}

fn unavailable(mode: SandboxMode, detail: &str) -> HarnessError {
    HarnessError::policy(format!(
        "sandbox mode {mode:?} was requested but no enforcing backend is usable; refusing to run unconfined: {detail}"
    ))
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
fn seatbelt_string(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
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
            mode,
            workspace_root: PathBuf::from("/work tree"),
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
                mode: SandboxMode::WorkspaceWrite,
                workspace_root: PathBuf::from("/tmp/a \"quoted\" path"),
            },
        );
        let profile = spec.arguments[1].to_string_lossy();
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("a \\\"quoted\\\" path"));
        assert_eq!(spec.enforcement, SandboxEnforcement::Full);
    }

    #[test]
    fn windows_runner_receives_exact_mode_workspace_and_partial_enforcement() {
        let spec = windows_command(
            PathBuf::from("ternilo-sandbox-windows.exe"),
            PathBuf::from("C:/state"),
            OsString::from("powershell.exe"),
            vec![OsString::from("-Command"), OsString::from("echo ok")],
            &SandboxPolicy {
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
