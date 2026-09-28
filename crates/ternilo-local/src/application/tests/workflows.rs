use super::*;

#[tokio::test]
async fn workflow_fans_out_pipelines_records_projection_and_disposes_children() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();
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
    let arguments = serde_json::json!({
        "meta": {
            "name": "review-batch",
            "description": "Review two independent inputs",
            "phases": [{ "title": "scan" }, { "title": "verify" }]
        },
        "script": r#"
                phase("scan");
                let scanned = parallel([
                    task("alpha", #{ label: "alpha" }),
                    task("beta", #{ label: "beta" })
                ]);
                phase("verify");
                log("checking both results");
                let checked = pipeline(scanned, [
                    stage("verify {{prev}}", #{ label: "verify" })
                ]);
                #{ scanned: scanned, checked: checked }
            "#,
        "args": {}
    });
    let outcome = run_approved_turn(
        &application,
        session_id,
        format!("/workflow {arguments}"),
        "workflow",
    )
    .await;
    assert!(outcome.answer.contains("\"agents_started\": 4"));
    assert!(outcome.answer.contains("ternilo: alpha"));
    assert!(outcome.answer.contains("verify ternilo: beta"));

    let events = application.events(session_id).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::WorkflowAgentStarted { .. }))
            .count(),
        4
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::WorkflowAgentFinished { .. }))
            .count(),
        4
    );
    assert!(events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::WorkflowRunFinished {
            stop_reason: ternilo_protocol::WorkflowStopReason::Completed,
            agents_started: 4,
            ..
        }
    )));
    let projection = application
        .session_projection(session.identity.session_id.clone())
        .await
        .unwrap();
    let workflows = projection.values.get("workflows").unwrap();
    assert_eq!(
        workflows["runs"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["status"],
        "completed"
    );
    let listed = application
        .run_turn(session_id, None, "/agents".to_owned())
        .await
        .unwrap();
    assert!(listed.answer.contains("[]"), "{}", listed.answer);

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "workflow cancellation scenario keeps child lifecycle assertions together"
)]
async fn cancelling_a_workflow_stops_and_disposes_its_active_child() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();
    let application = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    let run_id = "workflow-cancel-test";
    let arguments = serde_json::json!({
        "meta": {
            "name": "slow-child",
            "description": "Exercise workflow cancellation"
        },
        "script": r#"agent("/shell sleep 30")"#,
        "args": {}
    });
    let running = {
        let application = application.clone();
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(
                    &session_id,
                    Some(run_id.to_owned()),
                    format!("/workflow {arguments}"),
                )
                .await
        })
    };
    let approval = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(question) = application
                .pending_questions(Some(&session_id))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("workflow requested approval");
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: approval.question.id,
            selected: vec!["Allow once".to_owned()],
            custom: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if application
                .events(&session_id)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::WorkflowAgentStarted { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("workflow started its child");
    application.cancel_turn(&session_id, run_id).await.unwrap();
    assert!(running.await.unwrap().unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if application
                .events(&session_id)
                .await
                .unwrap()
                .iter()
                .any(|event| {
                    matches!(
                        event.kind,
                        SessionEventKind::WorkflowRunFinished {
                            stop_reason: ternilo_protocol::WorkflowStopReason::Cancelled,
                            ..
                        }
                    )
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled workflow reached a durable terminal event");

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn workflow_wall_limit_includes_time_waiting_for_a_child() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await.unwrap();
    let mut profile = crate::local_profile();
    profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "workflow-engine")
        .unwrap()
        .config = serde_json::json!({ "max_wall_ms": 75 });
    let application = Arc::new(
        LocalApplication::open(
            crate::catalog().unwrap(),
            profile,
            HostPolicy::local(RunLimits::default()),
            data_dir.clone(),
        )
        .await
        .unwrap(),
    );
    let workspace = application
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    let arguments = serde_json::json!({
        "meta": {
            "name": "wall-limit",
            "description": "Ensure child waits share the workflow deadline"
        },
        "script": r#"agent("/shell sleep 30")"#,
        "args": {}
    });
    let started = tokio::time::Instant::now();
    let outcome = run_approved_turn(
        &application,
        session_id,
        format!("/workflow {arguments}"),
        "workflow",
    )
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(outcome.answer.contains("max_wall_ms (75)"));
    let events = application.events(session_id).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::WorkflowRunFinished {
            stop_reason: ternilo_protocol::WorkflowStopReason::Error,
            error: Some(error),
            ..
        } if error.contains("max_wall_ms (75)")
    )));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let events = application.events(session_id).await.unwrap();
            if events.iter().any(|event| {
                matches!(
                    event.kind,
                    SessionEventKind::SubagentUpdated { ref subagent }
                        if subagent.status == ternilo_protocol::SubagentStatus::Cancelled
                )
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a child still starting at the workflow deadline is cancelled durably");
    let managed = application
        .live
        .read()
        .await
        .get(session_id)
        .cloned()
        .unwrap();
    assert!(managed.harness.subagents().unwrap().list().await.is_empty());

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the scenario creates a schedule, restarts the host, and waits for its completed turn"
)]
async fn durable_schedule_replays_after_restart_and_dispatches_into_the_session() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let first = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = first
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = first
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    let scheduled = {
        let first = Arc::clone(&first);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            first
                .run_turn(
                    &session_id,
                    None,
                    "/schedule-after 2 replay this durable reminder".to_owned(),
                )
                .await
        })
    };
    let approval = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Some(question) = first
                .pending_questions(Some(&session_id))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        approval
            .question
            .tool_approval
            .as_ref()
            .map(|context| context.tool_name.as_str()),
        Some("schedule_create")
    );
    first
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: approval.question.id,
            selected: vec!["Allow once".to_owned()],
            custom: None,
        })
        .await
        .unwrap();
    scheduled.await.unwrap().unwrap();
    let creator = first
        .events(&session_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::UserMessage { provenance, .. } => Some(provenance),
            _ => None,
        })
        .unwrap();
    first
        .run_turn(&session_id, None, "/code \"later input\"".to_owned())
        .await
        .unwrap();
    let before_restart = first.events(&session_id).await.unwrap().last().unwrap().seq;
    first.shutdown().await.unwrap();
    drop(first);

    let restored = open_test_application(data_dir.clone()).await;
    restored.events(&session_id).await.unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let events = restored.events(&session_id).await.unwrap();
            let dispatched = events.iter().any(|event| {
                matches!(
                    event.kind,
                    SessionEventKind::ScheduleChanged {
                        change: ternilo_protocol::ScheduleChange::Dispatch { .. }
                    }
                )
            });
            let delivered_run = events.iter().find_map(|event| match &event.kind {
                SessionEventKind::UserMessage { content, .. }
                    if event.seq > before_restart
                        && content.contains("replay this durable reminder") =>
                {
                    Some(&event.run_id)
                }
                _ => None,
            });
            let completed = delivered_run.is_some_and(|run_id| {
                events.iter().any(|event| {
                    &event.run_id == run_id
                        && matches!(event.kind, SessionEventKind::TurnFinished { .. })
                })
            });
            if dispatched && completed {
                break events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
    );

    assert!(events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::UserMessage {
            provenance: Some(InputProvenance {
                author: InputAuthor::Automation {
                    source: ternilo_protocol::AutomatedInputSource::Schedule
                },
                ..
            }),
            ..
        }
    )));

    let dispatched = events
        .iter()
        .find(|event| {
            matches!(
                &event.kind,
                SessionEventKind::UserMessage {
                    source: Some(ternilo_protocol::UserMessageSource::Schedule { .. }),
                    ..
                }
            )
        })
        .unwrap();
    let origin = restored
        .model_input_origin(&session_id, &dispatched.run_id)
        .await
        .unwrap();
    assert_eq!(origin.provenance, creator);
    assert_eq!(origin.schedule_origins.len(), 1);
    let proof = &origin.schedule_origins[0];
    assert!(proof.created_seq < before_restart);
    assert!(proof.dispatched_seq > before_restart);
    assert_eq!(proof.session_id, session.identity.session_id);
    restored.shutdown().await.unwrap();
    drop(restored);
    let reopened = open_test_application(data_dir.clone()).await;
    assert!(
        !ternilo_builtins::has_pending_schedules(&reopened.events(&session_id).await.unwrap())
            .unwrap()
    );
    assert_eq!(
        reopened.events(&session_id).await.unwrap().len(),
        events.len()
    );
    reopened.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
