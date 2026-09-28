use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the ACP process fixture verifies registration, execution, and persisted lifecycle"
)]
async fn external_acp_subagent_uses_the_shared_provider_registry() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let fixture = workspace_dir.join("acp_fixture.py");
    tokio::fs::write(
        &fixture,
        r#"import json
import sys

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1}
    elif method == "session/new":
        result = {"sessionId": "fixture-session"}
    elif method == "session/prompt":
        prompt = "".join(
            block.get("text", "")
            for block in frame["params"]["prompt"]
            if block.get("type") == "text"
        )
        update = {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "fixture-session",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "external:" + prompt},
                },
            },
        }
        print(json.dumps(update, separators=(",", ":")), flush=True)
        result = {"stopReason": "end_turn"}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}, separators=(",", ":")), flush=True)
"#,
    )
    .await
    .unwrap();

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
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "fixture-acp-subagent".to_owned(),
                kind: ternilo_builtins::ACP_SUBAGENT_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "providerName": "fixture",
                    "command": "python3",
                    "args": [fixture],
                    "permission": "reject",
                }),
            }],
        )
        .await
        .unwrap();
    let outcome = run_approved_turn(
        &application,
        session_id,
        "/agent-on fixture delegated-task",
        "spawn_agent",
    )
    .await;
    assert!(outcome.answer.contains("external:delegated-task"));
    let events = application.events(session_id).await.unwrap();
    let subagent = events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::SubagentUpdated { subagent }
                if subagent.provider == "fixture"
                    && subagent.status == ternilo_protocol::SubagentStatus::Idle
                    && subagent.output.as_deref() == Some("external:delegated-task") =>
            {
                Some(subagent.clone())
            }
            _ => None,
        })
        .expect("ACP subagent completed");
    assert_eq!(
        subagent.transcript_kind,
        ternilo_protocol::SubagentTranscriptKind::ProcessLifecycle
    );
    let lifecycle_session_id = subagent
        .session_id
        .clone()
        .expect("ACP subagent exposes its lifecycle Session");
    let lifecycle_session = application
        .snapshot()
        .await
        .sessions
        .into_iter()
        .find(|session| session.identity.session_id == lifecycle_session_id)
        .expect("ACP lifecycle Session is persisted");
    assert_eq!(
        lifecycle_session.parent_session_id.as_ref(),
        Some(&session.identity.session_id)
    );
    assert_eq!(
        lifecycle_session
            .subagent
            .as_ref()
            .map(|metadata| metadata.transcript_kind),
        Some(ternilo_protocol::SubagentTranscriptKind::ProcessLifecycle)
    );
    let lifecycle_events = application
        .events(lifecycle_session_id.as_str())
        .await
        .unwrap();
    assert!(lifecycle_events.len() >= 2);
    assert!(
        lifecycle_events
            .iter()
            .all(|event| matches!(event.kind, SessionEventKind::SubagentUpdated { .. }))
    );

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one follow-up scenario compares the owner and child conversation histories"
)]
async fn addressed_subagent_followup_does_not_create_an_owner_turn() {
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

    let outcome = run_approved_turn(
        &application,
        session_id,
        "/agent remember-first-pass",
        "spawn_agent",
    )
    .await;
    let subagent = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(subagent) = application
                .events(session_id)
                .await
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::SubagentUpdated { subagent }
                        if subagent.status == ternilo_protocol::SubagentStatus::Idle =>
                    {
                        Some(subagent)
                    }
                    _ => None,
                })
            {
                break subagent;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("foreground in-process subagent became idle; {outcome:?}"));
    let before = application.events(session_id).await.unwrap();
    assert!(subagent.supports_followup);
    let turns_before = before
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
        .count();

    let accepted = application
        .followup_subagent_with_provenance(
            session_id,
            subagent.subagent_id.clone(),
            "second-pass".to_owned(),
            InputProvenance {
                run_id: None,
                input_id: SubmissionId::new("human-child-followup"),
                author: InputAuthor::Account {
                    user_id: UserId::new("participant"),
                    username: "participant".to_owned(),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(accepted.status, ternilo_protocol::SubagentStatus::Running);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let completed = application
                .events(session_id)
                .await
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::SubagentUpdated { subagent }
                        if subagent.subagent_id == accepted.subagent_id
                            && subagent.status == ternilo_protocol::SubagentStatus::Idle
                            && subagent.output.as_deref() == Some("ternilo: second-pass") =>
                    {
                        Some(subagent)
                    }
                    _ => None,
                });
            if let Some(completed) = completed {
                assert_eq!(completed.output.as_deref(), Some("ternilo: second-pass"));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("addressed follow-up completed");

    let after = application.events(session_id).await.unwrap();
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
            .count(),
        turns_before
    );
    assert!(!after.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::UserMessage { content, .. }
            if content.starts_with("/agent-send ")
    )));

    let child_messages = application
        .events(subagent.session_id.as_ref().unwrap().as_str())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            SessionEventKind::UserMessage { provenance, .. } => provenance,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(child_messages.len(), 2);
    assert_eq!(
        child_messages[0].author,
        InputAuthor::Automation {
            source: ternilo_protocol::AutomatedInputSource::Subagent
        }
    );
    assert_eq!(
        child_messages[1].input_id,
        SubmissionId::new("human-child-followup")
    );
    assert_eq!(
        child_messages[1].author,
        InputAuthor::Account {
            user_id: UserId::new("participant"),
            username: "participant".to_owned()
        }
    );

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the scenario verifies multiple conversation generations and restart persistence"
)]
async fn in_process_subagents_are_persistent_recursive_conversation_sessions() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let root = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let root_id = root.identity.session_id.clone();

    run_approved_turn(
        &application,
        root_id.as_str(),
        "/agent first-child",
        "spawn_agent",
    )
    .await;
    let child = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(snapshot) = application
                .events(root_id.as_str())
                .await
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::SubagentUpdated { subagent }
                        if subagent.status == ternilo_protocol::SubagentStatus::Idle =>
                    {
                        Some(subagent)
                    }
                    _ => None,
                })
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("child conversation completed");
    let child_id = child
        .session_id
        .clone()
        .expect("in-process subagent exposes its canonical Session id");
    assert_ne!(child_id.as_str(), child.subagent_id.as_str());
    assert_eq!(
        child.transcript_kind,
        ternilo_protocol::SubagentTranscriptKind::Conversation
    );

    let state = application.snapshot().await;
    let persisted_child = state
        .sessions
        .iter()
        .find(|session| session.identity.session_id == child_id)
        .expect("child Session is persisted");
    assert_eq!(persisted_child.parent_session_id.as_ref(), Some(&root_id));
    assert_eq!(
        persisted_child
            .subagent
            .as_ref()
            .map(|metadata| &metadata.subagent_id),
        Some(&child.subagent_id)
    );
    let child_events = application.events(child_id.as_str()).await.unwrap();
    assert!(child_events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::UserMessage { content, .. } if content == "first-child"
    )));
    assert!(
        child_events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::AssistantMessage { .. }))
    );
    assert!(
        child_events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
    );

    run_approved_turn(
        &application,
        child_id.as_str(),
        "/agent nested-child",
        "spawn_agent",
    )
    .await;
    let grandchild = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if let Some(snapshot) = application
                .events(child_id.as_str())
                .await
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::SubagentUpdated { subagent }
                        if subagent.task == "nested-child"
                            && subagent.status == ternilo_protocol::SubagentStatus::Idle =>
                    {
                        Some(subagent)
                    }
                    _ => None,
                })
            {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("nested child conversation completed");
    let grandchild_id = grandchild
        .session_id
        .clone()
        .expect("nested child exposes its canonical Session id");
    let state = application.snapshot().await;
    let persisted_grandchild = state
        .sessions
        .iter()
        .find(|session| session.identity.session_id == grandchild_id)
        .expect("nested child Session is persisted");
    assert_eq!(
        persisted_grandchild.parent_session_id.as_ref(),
        Some(&child_id)
    );
    let root_trace = application.trace_session(root_id.clone()).await.unwrap();
    assert!(root_trace.descendant_session_ids.contains(&child_id));
    assert!(root_trace.descendant_session_ids.contains(&grandchild_id));
    assert_eq!(
        application
            .trace_session(child_id.clone())
            .await
            .unwrap()
            .descendant_session_ids,
        vec![grandchild_id.clone()]
    );

    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir.clone()).await;
    let restored = reopened.snapshot().await;
    assert!(restored.sessions.iter().any(|session| {
        session.identity.session_id == child_id
            && session.parent_session_id.as_ref() == Some(&root_id)
    }));
    assert!(restored.sessions.iter().any(|session| {
        session.identity.session_id == grandchild_id
            && session.parent_session_id.as_ref() == Some(&child_id)
    }));
    assert!(
        reopened
            .events(child_id.as_str())
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::AssistantMessage { .. }))
    );
    reopened.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn equal_subagent_ids_under_different_parents_get_distinct_sessions() {
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
    let request = SubagentSessionRequest {
        subagent_id: SubagentId::new("agent-session-local-id"),
        provider: "in-process".to_owned(),
        label: "worker".to_owned(),
        task: "inspect".to_owned(),
        transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
    };
    let host = application.subagent_session_host();
    let first_child = host
        .create(first.identity.clone(), request.clone())
        .await
        .unwrap()
        .unwrap();
    let second_child = host
        .create(second.identity.clone(), request)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first_child.session_id, second_child.session_id);

    let state = application.snapshot().await;
    assert!(state.sessions.iter().any(|session| {
        session.identity.session_id == first_child.session_id
            && session.parent_session_id.as_ref() == Some(&first.identity.session_id)
    }));
    assert!(state.sessions.iter().any(|session| {
        session.identity.session_id == second_child.session_id
            && session.parent_session_id.as_ref() == Some(&second.identity.session_id)
    }));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "ACP process-group integration test keeps the external process lifecycle together"
)]
async fn interrupting_external_acp_subagent_reaps_its_process_group() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let fixture = workspace_dir.join("slow_acp_fixture.py");
    tokio::fs::write(
        &fixture,
        r#"import json
import os
import sys
import time

with open(".slow-acp.pid", "w", encoding="utf-8") as marker:
    marker.write(str(os.getpid()))

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1}
    elif method == "session/new":
        result = {"sessionId": "slow-session"}
    elif method == "session/prompt":
        time.sleep(30)
        result = {"stopReason": "end_turn"}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}, separators=(",", ":")), flush=True)
"#,
    )
    .await
    .unwrap();

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
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "slow-acp-subagent".to_owned(),
                kind: ternilo_builtins::ACP_SUBAGENT_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "providerName": "slow",
                    "command": "python3",
                    "args": [fixture],
                    "shutdownGraceMs": 100,
                }),
            }],
        )
        .await
        .unwrap();
    run_approved_turn(
        &application,
        session_id,
        "/agent-bg-on slow never-finish",
        "spawn_agent",
    )
    .await;
    let subagent = application
        .events(session_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::SubagentUpdated { subagent } if subagent.provider == "slow" => {
                Some(subagent)
            }
            _ => None,
        })
        .unwrap();
    let marker = workspace_dir.join(".slow-acp.pid");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !tokio::fs::try_exists(&marker).await.unwrap() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = tokio::fs::read_to_string(&marker)
        .await
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let before = application.events(session_id).await.unwrap();
    let turns_before = before
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
        .count();
    let interrupted = application
        .interrupt_subagent(session_id, subagent.subagent_id.clone())
        .await
        .unwrap();
    assert_eq!(
        interrupted.status,
        ternilo_protocol::SubagentStatus::Cancelled
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while tokio::fs::try_exists(format!("/proc/{pid}")).await.unwrap() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        application
            .events(session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                &event.kind,
                SessionEventKind::SubagentUpdated { subagent: snapshot }
                    if snapshot.subagent_id == subagent.subagent_id
                        && snapshot.status == ternilo_protocol::SubagentStatus::Cancelled
            ))
    );
    let after = application.events(session_id).await.unwrap();
    assert_eq!(
        after
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
            .count(),
        turns_before
    );
    assert!(!after.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::UserMessage { content, .. }
            if content.starts_with("/agent-stop ")
    )));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
