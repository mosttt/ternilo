use std::{path::Path, sync::Arc, time::Duration};

use ternilo_kernel::{RunCancellation, SubagentRunStart};
use ternilo_protocol::{RunOutcome, SubagentTranscriptKind};
use tokio::task::JoinHandle;

use super::{test_model::TestModel, tests::open_test_application, *};

struct Fixture {
    _directory: tempfile::TempDir,
    application: Arc<LocalApplication>,
    left: PathBuf,
    right: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        tokio::fs::create_dir(&left).await.unwrap();
        tokio::fs::create_dir(&right).await.unwrap();
        let mut application = open_test_application(directory.path().join("data")).await;
        application.directory_coordinator =
            crate::DirectoryCoordinator::new(directory.path().join("locks"));
        Self {
            _directory: directory,
            application: Arc::new(application),
            left,
            right,
        }
    }

    async fn session(&self, directory: &Path, id: &str) -> LocalSession {
        let workspace = self
            .application
            .add_workspace(directory.to_str().unwrap())
            .await
            .unwrap();
        self.application
            .create_session(workspace.workspace_id, Some(id.to_owned()), None)
            .await
            .unwrap()
    }

    fn run(
        &self,
        session: &str,
        run: &str,
        input: &str,
    ) -> JoinHandle<Result<RunOutcome, HarnessError>> {
        let application = Arc::clone(&self.application);
        let session = session.to_owned();
        let run = run.to_owned();
        let input = input.to_owned();
        tokio::spawn(async move { application.run_turn(&session, Some(run), input).await })
    }

    fn run_as(
        &self,
        session: &str,
        run: &str,
        input: &str,
        author: InputAuthor,
    ) -> JoinHandle<Result<RunOutcome, HarnessError>> {
        let application = Arc::clone(&self.application);
        let session = session.to_owned();
        let request = SessionSubmissionRequest {
            run_id: Some(RunId::new(run)),
            content: SubmissionContent::Prompt {
                input: input.to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
            delivery: SubmissionDelivery::Queue,
        };
        let provenance = InputProvenance {
            input_id: SubmissionId::new(format!("input-{run}")),
            run_id: None,
            author,
        };
        tokio::spawn(async move {
            application
                .run_session_input_with_provenance(&session, request, provenance)
                .await
        })
    }
}

async fn wait_for_question(application: &LocalApplication, session: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while application
            .pending_questions(Some(session))
            .await
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session did not reach its question");
}

#[tokio::test]
async fn same_user_sessions_and_forks_run_concurrently_without_releasing_each_others_leases() {
    for author in [
        InputAuthor::Local,
        InputAuthor::Account {
            user_id: UserId::new("shared-account"),
            username: "alice".to_owned(),
        },
    ] {
        let fixture = Fixture::new().await;
        let app = &fixture.application;
        for session in ["first", "second", "outsider"] {
            fixture.session(&fixture.left, session).await;
        }
        app.run_turn(
            "first",
            Some("fork-base".to_owned()),
            "/code \"fork base\"".to_owned(),
        )
        .await
        .unwrap();
        let fork = app
            .fork_session("first", Some("fork".to_owned()), None)
            .await
            .unwrap();
        let first = fixture.run_as(
            "first",
            "first-run",
            "/ask First active task?",
            author.clone(),
        );
        wait_for_question(app, "first").await;
        let second = fixture.run_as(
            "second",
            "second-run",
            "/ask Second active task?",
            author.clone(),
        );
        wait_for_question(app, "second").await;
        assert!(
            !app.events("second")
                .await
                .unwrap()
                .iter()
                .any(|event| { matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting) })
        );
        let forked = fixture.run_as(
            fork.identity.session_id.as_str(),
            "fork-user-run",
            "/code \"fork completed\"",
            author,
        );
        let outcome = tokio::time::timeout(Duration::from_secs(5), forked)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(outcome.answer.contains("fork completed"));
        assert!(!outcome.events.iter().any(|event| {
            event.run_id.as_str() == "fork-user-run"
                && matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting)
        }));
        let outsider = fixture.run_as(
            "outsider",
            "outsider-run",
            "/code \"other user completed\"",
            InputAuthor::Account {
                user_id: UserId::new("different-account"),
                username: "bob".to_owned(),
            },
        );
        wait_for_directory(app, "outsider").await;
        app.cancel_turn("first", "first-run").await.unwrap();
        assert!(first.await.unwrap().unwrap_err().is_cancelled());
        assert!(!second.is_finished());
        assert!(!outsider.is_finished());
        assert!(
            app.directory_coordinator
                .bind_user("account:different-account".to_owned(), fixture.left.clone())
                .try_acquire()
                .await
                .unwrap()
                .is_none()
        );
        app.cancel_turn("second", "second-run").await.unwrap();
        assert!(second.await.unwrap().unwrap_err().is_cancelled());
        let outcome = tokio::time::timeout(Duration::from_secs(5), outsider)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(outcome.answer.contains("other user completed"));
        assert!(
            outcome
                .events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
        );
        app.shutdown().await.unwrap();
    }
}

async fn wait_for_directory(application: &LocalApplication, session: &str) -> Vec<SessionEvent> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = application.events(session).await.unwrap();
            if events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting))
            {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("session did not enter directory waiting")
}

#[tokio::test]
async fn authenticated_computer_owner_reuses_local_directory_access_but_other_accounts_do_not() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "local").await;
    fixture.session(&fixture.left, "remote").await;
    let local = fixture.run("local", "local-run", "/ask Local work remains active?");
    wait_for_question(app, "local").await;
    app.set_directory_account_owner(UserId::new("computer-owner"))
        .unwrap();
    let owner = fixture.run_as(
        "remote",
        "owner-run",
        "/code \"owner runs concurrently\"",
        InputAuthor::Account {
            user_id: UserId::new("computer-owner"),
            username: "owner".to_owned(),
        },
    );
    let outcome = tokio::time::timeout(Duration::from_secs(5), owner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(outcome.answer.contains("owner runs concurrently"));
    assert!(
        !outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting))
    );
    let other = fixture.run_as(
        "remote",
        "other-account-run",
        "/code \"not admitted\"",
        InputAuthor::Account {
            user_id: UserId::new("unrelated-account"),
            username: "other".to_owned(),
        },
    );
    wait_for_directory(app, "remote").await;
    assert!(!local.is_finished());
    app.cancel_turn("remote", "other-account-run")
        .await
        .unwrap();
    assert!(other.await.unwrap().unwrap_err().is_cancelled());
    assert!(!local.is_finished());
    app.cancel_turn("local", "local-run").await.unwrap();
    assert!(local.await.unwrap().unwrap_err().is_cancelled());
    app.shutdown().await.unwrap();
}

#[cfg(unix)]
async fn install_hook(application: &LocalApplication, session: &str, directory: &Path) {
    let config = directory.join("hooks.json");
    tokio::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "hooks": {
                "SessionStart": [{"hooks": [{"command": "cat > hook-ran.json; printf '{}'"}]}]
            }
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    application
        .update_profile_plugins(
            session,
            vec![PluginEntry {
                id: "directory-admission-hook".to_owned(),
                kind: ternilo_builtins::CLAUDE_CODE_HOOKS_KIND.to_owned(),
                enabled: true,
                config: serde_json::json!({"configPath": config, "projectDir": directory}),
            }],
        )
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn waiting_session_defers_model_and_hooks_allows_other_directory_and_can_be_cancelled() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "holder").await;
    fixture.session(&fixture.left, "waiter").await;
    fixture.session(&fixture.right, "independent").await;
    let model = TestModel::start().await;
    model.install(app, "waiter").await.unwrap();
    model.install(app, "independent").await.unwrap();
    install_hook(app, "waiter", &fixture.left).await;
    let holder = app
        .directory_coordinator
        .bind(
            app.execution_scope("holder").await.unwrap(),
            fixture.left.clone(),
        )
        .acquire(RunCancellation::new())
        .await
        .unwrap();
    let waiting = fixture.run("waiter", "waiting-run", "Wait for this directory");
    let events = wait_for_directory(app, "waiter").await;
    let input = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        .unwrap();
    let wait = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting))
        .unwrap();
    assert!(input < wait);
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::ModelRequestStarted { .. } | SessionEventKind::HookResult { .. }
    )));
    assert!(!fixture.left.join("hook-ran.json").exists());
    let independent = tokio::time::timeout(
        Duration::from_secs(5),
        app.run_turn(
            "independent",
            Some("independent-run".to_owned()),
            "Another directory runs now".to_owned(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(independent.answer.contains("Another directory runs now"));
    assert!(!waiting.is_finished());
    app.cancel_turn("waiter", "waiting-run").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    assert!(!fixture.left.join("hook-ran.json").exists());
    let cancelled = app.events("waiter").await.unwrap();
    assert!(!cancelled.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::WorkspaceExecutionAcquired | SessionEventKind::ModelRequestStarted { .. }
    )));
    assert!(
        app.directory_coordinator
            .bind("unrelated".to_owned(), fixture.left.clone())
            .try_acquire()
            .await
            .unwrap()
            .is_none(),
        "cancelling the waiter must preserve the holder"
    );
    drop(holder);
    let resumed = tokio::time::timeout(
        Duration::from_secs(5),
        app.run_turn(
            "waiter",
            Some("resumed-run".to_owned()),
            "Run after the directory is free".to_owned(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(resumed.answer.contains("Run after the directory is free"));
    assert!(fixture.left.join("hook-ran.json").exists());
    assert!(
        resumed
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn true_subagent_reenters_its_parent_directory_but_an_ordinary_fork_waits() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    let parent = fixture.session(&fixture.left, "parent").await;
    let scope = app.execution_scope("parent").await.unwrap();
    let holder = app
        .directory_coordinator
        .bind(scope.clone(), fixture.left.clone())
        .acquire(RunCancellation::new())
        .await
        .unwrap();
    let host = app.subagent_session_host();
    let child = host
        .create(
            parent.identity,
            SubagentSessionRequest {
                subagent_id: SubagentId::new("child"),
                provider: "in-process".to_owned(),
                label: "child".to_owned(),
                task: "cooperate in the parent's directory".to_owned(),
                transcript_kind: SubagentTranscriptKind::Conversation,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        app.execution_scope(child.session_id.as_str())
            .await
            .unwrap(),
        scope
    );
    let child_outcome = tokio::time::timeout(
        Duration::from_secs(5),
        host.run(
            child.session_id.clone(),
            RunId::new("child-run"),
            "/code \"child finished\"".to_owned(),
            RunCancellation::new(),
            SubagentRunStart::new(),
        ),
    )
    .await
    .expect("a cooperating child must not wait for its held parent")
    .unwrap();
    assert!(child_outcome.answer.contains("child finished"));
    assert!(
        !child_outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting))
    );
    let fork = app
        .fork_session(
            child.session_id.as_str(),
            Some("ordinary-fork".to_owned()),
            None,
        )
        .await
        .unwrap();
    let fork_id = fork.identity.session_id.as_str();
    assert_ne!(app.execution_scope(fork_id).await.unwrap(), scope);
    let waiting = fixture.run(fork_id, "fork-run", "/code \"fork finished\"");
    wait_for_directory(app, fork_id).await;
    assert!(!waiting.is_finished());
    drop(holder);
    let outcome = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(outcome.answer.contains("fork finished"));
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_live_turn_holds_the_directory_against_another_user_until_cancellation_completes() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "asking").await;
    fixture.session(&fixture.left, "following").await;
    let asking = fixture.run("asking", "ask-run", "/ask Keep the directory busy?");
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.pending_questions(Some("asking")).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let following = fixture.run_as(
        "following",
        "following-run",
        "/code \"acquired after cancellation\"",
        InputAuthor::Account {
            user_id: UserId::new("another-user"),
            username: "another".to_owned(),
        },
    );
    wait_for_directory(app, "following").await;
    assert!(!following.is_finished());
    app.cancel_turn("asking", "ask-run").await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), asking)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    let outcome = tokio::time::timeout(Duration::from_secs(5), following)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(outcome.answer.contains("acquired after cancellation"));
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn file_reference_is_read_after_the_waiting_session_acquires_its_directory() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "holder").await;
    fixture.session(&fixture.left, "reader").await;
    let model = TestModel::start().await;
    model.install(app, "reader").await.unwrap();
    let file = fixture.left.join("context.txt");
    tokio::fs::write(&file, "STALE_BEFORE_WAIT").await.unwrap();
    let holder = app
        .directory_coordinator
        .bind(
            app.execution_scope("holder").await.unwrap(),
            fixture.left.clone(),
        )
        .acquire(RunCancellation::new())
        .await
        .unwrap();
    let reading = {
        let app = Arc::clone(app);
        tokio::spawn(async move {
            let provenance = app.local_input_provenance().unwrap();
            app.run_session_input_with_provenance(
                "reader",
                SessionSubmissionRequest {
                    run_id: Some(RunId::new("reference-run")),
                    content: SubmissionContent::Prompt {
                        input: "Use the current file".to_owned(),
                    },
                    references: vec![SubmissionReference::File {
                        path: "context.txt".to_owned(),
                        file_kind: ternilo_protocol::ReferenceFileKind::File,
                    }],
                    attachments: Vec::new(),
                    delivery: ternilo_protocol::SubmissionDelivery::Queue,
                },
                provenance,
            )
            .await
        })
    };
    let events = wait_for_directory(app, "reader").await;
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::HookContextAdded { .. } | SessionEventKind::ModelRequestStarted { .. }
    )));
    tokio::fs::write(&file, "FRESH_AFTER_HOLDER_EDIT")
        .await
        .unwrap();
    drop(holder);
    let outcome = tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(outcome.answer.contains("FRESH_AFTER_HOLDER_EDIT"));
    assert!(!outcome.answer.contains("STALE_BEFORE_WAIT"));
    let acquired = outcome
        .events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
        .unwrap();
    let context = outcome.events.iter().position(|event| matches!(&event.kind,
        SessionEventKind::HookContextAdded { content, reference: Some(_), .. }
            if content.contains("FRESH_AFTER_HOLDER_EDIT") && !content.contains("STALE_BEFORE_WAIT")
    )).unwrap();
    let model = outcome
        .events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }))
        .unwrap();
    assert!(acquired < context && context < model);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_reference_resolution_error_finishes_the_accepted_run_as_failed() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "reader").await;
    let result = app
        .run_session_input_with_provenance(
            "reader",
            SessionSubmissionRequest {
                run_id: Some(RunId::new("missing-reference")),
                content: SubmissionContent::Prompt {
                    input: "/code \"must not execute\"".to_owned(),
                },
                references: vec![SubmissionReference::File {
                    path: "missing.txt".to_owned(),
                    file_kind: ternilo_protocol::ReferenceFileKind::File,
                }],
                attachments: Vec::new(),
                delivery: ternilo_protocol::SubmissionDelivery::Queue,
            },
            app.local_input_provenance().unwrap(),
        )
        .await;
    assert!(result.is_err());
    let events = app.events("reader").await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnFailed { .. }))
    );
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::CommandStarted { .. } | SessionEventKind::ModelRequestStarted { .. }
    )));
    app.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "verify interruption, separate authored runs, and fresh references across one directory handoff"
)]
async fn interrupting_a_waiting_run_separates_authors_and_resolves_fresh_references() {
    let fixture = Fixture::new().await;
    let app = &fixture.application;
    fixture.session(&fixture.left, "holder").await;
    fixture.session(&fixture.left, "reader").await;
    let model = TestModel::start().await;
    model.install(app, "reader").await.unwrap();
    let file = fixture.left.join("steered-context.txt");
    tokio::fs::write(&file, "STALE_STEERING_CONTEXT")
        .await
        .unwrap();
    let holder = app
        .directory_coordinator
        .bind(
            app.execution_scope("holder").await.unwrap(),
            fixture.left.clone(),
        )
        .acquire(RunCancellation::new())
        .await
        .unwrap();
    let reading = fixture.run(
        "reader",
        "steered-reference-run",
        "Use later steering context",
    );
    wait_for_directory(app, "reader").await;
    let first_author = InputProvenance {
        run_id: None,
        input_id: SubmissionId::new("alice-queued-context"),
        author: InputAuthor::Account {
            user_id: UserId::new("alice-account"),
            username: "alice".to_owned(),
        },
    };
    let first = app
        .submit_session_with_provenance(
            "reader",
            SessionSubmissionRequest {
                run_id: Some(RunId::new("replacement-batch")),
                content: SubmissionContent::Prompt {
                    input: "B: queued before interruption".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
                delivery: ternilo_protocol::SubmissionDelivery::Queue,
            },
            first_author.clone(),
        )
        .await
        .unwrap();
    let provenance = InputProvenance {
        run_id: None,
        input_id: SubmissionId::new("bob-steered-context"),
        author: InputAuthor::Account {
            user_id: UserId::new("bob-account"),
            username: "bob".to_owned(),
        },
    };
    let accepted = app
        .submit_session_with_provenance(
            "reader",
            SessionSubmissionRequest {
                run_id: Some(RunId::new("interrupting-submission")),
                content: SubmissionContent::Prompt {
                    input: "Use this collaborator's file".to_owned(),
                },
                references: vec![SubmissionReference::File {
                    path: "steered-context.txt".to_owned(),
                    file_kind: ternilo_protocol::ReferenceFileKind::File,
                }],
                attachments: Vec::new(),
                delivery: ternilo_protocol::SubmissionDelivery::Steer,
            },
            provenance.clone(),
        )
        .await
        .unwrap();
    assert_eq!(
        accepted.placement,
        ternilo_protocol::SubmissionPlacement::Queued
    );
    assert_eq!(accepted.provenance.as_ref(), Some(&provenance));
    assert_ne!(accepted.run_id, first.run_id);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), reading)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.events("reader").await.unwrap().iter().any(|event| {
                event.run_id == first.run_id
                    && matches!(event.kind, SessionEventKind::WorkspaceExecutionWaiting)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement batch did not wait for the directory");
    assert!(model.requests().is_empty());
    tokio::fs::write(&file, "FRESH_STEERING_AFTER_HOLDER_EDIT")
        .await
        .unwrap();
    drop(holder);
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = app.events("reader").await.unwrap();
            if events.iter().any(|event| {
                event.run_id == first.run_id
                    && matches!(event.kind, SessionEventKind::TurnFinished { .. })
            }) && events.iter().any(|event| {
                event.run_id == accepted.run_id
                    && matches!(event.kind, SessionEventKind::TurnFinished { .. })
            }) && app.session_inbox("reader").await.unwrap().items.is_empty()
            {
                break events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("separate replacement runs did not finish");
    let cancelled = events
        .iter()
        .find(|event| {
            event.run_id.as_str() == "steered-reference-run"
                && matches!(event.kind, SessionEventKind::TurnCancelled)
        })
        .unwrap();
    assert!(!events.iter().any(|event| {
        event.run_id.as_str() == "steered-reference-run"
            && matches!(event.kind, SessionEventKind::ModelRequestStarted { .. })
    }));
    let first_run = events
        .iter()
        .filter(|event| event.run_id == first.run_id)
        .collect::<Vec<_>>();
    let second_run = events
        .iter()
        .filter(|event| event.run_id == accepted.run_id)
        .collect::<Vec<_>>();
    let messages = events
        .iter()
        .filter(|event| event.run_id == first.run_id || event.run_id == accepted.run_id)
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                content,
                provenance,
                ..
            } => {
                assert!(event.seq > cancelled.seq);
                Some((&event.run_id, content.as_str(), provenance.as_ref()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![
            (
                &first.run_id,
                "B: queued before interruption",
                Some(&first_author)
            ),
            (
                &accepted.run_id,
                "Use this collaborator's file",
                Some(&provenance)
            ),
        ]
    );
    let first_finished = first_run
        .iter()
        .find(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
        .unwrap();
    let second_message = second_run
        .iter()
        .find(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
        .unwrap();
    assert!(first_finished.seq < second_message.seq);
    assert!(!first_run.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::HookContextAdded {
            reference: Some(_),
            ..
        }
    )));
    let authored = second_run
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::UserMessage {
                provenance: Some(author),
                content,
                source,
                ..
            } if author.input_id == provenance.input_id => Some((author, content, source)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(authored.len(), 1);
    assert_eq!(authored[0].0, &provenance);
    assert_eq!(authored[0].1, "Use this collaborator's file");
    assert!(
        matches!(authored[0].2, Some(UserMessageSource::Submission { submission_id, delivery: ternilo_protocol::SubmissionDelivery::Queue, .. })
        if submission_id == &provenance.input_id)
    );
    let acquired = first_run
        .iter()
        .find(|event| matches!(event.kind, SessionEventKind::WorkspaceExecutionAcquired))
        .unwrap();
    let turn_started = second_run
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::TurnStarted))
        .unwrap();
    // The released directory is admitted immediately; these events describe a wait only.
    assert!(!second_run.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::WorkspaceExecutionWaiting | SessionEventKind::WorkspaceExecutionAcquired
    )));
    let context = second_run.iter().position(|event| matches!(&event.kind,
        SessionEventKind::HookContextAdded { content, reference: Some(_), .. }
            if content.contains("FRESH_STEERING_AFTER_HOLDER_EDIT") && !content.contains("STALE_STEERING_CONTEXT")
    )).unwrap();
    let started = second_run
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::ModelRequestStarted { .. }))
        .unwrap();
    assert!(acquired.seq < first_finished.seq);
    assert!(first_finished.seq < second_run[turn_started].seq);
    assert!(turn_started < context && context < started);
    let requests = model.requests();
    let completions = requests
        .iter()
        .filter(|request| {
            request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty())
        })
        .collect::<Vec<_>>();
    assert_eq!(completions.len(), 2);
    let first_messages = completions[0]["messages"].to_string();
    assert!(first_messages.contains("B: queued before interruption"));
    assert!(!first_messages.contains("Use this collaborator's file"));
    assert!(!first_messages.contains("FRESH_STEERING_AFTER_HOLDER_EDIT"));
    let users = completions[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .collect::<Vec<_>>();
    let first_user = users
        .iter()
        .position(|message| {
            message["content"]
                .to_string()
                .contains("B: queued before interruption")
        })
        .unwrap();
    let second_user = users
        .iter()
        .position(|message| {
            message["content"]
                .to_string()
                .contains("Use this collaborator's file")
        })
        .unwrap();
    assert!(first_user < second_user);
    assert!(
        completions[1]["messages"]
            .to_string()
            .contains("FRESH_STEERING_AFTER_HOLDER_EDIT")
    );
    assert_eq!(
        app.model_input_origin("reader", &first.run_id)
            .await
            .map(|origin| (origin.session_id, origin.provenance))
            .unwrap(),
        (SessionId::new("reader"), Some(first_author))
    );
    assert_eq!(
        app.model_input_origin("reader", &accepted.run_id)
            .await
            .map(|origin| (origin.session_id, origin.provenance))
            .unwrap(),
        (SessionId::new("reader"), Some(provenance))
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.to_string().contains("STALE_STEERING_CONTEXT"))
    );
    app.shutdown().await.unwrap();
}
