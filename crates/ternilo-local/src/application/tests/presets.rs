use super::*;

#[tokio::test]
async fn new_session_accepts_the_selected_default_permission() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();

    let session = application
        .create_session_with_options(
            workspace.workspace_id,
            None,
            None,
            None,
            Some(PermissionPreset::ReadOnly),
        )
        .await
        .unwrap();
    assert_eq!(session.permissions, PermissionPreset::ReadOnly);

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn command_catalog_follows_each_sessions_composed_preset() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let standard = application
        .create_session_with_options(
            workspace.workspace_id.clone(),
            Some("catalog-standard".to_owned()),
            None,
            Some("standard".to_owned()),
            None,
        )
        .await
        .unwrap();
    let minimal = application
        .create_session_with_options(
            workspace.workspace_id,
            Some("catalog-minimal".to_owned()),
            None,
            Some("minimal".to_owned()),
            None,
        )
        .await
        .unwrap();

    let standard = application
        .session_command_catalog(standard.identity.session_id.as_str())
        .await
        .unwrap();
    let minimal = application
        .session_command_catalog(minimal.identity.session_id.as_str())
        .await
        .unwrap();
    let standard_names = standard
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let minimal_names = minimal
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(standard_names.contains("read"));
    assert!(standard_names.contains("goal"));
    assert!(standard_names.contains("jobs"));
    assert!(minimal_names.contains("read"));
    assert!(!minimal_names.contains("goal"));
    assert!(!minimal_names.contains("jobs"));
    assert!(
        standard
            .commands
            .iter()
            .all(|command| command.description.chars().count() <= 64)
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "end-to-end preset scenario keeps setup, mutation, and snapshot assertions together"
)]
async fn agent_presets_are_snapshotted_and_lock_after_the_first_turn() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let roster = application.agent_preset_roster().await;
    assert_eq!(roster.default_id, crate::DEFAULT_AGENT_PRESET);
    assert_eq!(roster.presets.len(), 4);

    let copied = application
        .copy_agent_preset(AgentPresetCopyRequest {
            from: "minimal".to_owned(),
            id: "my-agent".to_owned(),
            display_name: Some("My Agent".to_owned()),
        })
        .await
        .unwrap();
    application
        .set_default_agent_preset("my-agent")
        .await
        .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let snapshotted = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    assert_eq!(snapshotted.agent_preset, "my-agent");
    assert_eq!(snapshotted.preset_plugins, copied.profile.plugins);

    application
        .update_agent_preset(
            "my-agent",
            AgentPresetUpdateRequest {
                display_name: "My Agent v2".to_owned(),
                description: "Future sessions use this revision.".to_owned(),
                profile: Profile::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        application
            .snapshot()
            .await
            .sessions
            .iter()
            .find(|session| session.identity.session_id == snapshotted.identity.session_id)
            .unwrap()
            .preset_plugins,
        copied.profile.plugins
    );

    let blank = application
        .create_session_with_preset(
            workspace.workspace_id.clone(),
            None,
            None,
            Some("standard".to_owned()),
        )
        .await
        .unwrap();
    let switched = application
        .update_session(
            blank.identity.session_id.as_str(),
            LocalSessionUpdate {
                agent_preset: Some("ptc".to_owned()),
                ..LocalSessionUpdate::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(switched.agent_preset, "ptc");
    assert!(
        switched
            .preset_plugins
            .iter()
            .any(|plugin| plugin.id == "code-mode" && plugin.config["mode"] == "code")
    );
    application
        .run_turn(
            switched.identity.session_id.as_str(),
            None,
            "/code \"preset locked\"".to_owned(),
        )
        .await
        .unwrap();
    let locked = application
        .update_session(
            switched.identity.session_id.as_str(),
            LocalSessionUpdate {
                agent_preset: Some("standard".to_owned()),
                ..LocalSessionUpdate::default()
            },
        )
        .await
        .unwrap_err();
    assert!(locked.to_string().contains("preset is locked"));
    assert_eq!(
        application
            .state
            .session(switched.identity.session_id.as_str())
            .await
            .unwrap()
            .agent_preset,
        "ptc"
    );

    application.remove_agent_preset("my-agent").await.unwrap();
    assert_eq!(
        application.agent_preset_roster().await.default_id,
        crate::DEFAULT_AGENT_PRESET
    );
    application.shutdown().await.unwrap();
    drop(application);

    let restored = open_test_application(data_dir.clone()).await;
    let restored_snapshot = restored.snapshot().await;
    let restored_session = restored_snapshot
        .sessions
        .iter()
        .find(|session| session.identity.session_id == snapshotted.identity.session_id)
        .unwrap();
    assert_eq!(restored_session.agent_preset, "my-agent");
    assert_eq!(restored_session.preset_plugins, copied.profile.plugins);
    restored.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn preset_lock_survives_a_removed_queued_task_restart_and_fork() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    application.inbox.pause(session_id).await.unwrap();
    let submission = application
        .submit_session(
            session_id,
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Queue,
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: "/read README.md".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    let replacement = || LocalSessionUpdate {
        agent_preset: Some("minimal".to_owned()),
        ..LocalSessionUpdate::default()
    };
    assert!(
        application
            .update_session(session_id, replacement())
            .await
            .unwrap_err()
            .to_string()
            .contains("preset is locked")
    );
    application
        .remove_session_queue_item(session_id, submission.id)
        .await
        .unwrap();
    application.shutdown().await.unwrap();
    drop(application);
    let restored = open_test_application(data_dir.clone()).await;
    assert!(
        restored
            .update_session(session_id, replacement())
            .await
            .unwrap_err()
            .to_string()
            .contains("preset is locked")
    );
    restored
        .run_turn(
            session_id,
            None,
            "/code \"first completed turn\"".to_owned(),
        )
        .await
        .unwrap();
    let fork = restored.fork_session(session_id, None, None).await.unwrap();
    assert!(
        restored
            .update_session(fork.identity.session_id.as_str(), replacement())
            .await
            .unwrap_err()
            .to_string()
            .contains("preset is locked")
    );
    restored
        .update_session(
            session_id,
            LocalSessionUpdate {
                mode: Some(SessionMode::Plan),
                ..LocalSessionUpdate::default()
            },
        )
        .await
        .unwrap();
    restored.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn multi_field_session_update_is_atomic_when_plugin_config_is_invalid() {
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

    let result = application
        .update_session(
            session_id,
            LocalSessionUpdate {
                title: Some("must-not-persist".to_owned()),
                profile_plugins: Some(vec![PluginEntry {
                    id: "web-fetch".to_owned(),
                    kind: ternilo_builtins::WEB_FETCH_KIND.to_owned(),
                    enabled: true,
                    config: serde_json::json!({ "max_bytes": 0 }),
                }]),
                ..LocalSessionUpdate::default()
            },
        )
        .await;
    assert!(result.is_err());
    let unchanged = application
        .snapshot()
        .await
        .sessions
        .into_iter()
        .find(|item| item.identity.session_id.as_str() == session_id)
        .unwrap();
    assert_eq!(unchanged.title, "New session");
    assert!(unchanged.profile_plugins.is_empty());

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
