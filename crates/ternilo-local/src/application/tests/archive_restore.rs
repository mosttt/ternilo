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

async fn archive_child(
    application: &LocalApplication,
    parent: &LocalSession,
    name: &str,
) -> LocalSession {
    let binding = application
        .subagent_session_host()
        .create(
            parent.identity.clone(),
            SubagentSessionRequest {
                role: None,
                subagent_id: ternilo_protocol::SubagentId::new(name),
                provider: "in-process".into(),
                label: name.into(),
                task: "archive fixture".into(),
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
            },
        )
        .await
        .unwrap()
        .unwrap();
    application
        .state
        .session(binding.session_id.as_str())
        .await
        .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify archive, restart/restore and single-record deletion on the same delegated family."
)]
async fn archive_and_restore_follow_delegated_descendants_but_not_forks() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = open_test_application(directory.path().to_path_buf()).await;
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let parent = application
        .create_session(workspace.workspace_id, Some("archive-parent".into()), None)
        .await
        .unwrap();
    let child = archive_child(&application, &parent, "child").await;
    let grandchild = archive_child(&application, &child, "grandchild").await;
    application
        .run_turn(
            child.identity.session_id.as_str(),
            None,
            "/code \"retained child\"".into(),
        )
        .await
        .unwrap();
    let fork = application
        .fork_session(
            child.identity.session_id.as_str(),
            Some("independent-fork".into()),
            None,
        )
        .await
        .unwrap();
    let fork_child = archive_child(&application, &fork, "independent-child").await;
    let events = application
        .events(child.identity.session_id.as_str())
        .await
        .unwrap();
    application
        .inbox
        .enqueue(
            grandchild.identity.session_id.as_str(),
            queued_submission("keep-pending"),
        )
        .await
        .unwrap();
    application
        .archive_session(parent.identity.session_id.as_str())
        .await
        .unwrap();
    for session in [&parent, &child, &grandchild] {
        let id = session.identity.session_id.as_str();
        assert!(
            application
                .state
                .session(id)
                .await
                .unwrap()
                .archived_at_ms
                .is_some()
        );
        assert!(application.inbox.snapshot(id).await.unwrap().paused);
        assert!(!application.live.read().await.contains_key(id));
    }
    for session in [&fork, &fork_child] {
        assert!(
            application
                .state
                .session(session.identity.session_id.as_str())
                .await
                .unwrap()
                .archived_at_ms
                .is_none()
        );
    }
    application.shutdown().await.unwrap();
    drop(application);
    let application = open_test_application(directory.path().to_path_buf()).await;
    application
        .restore_session(parent.identity.session_id.as_str())
        .await
        .unwrap();
    for session in [&parent, &child, &grandchild] {
        let id = session.identity.session_id.as_str();
        assert!(
            application
                .state
                .session(id)
                .await
                .unwrap()
                .archived_at_ms
                .is_none()
        );
        assert!(application.inbox.snapshot(id).await.unwrap().paused);
        assert!(!application.live.read().await.contains_key(id));
    }
    assert_eq!(
        application
            .events(child.identity.session_id.as_str())
            .await
            .unwrap(),
        events
    );
    assert_eq!(
        application
            .inbox
            .snapshot(grandchild.identity.session_id.as_str())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    application
        .archive_session(parent.identity.session_id.as_str())
        .await
        .unwrap();
    application
        .delete_session(child.identity.session_id.as_str())
        .await
        .unwrap();
    assert!(
        application
            .state
            .session(child.identity.session_id.as_str())
            .await
            .is_none()
    );
    assert!(
        application
            .state
            .session(grandchild.identity.session_id.as_str())
            .await
            .is_some()
    );
    assert!(
        application
            .state
            .session(parent.identity.session_id.as_str())
            .await
            .is_some()
    );
    application.shutdown().await.unwrap();
}

#[tokio::test]
async fn archiving_parent_cancels_a_child_waiting_for_approval_and_parks_its_queue() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let app = Arc::new(open_test_application(directory.path().to_path_buf()).await);
    let workspace = app
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let parent = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let child = archive_child(&app, &parent, "waiting-child").await;
    let id = child.identity.session_id.to_string();
    let run = {
        let app = Arc::clone(&app);
        let id = id.clone();
        tokio::spawn(async move {
            app.run_turn(&id, None, "/agent-on in-process never-start".into())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app.pending_questions(Some(&id)).await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child reaches approval");
    app.inbox
        .enqueue(&id, queued_submission("keep-queued-child"))
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.archive_session(parent.identity.session_id.as_str()),
    )
    .await
    .expect("archive cancels the waiting child")
    .unwrap();
    assert!(run.await.unwrap().unwrap_err().is_cancelled());
    assert!(app.pending_questions(Some(&id)).await.is_empty());
    assert!(!app.live.read().await.contains_key(&id));
    let inbox = app.inbox.snapshot(&id).await.unwrap();
    assert!(inbox.paused);
    assert_eq!(inbox.items.len(), 1);
    assert!(
        app.state
            .session(&id)
            .await
            .unwrap()
            .archived_at_ms
            .is_some()
    );
    app.shutdown().await.unwrap();
}
