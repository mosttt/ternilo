use super::*;
use crate::process_lifetime::ManagedChild;
use std::{path::Path, process::Stdio};

fn sandbox_command(workspace: &Path) -> Command {
    let mut command = Command::new("/usr/sbin/bwrap");
    SandboxLifetime::add_options(&mut command);
    command.args(["--unshare-all", "--die-with-parent", "--new-session", "--ro-bind", "/usr", "/usr",
        "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64",
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--bind"])
        .arg(workspace).arg("/workspace")
        .args(["--chdir", "/workspace", "--", "/bin/sh", "-c",
            "setsid /bin/sh -c 'while :; do printf x >> /workspace/writes; sleep 0.01; done' & printf ready > /workspace/ready; sleep 60"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::inherit());
    command
}

fn spawn(workspace: &Path) -> ManagedChild {
    let mut command = sandbox_command(workspace);
    let lifetime = SandboxLifetime::prepare(&mut command).unwrap();
    ManagedChild::spawn(&mut command, false, Some(lifetime)).unwrap()
}

async fn writing(workspace: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::metadata(workspace.join("writes")).is_ok_and(|value| value.len() >= 3) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the real namespace descendant must begin writing");
}

async fn assert_stopped(child: &mut ManagedChild, workspace: &Path) {
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    let length = std::fs::metadata(workspace.join("writes")).unwrap().len();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(
        std::fs::metadata(workspace.join("writes")).unwrap().len(),
        length,
        "a new execution may only enter after the old descendant can no longer write"
    );
    assert!(child.try_wait().unwrap().is_some());
}

fn kill(pid: u32) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
}

#[tokio::test]
async fn namespace_workload_cannot_start_before_its_init_pidfd_is_pinned() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = spawn(directory.path());
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(!directory.path().join("ready").exists());
    assert!(!directory.path().join("writes").exists());
    assert!(child.try_wait().unwrap().is_none());
    child.establish_sandbox(|_| Ok(())).await.unwrap();
    writing(directory.path()).await;
    child.start_kill().unwrap();
    assert_stopped(&mut child, directory.path()).await;
}

#[tokio::test]
async fn killing_the_outer_monitor_does_not_bypass_descendant_exit_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = spawn(directory.path());
    child.establish_sandbox(|_| Ok(())).await.unwrap();
    writing(directory.path()).await;
    kill(child.id().unwrap());
    assert_stopped(&mut child, directory.path()).await;
}

#[tokio::test]
async fn killing_namespace_init_waits_for_writers_in_separate_process_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = spawn(directory.path());
    child.establish_sandbox(|_| Ok(())).await.unwrap();
    writing(directory.path()).await;
    let monitor = child.id().unwrap();
    let children =
        std::fs::read_to_string(format!("/proc/{monitor}/task/{monitor}/children")).unwrap();
    let init = children
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    kill(init);
    assert_stopped(&mut child, directory.path()).await;
}

#[tokio::test]
async fn cancelling_a_wait_keeps_the_namespace_identity_for_later_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = spawn(directory.path());
    child.establish_sandbox(|_| Ok(())).await.unwrap();
    writing(directory.path()).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), child.wait())
            .await
            .is_err()
    );
    child.start_kill().unwrap();
    assert_stopped(&mut child, directory.path()).await;
}

#[tokio::test]
#[ignore = "Requires a root container with the Worker namespace capabilities."]
async fn container_mode_retains_init_proof_while_the_workload_uses_an_unprivileged_uid() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("execute"),
        "/usr/bin/id -u > /workspace/uid; /usr/bin/setsid /usr/bin/sh -c 'while :; do printf x >> /workspace/writes; sleep 0.01; done' & sleep 60\n").unwrap();
    let policy = directory.path().join("policy.json");
    let envelope = directory.path().join("envelope.json");
    std::fs::write(&policy, b"{}").unwrap();
    std::fs::write(&envelope, b"{}").unwrap();
    std::os::unix::fs::chown(directory.path(), Some(10_001), Some(10_001)).unwrap();
    let mut command = crate::child_command(
        crate::SandboxMode::Container,
        Path::new("/usr/bin/sh"),
        &policy,
        &envelope,
        directory.path(),
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let lifetime = SandboxLifetime::prepare(&mut command).unwrap();
    let mut child = ManagedChild::spawn(&mut command, false, Some(lifetime)).unwrap();
    drop(command);
    child.establish_sandbox(|_| Ok(())).await.unwrap();
    writing(directory.path()).await;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("uid"))
            .unwrap()
            .trim(),
        "10001"
    );
    child.start_kill().unwrap();
    assert_stopped(&mut child, directory.path()).await;
    std::os::unix::fs::chown(directory.path(), Some(0), Some(0)).unwrap();
}
