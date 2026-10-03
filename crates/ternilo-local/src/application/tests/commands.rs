use super::*;

#[tokio::test]
async fn feedback_command_is_durable_and_never_opens_a_model_turn() {
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

    let accepted = application
        .record_command_feedback(session_id, "  the diff is unreadable  ".to_owned())
        .await
        .unwrap();
    assert_eq!(accepted.events.len(), 3);
    assert!(matches!(
        &accepted.events[1].kind,
        SessionEventKind::FeedbackSubmitted { text, .. } if text == "the diff is unreadable"
    ));
    assert!(matches!(
        &accepted.events[2].kind,
        SessionEventKind::CommandFinished { outcome, .. }
            if outcome.kind == SessionCommandOutcomeKind::Success
                && outcome.code == "feedback_recorded"
    ));
    let rejected = application
        .record_command_feedback(session_id, " \n\t ".to_owned())
        .await
        .unwrap();
    assert_eq!(rejected.events.len(), 2);
    assert!(matches!(
        &rejected.events[1].kind,
        SessionEventKind::CommandFinished { outcome, .. }
            if outcome.kind == SessionCommandOutcomeKind::Error
                && outcome.code == "feedback_text_required"
    ));
    assert!(
        application
            .events(session_id)
            .await
            .unwrap()
            .iter()
            .all(|event| !matches!(
                event.kind,
                SessionEventKind::TurnStarted | SessionEventKind::UserMessage { .. }
            ))
    );
    let managed = application
        .live
        .read()
        .await
        .get(session_id)
        .cloned()
        .unwrap();
    assert_eq!(managed.harness.active_run().await, None);

    application.shutdown().await.unwrap();
    drop(application);
    let reopened = open_test_application(data_dir.clone()).await;
    assert_eq!(reopened.events(session_id).await.unwrap().len(), 5);
    reopened.shutdown().await.unwrap();
    drop(reopened);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one provider fixture compares direct command success, errors, and canonical events"
)]
async fn direct_commands_bypass_a_real_provider_and_keep_canonical_outcomes() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    tokio::fs::write(workspace_dir.join("note.txt"), "direct provider bypass")
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            observed.fetch_add(1, AtomicOrdering::SeqCst);
            let mut request = vec![0_u8; 32 * 1024];
            let _ = stream.read(&mut request).await.unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"provider reached\"}}]}\n\n",
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
    application
        .upsert_provider_profile(ProviderProfile {
            hosted_tools: None,
            id: "direct-fixture".to_owned(),
            display_name: "Direct Fixture".to_owned(),
            base_url: format!("http://{address}/v1"),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            api_key_ref: None,
            defaults: ProviderModelDefaults {
                context_window: 128_000,
                max_output_tokens: 8_192,
                reasoning: None,
            },
            models: vec![ProviderModel {
                id: "fixture-model".to_owned(),
                display_name: None,
                settings: ProviderModelSettings::Inherit,
            }],
            timeout_ms: 5_000,
            max_attempts: 1,
            retry_base_delay_ms: 10,
        })
        .await
        .unwrap();
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
        .update_model(
            session_id,
            ModelSelection::NamedProvider {
                provider_id: "direct-fixture".to_owned(),
                model: "fixture-model".to_owned(),
                reasoning_effort: None,
            },
        )
        .await
        .unwrap();

    let read = application
        .run_turn(
            session_id,
            Some("direct-read".to_owned()),
            "/read note.txt".to_owned(),
        )
        .await
        .unwrap();
    assert!(read.answer.contains("direct provider bypass"));
    let goal = application
        .run_turn(
            session_id,
            Some("direct-goal".to_owned()),
            "/goal edit ship direct commands".to_owned(),
        )
        .await
        .unwrap();
    assert!(!goal.answer.is_empty());
    for (run_id, command) in [
        ("direct-goal-blocked", "/goal blocked ship direct commands"),
        ("direct-goal-edit", "/goal edit ship direct commands"),
        (
            "direct-goal-complete",
            "/goal complete ship direct commands",
        ),
    ] {
        let outcome = application
            .run_turn(session_id, Some(run_id.to_owned()), command.to_owned())
            .await
            .unwrap();
        assert!(!outcome.answer.is_empty());
    }
    let compact = application
        .run_turn(
            session_id,
            Some("direct-compact".to_owned()),
            "/compact".to_owned(),
        )
        .await
        .unwrap();
    assert!(compact.answer.contains("through_seq"));
    assert_eq!(requests.load(AtomicOrdering::SeqCst), 0);

    let events = application.events(session_id).await.unwrap();
    let direct_compaction = events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::ContextCompactionStarted {
                compaction_id,
                automatic,
                source_command_id,
                turn,
            } if event.run_id.as_str() == "direct-compact" => Some((
                compaction_id.clone(),
                *automatic,
                source_command_id.clone(),
                *turn,
            )),
            _ => None,
        })
        .expect("direct /compact emits a lifecycle start");
    assert!(!direct_compaction.1);
    assert_eq!(
        direct_compaction.2.as_deref(),
        Some("direct-direct-compact")
    );
    assert!(direct_compaction.3 > 0);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::ContextCompacted { compaction_id, compaction }
            if event.run_id.as_str() == "direct-compact"
                && compaction_id == &direct_compaction.0
                && !compaction.automatic
    )));
    for run in [
        "direct-read",
        "direct-goal",
        "direct-goal-blocked",
        "direct-goal-edit",
        "direct-goal-complete",
        "direct-compact",
    ] {
        let run_events = events
            .iter()
            .filter(|event| event.run_id.as_str() == run)
            .collect::<Vec<_>>();
        assert!(
            run_events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::CommandStarted { .. }))
        );
        assert!(
            run_events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::ToolCallFinished { .. }))
        );
        assert!(
            run_events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::CommandFinished { .. }))
        );
        assert!(
            run_events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
        );
        assert!(
            !run_events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }))
        );
    }
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::GoalUpdated { status, .. } => Some(*status),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![
            GoalStatus::Active,
            GoalStatus::Blocked,
            GoalStatus::Active,
            GoalStatus::Complete,
        ]
    );

    let normal = application
        .run_turn(
            session_id,
            Some("normal-provider".to_owned()),
            "hello".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(normal.answer, "provider reached");
    assert_eq!(requests.load(AtomicOrdering::SeqCst), 1);

    application.shutdown().await.unwrap();
    drop(application);
    server.abort();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
