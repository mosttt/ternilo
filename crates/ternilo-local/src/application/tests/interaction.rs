use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    clippy::items_after_statements,
    reason = "plan review helper belongs to this single end-to-end interaction scenario"
)]
async fn reviewed_plan_exit_requires_approval_and_retires_the_read_only_runtime() {
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
    let session_id = session.identity.session_id.as_str().to_owned();
    application
        .update_mode(&session_id, SessionMode::Plan)
        .await
        .unwrap();

    async fn review(
        application: &Arc<LocalApplication>,
        session_id: &str,
        answer: &str,
    ) -> RunOutcome {
        let running = {
            let application = Arc::clone(application);
            let session_id = session_id.to_owned();
            tokio::spawn(async move {
                application
                    .run_turn(
                        &session_id,
                        None,
                        "/exit-plan # Fixture plan\n\n1. inspect\n2. implement\n3. verify"
                            .to_owned(),
                    )
                    .await
            })
        };
        let pending = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Some(question) = application
                    .pending_questions(Some(session_id))
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
        .expect("exit plan mode opened a review");
        assert!(matches!(
            pending.question.presentation,
            Some(ternilo_protocol::UserQuestionPresentation::PlanReview { .. })
        ));
        application
            .answer_question(ternilo_protocol::UserAnswer {
                question_id: pending.question.id,
                selected: if matches!(answer, "Approve" | "Keep planning") {
                    vec![answer.to_owned()]
                } else {
                    Vec::new()
                },
                custom: (!matches!(answer, "Approve" | "Keep planning")).then(|| answer.to_owned()),
            })
            .await
            .unwrap();
        running.await.unwrap().unwrap()
    }

    let revision = review(&application, &session_id, "Clarify verification").await;
    assert!(revision.events.iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::PlanReviewCompleted {
            approved: false,
            feedback: Some(feedback),
            ..
        } if feedback == "Clarify verification"
    )));
    assert_eq!(
        application
            .snapshot()
            .await
            .sessions
            .into_iter()
            .find(|session| session.identity.session_id.as_str() == session_id)
            .unwrap()
            .mode,
        SessionMode::Plan
    );

    let approved = review(&application, &session_id, "Approve").await;
    assert!(approved.events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::PlanReviewCompleted { approved: true, .. }
    )));
    assert_eq!(
        application
            .snapshot()
            .await
            .sessions
            .into_iter()
            .find(|session| session.identity.session_id.as_str() == session_id)
            .unwrap()
            .mode,
        SessionMode::Execute
    );
    application
        .run_turn(
            &session_id,
            None,
            "/write approved.txt implemented".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(workspace_dir.join("approved.txt"))
            .await
            .unwrap(),
        "implemented"
    );

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn interactive_tool_waits_for_and_records_the_user_answer() {
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
    let session_id = session.identity.session_id.as_str().to_owned();
    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&session_id, None, "/ask Continue?".to_owned())
                .await
        })
    };

    let question = tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
    .unwrap();
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: question.question.id,
            selected: Vec::new(),
            custom: Some("Yes".to_owned()),
        })
        .await
        .unwrap();
    let outcome = running.await.unwrap().unwrap();
    assert_eq!(
        outcome.answer,
        r#"{"answers":[{"id":"question","selected":[],"custom":"Yes"}]}"#
    );
    let events = application.events(&session_id).await.unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        ternilo_protocol::SessionEventKind::UserQuestionAnswered { answer }
            if answer.selected.is_empty() && answer.custom.as_deref() == Some("Yes")
    )));

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn active_turn_can_be_cancelled_and_releases_interaction_state() {
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
    let session_id = session.identity.session_id.as_str().to_owned();
    let run_id = "cancel-active-turn".to_owned();
    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        let run_id = run_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(
                    &session_id,
                    Some(run_id),
                    "/ask Wait for cancellation?".to_owned(),
                )
                .await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !application
                .pending_questions(Some(&session_id))
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    application.cancel_turn(&session_id, &run_id).await.unwrap();
    let error = running.await.unwrap().unwrap_err();
    assert!(error.is_cancelled(), "{error}");
    assert!(
        application
            .pending_questions(Some(&session_id))
            .await
            .is_empty()
    );
    let events = application.events(&session_id).await.unwrap();
    assert!(events.iter().any(|event| {
        event.run_id.as_str() == run_id && matches!(event.kind, SessionEventKind::TurnCancelled)
    }));
    assert_eq!(
        application
            .stats(&session_id)
            .await
            .unwrap()
            .cancelled_turns,
        1
    );
    let next = application
        .run_turn(&session_id, None, "/code \"still usable\"".to_owned())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&next.answer).unwrap()["result"],
        "still usable"
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn lifecycle_update_closes_interaction_without_event_reads_booting_another_runtime() {
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
    let session_id = session.identity.session_id.as_str().to_owned();

    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&session_id, None, "/ask Continue?".to_owned())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !application
                .pending_questions(Some(&session_id))
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let update = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .update_permissions(&session_id, PermissionPreset::ReadOnly)
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !application.live.read().await.contains_key(&session_id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let event_read = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move { application.events(&session_id).await })
    };
    running.await.unwrap().unwrap();
    let updated = update.await.unwrap().unwrap();
    assert_eq!(updated.permissions, PermissionPreset::ReadOnly);
    let events = tokio::time::timeout(std::time::Duration::from_secs(2), event_read)
        .await
        .expect("event read completes after the pending interaction is closed")
        .unwrap()
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::UserQuestionAsked { .. }))
    );
    assert!(
        application
            .pending_questions(Some(&session_id))
            .await
            .is_empty()
    );
    assert!(application.live.read().await.is_empty());
    application
        .run_turn(&session_id, None, "/code \"runtime reopened\"".to_owned())
        .await
        .unwrap();
    assert_eq!(application.live.read().await.len(), 1);

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn plugin_recomposition_cancels_a_pending_interaction_before_joining_the_runtime() {
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
    let session_id = session.identity.session_id.as_str().to_owned();
    let running = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&session_id, None, "/ask Continue this run?".to_owned())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while application
            .pending_questions(Some(&session_id))
            .await
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run opened a pending interaction");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        application.update_profile_plugins(&session_id, Vec::new()),
    )
    .await
    .expect("plugin recomposition must not wait forever on the interaction")
    .unwrap();

    assert!(
        application
            .pending_questions(Some(&session_id))
            .await
            .is_empty()
    );
    running.await.unwrap().unwrap();
    let resumed = application
        .run_turn(&session_id, None, "/code \"runtime restarted\"".to_owned())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&resumed.answer).unwrap()["result"],
        "runtime restarted"
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one FIFO scenario verifies steering settlement, cancellation pause, and resumed order"
)]
async fn authoritative_inbox_interrupts_direct_turn_and_parks_fifo_on_cancel() {
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
    let session_id = session.identity.session_id.as_str().to_owned();

    let active_run_id = "steering-active".to_owned();
    let active = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        let run_id = active_run_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(&session_id, Some(run_id), "/ask Hold this step?".to_owned())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
    .unwrap();
    let steered = application
        .submit_session(
            &session_id,
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Steer,
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: "/code \"steered at the step boundary\"".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(steered.placement, SubmissionPlacement::Queued);
    assert!(active.await.unwrap().unwrap_err().is_cancelled());
    assert!(
        application
            .pending_questions(Some(&session_id))
            .await
            .is_empty()
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if application
                .session_inbox(&session_id)
                .await
                .unwrap()
                .items
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        application
            .events(&session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| {
                matches!(
                    &event.kind,
                    SessionEventKind::UserMessage {
                        source: Some(UserMessageSource::Submission {
                            submission_id,
                            delivery: SubmissionDelivery::Queue,
                            ..
                        }),
                        ..
                    } if submission_id == &steered.id
                )
            })
    );

    let cancelled_run_id = "queue-cancel".to_owned();
    let cancelled = {
        let application = Arc::clone(&application);
        let session_id = session_id.clone();
        let run_id = cancelled_run_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(
                    &session_id,
                    Some(run_id),
                    "/ask Cancel this turn?".to_owned(),
                )
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !application
                .pending_questions(Some(&session_id))
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for input in ["/code \"queued first\"", "/code \"queued second\""] {
        application
            .submit_session(
                &session_id,
                SessionSubmissionRequest {
                    delivery: SubmissionDelivery::Queue,
                    run_id: None,
                    content: SubmissionContent::Prompt {
                        input: input.to_owned(),
                    },
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            )
            .await
            .unwrap();
    }
    application
        .cancel_turn(&session_id, &cancelled_run_id)
        .await
        .unwrap();
    assert!(cancelled.await.unwrap().unwrap_err().is_cancelled());
    let parked = application.session_inbox(&session_id).await.unwrap();
    assert!(parked.paused);
    assert_eq!(
        parked
            .items
            .iter()
            .map(|item| item.content.input())
            .collect::<Vec<_>>(),
        ["/code \"queued first\"", "/code \"queued second\""]
    );

    application
        .submit_session(
            &session_id,
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Queue,
                run_id: None,
                content: SubmissionContent::Prompt {
                    input: "/code \"queued wake\"".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if application
                .session_inbox(&session_id)
                .await
                .unwrap()
                .items
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let queued_messages = application
        .events(&session_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            SessionEventKind::UserMessage {
                content,
                source:
                    Some(UserMessageSource::Submission {
                        delivery: SubmissionDelivery::Queue,
                        ..
                    }),
                ..
            } => Some(content),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        queued_messages,
        [
            "/code \"steered at the step boundary\"",
            "/code \"queued first\"",
            "/code \"queued second\"",
            "/code \"queued wake\"",
        ]
    );

    application.shutdown().await.unwrap();
    drop(application);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
