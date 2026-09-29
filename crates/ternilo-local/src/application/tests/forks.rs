use super::super::sessions::{increased_fork_title, is_terminal_turn_event};
use super::*;

#[test]
fn fork_titles_follow_the_durable_dsh_suffix_rule() {
    assert_eq!(increased_fork_title("Roadmap"), "Roadmap (1)");
    assert_eq!(increased_fork_title("Roadmap (1)"), "Roadmap (2)");
    assert_eq!(increased_fork_title("计划（1）"), "计划（2）");
    assert_eq!(increased_fork_title("计划 （9）"), "计划 （10）");
    assert_eq!(increased_fork_title("Large (999)"), "Large (1000)");
    assert_eq!(increased_fork_title("Leading (009)"), "Leading (10)");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "fork and archive assertions compare a single durable conversation across restart"
)]
async fn fork_copies_only_a_complete_turn_prefix_and_archive_keeps_it_durable() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let source = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let source_id = source.identity.session_id.as_str().to_owned();
    assert!(source.blank);
    assert!(
        application
            .run_turn(&source_id, None, "   ".to_owned())
            .await
            .is_err()
    );
    assert!(application.state.session(&source_id).await.unwrap().blank);

    application
        .run_turn(&source_id, None, "/code \"first\"".to_owned())
        .await
        .unwrap();
    assert!(!application.state.session(&source_id).await.unwrap().blank);
    let first_events = application.events(&source_id).await.unwrap();
    let first_terminal = first_events
        .iter()
        .rposition(|event| is_terminal_turn_event(&event.kind))
        .unwrap();
    application
        .run_turn(&source_id, None, "/code \"second\"".to_owned())
        .await
        .unwrap();
    application
        .update_title(&source_id, "计划（1）".to_owned())
        .await
        .unwrap();
    let settled_events = application.events(&source_id).await.unwrap();
    let second_terminal = settled_events
        .iter()
        .rposition(|event| is_terminal_turn_event(&event.kind))
        .unwrap();
    assert!(second_terminal > first_terminal);

    let first_turn_child = application
        .fork_session_at(&source_id, Some(0), None, None)
        .await
        .unwrap();
    assert_eq!(first_turn_child.title, "计划（2）");
    assert!(!first_turn_child.blank);
    assert_eq!(first_turn_child.archived_at_ms, None);
    let first_turn_child_id = first_turn_child.identity.session_id.as_str();
    assert!(
        !application
            .live
            .read()
            .await
            .contains_key(first_turn_child_id)
    );
    assert_eq!(
        application
            .events_after(first_turn_child_id, None)
            .await
            .unwrap(),
        settled_events[..=first_terminal]
    );
    assert!(
        !application
            .live
            .read()
            .await
            .contains_key(first_turn_child_id)
    );
    let fork_search_hits = application
        .search_sessions(SessionSearchRequest {
            query: "first".to_owned(),
            session_id: Some(first_turn_child.identity.session_id.clone()),
            workspace_id: None,
            filters: ternilo_protocol::SessionSearchFilters::default(),
            limit: 20,
        })
        .await
        .unwrap();
    assert!(fork_search_hits.iter().any(|hit| {
        hit.session_id == first_turn_child.identity.session_id && hit.excerpt.contains("first")
    }));

    let second_turn_anchor = settled_events[first_terminal].seq.checked_add(1).unwrap();
    let anchored_child = application
        .fork_session_at(&source_id, Some(second_turn_anchor), None, None)
        .await
        .unwrap();
    assert_eq!(
        application
            .events(anchored_child.identity.session_id.as_str())
            .await
            .unwrap(),
        settled_events[..=second_terminal]
    );

    let managed = application.ensure_session_locked(&source_id).await.unwrap();
    let unfinished = managed
        .harness
        .append_event(RunId::new("unfinished-run"), SessionEventKind::TurnStarted)
        .await
        .unwrap();
    let session_count = application.snapshot().await.sessions.len();
    let error = application
        .fork_session_at(&source_id, Some(unfinished.seq), None, None)
        .await
        .unwrap_err();
    assert!(error.message.contains("has not completed"), "{error}");
    assert_eq!(application.snapshot().await.sessions.len(), session_count);

    let latest_child = application
        .fork_session(&source_id, None, None)
        .await
        .unwrap();
    assert_eq!(
        latest_child.parent_session_id.as_ref(),
        Some(&source.identity.session_id)
    );
    assert_eq!(&latest_child.workspace_id, &workspace.workspace_id);
    let latest_child_id = latest_child.identity.session_id.as_str().to_owned();
    assert_eq!(
        application.events(&latest_child_id).await.unwrap(),
        settled_events[..=second_terminal]
    );
    application
        .run_turn(
            &latest_child_id,
            None,
            "/code \"child continues\"".to_owned(),
        )
        .await
        .unwrap();
    let continued = application.events(&latest_child_id).await.unwrap();
    assert_eq!(
        continued[..=second_terminal],
        settled_events[..=second_terminal]
    );
    assert_eq!(
        continued[second_terminal + 1].seq,
        u64::try_from(second_terminal + 1).unwrap()
    );

    let source_log = JsonlEventStore::new(
        &application.state.sessions_dir(),
        &source.identity.session_id,
    );
    let before_archive = source_log.load_events().await.unwrap();
    let archived = application.archive_session(&source_id).await.unwrap();
    assert!(archived.archived_at_ms.is_some());
    assert!(!application.live.read().await.contains_key(&source_id));
    assert_eq!(source_log.load_events().await.unwrap(), before_archive);
    assert_eq!(application.snapshot().await.workspaces, vec![workspace]);
    assert!(
        application
            .snapshot()
            .await
            .sessions
            .iter()
            .any(|session| session.identity.session_id.as_str() == source_id)
    );
    assert_eq!(
        application
            .archive_session(&source_id)
            .await
            .unwrap()
            .archived_at_ms,
        archived.archived_at_ms
    );

    application.close().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir.clone()).await;
    let restored = reopened.state.session(&source_id).await.unwrap();
    assert_eq!(restored.archived_at_ms, archived.archived_at_ms);
    assert!(!restored.blank);
    let restored_child = reopened.state.session(&latest_child_id).await.unwrap();
    assert_eq!(
        restored_child.parent_session_id,
        Some(restored.identity.session_id)
    );
    assert_eq!(reopened.events(&latest_child_id).await.unwrap(), continued);
    reopened.close().await.unwrap();
    drop(reopened);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn fork_opens_a_long_history_without_per_event_index_transactions() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let source = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let run_id = RunId::new("long-fork");
    let mut seed = (0_u64..5_000)
        .map(|seq| SessionEvent {
            seq,
            occurred_at_ms: seq,
            run_id: run_id.clone(),
            kind: SessionEventKind::AssistantMessageDelta {
                step: 1,
                delta: format!("history-{seq:05}-{}", "x".repeat(192)),
            },
        })
        .collect::<Vec<_>>();
    seed.push(SessionEvent {
        seq: 5_000,
        occurred_at_ms: 5_000,
        run_id,
        kind: SessionEventKind::TurnFinished {
            answer: "done".to_owned(),
            finish_reason: ternilo_protocol::TurnFinishReason::Completed,
        },
    });
    JsonlEventStore::new(
        &application.state.sessions_dir(),
        &source.identity.session_id,
    )
    .seed_events(&seed)
    .await
    .unwrap();

    let started = std::time::Instant::now();
    let child = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        application.fork_session(source.identity.session_id.as_str(), None, None),
    )
    .await
    .expect("long-history fork exceeded its interactive budget")
    .unwrap();
    let fork_elapsed = started.elapsed();

    let hydration_started = std::time::Instant::now();
    let copied = application
        .events_after(child.identity.session_id.as_str(), None)
        .await
        .unwrap();
    let hydration_elapsed = hydration_started.elapsed();
    eprintln!(
        "long fork: events={} fork_ms={} hydrate_ms={}",
        copied.len(),
        fork_elapsed.as_millis(),
        hydration_elapsed.as_millis(),
    );
    assert_eq!(copied, seed);

    application.close().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
