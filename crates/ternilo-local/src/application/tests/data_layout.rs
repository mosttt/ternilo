use super::*;

#[tokio::test]
async fn categorized_stores_reopen_presets_credentials_workspaces_and_history() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("instance");
    let workspace = temporary.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("note.txt"), "hello")
        .await
        .unwrap();
    let application = open_test_application(root.clone()).await;
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    application
        .copy_agent_preset(AgentPresetCopyRequest {
            from: "minimal".into(),
            id: "custom".into(),
            display_name: Some("Custom".into()),
        })
        .await
        .unwrap();
    application
        .set_credential("LAYOUT_TEST_KEY".into(), "fixture-secret".into())
        .await
        .unwrap();
    let session = application
        .create_session_with_options(
            workspace.workspace_id.clone(),
            Some("durable".into()),
            None,
            Some("custom".into()),
            None,
        )
        .await
        .unwrap();
    let outcome = application
        .run_turn(
            session.identity.session_id.as_str(),
            None,
            "/read note.txt".into(),
        )
        .await
        .unwrap();
    let events = application
        .events(session.identity.session_id.as_str())
        .await
        .unwrap();
    assert!(!outcome.answer.is_empty());
    assert!(!events.is_empty());
    application.shutdown().await.unwrap();
    application.close().await.unwrap();
    drop(application);
    for path in [
        "config/agent-presets.json",
        "secrets/credentials.json",
        "secrets/node-authorizations.json",
        "data/state.json",
        "data/db/inbox.sqlite3",
        "data/db/preferences.sqlite3",
        "data/db/agent-team.sqlite3",
        "cache/session-search.sqlite3",
        "cache/session-projections.sqlite3",
        "runtime/writer.lock",
    ] {
        assert!(root.join(path).is_file(), "missing {path}");
    }
    assert!(
        std::fs::read_dir(root.join("data/sessions"))
            .unwrap()
            .next()
            .is_some()
    );
    assert!(
        std::fs::read_dir(&root)
            .unwrap()
            .all(|entry| entry.unwrap().file_type().unwrap().is_dir())
    );
    let reopened = open_test_application(root).await;
    assert_eq!(
        reopened.snapshot().await.workspaces[0].workspace_id,
        workspace.workspace_id
    );
    assert_eq!(reopened.credential_names().await, vec!["LAYOUT_TEST_KEY"]);
    assert_eq!(
        reopened.agent_preset("custom").await.unwrap().summary.id,
        "custom"
    );
    assert_eq!(
        reopened
            .events(session.identity.session_id.as_str())
            .await
            .unwrap(),
        events
    );
    reopened.shutdown().await.unwrap();
    reopened.close().await.unwrap();
}
