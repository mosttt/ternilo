use super::{tests::open_test_application, *};
use ternilo_protocol::{QueueEditRequest, SubmissionDelivery, SubmissionPlacement};

fn account_input(id: &str, username: &str) -> InputProvenance {
    InputProvenance {
        run_id: None,
        input_id: SubmissionId::new(id),
        author: InputAuthor::Account {
            user_id: UserId::new(format!("user-{username}")),
            username: username.to_owned(),
        },
    }
}

fn request(text: &str, delivery: SubmissionDelivery) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        run_id: None,
        content: SubmissionContent::Prompt {
            input: text.to_owned(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
        delivery,
    }
}

fn authors(events: &[SessionEvent]) -> Vec<(String, Option<InputProvenance>)> {
    events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                content,
                provenance,
                ..
            } => Some((content.clone(), provenance.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep nested child lineage and later human input checks in one provenance scenario."
)]
async fn model_origin_stays_with_the_run_across_later_inputs_and_nested_children() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let app = open_test_application(directory.path().join("data")).await;
    let workspace = app
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let parent = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let original = account_input("original-input", "alice");
    let run = RunId::new("original-run");
    let mut first = request("/code \"original\"", SubmissionDelivery::Queue);
    first.run_id = Some(run.clone());
    app.run_session_input_with_provenance(
        parent.identity.session_id.as_str(),
        first,
        original.clone(),
    )
    .await
    .unwrap();
    let mut later = request("/code \"later\"", SubmissionDelivery::Queue);
    later.run_id = Some(RunId::new("later-run"));
    app.run_session_input_with_provenance(
        parent.identity.session_id.as_str(),
        later,
        account_input("later-input", "bob"),
    )
    .await
    .unwrap();
    let mut identity = parent.identity.clone();
    for name in ["child", "nested"] {
        let child = app
            .subagent_session_host()
            .create(
                identity,
                SubagentSessionRequest {
                    subagent_id: SubagentId::new(name),
                    provider: "in-process".to_owned(),
                    label: name.to_owned(),
                    task: "inspect".to_owned(),
                    transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
                },
            )
            .await
            .unwrap()
            .unwrap();
        let mut input = request("/code \"child\"", SubmissionDelivery::Queue);
        input.run_id = Some(run.clone());
        app.run_session_input_with_provenance(
            child.session_id.as_str(),
            input,
            InputProvenance {
                input_id: SubmissionId::new(format!("{name}-input")),
                run_id: None,
                author: InputAuthor::Automation {
                    source: ternilo_protocol::AutomatedInputSource::Subagent,
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(
            app.model_input_origin(child.session_id.as_str(), &run)
                .await
                .map(|origin| (origin.session_id, origin.provenance))
                .unwrap(),
            (parent.identity.session_id.clone(), Some(original.clone()))
        );
        identity = app
            .state
            .session(child.session_id.as_str())
            .await
            .unwrap()
            .identity;
    }
    let followup_run = RunId::new("human-followup");
    let followup = account_input("human-followup-input", "bob");
    let mut input = request("/code \"followup\"", SubmissionDelivery::Queue);
    input.run_id = Some(followup_run.clone());
    app.run_session_input_with_provenance(identity.session_id.as_str(), input, followup.clone())
        .await
        .unwrap();
    assert_eq!(
        app.model_input_origin(identity.session_id.as_str(), &followup_run)
            .await
            .map(|origin| (origin.session_id, origin.provenance))
            .unwrap(),
        (identity.session_id.clone(), Some(followup))
    );
    assert_eq!(
        app.model_input_origin(identity.session_id.as_str(), &run)
            .await
            .map(|origin| (origin.session_id, origin.provenance))
            .unwrap(),
        (parent.identity.session_id, Some(original))
    );
    assert!(
        app.model_input_origin(identity.session_id.as_str(), &RunId::new("missing-run"))
            .await
            .is_err()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise accepted authors through execution, queue edits, cancellation, restart, and fork."
)]
async fn input_provenance_survives_multiuser_queue_restart_and_fork() {
    let root = tempfile::tempdir().unwrap();
    let workspace_dir = root.path().join("workspace");
    tokio::fs::create_dir(&workspace_dir).await.unwrap();
    let data_dir = root.path().join("data");
    let app = Arc::new(open_test_application(data_dir.clone()).await);
    let workspace = app
        .add_workspace(workspace_dir.to_str().unwrap())
        .await
        .unwrap();
    let session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.as_str().to_owned();
    let direct = account_input("direct-alice", "alice");
    app.run_session_input_with_provenance(
        &id,
        request("/code \"original\"", SubmissionDelivery::Queue),
        direct.clone(),
    )
    .await
    .unwrap();
    let model = super::test_model::TestModel::start().await;
    model.install(&app, &id).await.unwrap();
    let active = {
        let app = Arc::clone(&app);
        let id = id.clone();
        tokio::spawn(async move {
            app.run_turn(&id, Some("hold".to_owned()), "/ask Hold input?".to_owned())
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while app.pending_questions(Some(&id)).await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let alice = account_input("queued-alice", "alice");
    let bob = account_input("queued-bob", "bob");
    let accepted = app
        .submit_session_with_provenance(
            &id,
            request("Alice's task", SubmissionDelivery::Queue),
            alice.clone(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.id, alice.input_id);
    assert_eq!(accepted.provenance, Some(alice.clone()));
    let queued = app
        .submit_session_with_provenance(
            &id,
            request("Bob's task", SubmissionDelivery::Queue),
            bob.clone(),
        )
        .await
        .unwrap();
    assert_eq!(queued.placement, SubmissionPlacement::Queued);
    assert_eq!(queued.provenance, Some(bob.clone()));
    let edited = app
        .edit_session_queue_item(
            &id,
            queued.id.clone(),
            QueueEditRequest {
                input: "Bob's edited task".to_owned(),
                expected_updated_at_ms: queued.updated_at_ms,
            },
        )
        .await
        .unwrap();
    assert_eq!(edited.provenance, Some(bob.clone()));
    let removed = app
        .remove_session_queue_item(&id, accepted.id.clone())
        .await
        .unwrap();
    assert_eq!(removed.provenance, Some(alice.clone()));
    app.inbox.enqueue(&id, removed).await.unwrap();
    app.cancel_turn(&id, "hold").await.unwrap();
    assert!(active.await.unwrap().unwrap_err().is_cancelled());
    let parked = app.session_inbox(&id).await.unwrap();
    assert!(parked.paused);
    assert_eq!(
        parked
            .items
            .iter()
            .map(|item| item.provenance.clone())
            .collect::<Vec<_>>(),
        vec![Some(bob.clone()), Some(alice.clone())]
    );
    app.shutdown().await.unwrap();
    drop(app);

    let restored = Arc::new(open_test_application(data_dir).await);
    assert_eq!(
        restored.session_inbox(&id).await.unwrap().items,
        parked.items
    );
    let wake = restored
        .submit_session(&id, request("Local wake", SubmissionDelivery::Queue))
        .await
        .unwrap();
    assert_eq!(wake.provenance.as_ref().unwrap().author, InputAuthor::Local);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !restored.session_inbox(&id).await.unwrap().items.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let events = restored.events(&id).await.unwrap();
    let recorded = authors(&events);
    assert_eq!(recorded[0], ("/code \"original\"".to_owned(), Some(direct)));
    assert_eq!(recorded[1].1.as_ref().unwrap().author, InputAuthor::Local);
    assert_eq!(
        recorded[2],
        ("Bob's edited task".to_owned(), Some(bob.clone()))
    );
    assert_eq!(
        recorded[3],
        ("Alice's task".to_owned(), Some(alice.clone()))
    );
    assert_eq!(recorded[4].1, wake.provenance);
    assert_eq!(recorded.len(), 5);
    let resumed = events
        .iter()
        .filter(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        .skip(2)
        .collect::<Vec<_>>();
    assert_eq!(
        resumed
            .iter()
            .map(|event| &event.run_id)
            .collect::<Vec<_>>(),
        [&queued.run_id, &accepted.run_id, &wake.run_id]
    );
    assert_ne!(queued.run_id, accepted.run_id);
    assert_ne!(queued.run_id, wake.run_id);
    assert_ne!(accepted.run_id, wake.run_id);
    for (run, provenance) in [
        (&queued.run_id, Some(bob.clone())),
        (&accepted.run_id, Some(alice.clone())),
        (&wake.run_id, wake.provenance.clone()),
    ] {
        assert!(events.iter().any(|event| &event.run_id == run
            && matches!(event.kind, SessionEventKind::TurnFinished { .. })));
        assert_eq!(
            restored
                .model_input_origin(&id, run)
                .await
                .map(|origin| (origin.session_id, origin.provenance))
                .unwrap(),
            (session.identity.session_id.clone(), provenance)
        );
    }
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    for (request, expected) in
        requests
            .iter()
            .zip(["Bob's edited task", "Alice's task", "Local wake"])
    {
        let users = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .collect::<Vec<_>>();
        assert_eq!(users.last().unwrap()["content"], expected);
    }
    let users = requests[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .collect::<Vec<_>>();
    for (message, expected) in
        users[users.len() - 3..]
            .iter()
            .zip(["Bob's edited task", "Alice's task", "Local wake"])
    {
        assert_eq!(message["content"], expected);
    }
    let fork = restored.fork_session(&id, None, None).await.unwrap();
    assert_eq!(
        authors(
            &restored
                .events(fork.identity.session_id.as_str())
                .await
                .unwrap()
        ),
        recorded
    );
    restored.shutdown().await.unwrap();
}
