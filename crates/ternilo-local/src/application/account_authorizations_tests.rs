use super::*;
use ternilo_protocol::{ErrorCode, RunLimits, SubmissionPlacement};
use ternilo_transport::{
    ExecutorId, NodeAccountAuthorization, NodeCleanupRequest, NodeCleanupSnapshot,
    NodeCleanupState, NodeInputAuthorization,
};

fn binding() -> crate::LocalServerBinding {
    crate::LocalServerBinding {
        server_url: "https://server.example.test/".to_owned(),
        node_id: "shared-node".to_owned(),
    }
}
fn account_input(id: &str, name: &str) -> InputProvenance {
    InputProvenance {
        input_id: SubmissionId::new(id),
        run_id: None,
        author: InputAuthor::Account {
            user_id: UserId::new(name),
            username: name.to_owned(),
        },
    }
}
fn proof(revision: u64) -> NodeInputAuthorization {
    NodeInputAuthorization {
        credential_id: "credential-one".to_owned(),
        status_revision: revision,
    }
}
fn snapshot(alice_revision: u64, alice_active: bool, revoke: bool) -> NodeCleanupSnapshot {
    NodeCleanupSnapshot {
        protocol_version: 1,
        server_id: "server-owner".to_owned(),
        tenant_id: TenantId::new("tenant"),
        executor_id: ExecutorId::new("shared-node"),
        credential_id: "credential-one".to_owned(),
        connection_allowed: true,
        authorizations: vec![
            NodeAccountAuthorization {
                user_id: UserId::new("alice"),
                status_revision: alice_revision,
                active: alice_active,
            },
            NodeAccountAuthorization {
                user_id: UserId::new("bob"),
                status_revision: 1,
                active: true,
            },
        ],
        requests: if revoke {
            vec![NodeCleanupRequest {
                request_id: "cleanup-alice".to_owned(),
                user_id: UserId::new("alice"),
                status_revision: 2,
                created_at_ms: 20,
                state: NodeCleanupState::Pending,
                detail: None,
                confirmed_at_ms: None,
            }]
        } else {
            vec![]
        },
    }
}
async fn open(path: PathBuf, bound: bool) -> Arc<LocalApplication> {
    Arc::new(
        LocalApplication::open_with_options(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            path,
            crate::LocalApplicationOpenOptions {
                server_binding: bound.then(binding),
            },
        )
        .await
        .unwrap(),
    )
}
fn queued(provenance: InputProvenance, message: &str) -> SessionSubmission {
    SessionSubmission {
        id: provenance.input_id.clone(),
        run_id: RunId::new(format!("run-{}", provenance.input_id)),
        provenance: Some(provenance),
        content: SubmissionContent::Prompt {
            input: message.to_owned(),
        },
        references: vec![],
        attachments: vec![],
        placement: SubmissionPlacement::Queued,
        created_at_ms: 10,
        updated_at_ms: 10,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise mixed-author behavior across the complete cleanup lifecycle"
)]
async fn restart_requires_authority_and_revocation_preserves_bob_and_local_inputs() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("data");
    let workspace_path = root.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await.unwrap();
    let app = open(directory.clone(), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let workspace = app
        .add_workspace(workspace_path.to_str().unwrap())
        .await
        .unwrap();
    let session = app
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let id = session.identity.session_id.to_string();
    let alice = account_input("alice-old", "alice");
    let bob = account_input("bob-old", "bob");
    app.record_account_input_authorization(&alice, &proof(1))
        .await
        .unwrap();
    app.record_account_input_authorization(&bob, &proof(1))
        .await
        .unwrap();
    app.inbox
        .enqueue(&id, queued(alice.clone(), "/code \"alice\""))
        .await
        .unwrap();
    app.inbox
        .enqueue(&id, queued(bob.clone(), "/code \"bob\""))
        .await
        .unwrap();
    app.inbox
        .enqueue(
            &id,
            queued(app.local_input_provenance().unwrap(), "/code \"local\""),
        )
        .await
        .unwrap();
    app.close().await.unwrap();
    drop(app);
    // Opening through an ordinary local/desktop entry cannot clear the stored binding.
    let app = open(directory, false).await;
    assert_eq!(app.account_server_binding().await, Some(binding()));
    assert_eq!(
        app.account_authorizations
            .check(Some(&alice))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Unavailable
    );
    app.resume_pending_submissions().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(app.events(&id).await.unwrap().is_empty());
    assert_eq!(app.inbox.snapshot(&id).await.unwrap().items.len(), 3);
    app.synchronize_account_authorizations(&snapshot(3, true, true))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if app.inbox.snapshot(&id).await.unwrap().items.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let events = app.events(&id).await.unwrap();
    let authors: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let SessionEventKind::UserMessage { content, .. } = &event.kind {
                Some(content.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(authors, vec!["/code \"bob\"", "/code \"local\""]);
    assert_eq!(
        app.record_account_input_authorization(&alice, &proof(1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let fresh = account_input("alice-fresh", "alice");
    app.record_account_input_authorization(&fresh, &proof(3))
        .await
        .unwrap();
    app.run_session_input_with_provenance(
        &id,
        SessionSubmissionRequest {
            run_id: Some(RunId::new("fresh-run")),
            content: SubmissionContent::Prompt {
                input: "/code \"fresh\"".to_owned(),
            },
            references: vec![],
            attachments: vec![],
            delivery: SubmissionDelivery::Queue,
        },
        fresh,
    )
    .await
    .unwrap();
    let mut replacement = snapshot(3, true, true);
    replacement.credential_id = "credential-two".to_owned();
    app.synchronize_account_authorizations(&replacement)
        .await
        .unwrap();
    assert_eq!(
        app.account_authorizations
            .check(Some(&bob))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    app.close().await.unwrap();
}

#[tokio::test]
async fn binding_and_input_identity_cannot_be_reassigned() {
    let root = tempfile::tempdir().unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let alice = account_input("one-input", "alice");
    app.record_account_input_authorization(&alice, &proof(1))
        .await
        .unwrap();
    assert_eq!(
        app.record_account_input_authorization(&account_input("one-input", "bob"), &proof(1))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut other = snapshot(1, true, false);
    other.server_id = "other-server-owner".to_owned();
    assert_eq!(
        app.synchronize_account_authorizations(&other)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    app.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise mixed-author behavior across the complete cleanup lifecycle"
)]
async fn revocation_stops_only_its_owned_background_writer_and_releases_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let mut directories = Vec::new();
    for name in ["alice", "bob", "local"] {
        let directory = root.path().join(name);
        tokio::fs::create_dir(&directory).await.unwrap();
        let workspace = app
            .add_workspace(directory.to_str().unwrap())
            .await
            .unwrap();
        app.create_session(workspace.workspace_id, Some(name.to_owned()), None)
            .await
            .unwrap();
        app.update_permissions(name, ternilo_protocol::PermissionPreset::FullAccess)
            .await
            .unwrap();
        let input = if name == "local" {
            app.local_input_provenance().unwrap()
        } else {
            account_input(&format!("{name}-writer"), name)
        };
        if name != "local" {
            app.record_account_input_authorization(&input, &proof(1))
                .await
                .unwrap();
        }
        app.run_session_input_with_provenance(
            name,
            SessionSubmissionRequest {
                run_id: Some(RunId::new(format!("{name}-job-run"))),
                content: SubmissionContent::Prompt {
                    input: "/job trap '' TERM; while :; do printf x >> heartbeat; sleep 0.01; done"
                        .to_owned(),
                },
                references: vec![],
                attachments: vec![],
                delivery: SubmissionDelivery::Queue,
            },
            input,
        )
        .await
        .unwrap();
        directories.push(directory);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while directories
            .iter()
            .any(|directory| !directory.join("heartbeat").exists())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let receipts = app
        .synchronize_account_authorizations(&snapshot(2, false, true))
        .await
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].state,
        NodeCleanupState::Confirmed,
        "{receipts:?}"
    );
    let before = tokio::fs::metadata(directories[0].join("heartbeat"))
        .await
        .unwrap()
        .len();
    let other = tokio::fs::metadata(directories[1].join("heartbeat"))
        .await
        .unwrap()
        .len();
    let local = tokio::fs::metadata(directories[2].join("heartbeat"))
        .await
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        before,
        tokio::fs::metadata(directories[0].join("heartbeat"))
            .await
            .unwrap()
            .len()
    );
    assert!(
        other
            < tokio::fs::metadata(directories[1].join("heartbeat"))
                .await
                .unwrap()
                .len()
    );
    assert!(
        local
            < tokio::fs::metadata(directories[2].join("heartbeat"))
                .await
                .unwrap()
                .len()
    );
    app.run_turn(
        "alice",
        Some("local-successor".to_owned()),
        "/shell printf released > successor".to_owned(),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(directories[0].join("successor"))
            .await
            .unwrap(),
        "released"
    );
    app.archive_session("bob").await.unwrap();
    let stopped_bob = tokio::fs::metadata(directories[1].join("heartbeat"))
        .await
        .unwrap()
        .len();
    let running_local = tokio::fs::metadata(directories[2].join("heartbeat"))
        .await
        .unwrap()
        .len();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        stopped_bob,
        tokio::fs::metadata(directories[1].join("heartbeat"))
            .await
            .unwrap()
            .len()
    );
    assert!(
        running_local
            < tokio::fs::metadata(directories[2].join("heartbeat"))
                .await
                .unwrap()
                .len()
    );
    app.close().await.unwrap();
}

fn request(input: &str, run: &str) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        run_id: Some(RunId::new(run)),
        content: SubmissionContent::Prompt {
            input: input.to_owned(),
        },
        references: vec![],
        attachments: vec![],
        delivery: SubmissionDelivery::Queue,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_alice_does_not_pause_bob_or_local_queued_behind_her() {
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
    app.create_session(workspace.workspace_id, Some("mixed".into()), None)
        .await
        .unwrap();
    app.update_permissions("mixed", ternilo_protocol::PermissionPreset::FullAccess)
        .await
        .unwrap();
    let alice = account_input("alice-active", "alice");
    let bob = account_input("bob-queued", "bob");
    for input in [&alice, &bob] {
        app.record_account_input_authorization(input, &proof(1))
            .await
            .unwrap();
    }
    app.submit_session_with_provenance(
        "mixed",
        request("/shell printf ready > started; sleep 120", "alice-active"),
        alice,
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !directory.join("started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    app.submit_session_with_provenance("mixed", request("/code \"bob\"", "bob-queued"), bob)
        .await
        .unwrap();
    app.submit_session("mixed", request("/code \"local\"", "local-queued"))
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
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let inbox = app.session_inbox("mixed").await.unwrap();
            assert!(!inbox.paused, "revocation must not pause other authors");
            if inbox.items.is_empty() && inbox.active_run_id.is_none() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let inputs = app
        .events("mixed")
        .await
        .unwrap()
        .into_iter()
        .filter_map(|event| {
            if let SessionEventKind::UserMessage { content, .. } = event.kind {
                Some(content)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(inputs.iter().any(|input| input == "/code \"bob\""));
    assert!(inputs.iter().any(|input| input == "/code \"local\""));
    app.close().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "verify archived event persistence, lost ACK replay and restart without booting work"
)]
async fn archived_cleanup_removes_only_revoked_work_without_starting_the_session() {
    use ternilo_protocol::{GoalStatus, ScheduleChange, ScheduleId, ScheduleRecord, ScheduleRule};
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let app = open(data.clone(), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let workspace = app
        .add_workspace(root.path().to_str().unwrap())
        .await
        .unwrap();
    app.create_session(workspace.workspace_id, Some("archived".into()), None)
        .await
        .unwrap();
    for name in ["alice", "bob"] {
        let input = account_input(name, name);
        app.record_account_input_authorization(&input, &proof(1))
            .await
            .unwrap();
        app.run_session_input_with_provenance("archived", request("/code 1", name), input)
            .await
            .unwrap();
        app.addressable_session("archived")
            .await
            .unwrap()
            .harness
            .append_event(
                RunId::new(name),
                SessionEventKind::ScheduleChanged {
                    change: ScheduleChange::Create {
                        schedule: ScheduleRecord {
                            id: ScheduleId::new(name),
                            prompt: format!("reminder for {name}"),
                            rule: ScheduleRule::After {
                                after_seconds: 3600,
                            },
                            scheduled_at_ms: now_ms().unwrap() + 3_600_000,
                            created_at_ms: now_ms().unwrap(),
                        },
                    },
                },
            )
            .await
            .unwrap();
    }
    app.addressable_session("archived")
        .await
        .unwrap()
        .harness
        .append_event(
            RunId::new("alice"),
            SessionEventKind::GoalUpdated {
                objective: "Alice goal".into(),
                status: GoalStatus::Active,
            },
        )
        .await
        .unwrap();
    app.archive_session("archived").await.unwrap();
    assert!(app.live.read().await.is_empty());
    let receipts = app
        .synchronize_account_authorizations(&snapshot(2, false, true))
        .await
        .unwrap();
    assert_eq!(
        receipts[0].state,
        NodeCleanupState::Confirmed,
        "{receipts:?}"
    );
    assert!(
        app.live.read().await.is_empty(),
        "cleanup must not boot an archived runtime"
    );
    assert_eq!(
        app.synchronize_account_authorizations(&snapshot(2, false, true))
            .await
            .unwrap(),
        receipts,
        "a lost completion ACK is replayed from the durable receipt"
    );
    let events = app.archived_events("archived").await.unwrap();
    assert_eq!(
        ternilo_builtins::pending_schedules(&events)
            .unwrap()
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>(),
        ["bob"]
    );
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::GoalUpdated {
            status: GoalStatus::Blocked,
            ..
        }
    ));
    for (seq, event) in events.iter().enumerate() {
        assert_eq!(event.seq, seq as u64);
    }
    app.close().await.unwrap();
    drop(app);
    let restarted = open(data, false).await;
    assert!(restarted.live.read().await.is_empty());
    assert_eq!(restarted.archived_events("archived").await.unwrap(), events);
    restarted.close().await.unwrap();
}

#[tokio::test]
async fn queued_admission_rechecks_revocation_after_waiting_for_the_session() {
    let root = tempfile::tempdir().unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let workspace = app
        .add_workspace(root.path().to_str().unwrap())
        .await
        .unwrap();
    app.create_session(workspace.workspace_id, Some("admission".into()), None)
        .await
        .unwrap();
    let input = account_input("late-alice", "alice");
    app.record_account_input_authorization(&input, &proof(1))
        .await
        .unwrap();
    let lifecycle = app.session_lifecycle("admission").await;
    let held = lifecycle.lock().await;
    let submitting =
        app.submit_session_with_provenance("admission", request("/code 1", "late-alice"), input);
    tokio::pin!(submitting);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut submitting)
            .await
            .is_err()
    );
    app.account_authorizations
        .synchronize(&snapshot(2, false, true))
        .await
        .unwrap();
    drop(held);
    assert_eq!(submitting.await.unwrap_err().code, ErrorCode::PolicyDenied);
    assert!(
        app.session_inbox("admission")
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(app.events("admission").await.unwrap().is_empty());
    app.close().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "exercise mixed-author behavior across the complete cleanup lifecycle"
)]
async fn live_schedule_cleanup_refreshes_the_catalog_and_preserves_bobs_new_goal() {
    use ternilo_protocol::GoalStatus;
    let root = tempfile::tempdir().unwrap();
    let app = open(root.path().join("data"), true).await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let workspace = app
        .add_workspace(root.path().to_str().unwrap())
        .await
        .unwrap();
    app.create_session(workspace.workspace_id, Some("live-work".into()), None)
        .await
        .unwrap();
    app.update_permissions("live-work", ternilo_protocol::PermissionPreset::FullAccess)
        .await
        .unwrap();
    for name in ["alice", "bob"] {
        let input = account_input(name, name);
        app.record_account_input_authorization(&input, &proof(1))
            .await
            .unwrap();
        let running = {
            let app = Arc::clone(&app);
            tokio::spawn(async move {
                app.run_session_input_with_provenance(
                    "live-work",
                    request(&format!("/schedule-after 3600 {name}"), name),
                    input,
                )
                .await
            })
        };
        let question = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(question) = app
                    .pending_questions(Some("live-work"))
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
        assert_eq!(
            question.question.tool_approval.as_ref().unwrap().tool_name,
            "schedule_create"
        );
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
    }
    let managed = app.addressable_session("live-work").await.unwrap();
    let next_seq = managed.harness.events().await.len() as u64;
    managed
        .harness
        .append_event(
            RunId::new("bob"),
            SessionEventKind::GoalUpdated {
                objective: "Bob continues".into(),
                status: GoalStatus::Active,
            },
        )
        .await
        .unwrap();
    assert!(
        managed
            .harness
            .append_event_if_next_seq(
                next_seq,
                RunId::new("alice"),
                SessionEventKind::GoalUpdated {
                    objective: "Stale Alice snapshot".into(),
                    status: GoalStatus::Blocked,
                }
            )
            .await
            .unwrap()
            .is_none()
    );
    let receipts = app
        .synchronize_account_authorizations(&snapshot(2, false, true))
        .await
        .unwrap();
    assert_eq!(
        receipts[0].state,
        NodeCleanupState::Confirmed,
        "{receipts:?}"
    );
    let events = app.events("live-work").await.unwrap();
    let goal = events
        .iter()
        .rev()
        .find(|event| matches!(event.kind, SessionEventKind::GoalUpdated { .. }))
        .unwrap();
    assert_eq!(goal.run_id.as_str(), "bob");
    assert!(matches!(
        goal.kind,
        SessionEventKind::GoalUpdated {
            status: GoalStatus::Active,
            ..
        }
    ));
    managed
        .harness
        .append_event(
            RunId::new("bob"),
            SessionEventKind::GoalUpdated {
                objective: "Bob continues".into(),
                status: GoalStatus::Complete,
            },
        )
        .await
        .unwrap();
    let outcome = app
        .run_turn("live-work", None, "/schedules".into())
        .await
        .unwrap();
    let output = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            SessionEventKind::ToolCallFinished { name, output, .. } if name == "schedule_list" => {
                Some(&output.content)
            }
            _ => None,
        })
        .unwrap();
    let listed: serde_json::Value = serde_json::from_str(output).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["prompt"], "bob");
    app.close().await.unwrap();
}

#[tokio::test]
async fn restart_without_a_resource_completion_observer_cannot_confirm_cleanup() {
    use std::{future::Future, pin::Pin};
    struct Unfinished;
    impl ternilo_kernel::ExecutionResourceControl for Unfinished {
        fn stop<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async { Err(HarnessError::unavailable("unfinished execution")) })
        }
        fn is_finished(&self) -> bool {
            false
        }
    }
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let app = open(data.clone(), true).await;
    let storage_id = app.node_storage_instance_id().await;
    app.synchronize_account_authorizations(&snapshot(1, true, false))
        .await
        .unwrap();
    let workspace = app
        .add_workspace(root.path().to_str().unwrap())
        .await
        .unwrap();
    app.create_session(
        workspace.workspace_id,
        Some("unknown-resource".into()),
        None,
    )
    .await
    .unwrap();
    let input = account_input("alice", "alice");
    app.record_account_input_authorization(&input, &proof(1))
        .await
        .unwrap();
    app.run_session_input_with_provenance("unknown-resource", request("/code 1", "alice"), input)
        .await
        .unwrap();
    app.execution_resources
        .for_session("unknown-resource".into())
        .register(
            RunId::new("alice"),
            "unfinished-process".into(),
            Arc::new(Unfinished),
        )
        .await
        .unwrap();
    assert!(app.close().await.is_err());
    drop(app);
    let app = open(data, false).await;
    assert_eq!(app.node_storage_instance_id().await, storage_id);
    let receipts = app
        .synchronize_account_authorizations(&snapshot(2, false, true))
        .await
        .unwrap();
    assert_eq!(receipts[0].state, NodeCleanupState::Pending);
    assert_eq!(receipts[0].detail.as_deref(), Some("process_state_unknown"));
    let mut reported = snapshot(2, false, true);
    reported.requests[0].detail = Some("process_state_unknown".into());
    assert!(
        app.synchronize_account_authorizations(&reported)
            .await
            .unwrap()
            .is_empty(),
        "unchanged pending diagnostics must not be uploaded repeatedly"
    );
    app.close().await.unwrap();
}

#[cfg(unix)]
mod acp;

#[cfg(unix)]
mod delegation;
