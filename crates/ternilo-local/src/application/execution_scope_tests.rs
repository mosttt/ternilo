use super::{tests::open_test_application, *};
use ternilo_protocol::SubagentTranscriptKind;

async fn child(application: &LocalApplication, parent: &LocalSession, name: &str) -> LocalSession {
    let binding = application
        .subagent_session_host()
        .create(
            parent.identity.clone(),
            SubagentSessionRequest {
                subagent_id: SubagentId::new(name),
                provider: "in-process".to_owned(),
                label: name.to_owned(),
                task: "scope fixture".to_owned(),
                transcript_kind: SubagentTranscriptKind::Conversation,
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
async fn creation_hooks_separate_forks_and_retain_subagent_scope_after_delete_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_path = directory.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let data = directory.path().join("data");
    let application = open_test_application(data.clone()).await;
    let workspace = application
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let root = application
        .create_session(workspace.workspace_id, Some("root".to_owned()), None)
        .await
        .unwrap();
    let nested = child(&application, &root, "child").await;
    let grandchild = child(&application, &nested, "grandchild").await;
    let scope = application.execution_scope("root").await.unwrap();
    assert_eq!(
        application
            .execution_scope(nested.identity.session_id.as_str())
            .await
            .unwrap(),
        scope
    );
    assert_eq!(
        application
            .execution_scope(grandchild.identity.session_id.as_str())
            .await
            .unwrap(),
        scope
    );
    application
        .run_turn(
            nested.identity.session_id.as_str(),
            None,
            "/code \"fork seed\"".to_owned(),
        )
        .await
        .unwrap();
    let fork = application
        .fork_session(
            nested.identity.session_id.as_str(),
            Some("ordinary-fork".to_owned()),
            None,
        )
        .await
        .unwrap();
    let fork_scope = application
        .execution_scope(fork.identity.session_id.as_str())
        .await
        .unwrap();
    assert_ne!(fork_scope, scope);
    let fork_child = child(&application, &fork, "fork-child").await;
    assert_eq!(
        application
            .execution_scope(fork_child.identity.session_id.as_str())
            .await
            .unwrap(),
        fork_scope
    );
    application.delete_session("root").await.unwrap();
    assert_eq!(
        application
            .execution_scope(nested.identity.session_id.as_str())
            .await
            .unwrap(),
        scope
    );
    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data).await;
    assert_eq!(
        reopened
            .execution_scope(grandchild.identity.session_id.as_str())
            .await
            .unwrap(),
        scope
    );
    assert_eq!(
        reopened
            .execution_scope(fork_child.identity.session_id.as_str())
            .await
            .unwrap(),
        fork_scope
    );
    assert!(
        reopened
            .snapshot()
            .await
            .sessions
            .iter()
            .all(|session| session.identity.session_id.as_str() != "root")
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn opening_existing_sessions_initializes_scope_without_executing_them() {
    let directory = tempfile::tempdir().unwrap();
    let workspace_path = directory.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let data = directory.path().join("data");
    let application = open_test_application(data.clone()).await;
    let workspace = application
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let root = application
        .create_session(
            workspace.workspace_id,
            Some("z-legacy-root".to_owned()),
            None,
        )
        .await
        .unwrap();
    let nested = child(&application, &root, "legacy-child").await;
    let expected = application.execution_scope("z-legacy-root").await.unwrap();
    application.shutdown().await.unwrap();
    drop(application);
    let database =
        tokio_rusqlite::rusqlite::Connection::open(data.join("data/db/inbox.sqlite3")).unwrap();
    database
        .execute("DELETE FROM session_execution_scopes", [])
        .unwrap();
    drop(database);
    let reopened = open_test_application(data).await;
    assert_eq!(
        reopened.execution_scope("z-legacy-root").await.unwrap(),
        expected
    );
    assert_eq!(
        reopened
            .execution_scope(nested.identity.session_id.as_str())
            .await
            .unwrap(),
        expected
    );
    assert!(reopened.live.read().await.is_empty());
    assert!(
        reopened
            .events(nested.identity.session_id.as_str())
            .await
            .unwrap()
            .is_empty()
    );
    reopened.shutdown().await.unwrap();
}
