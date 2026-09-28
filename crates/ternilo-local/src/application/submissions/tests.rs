use super::*;
use std::time::Duration;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{RunLimits, SessionMode};

#[tokio::test]
async fn idle_queue_driver_waits_for_profile_change_before_recreating_runtime() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let app = Arc::new(
        LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            root.path().join("data"),
        )
        .await
        .unwrap(),
    );
    let workspace = app
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let mut session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.to_string();
    let lifecycle = app.session_lifecycle(&id).await;
    let changing = lifecycle.lock().await;
    app.ensure_session_locked(&id).await.unwrap();
    app.stop_live_session(&id).await.unwrap();

    // An idle driver can wake while a profile update has removed its old runtime.
    let driving = app.drive_session_inbox(&id);
    tokio::pin!(driving);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut driving)
            .await
            .is_err()
    );
    assert!(app.live.read().await.is_empty());
    session.mode = SessionMode::Plan;
    app.state.replace_session(&id, session).await.unwrap();
    drop(changing);

    tokio::time::timeout(Duration::from_secs(5), driving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        app.live.read().await.get(&id).unwrap().boot_mode,
        SessionMode::Plan
    );
    app.shutdown().await.unwrap();
}
