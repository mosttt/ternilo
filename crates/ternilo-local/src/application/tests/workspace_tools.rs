use super::*;

#[tokio::test]
async fn restores_workspace_session_and_events_after_restart() {
    let model = super::test_model::TestModel::start().await;
    let data_dir = test_data_dir();
    let workspace_path = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap();
    let first = open_test_application(data_dir.clone()).await;
    let workspace = first
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let session = first
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    model
        .install(&first, session.identity.session_id.as_str())
        .await
        .unwrap();
    first
        .run_turn(
            session.identity.session_id.as_str(),
            None,
            "persisted".to_owned(),
        )
        .await
        .unwrap();
    first.shutdown().await.unwrap();
    drop(first);

    let restored = open_test_application(data_dir.clone()).await;
    let snapshot = restored.snapshot().await;
    assert_eq!(snapshot.workspaces, vec![workspace]);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].title, "persisted");
    let events = restored
        .events(session.identity.session_id.as_str())
        .await
        .unwrap();
    assert_eq!(events.first().map(|event| event.seq), Some(0));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        ternilo_protocol::SessionEventKind::TurnFinished { .. }
    )));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        ternilo_protocol::SessionEventKind::SessionTitleGenerated { title }
            if title == "persisted"
    )));
    let title_lifecycle = events
        .iter()
        .filter_map(|event| match &event.kind {
            ternilo_protocol::SessionEventKind::SessionTitleGenerationStarted => {
                Some((event.seq, "started"))
            }
            ternilo_protocol::SessionEventKind::SessionTitleGenerated { .. } => {
                Some((event.seq, "generated"))
            }
            ternilo_protocol::SessionEventKind::SessionTitleGenerationFinished {
                generated: true,
            } => Some((event.seq, "finished")),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        title_lifecycle
            .iter()
            .map(|(_, state)| *state)
            .collect::<Vec<_>>(),
        ["started", "generated", "finished"]
    );
    assert!(title_lifecycle.windows(2).all(|pair| pair[0].0 < pair[1].0));
    restored.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
}

#[tokio::test]
async fn unregistered_workspace_sessions_resume_from_their_captured_path() {
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let first = open_test_application(data_dir.clone()).await;
    let workspace = first
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = first
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str().to_owned();
    first
        .run_turn(&session_id, None, "/code \"before unregister\"".to_owned())
        .await
        .unwrap();
    let renamed = first
        .rename_workspace(workspace.workspace_id.clone(), "renamed".to_owned())
        .await
        .unwrap();
    assert_eq!(renamed.title, "renamed");
    first
        .unregister_workspace(workspace.workspace_id.clone())
        .await
        .unwrap();
    assert!(first.snapshot().await.workspaces.is_empty());
    assert_eq!(
        first.snapshot().await.sessions[0].workspace_path,
        workspace.path
    );
    let child = first.fork_session(&session_id, None, None).await.unwrap();
    assert_eq!(child.workspace_id, session.workspace_id);
    assert_eq!(child.workspace_path, session.workspace_path);
    assert_eq!(
        first
            .events(child.identity.session_id.as_str())
            .await
            .unwrap(),
        first.events(&session_id).await.unwrap()
    );
    let child_id = child.identity.session_id.as_str().to_owned();
    first.shutdown().await.unwrap();
    drop(first);

    let restored = open_test_application(data_dir.clone()).await;
    assert!(restored.snapshot().await.workspaces.is_empty());
    let outcome = restored
        .run_turn(&session_id, None, "/code \"after unregister\"".to_owned())
        .await
        .unwrap();
    assert!(outcome.answer.contains("after unregister"));
    let child_outcome = restored
        .run_turn(
            &child_id,
            None,
            "/code \"child after unregister\"".to_owned(),
        )
        .await
        .unwrap();
    assert!(child_outcome.answer.contains("child after unregister"));
    restored.shutdown().await.unwrap();
    drop(restored);
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "the shared scenario verifies workspace and skill tools against one application"
)]
async fn verify_workspace_and_skill_tools(
    application: &LocalApplication,
    workspace_dir: &Path,
    session_id: &str,
) {
    application
        .run_turn(
            session_id,
            None,
            "/write note.txt hello workspace".to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(workspace_dir.join("note.txt"))
            .await
            .unwrap(),
        "hello workspace"
    );
    let deliverable = application
        .events(session_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::DeliverableProduced {
                path, attachment, ..
            } if path == "note.txt" => Some(attachment),
            _ => None,
        })
        .expect("write produced a retained deliverable");
    assert!(deliverable.is_reference());
    assert_eq!(
        application
            .resolve_attachment(deliverable)
            .await
            .unwrap()
            .content,
        "hello workspace"
    );
    let read = application
        .run_turn(session_id, None, "/read note.txt".to_owned())
        .await
        .unwrap();
    assert!(read.answer.contains("hello workspace"));
    let search = application
        .run_turn(session_id, None, "/grep hello".to_owned())
        .await
        .unwrap();
    assert!(search.answer.contains("note.txt"));
    let skill_dir = workspace_dir.join(".ternilo/skills/demo");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo workflow\n---\n# Demo\nUse the demo workflow.",
    )
    .await
    .unwrap();
    let skills = application
        .run_turn(session_id, None, "/skills".to_owned())
        .await
        .unwrap();
    assert!(skills.answer.contains("\"name\": \"demo\""));
    let loaded_skill = application
        .run_skill_turn(
            session_id,
            None,
            "demo".to_owned(),
            String::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(loaded_skill.answer.contains("demo workflow"));
    let attachment = ternilo_protocol::Attachment {
        name: "context.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content: "durable attachment content".to_owned(),
    };
    let attachment_outcome = application
        .run_turn_with_attachments(
            session_id,
            None,
            "use the attachment".to_owned(),
            vec![attachment],
        )
        .await
        .unwrap();
    assert!(
        attachment_outcome
            .answer
            .contains("durable attachment content")
    );
    let stored = application.events(session_id).await.unwrap();
    let stored_attachment = stored.iter().rev().find_map(|event| match &event.kind {
        ternilo_protocol::SessionEventKind::UserMessage { attachments, .. } => attachments.first(),
        _ => None,
    });
    assert!(stored_attachment.is_some_and(ternilo_protocol::Attachment::is_reference));
    assert!(
        !stored_attachment
            .expect("attachment event exists")
            .content
            .contains("durable attachment content")
    );
}

#[tokio::test]
async fn oversized_tool_output_is_spilled_and_resolves_after_restart() {
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
    let payload = "retained-output-".repeat(1_200);
    tokio::fs::write(workspace_dir.join("output.txt"), &payload)
        .await
        .unwrap();
    application
        .run_turn(&session_id, None, "/read output.txt".to_owned())
        .await
        .unwrap();
    let (preview, retained) = application
        .events(&session_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::ToolCallFinished {
                name,
                output,
                retained_output: Some(retained),
                ..
            } if name == "read_file" => Some((output.content, retained)),
            _ => None,
        })
        .expect("oversized output was retained");
    assert!(preview.contains("tool_result_retained"));
    assert!(preview.len() < payload.len());
    application.shutdown().await.unwrap();
    drop(application);

    let restored = open_test_application(data_dir.clone()).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &restored.resolve_attachment(retained).await.unwrap().content
        )
        .unwrap()["content"],
        payload
    );
    restored.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

async fn verify_jobs_and_planning(application: &LocalApplication, session_id: &str) {
    let started_job = application
        .run_turn(
            session_id,
            None,
            "/job sleep 0.02; echo background-complete".to_owned(),
        )
        .await
        .unwrap();
    assert!(started_job.answer.contains("job-1"));
    let job = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let events = application.events(session_id).await.unwrap();
            if let Some(job) = events.into_iter().find_map(|event| match event.kind {
                SessionEventKind::JobUpdated { job }
                    if job.status == ternilo_protocol::JobStatus::Completed =>
                {
                    Some(job)
                }
                _ => None,
            }) {
                break job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("background job did not complete within five seconds");
    assert!(
        job.result
            .as_ref()
            .is_some_and(|result| result.stdout.contains("background-complete"))
    );
    assert!(
        application
            .events(session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                &event.kind,
                SessionEventKind::JobUpdated { job }
                    if job.status == ternilo_protocol::JobStatus::Running
            ))
    );
    application
        .run_turn(
            session_id,
            None,
            "/update-plan inspect; implement; verify".to_owned(),
        )
        .await
        .unwrap();
    application
        .run_turn(
            session_id,
            None,
            "/goal edit ship the workspace flow".to_owned(),
        )
        .await
        .unwrap();
    let planning_events = application.events(session_id).await.unwrap();
    assert!(planning_events.iter().any(|event| matches!(
        event.kind,
        ternilo_protocol::SessionEventKind::PlanUpdated { .. }
    )));
    assert!(planning_events.iter().any(|event| matches!(
        event.kind,
        ternilo_protocol::SessionEventKind::GoalUpdated { .. }
    )));
}

async fn verify_session_policy(
    application: &LocalApplication,
    workspace_dir: &Path,
    session_id: &str,
) {
    application
        .update_mode(session_id, SessionMode::Plan)
        .await
        .unwrap();
    let plan_denied = application
        .run_turn(session_id, None, "/write plan-blocked.txt no".to_owned())
        .await
        .unwrap();
    assert!(plan_denied.answer.contains("denied by host policy"));
    application
        .update_mode(session_id, SessionMode::Execute)
        .await
        .unwrap();

    application
        .update_permissions(session_id, PermissionPreset::ReadOnly)
        .await
        .unwrap();
    let denied = application
        .run_turn(session_id, None, "/write blocked.txt no".to_owned())
        .await
        .unwrap();
    assert!(denied.answer.contains("denied by host policy"));
    assert!(
        !tokio::fs::try_exists(workspace_dir.join("blocked.txt"))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn workspace_tools_obey_the_session_permission_preset() {
    let model = super::test_model::TestModel::start().await;
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

    model.install(&application, session_id).await.unwrap();
    verify_workspace_and_skill_tools(&application, &workspace_dir, session_id).await;
    verify_jobs_and_planning(&application, session_id).await;
    verify_session_policy(&application, &workspace_dir, session_id).await;

    application
        .update_permissions(session_id, PermissionPreset::WorkspaceWrite)
        .await
        .unwrap();
    let pending_job = application
        .run_turn(session_id, None, "/job sleep 30".to_owned())
        .await
        .unwrap();
    assert!(pending_job.answer.contains("job-"));
    application.shutdown().await.unwrap();
    assert!(
        application
            .events(session_id)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                &event.kind,
                SessionEventKind::JobUpdated { job }
                    if job.command == "sleep 30"
                        && job.status == ternilo_protocol::JobStatus::Cancelled
            ))
    );
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}

#[tokio::test]
async fn explicit_skill_turn_keeps_the_skill_model_visible_and_the_ui_event_concise() {
    let model = super::test_model::TestModel::start().await;
    let data_dir = test_data_dir();
    let workspace_dir = data_dir.with_extension("workspace");
    let skill_dir = workspace_dir.join(".ternilo/skills/review-code");
    tokio::fs::create_dir_all(&skill_dir).await.unwrap();
    tokio::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review-code\ndescription: Review a change\nuser-invocable: true\n---\n# Review code\nLook for correctness and maintainability.",
    )
    .await
    .unwrap();

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

    model.install(&application, session_id).await.unwrap();
    let outcome = application
        .run_skill_turn(
            session_id,
            None,
            "review-code".to_owned(),
            "Review src/lib.rs".to_owned(),
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(
        outcome
            .answer
            .contains("<skill_content name=\"review-code\">")
    );
    assert!(
        outcome
            .answer
            .contains("Look for correctness and maintainability.")
    );
    assert!(outcome.answer.contains("Review src/lib.rs"));

    let event = application
        .events(session_id)
        .await
        .unwrap()
        .into_iter()
        .find_map(|event| match event.kind {
            SessionEventKind::UserMessage {
                content,
                display_content,
                source,
                ..
            } => Some((content, display_content, source)),
            _ => None,
        })
        .expect("skill turn stored a user message");
    assert!(event.0.contains("<skill_content name=\"review-code\">"));
    assert_eq!(
        event.1.as_deref(),
        Some("/skill review-code\n\nReview src/lib.rs")
    );
    assert!(matches!(
        event.2,
        Some(UserMessageSource::SkillInvocation { name }) if name == "review-code"
    ));

    application.shutdown().await.unwrap();
    tokio::fs::remove_dir_all(data_dir).await.unwrap();
    tokio::fs::remove_dir_all(workspace_dir).await.unwrap();
}
