use super::*;

#[tokio::test]
async fn session_commit_publishes_workbench_invalidation_and_failure_does_not() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let mut invalidations = application.subscribe_invalidations();

    let session = application
        .create_session(
            workspace.workspace_id.clone(),
            Some("notification-session".to_owned()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        invalidations.recv().await.unwrap(),
        crate::LocalInvalidationNotification {
            session_id: Some(session.identity.session_id.as_str().to_owned()),
            category: crate::LocalInvalidationCategory::Workbench,
            revision: None,
        }
    );

    application
        .create_session(
            workspace.workspace_id,
            Some("notification-session".to_owned()),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalidations.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn queue_commit_targets_its_session_and_failed_edit_stays_silent() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let first = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let second = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let first_id = first.identity.session_id.as_str();
    let second_id = second.identity.session_id.as_str();
    let mut invalidations = application.subscribe_invalidations();

    application
        .inbox
        .enqueue(first_id, queued_submission("queued-notification"))
        .await
        .unwrap();
    let notification = invalidations.recv().await.unwrap();
    assert_eq!(notification.session_id.as_deref(), Some(first_id));
    assert_eq!(
        notification.category,
        crate::LocalInvalidationCategory::Inbox
    );
    assert_ne!(notification.session_id.as_deref(), Some(second_id));

    application
        .edit_session_queue_item(
            second_id,
            ternilo_protocol::SubmissionId::new("missing"),
            ternilo_protocol::QueueEditRequest {
                input: "still missing".to_owned(),
                expected_updated_at_ms: 1,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalidations.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn agent_team_commit_notifies_members_with_task_revision() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    let mut invalidations = application.subscribe_invalidations();

    let task = application
        .create_agent_team_task(
            session_id,
            AgentTeamTaskCreate {
                subject: "Review contract".to_owned(),
                description: String::new(),
                status: ternilo_protocol::AgentTeamTaskStatus::Pending,
                dependencies: Vec::new(),
                owner: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        invalidations.recv().await.unwrap(),
        crate::LocalInvalidationNotification {
            session_id: Some(session_id.to_owned()),
            category: crate::LocalInvalidationCategory::AgentTeam,
            revision: Some(task.revision),
        }
    );
    application
        .create_agent_team_task(
            session_id,
            AgentTeamTaskCreate {
                subject: String::new(),
                description: String::new(),
                status: ternilo_protocol::AgentTeamTaskStatus::Pending,
                dependencies: Vec::new(),
                owner: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        invalidations.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn invalidation_lag_is_recovered_by_reading_canonical_state() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    let mut invalidations = application.subscribe_invalidations();

    for _ in 0..300 {
        application.invalidate(
            Some(&session_id),
            crate::LocalInvalidationCategory::Profile,
            None,
        );
    }
    assert!(matches!(
        invalidations.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    assert!(
        application
            .snapshot()
            .await
            .sessions
            .iter()
            .any(|current| current.identity.session_id.as_str() == session_id)
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
