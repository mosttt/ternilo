use std::{future::Future, path::Path, pin::Pin, sync::Arc, time::Duration};

use ternilo_kernel::{
    ActivityBranch, ExecutionActivityOutput, ExecutionAdmission, UserInteraction,
    WorkspaceExecutionLease,
};
use ternilo_protocol::{
    ExecutionActivityPhase, SessionExecutionPhase, UserAnswer, UserQuestion, WorkflowMeta,
    WorkflowRunId,
};

use super::{tests::open_test_application, *};

struct PersistedEvent {
    sequence: u64,
    release: tokio::sync::oneshot::Sender<Result<(), HarnessError>>,
}

enum CommitPoint {
    Activity,
    WorkflowStart,
}

struct CommitBarrier {
    store: Arc<JsonlEventStore>,
    point: CommitPoint,
    committed: tokio::sync::mpsc::UnboundedSender<PersistedEvent>,
}

impl SessionEventStore for CommitBarrier {
    fn load<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>> {
        self.store.load()
    }

    fn append<'a>(
        &'a self,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.store.append(event.clone()).await?;
            if matches!(
                (&self.point, &event.kind),
                (
                    CommitPoint::Activity,
                    SessionEventKind::ExecutionActivityChanged { .. }
                ) | (
                    CommitPoint::WorkflowStart,
                    SessionEventKind::WorkflowRunStarted { .. }
                )
            ) {
                let (release, waiting) = tokio::sync::oneshot::channel();
                self.committed
                    .send(PersistedEvent {
                        sequence: event.seq,
                        release,
                    })
                    .unwrap();
                waiting
                    .await
                    .map_err(|_| HarnessError::execution("event commit barrier closed"))??;
            }
            Ok(())
        })
    }
}

struct SessionPhaseOutput {
    harness: Arc<HarnessSession>,
    run: RunId,
}

impl ExecutionActivityOutput for SessionPhaseOutput {
    fn changed<'a>(
        &'a self,
        phase: ExecutionActivityPhase,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.harness
                .append_event(
                    self.run.clone(),
                    SessionEventKind::ExecutionActivityChanged { phase },
                )
                .await
                .map(|_| ())
        })
    }
}

struct ParkOnlyAdmission;

impl ExecutionAdmission for ParkOnlyAdmission {
    fn park<'a>(
        &'a self,
        _: Vec<ternilo_protocol::AcceptedSubagentRun>,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn resume<'a>(
        &'a self,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { panic!("a cancelled activity must not request resumption") })
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep durable phase storage, both cancellation paths, blocked terminal publication and sequence recovery together."
)]
async fn cancelling_activity_after_phase_storage_cannot_reuse_its_sequence_for_the_terminal_event()
{
    for cancel_run in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let session = SessionId::new("phase-commit");
        let run = RunId::new("phase-run");
        let store = Arc::new(JsonlEventStore::new(directory.path(), &session));
        let (committed, mut phases) = tokio::sync::mpsc::unbounded_channel();
        let environment = HostEnvironment::new(
            SessionIdentity {
                tenant_id: TenantId::new("local"),
                user_id: UserId::new("owner"),
                agent_id: AgentId::new("agent"),
                session_id: session,
            },
            None,
            HostPolicy::local(ternilo_protocol::RunLimits::default()),
            Arc::new(CommitBarrier {
                store: Arc::clone(&store),
                point: CommitPoint::Activity,
                committed,
            }),
        );
        let harness = Arc::new(
            HarnessSession::boot(
                &ternilo_builtins::catalog().unwrap(),
                &ternilo_builtins::local_profile(),
                environment,
            )
            .await
            .unwrap(),
        );
        harness
            .append_event(run.clone(), SessionEventKind::TurnStarted)
            .await
            .unwrap();
        let cancellation = RunCancellation::new();
        let activity = ActivityBranch::managed_with_output(
            Arc::new(ParkOnlyAdmission),
            cancellation.clone(),
            Arc::new(SessionPhaseOutput {
                harness: Arc::clone(&harness),
                run: run.clone(),
            }),
        );
        let waiting = {
            let branch = activity.clone();
            tokio::spawn(async move {
                branch
                    .wait_for::<()>(
                        ternilo_protocol::AcceptedSubagentRun {
                            session_id: SessionId::new("child"),
                            run_id: RunId::new("child-run"),
                        },
                        std::future::pending(),
                    )
                    .await
            })
        };
        let phase = tokio::time::timeout(Duration::from_secs(3), phases.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(phase.sequence, 1);
        assert_eq!(
            store.load().await.unwrap().len(),
            2,
            "the phase must already be durable while Sessions still holds its sequence lock"
        );
        if cancel_run {
            cancellation.cancel();
            assert!(
                tokio::time::timeout(Duration::from_secs(3), waiting)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err()
                    .is_cancelled()
            );
        } else {
            waiting.abort();
            assert!(waiting.await.unwrap_err().is_cancelled());
        }
        drop(activity);
        let mut terminal = {
            let harness = Arc::clone(&harness);
            tokio::spawn(async move {
                harness
                    .append_event(run, SessionEventKind::TurnCancelled)
                    .await
            })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut terminal)
                .await
                .is_err(),
            "terminal publication must wait for the already-written phase commit"
        );
        assert!(
            !phase.release.is_closed(),
            "neither cancellation nor dropping the last activity may discard its in-progress commit"
        );
        phase.release.send(Ok(())).unwrap();
        let terminal = tokio::time::timeout(Duration::from_secs(3), terminal)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(terminal.seq, 2);
        let events = store.load().await.unwrap();
        assert_eq!(
            events.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(harness.events().await, events);
        harness.shutdown().await.unwrap();
    }
}

struct ApproveWorkflow;

impl UserInteraction for ApproveWorkflow {
    fn ask<'a>(
        &'a self,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Result<UserAnswer, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(
                question
                    .tool_approval
                    .as_ref()
                    .map(|approval| approval.tool_name.as_str()),
                Some("workflow")
            );
            Ok(UserAnswer {
                question_id: question.id,
                selected: vec!["Allow once".to_owned()],
                custom: None,
            })
        })
    }
}

async fn workflow_commit_harness(store: Arc<dyn SessionEventStore>) -> Arc<HarnessSession> {
    let environment = HostEnvironment::with_interaction(
        SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("owner"),
            agent_id: AgentId::new("agent"),
            session_id: SessionId::new("workflow-commit"),
        },
        None,
        HostPolicy::local(ternilo_protocol::RunLimits::default()),
        store,
        Arc::new(ApproveWorkflow),
    );
    Arc::new(
        HarnessSession::boot(
            &ternilo_builtins::catalog().unwrap(),
            &ternilo_builtins::local_profile(),
            environment,
        )
        .await
        .unwrap(),
    )
}

fn workflow_started() -> SessionEventKind {
    SessionEventKind::WorkflowRunStarted {
        workflow_id: WorkflowRunId::new("workflow-commit-test"),
        meta: WorkflowMeta {
            name: "commit-test".to_owned(),
            description: "Verify ownership of a started event commit".to_owned(),
            when_to_use: None,
            phases: Vec::new(),
        },
    }
}

#[tokio::test]
async fn cancelling_workflow_during_durable_start_finishes_without_deadlock_and_reloads() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlEventStore::new(
        directory.path(),
        &SessionId::new("workflow-commit"),
    ));
    let (committed, mut persisted) = tokio::sync::mpsc::unbounded_channel();
    let harness = workflow_commit_harness(Arc::new(CommitBarrier {
        store: Arc::clone(&store),
        point: CommitPoint::WorkflowStart,
        committed,
    }))
    .await;
    let run = RunId::new("workflow-run");
    let mut running = {
        let harness = Arc::clone(&harness);
        let run = run.clone();
        tokio::spawn(async move {
            harness
                .run(
                    run,
                    format!(
                        "/workflow {}",
                        serde_json::json!({
                            "meta": {
                                "name": "commit-test",
                                "description": "Cancel while WorkflowRunStarted is being committed",
                            },
                            "script": "42",
                        })
                    ),
                )
                .await
        })
    };
    let persisted = tokio::time::timeout(Duration::from_secs(3), persisted.recv())
        .await
        .unwrap()
        .unwrap();
    let events = store.load().await.unwrap();
    assert_eq!(events.last().unwrap().seq, persisted.sequence);
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::WorkflowRunStarted { .. }
    ));
    harness.cancel(run).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut running)
            .await
            .is_err(),
        "the cancelled command must wait for the already-durable workflow event"
    );
    assert!(!persisted.release.is_closed());
    persisted.release.send(Ok(())).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .expect("the cancelled tool future must not retain the Sessions lock")
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    let events = store.load().await.unwrap();
    assert_eq!(harness.events().await, events);
    assert!(
        events
            .iter()
            .enumerate()
            .all(|(seq, event)| event.seq == u64::try_from(seq).unwrap())
    );
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::TurnCancelled
    ));
    harness.shutdown().await.unwrap();
    let reloaded = workflow_commit_harness(store).await;
    assert_eq!(reloaded.events().await, events);
    reloaded.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_awaits_workflow_event_commit_after_its_caller_is_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlEventStore::new(
        directory.path(),
        &SessionId::new("workflow-commit"),
    ));
    let (committed, mut persisted) = tokio::sync::mpsc::unbounded_channel();
    let harness = workflow_commit_harness(Arc::new(CommitBarrier {
        store: Arc::clone(&store),
        point: CommitPoint::WorkflowStart,
        committed,
    }))
    .await;
    let caller = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            harness
                .append_event(RunId::new("workflow-run"), workflow_started())
                .await
        })
    };
    let persisted = tokio::time::timeout(Duration::from_secs(3), persisted.recv())
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let mut shutdown = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move { harness.shutdown().await })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut shutdown)
            .await
            .is_err(),
        "plugin shutdown must drain its owned commits before unloading dependencies"
    );
    assert!(!persisted.release.is_closed());
    persisted.release.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(3), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let events = store.load().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 0);
    let reloaded = workflow_commit_harness(store).await;
    assert_eq!(reloaded.events().await, events);
    reloaded.shutdown().await.unwrap();
}

#[tokio::test]
async fn workflow_commit_io_error_is_preserved_and_its_sequence_is_not_reused() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlEventStore::new(
        directory.path(),
        &SessionId::new("workflow-commit"),
    ));
    let (committed, mut persisted) = tokio::sync::mpsc::unbounded_channel();
    let harness = workflow_commit_harness(Arc::new(CommitBarrier {
        store: Arc::clone(&store),
        point: CommitPoint::WorkflowStart,
        committed,
    }))
    .await;
    let caller = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            harness
                .append_event(RunId::new("workflow-run"), workflow_started())
                .await
        })
    };
    let persisted = tokio::time::timeout(Duration::from_secs(3), persisted.recv())
        .await
        .unwrap()
        .unwrap();
    let error = HarnessError::execution("injected post-write storage failure");
    persisted.release.send(Err(error.clone())).unwrap();
    assert_eq!(caller.await.unwrap().unwrap_err(), error);
    assert_eq!(
        harness
            .append_event(RunId::new("workflow-run"), SessionEventKind::TurnCancelled)
            .await
            .unwrap_err(),
        error
    );
    assert!(harness.shutdown().await.is_err());
    let events = store.load().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 0);
    let reloaded = workflow_commit_harness(store).await;
    assert_eq!(reloaded.events().await, events);
    reloaded.shutdown().await.unwrap();
}

struct WaitingRun {
    session: String,
    run: RunId,
    managed: Arc<ManagedSession>,
    task: tokio::task::JoinHandle<Result<RunOutcome, HarnessError>>,
    _directory: WorkspaceExecutionLease,
}

async fn waiting_run(app: &Arc<LocalApplication>, root: &Path, name: &str) -> WaitingRun {
    let path = root.join(name);
    tokio::fs::create_dir(&path).await.unwrap();
    tokio::fs::write(path.join("file.txt"), "read after admission")
        .await
        .unwrap();
    let workspace = app.add_workspace(path.to_str().unwrap()).await.unwrap();
    let session = app
        .create_session(workspace.workspace_id, Some(name.to_owned()), None)
        .await
        .unwrap();
    let id = session.identity.session_id.to_string();
    let managed = app.addressable_session(&id).await.unwrap();
    let directory = app
        .directory_coordinator
        .bind(format!("held-{name}"), path)
        .acquire(RunCancellation::new())
        .await
        .unwrap();
    let mut events = app.subscribe_events();
    let run = RunId::new(format!("run-{name}"));
    let task = {
        let app = Arc::clone(app);
        let id = id.clone();
        let run = run.clone();
        tokio::spawn(async move {
            app.run_turn(
                &id,
                Some(run.to_string()),
                "/read {\"path\":\"file.txt\"}".to_owned(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.session_id == id
                && matches!(
                    event.event.kind,
                    SessionEventKind::WorkspaceExecutionWaiting
                )
            {
                break;
            }
        }
    })
    .await
    .expect("the real run must reach workspace waiting");
    WaitingRun {
        session: id,
        run,
        managed,
        task,
        _directory: directory,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep committed events, independent workspaces, live baselines and terminal cleanup in one integration scenario."
)]
async fn committed_execution_events_update_live_baselines_and_node_invalidations_without_state_rewrites()
 {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().join("data");
    let mut app = open_test_application(data.clone()).await;
    app.directory_coordinator = crate::DirectoryCoordinator::new(directory.path().join("locks"));
    let app = Arc::new(app);
    let first = waiting_run(&app, directory.path(), "first").await;
    let second = waiting_run(&app, directory.path(), "second").await;
    let state_before = tokio::fs::read(data.join("data/state.json")).await.unwrap();
    let state_modified = tokio::fs::metadata(data.join("data/state.json"))
        .await
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        app.live_activity(&second.session)
            .await
            .unwrap()
            .execution
            .unwrap()
            .phase,
        SessionExecutionPhase::WaitingForWorkspace
    );
    let mut notices = app.subscribe_invalidations();
    for (phase, expected) in [
        (
            ExecutionActivityPhase::WaitingForSubagents,
            SessionExecutionPhase::WaitingForSubagents,
        ),
        (
            ExecutionActivityPhase::WaitingForCapacity,
            SessionExecutionPhase::WaitingForCapacity,
        ),
        (
            ExecutionActivityPhase::Running,
            SessionExecutionPhase::Running,
        ),
    ] {
        let event = first
            .managed
            .harness
            .append_event(
                first.run.clone(),
                SessionEventKind::ExecutionActivityChanged { phase },
            )
            .await
            .unwrap();
        let notice = tokio::time::timeout(Duration::from_secs(3), notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notice.category, crate::LocalInvalidationCategory::Activity);
        assert_eq!(notice.session_id.as_deref(), Some(first.session.as_str()));
        assert_eq!(notice.revision, Some(event.seq));
        let current = app.live_activity(&first.session).await.unwrap();
        assert!(current.running);
        assert_eq!(current.execution.as_ref().unwrap().run_id, first.run);
        assert_eq!(current.execution.as_ref().unwrap().phase, expected);
        let baseline = app.live_activities().await;
        assert_eq!(
            baseline
                .iter()
                .find(|activity| activity.session_id.as_str() == first.session)
                .unwrap(),
            &current
        );
        assert_eq!(
            baseline
                .iter()
                .find(|activity| activity.session_id.as_str() == second.session)
                .unwrap()
                .execution
                .as_ref()
                .unwrap()
                .phase,
            SessionExecutionPhase::WaitingForWorkspace
        );
    }
    for kind in [
        SessionEventKind::ExecutionActivityChanged {
            phase: ExecutionActivityPhase::WaitingForCapacity,
        },
        SessionEventKind::TurnCancelled,
    ] {
        first
            .managed
            .harness
            .append_event(RunId::new("older-run"), kind)
            .await
            .unwrap();
    }
    assert!(notices.try_recv().is_err());
    assert_eq!(
        app.live_activity(&first.session)
            .await
            .unwrap()
            .execution
            .unwrap()
            .phase,
        SessionExecutionPhase::Running
    );
    assert_eq!(
        tokio::fs::read(data.join("data/state.json")).await.unwrap(),
        state_before
    );
    assert_eq!(
        tokio::fs::metadata(data.join("data/state.json"))
            .await
            .unwrap()
            .modified()
            .unwrap(),
        state_modified
    );
    app.cancel_turn(&first.session, first.run.as_str())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), first.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    let idle = app.live_activity(&first.session).await.unwrap();
    assert!(!idle.running && idle.execution.is_none());
    first
        .managed
        .harness
        .append_event(
            RunId::new("no-active-driver"),
            SessionEventKind::TurnStarted,
        )
        .await
        .unwrap();
    assert!(
        app.live_activity(&first.session)
            .await
            .unwrap()
            .execution
            .is_none(),
        "an event record cannot impersonate an actually running harness"
    );
    first
        .managed
        .harness
        .append_event(
            RunId::new("no-active-driver"),
            SessionEventKind::TurnCancelled,
        )
        .await
        .unwrap();
    app.cancel_turn(&second.session, second.run.as_str())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), second.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    app.shutdown().await.unwrap();
}
