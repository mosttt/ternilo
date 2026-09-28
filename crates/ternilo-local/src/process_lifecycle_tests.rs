use super::{CaptureResult, MAX_SHELL_CAPTURE_BYTES, capture_command};
use std::time::Duration;
use tokio::process::Command;

async fn wait_until_ready(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_shell_capture_stops_descendant_writes() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("bash");
    command.current_dir(root.path()).args([
        "-c",
        "(sleep 0.8; printf orphan > escaped-write) >/dev/null 2>&1 & printf ready > ready; wait",
    ]);
    let running = tokio::spawn(async move {
        capture_command(&mut command, 5_000, MAX_SHELL_CAPTURE_BYTES, None).await
    });
    wait_until_ready(&root.path().join("ready")).await;
    running.abort();
    assert!(matches!(running.await, Err(error) if error.is_cancelled()));
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(!root.path().join("escaped-write").exists());
}

#[tokio::test]
async fn completed_shell_capture_stops_descendants_after_leader_exit() {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new("bash");
    command.current_dir(root.path()).args([
        "-c",
        "(sleep 0.8; printf orphan > escaped-write) >/dev/null 2>&1 & exit 0",
    ]);
    let result = capture_command(&mut command, 5_000, MAX_SHELL_CAPTURE_BYTES, None)
        .await
        .unwrap();
    assert!(matches!(result, CaptureResult::Completed(output) if output.status.success()));
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(!root.path().join("escaped-write").exists());
}

#[tokio::test]
async fn exited_shell_leader_does_not_leave_capture_waiting_for_inherited_pipes() {
    let mut command = Command::new("bash");
    command.args(["-c", "sleep 20 & printf completed; exit 0"]);
    let result = capture_command(&mut command, 1_000, MAX_SHELL_CAPTURE_BYTES, None)
        .await
        .unwrap();
    let CaptureResult::Completed(output) = result else {
        panic!("an exited leader must finish without waiting for its descendants");
    };
    assert!(output.status.success());
    assert_eq!(output.stdout.render(), "completed");
}
