use super::*;
use std::time::Duration;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{RunLimits, SessionMode};

#[tokio::test]
async fn stop_and_send_acknowledges_before_cleanup_and_waits_for_the_old_turn_gate() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace_path = temporary.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let app = Arc::new(
        LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            temporary.path().join("instance"),
        )
        .await
        .unwrap(),
    );
    let workspace = app
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.to_string();
    let managed = app.addressable_session(&id).await.unwrap();
    let old_turn = managed.gate.lock().await;
    let driver = app.submission_driver(&id).await;
    let old_driver = driver.lock().await;
    let provenance = app.local_input_provenance().unwrap();
    let item_id = provenance.input_id.clone();
    app.inbox
        .enqueue(
            &id,
            SessionSubmission {
                id: item_id.clone(),
                run_id: ternilo_protocol::RunId::new("next-run"),
                provenance: Some(provenance),
                content: SubmissionContent::Prompt {
                    input: "/write next.txt accepted".into(),
                },
                references: vec![],
                attachments: vec![],
                placement: SubmissionPlacement::Queued,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        app.steer_queued_session_item(&id, item_id),
    )
    .await
    .expect("acceptance must not wait for the previous driver")
    .unwrap();
    assert!(app.session_inbox(&id).await.unwrap().paused);
    assert!(!workspace_path.join("next.txt").exists());
    drop(old_driver);
    assert!(app.session_inbox(&id).await.unwrap().paused);
    drop(old_turn);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.session_inbox(&id).await.unwrap().items.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(workspace_path.join("next.txt"))
            .await
            .unwrap(),
        "accepted"
    );
    assert!(!app.session_inbox(&id).await.unwrap().paused);
    app.shutdown().await.unwrap();
    app.close().await.unwrap();
}

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
