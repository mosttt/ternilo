#[cfg(target_os = "linux")]
use crate::sandbox_lifetime;
use crate::{config::SandboxMode, process_lifetime::ManagedChild};
use std::{path::Path, process::Stdio};
use ternilo_protocol::HarnessError;
use tokio::process::Command;

/// Verify the host before registration can replace a live Worker generation.
pub(crate) async fn verify(sandbox: SandboxMode) -> Result<(), HarnessError> {
    if sandbox == SandboxMode::Process {
        return Ok(());
    }
    let directory = tempfile::Builder::new()
        .prefix("ternilo-sandbox-probe-")
        .tempdir()
        .map_err(|error| HarnessError::execution(format!("prepare sandbox check: {error}")))?;
    let policy = directory.path().join("policy.json");
    let envelope = directory.path().join("envelope.json");
    std::fs::write(&policy, b"{}")
        .and_then(|()| std::fs::write(&envelope, b"{}"))
        .map_err(|error| {
            HarnessError::execution(format!("prepare sandbox check files: {error}"))
        })?;
    let mut child = spawn_child(
        sandbox,
        Path::new("/usr/bin/true"),
        &policy,
        &envelope,
        directory.path(),
    )?;
    if let Err(error) = child.establish_sandbox(|_| Ok(())).await {
        let _ = child.start_kill();
        let _ = child.wait().await;
        let detail = startup_diagnostic(&mut child).await;
        return Err(HarnessError::execution(format!(
            "sandbox startup check failed: {error}{detail}"
        )));
    }
    let status = child
        .wait()
        .await
        .map_err(|error| HarnessError::execution(format!("confirm sandbox check exit: {error}")))?;
    if !status.success() {
        let detail = startup_diagnostic(&mut child).await;
        return Err(HarnessError::execution(format!(
            "sandbox startup check exited with {status}{detail}"
        )));
    }
    Ok(())
}

pub(crate) async fn startup_diagnostic(child: &mut ManagedChild) -> String {
    use tokio::io::AsyncReadExt as _;
    let Some(stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        stderr.take(4096).read_to_end(&mut bytes),
    )
    .await;
    let message = String::from_utf8_lossy(&bytes);
    if message.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", message.trim())
    }
}

pub(crate) fn spawn_child(
    sandbox: SandboxMode,
    executable: &Path,
    policy_path: &Path,
    envelope_path: &Path,
    workspace_path: &Path,
) -> Result<ManagedChild, HarnessError> {
    let mut command = child_command(
        sandbox,
        executable,
        policy_path,
        envelope_path,
        workspace_path,
    );
    let lifetime = if sandbox == SandboxMode::Process {
        None
    } else {
        #[cfg(target_os = "linux")]
        {
            Some(
                sandbox_lifetime::SandboxLifetime::prepare(&mut command).map_err(|error| {
                    HarnessError::execution(format!("prepare sandbox lifetime: {error}"))
                })?,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            return Err(HarnessError::execution(
                "sandbox lifetime proof requires Linux",
            ));
        }
    };
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env(
            "HOME",
            if sandbox == SandboxMode::Process {
                workspace_path.as_os_str()
            } else {
                std::ffi::OsStr::new("/workspace")
            },
        )
        .env(
            "TERNILO_OUTER_SANDBOX",
            if sandbox == SandboxMode::Process {
                "0"
            } else {
                "1"
            },
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ManagedChild::spawn(&mut command, sandbox == SandboxMode::Process, lifetime)
        .map_err(|error| HarnessError::execution(format!("spawn isolated cloud child: {error}")))
}

pub(crate) fn child_command(
    sandbox: SandboxMode,
    executable: &Path,
    policy_path: &Path,
    envelope_path: &Path,
    workspace_path: &Path,
) -> Command {
    match sandbox {
        SandboxMode::Process => {
            let mut command = Command::new(executable);
            command
                .arg("execute")
                .arg("--policy")
                .arg(policy_path)
                .arg("--envelope")
                .arg(envelope_path);
            #[cfg(target_os = "linux")]
            command.arg("--owned-process-session");
            command
        }
        SandboxMode::Bubblewrap => bubblewrap_child(
            executable,
            policy_path,
            envelope_path,
            workspace_path,
            false,
        ),
        SandboxMode::Container => {
            bubblewrap_child(executable, policy_path, envelope_path, workspace_path, true)
        }
    }
}

fn bubblewrap_child(
    executable: &Path,
    policy_path: &Path,
    envelope_path: &Path,
    workspace_path: &Path,
    container_mode: bool,
) -> Command {
    let mut command = Command::new("/usr/sbin/bwrap");
    command.arg("--die-with-parent").arg("--new-session");
    #[cfg(target_os = "linux")]
    sandbox_lifetime::SandboxLifetime::add_options(&mut command);
    if container_mode {
        command
            .arg("--unshare-pid")
            .arg("--unshare-net")
            .arg("--unshare-ipc")
            .arg("--unshare-uts")
            .arg("--unshare-cgroup");
    } else {
        command.arg("--unshare-all");
    }
    command
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr")
        .arg("--symlink")
        .arg("usr/lib")
        .arg("/lib")
        .arg("--symlink")
        .arg("usr/lib64")
        .arg("/lib64")
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--chmod")
        .arg("1777")
        .arg("/tmp")
        .arg("--ro-bind")
        .arg(executable)
        .arg("/worker")
        .arg("--ro-bind")
        .arg(policy_path)
        .arg("/policy.json")
        .arg("--ro-bind")
        .arg(envelope_path)
        .arg("/envelope.json")
        .arg("--bind")
        .arg(workspace_path)
        .arg("/workspace")
        .arg("--chdir")
        .arg("/workspace");
    if container_mode {
        command
            .arg("--cap-add")
            .arg("CAP_SETUID")
            .arg("--cap-add")
            .arg("CAP_SETGID")
            .arg("--")
            .arg("/usr/bin/setpriv")
            .arg("--reuid=10001")
            .arg("--regid=10001")
            .arg("--clear-groups")
            .arg("--no-new-privs")
            .arg("--inh-caps=-all")
            .arg("--ambient-caps=-all")
            .arg("--bounding-set=-all")
            .arg("/worker")
            .arg("execute")
            .arg("--policy")
            .arg("/policy.json")
            .arg("--envelope")
            .arg("/envelope.json");
    } else {
        command
            .arg("--")
            .arg("/worker")
            .arg("execute")
            .arg("--policy")
            .arg("/policy.json")
            .arg("--envelope")
            .arg("/envelope.json");
    }
    command
}
