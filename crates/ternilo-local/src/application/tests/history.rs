use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "terminal, child, stats, and feedback assertions share one canonical session log"
)]
async fn persistent_terminal_subagents_stats_and_feedback_share_the_session_log() {
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

    let opened = application
        .run_turn(session_id, None, "/terminal-open test".to_owned())
        .await
        .unwrap();
    let terminal_id = opened
        .events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolCallFinished { name, output, .. } if name == "terminal_open" => {
                serde_json::from_str::<ternilo_protocol::TerminalSnapshot>(&output.content)
                    .ok()
                    .map(|snapshot| snapshot.terminal_id)
            }
            _ => None,
        })
        .expect("terminal_open returned a snapshot");
    application
        .run_turn(
            session_id,
            None,
            format!("/terminal-send {terminal_id} export TERNILO_TEST_VALUE=persistent-ready"),
        )
        .await
        .unwrap();
    let retained = application
        .run_turn(
            session_id,
            None,
            format!(r#"/terminal-send {terminal_id} printf "%s" "$TERNILO_TEST_VALUE""#),
        )
        .await
        .unwrap();
    assert!(
        retained.answer.contains("persistent-ready"),
        "{}",
        retained.answer
    );

    run_approved_turn(
        &application,
        session_id,
        "/agent inspect this delegated task",
        "spawn_agent",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let agents = application
        .run_turn(session_id, None, "/agents".to_owned())
        .await
        .unwrap();
    assert!(agents.answer.contains("inspect this delegated task"));
    assert!(agents.answer.contains("idle"), "{}", agents.answer);

    let model = super::test_model::TestModel::start().await;
    model.install(&application, session_id).await.unwrap();
    application
        .run_turn(session_id, None, "feedback target".to_owned())
        .await
        .unwrap();

    let events = application.events(session_id).await.unwrap();
    let assistant_seq = events
        .iter()
        .find(|event| matches!(event.kind, SessionEventKind::AssistantMessage { .. }))
        .unwrap()
        .seq;
    let feedback = application
        .record_feedback(
            session_id,
            assistant_seq,
            0,
            Some(FeedbackRating::Positive),
            Some("useful".to_owned()),
        )
        .await
        .unwrap();
    assert!(matches!(
        feedback.kind,
        SessionEventKind::FeedbackRecorded { revision: 1, .. }
    ));
    let conflict = application
        .record_feedback(
            session_id,
            assistant_seq,
            0,
            Some(FeedbackRating::Negative),
            Some("concurrent".to_owned()),
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ternilo_protocol::ErrorCode::Conflict);
    let stats = application.stats(session_id).await.unwrap();
    assert!(stats.turns >= 5);
    assert!(stats.tool_calls >= 5);
    let exported = application.export_session(session_id).await.unwrap();
    assert!(
        exported
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::FeedbackRecorded { .. }))
    );

    application
        .run_turn(session_id, None, format!("/terminal-close {terminal_id}"))
        .await
        .unwrap();
    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one model fixture verifies the profile overlay and automatic compaction events"
)]
async fn session_profile_overlay_drives_automatic_context_compaction() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let seed_model = super::test_model::TestModel::start().await;
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 64 * 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"summary\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
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
    application
        .update_profile_plugins(
            session_id,
            vec![PluginEntry {
                id: "context".to_owned(),
                kind: ternilo_builtins::CONTEXT_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "automatic_threshold_percent": 80,
                    "keep_recent_turns": 1,
                    "max_tool_result_chars": 12000
                }),
            }],
        )
        .await
        .unwrap();
    seed_model.install(&application, session_id).await.unwrap();
    application
        .run_turn(session_id, None, "a".repeat(2_000))
        .await
        .unwrap();
    application
        .run_turn(session_id, None, "continue".to_owned())
        .await
        .unwrap();
    assert!(
        !application
            .events(session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                event.kind,
                SessionEventKind::ContextCompactionStarted {
                    automatic: true,
                    ..
                }
            ))
    );
    application
        .upsert_provider_profile(ProviderProfile {
            hosted_tools: None,
            id: "small-context".to_owned(),
            display_name: "Small Context".to_owned(),
            base_url: format!("http://{address}/v1"),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: None,
            defaults: ProviderModelDefaults {
                context_window: 100,
                max_output_tokens: 32,
                reasoning: None,
            },
            models: vec![ProviderModel {
                id: "small-model".to_owned(),
                display_name: None,
                settings: ProviderModelSettings::Inherit,
            }],
            timeout_ms: 5_000,
            max_attempts: 1,
            retry_base_delay_ms: 10,
        })
        .await
        .unwrap();
    application
        .update_model(
            session_id,
            ModelSelection::NamedProvider {
                provider_id: "small-context".to_owned(),
                model: "small-model".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();
    application
        .run_turn(session_id, None, "trigger compaction".to_owned())
        .await
        .unwrap();
    server.await.unwrap();
    let events = application.events(session_id).await.unwrap();
    let automatic_start = events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ContextCompactionStarted {
                compaction_id,
                automatic: true,
                source_command_id,
                turn,
            } => Some((compaction_id.clone(), source_command_id.clone(), *turn)),
            _ => None,
        })
        .expect("automatic compaction emits a lifecycle start");
    assert_eq!(automatic_start.1, None);
    assert!(automatic_start.2 > 1);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::ContextCompacted { compaction_id, compaction }
            if compaction_id == &automatic_start.0
                && compaction.automatic
                && compaction.estimated_tokens_before >= 80
    )));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "archive scenario validates persisted indexing and retrieval as one operation"
)]
async fn session_archive_searches_and_reads_persisted_events() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let application = open_test_application(data_dir.clone()).await;
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.clone();
    application
        .run_turn(
            session_id.as_str(),
            None,
            "/code \"a durable cobalt needle\"".to_owned(),
        )
        .await
        .unwrap();

    let hits = application
        .search_sessions(SessionSearchRequest {
            query: "cobalt needle".to_owned(),
            session_id: None,
            workspace_id: Some(workspace.workspace_id.clone()),
            filters: ternilo_protocol::SessionSearchFilters::default(),
            limit: 20,
        })
        .await
        .unwrap();
    assert!(hits.iter().any(|hit| {
        hit.session_id == session_id
            && hit.event_seq.is_some()
            && hit.excerpt.contains("cobalt needle")
    }));
    let user_hit = application
        .search_sessions(SessionSearchRequest {
            query: "cobalt needle".to_owned(),
            session_id: Some(session_id.clone()),
            workspace_id: None,
            filters: ternilo_protocol::SessionSearchFilters {
                category: Some(ternilo_protocol::SessionEventCategory::User),
                ..Default::default()
            },
            limit: 20,
        })
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("FTS user-category hit");
    assert_eq!(
        user_hit.category,
        Some(ternilo_protocol::SessionEventCategory::User)
    );
    assert!(user_hit.run_id.is_some());
    let wrong_run = application
        .search_sessions(SessionSearchRequest {
            query: "cobalt needle".to_owned(),
            session_id: Some(session_id.clone()),
            workspace_id: None,
            filters: ternilo_protocol::SessionSearchFilters {
                run_id: Some(RunId::new("not-the-recorded-run")),
                ..Default::default()
            },
            limit: 20,
        })
        .await
        .unwrap();
    assert!(wrong_run.is_empty());
    let events = application
        .read_session_events(SessionEventReadRequest {
            session_id: session_id.clone(),
            start_seq: 0,
            limit: 200,
        })
        .await
        .unwrap();
    let trace = application.trace_session(session_id.clone()).await.unwrap();
    assert_eq!(trace.event_count, events.len() as u64);
    assert_eq!(trace.run_count, 1);
    let child = application
        .fork_session(session_id.as_str(), None, Some("reviewer".to_owned()))
        .await
        .unwrap();
    let parent_trace = application.trace_session(session_id.clone()).await.unwrap();
    assert_eq!(
        parent_trace.descendant_session_ids,
        vec![child.identity.session_id.clone()]
    );
    assert_eq!(
        application
            .trace_session(child.identity.session_id)
            .await
            .unwrap()
            .parent_session_id,
        Some(session_id.clone())
    );

    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir.clone()).await;
    let restored_hits = reopened
        .search_sessions(SessionSearchRequest {
            query: "cobalt needle".to_owned(),
            session_id: Some(session_id),
            workspace_id: None,
            filters: ternilo_protocol::SessionSearchFilters::default(),
            limit: 20,
        })
        .await
        .unwrap();
    assert!(restored_hits.iter().any(|hit| hit.event_seq.is_some()));
    reopened.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn recorded_model_replay_reuses_source_responses_without_provider_calls() {
    let model = super::test_model::TestModel::start().await;
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
    model
        .install(&application, source.identity.session_id.as_str())
        .await
        .unwrap();
    let expected = application
        .run_turn(
            source.identity.session_id.as_str(),
            None,
            "record this response".to_owned(),
        )
        .await
        .unwrap();
    let replay = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    application
        .update_profile_plugins(
            replay.identity.session_id.as_str(),
            vec![PluginEntry {
                id: "model".to_owned(),
                kind: ternilo_builtins::MODEL_REPLAY_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({
                    "source_session_id": source.identity.session_id,
                }),
            }],
        )
        .await
        .unwrap();
    drop(model);
    let actual = application
        .run_turn(
            replay.identity.session_id.as_str(),
            None,
            "the provider is intentionally unavailable".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(actual.answer, expected.answer);
    assert!(
        application
            .events(replay.identity.session_id.as_str())
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                &event.kind,
                SessionEventKind::AssistantMessage { response, .. }
                    if response.replayed && response.attempts == 0
            ))
    );

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn bounded_history_matches_active_and_archived_logs_and_checks_archive_state() {
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
    let id = session.identity.session_id.as_str();
    for index in 0..3 {
        application
            .record_command_feedback(id, format!("feedback {index}"))
            .await
            .unwrap();
    }
    let events = application.events(id).await.unwrap();
    let query = ternilo_protocol::SessionHistoryQuery {
        before_seq: None,
        limit: 2,
    };
    let active = application.history(id, query, false).await.unwrap();
    assert_eq!(active.events, events[events.len() - 2..]);
    assert!(application.history(id, query, true).await.is_err());
    let first = application
        .history(
            id,
            ternilo_protocol::SessionHistoryQuery {
                before_seq: active.next_before_seq,
                limit: 200,
            },
            false,
        )
        .await
        .unwrap();
    assert_eq!(first.events, events[..events.len() - 2]);
    assert_eq!(first.next_before_seq, None);
    application.archive_session(id).await.unwrap();
    assert_eq!(application.history(id, query, true).await.unwrap(), active);
    application.restore_session(id).await.unwrap();
    assert!(application.history(id, query, true).await.is_err());
    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
