use super::*;
use ternilo_protocol::{
    AgentId, ErrorCode, PermissionPreset, SessionIdentity, SessionMode, SubagentId,
    SubagentSessionMetadata, SubagentTranscriptKind, TenantId, UserId, WorkspaceId,
};

fn session(id: &str, parent: Option<&str>, subagent: bool) -> LocalSession {
    LocalSession {
        server_model: None,
        identity: SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("local-user"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new(id),
        },
        workspace_id: WorkspaceId::new("workspace"),
        workspace_path: "/workspace".to_owned(),
        parent_session_id: parent.map(SessionId::new),
        subagent: subagent.then(|| SubagentSessionMetadata {
            subagent_id: SubagentId::new(format!("agent-{id}")),
            provider: "in-process".to_owned(),
            transcript_kind: SubagentTranscriptKind::Conversation,
        }),
        title: id.to_owned(),
        archived_at_ms: None,
        blank: true,
        permissions: PermissionPreset::WorkspaceWrite,
        model: crate::ModelSelection::ProfileDefault,
        agent_preset: "standard".to_owned(),
        preset_plugins: Vec::new(),
        profile_plugins: Vec::new(),
        mode: SessionMode::Execute,
        created_at_ms: 1,
        updated_at_ms: 1,
    }
}

async fn open(path: &std::path::Path) -> LocalInboxStore {
    let (invalidations, _) = tokio::sync::broadcast::channel(8);
    LocalInboxStore::open(path, invalidations).await.unwrap()
}

#[tokio::test]
async fn registry_preserves_scope_after_parent_deletion_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inbox.sqlite3");
    let store = open(&path).await;
    let sessions = [
        session("a-grandchild", Some("b-child"), true),
        session("b-child", Some("z-root"), true),
        session("ordinary-fork", Some("b-child"), false),
        session("z-root", None, false),
    ];
    store.initialize_execution_scopes(&sessions).await.unwrap();
    let scope = store.execution_scope("z-root").await.unwrap();
    assert_eq!(scope, format!("local:{}:z-root", store.stream_id));
    assert_eq!(store.execution_scope("a-grandchild").await.unwrap(), scope);
    assert_eq!(store.execution_scope("b-child").await.unwrap(), scope);
    assert_ne!(store.execution_scope("ordinary-fork").await.unwrap(), scope);
    store.remove_session("z-root").await.unwrap();
    assert_eq!(store.execution_scope("z-root").await.unwrap(), scope);
    drop(store);
    let reopened = open(&path).await;
    reopened
        .initialize_execution_scopes(&sessions[..3])
        .await
        .unwrap();
    assert_eq!(
        reopened.execution_scope("a-grandchild").await.unwrap(),
        scope
    );
    assert_eq!(
        reopened
            .register_execution_scope(&session("late-child", Some("z-root"), true))
            .await
            .unwrap(),
        scope
    );
}

#[tokio::test]
async fn immutable_parent_and_child_kind_are_checked_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory.path().join("inbox.sqlite3")).await;
    let original = [
        session("root", None, false),
        session("child", Some("root"), true),
    ];
    store.initialize_execution_scopes(&original).await.unwrap();
    let scope = store.execution_scope("child").await.unwrap();
    for replacement in [
        session("child", Some("someone-else"), true),
        session("child", Some("root"), false),
    ] {
        let error = store
            .initialize_execution_scopes(&[session("a-must-rollback", None, false), replacement])
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(store.execution_scope("a-must-rollback").await.is_err());
        assert_eq!(store.execution_scope("child").await.unwrap(), scope);
    }
}

#[tokio::test]
async fn ancient_orphans_keep_independent_scopes_even_if_parent_later_appears() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(&directory.path().join("inbox.sqlite3")).await;
    let child = session("orphan", Some("missing-parent"), true);
    let sibling = session("other-orphan", Some("missing-parent"), true);
    store
        .initialize_execution_scopes(&[child.clone(), sibling.clone()])
        .await
        .unwrap();
    let original = store.execution_scope("orphan").await.unwrap();
    assert_ne!(
        store.execution_scope("other-orphan").await.unwrap(),
        original
    );
    store
        .initialize_execution_scopes(&[child, sibling, session("missing-parent", None, false)])
        .await
        .unwrap();
    assert_eq!(store.execution_scope("orphan").await.unwrap(), original);
    assert_ne!(
        store.execution_scope("missing-parent").await.unwrap(),
        original
    );
}

#[tokio::test]
async fn separate_data_roots_do_not_share_scopes_for_identical_session_ids() {
    let directory = tempfile::tempdir().unwrap();
    let first = open(&directory.path().join("first.sqlite3")).await;
    let second = open(&directory.path().join("second.sqlite3")).await;
    let root = session("same-session", None, false);
    let first_scope = first.register_execution_scope(&root).await.unwrap();
    let second_scope = second.register_execution_scope(&root).await.unwrap();
    assert_ne!(first_scope, second_scope);
    assert!(first.execution_scope("unknown-session").await.is_err());
}
