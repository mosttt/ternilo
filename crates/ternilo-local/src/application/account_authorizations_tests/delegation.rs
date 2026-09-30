use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "verify independent human follow-up ownership through a real child session and background process"
)]
async fn alice_revocation_preserves_bobs_independent_followup_in_her_child_session() {
    let root = tempfile::tempdir().unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let directory = root.path().join("workspace");
    tokio::fs::create_dir(&directory).await.unwrap();
    let workspace = app
        .add_workspace(directory.to_str().unwrap())
        .await
        .unwrap();
    app.create_session(workspace.workspace_id, Some("parent".into()), None)
        .await
        .unwrap();
    app.update_permissions("parent", ternilo_protocol::PermissionPreset::FullAccess)
        .await
        .unwrap();
    let input = account_input("alice-parent", "alice");
    app.record_account_input_authorization(&input, &proof(1))
        .await
        .unwrap();
    let running = {
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            app.run_session_input_with_provenance(
                "parent",
                request("/agent first-pass", "alice-parent"),
                input,
            )
            .await
        })
    };
    let question = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(question) = app
                .pending_questions(Some("parent"))
                .await
                .into_iter()
                .next()
            {
                break question;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    app.answer_question(ternilo_protocol::UserAnswer {
        question_id: question.question.id,
        selected: vec!["Allow once".into()],
        custom: None,
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let child = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let snapshot = app
                .events("parent")
                .await
                .unwrap()
                .into_iter()
                .rev()
                .find_map(|event| match event.kind {
                    SessionEventKind::SubagentUpdated { subagent } => Some(subagent),
                    _ => None,
                });
            if let Some(snapshot) = snapshot {
                assert_ne!(
                    snapshot.status,
                    ternilo_protocol::SubagentStatus::Failed,
                    "{snapshot:?}"
                );
                if snapshot.status == ternilo_protocol::SubagentStatus::Idle {
                    break snapshot;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let input = account_input("bob-followup", "bob");
    app.record_account_input_authorization(&input, &proof(1))
        .await
        .unwrap();
    app.followup_subagent_with_provenance(
        "parent",
        child.subagent_id,
        "/job while :; do printf x >> heartbeat; sleep 0.01; done".into(),
        input,
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !directory.join("heartbeat").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let receipts = app
        .synchronize_account_authorizations(&snapshot(2, false, true))
        .await
        .unwrap();
    assert_eq!(
        receipts[0].state,
        NodeCleanupState::Confirmed,
        "{receipts:?}"
    );
    let before = tokio::fs::metadata(directory.join("heartbeat"))
        .await
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        before
            < tokio::fs::metadata(directory.join("heartbeat"))
                .await
                .unwrap()
                .len()
    );
    let events = app
        .events(child.session_id.unwrap().as_str())
        .await
        .unwrap();
    let bob_run = events
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                provenance: Some(provenance),
                ..
            } if provenance.input_id.as_str() == "bob-followup" => Some(event.run_id.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        app.execution_resources
            .owners()
            .await
            .iter()
            .any(|(_, owner)| owner.run_id == bob_run)
    );
    app.close().await.unwrap();
}
