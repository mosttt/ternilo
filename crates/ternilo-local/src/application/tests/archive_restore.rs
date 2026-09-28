use super::*;

#[tokio::test]
async fn restore_preserves_history_queue_metadata_and_does_not_start_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let data_dir = directory.path().to_path_buf();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let original = application
        .create_session(
            workspace.workspace_id,
            Some("restore-session".to_owned()),
            None,
        )
        .await
        .unwrap();
    let session_id = original.identity.session_id.as_str();
    application
        .run_turn(session_id, None, "/code \"retained\"".to_owned())
        .await
        .unwrap();
    let mut archived = application.archive_session(session_id).await.unwrap();
    application.inbox.pause(session_id).await.unwrap();
    application
        .inbox
        .enqueue(session_id, queued_submission("retained-task"))
        .await
        .unwrap();
    application
        .unregister_workspace(original.workspace_id.clone())
        .await
        .unwrap();
    let events = application.events(session_id).await.unwrap();
    assert_eq!(
        application.archived_events(session_id).await.unwrap(),
        events
    );
    let inbox = application.inbox.snapshot(session_id).await.unwrap();
    let (first, second) = tokio::join!(
        application.restore_session(session_id),
        application.restore_session(session_id),
    );
    archived.archived_at_ms = None;
    assert_eq!(first.unwrap(), archived);
    assert_eq!(second.unwrap(), archived);
    assert!(application.archived_events(session_id).await.is_err());
    assert_eq!(application.events(session_id).await.unwrap(), events);
    assert_eq!(application.inbox.snapshot(session_id).await.unwrap(), inbox);
    assert!(!application.live.read().await.contains_key(session_id));
    assert_eq!(application.snapshot().await.sessions.len(), 1);
    assert!(application.snapshot().await.workspaces.is_empty());
    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir).await;
    assert_eq!(reopened.state.session(session_id).await.unwrap(), archived);
    assert_eq!(reopened.events(session_id).await.unwrap(), events);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_cannot_recreate_a_deleted_session_or_clear_its_tombstone() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = open_test_application(directory.path().to_path_buf()).await;
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let original = application
        .create_session(
            workspace.workspace_id,
            Some("deleted-session".to_owned()),
            None,
        )
        .await
        .unwrap();
    let session_id = original.identity.session_id.as_str();
    let archived = application.archive_session(session_id).await.unwrap();
    let (restored, deleted) = tokio::join!(
        application.restore_session(session_id),
        application.delete_session(session_id),
    );
    if let Ok(restored) = restored {
        assert_eq!(restored.identity, original.identity);
    }
    deleted.unwrap();
    assert!(application.restore_session(session_id).await.is_err());
    assert!(application.state.session(session_id).await.is_none());
    application.state.insert_session(archived).await.unwrap();
    assert!(application.restore_session(session_id).await.is_err());
    assert!(
        application
            .state
            .session(session_id)
            .await
            .unwrap()
            .archived_at_ms
            .is_some()
    );
    application.shutdown().await.unwrap();
}
