use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use ternilo_kernel::{
    ExecutionAdmission, HarnessSession, HostEnvironment, HostPolicy, MemoryEventStore,
    RunCancellation, SubagentAdmission, SubagentBackend, SubagentBackendContext,
    SubagentBackendRegistration, SubagentDriver, SubagentRunStart, SubagentSessionBinding,
    UserInteraction,
};
use ternilo_protocol::{
    AcceptedSubagentRun, AgentId, HarnessError, PermissionPreset, RunId, RunLimits, RunOutcome,
    SessionId, SessionIdentity, TenantId, UserAnswer, UserId, UserQuestion, WorkspaceBinding,
    WorkspaceId,
};
use tokio::sync::{mpsc, oneshot};

enum AdmissionEvent {
    Park(Vec<AcceptedSubagentRun>),
    Resume(oneshot::Sender<()>),
}

struct Admission(mpsc::UnboundedSender<AdmissionEvent>);

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

impl ExecutionAdmission for Admission {
    fn park<'a>(
        &'a self,
        dependencies: Vec<AcceptedSubagentRun>,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.send(AdmissionEvent::Park(dependencies)).unwrap();
            Ok(())
        })
    }

    fn resume<'a>(
        &'a self,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let (reply, resumed) = oneshot::channel();
            self.0.send(AdmissionEvent::Resume(reply)).unwrap();
            resumed.await.unwrap();
            Ok(())
        })
    }
}

struct Started {
    prompt: String,
    ticket: AcceptedSubagentRun,
    admission: SubagentRunStart,
    finish: oneshot::Sender<String>,
    dropped: oneshot::Receiver<()>,
}

impl Started {
    fn accept(&self) {
        self.admission
            .resolve(Ok(SubagentAdmission::Scheduled(self.ticket.clone())));
    }
}

#[derive(Clone)]
struct Backend {
    entered: mpsc::UnboundedSender<Started>,
    next: Arc<AtomicU64>,
}

impl SubagentBackend for Backend {
    fn create(&self, _: SubagentBackendContext) -> Result<Arc<dyn SubagentDriver>, HarnessError> {
        Ok(Arc::new(self.clone()))
    }
}

struct FinishedDriver(Option<oneshot::Sender<()>>);

impl Drop for FinishedDriver {
    fn drop(&mut self) {
        let _ = self.0.take().unwrap().send(());
    }
}

impl SubagentDriver for Backend {
    fn run<'a>(
        &'a self,
        _: RunId,
        prompt: String,
        _: RunCancellation,
        _: Option<SubagentSessionBinding>,
        admission: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
            let ticket = AcceptedSubagentRun {
                session_id: SessionId::new(format!("workflow-child-{ordinal}")),
                run_id: RunId::new(format!("workflow-run-{ordinal}")),
            };
            let (finish, result) = oneshot::channel();
            let (finished, dropped) = oneshot::channel();
            let _finished = FinishedDriver(Some(finished));
            self.entered
                .send(Started {
                    prompt,
                    ticket,
                    admission,
                    finish,
                    dropped,
                })
                .unwrap();
            result
                .await
                .map_err(|_| HarnessError::execution("fixture result closed"))
        })
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    harness: Arc<HarnessSession>,
    entries: mpsc::UnboundedReceiver<Started>,
    events: mpsc::UnboundedReceiver<AdmissionEvent>,
    running: tokio::task::JoinHandle<Result<RunOutcome, HarnessError>>,
}

impl Fixture {
    async fn start(script: &str, tool_timeout_ms: Option<u64>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let run_id = RunId::new("workflow-parent");
        let (events_tx, events) = mpsc::unbounded_channel();
        let environment = HostEnvironment::with_interaction(
            SessionIdentity {
                tenant_id: TenantId::new("workflow-tests"),
                user_id: UserId::new("owner"),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new("workflow-parent-session"),
            },
            Some(WorkspaceBinding {
                workspace_id: WorkspaceId::new("workspace"),
                path: directory.path().to_string_lossy().into_owned(),
            }),
            HostPolicy {
                permissions: PermissionPreset::FullAccess,
                ..HostPolicy::local(RunLimits::default())
            },
            Arc::new(MemoryEventStore::default()),
            Arc::new(ApproveWorkflow),
        )
        .with_execution_admission(run_id.clone(), Arc::new(Admission(events_tx)));
        let mut profile = crate::local_profile();
        profile
            .plugins
            .iter_mut()
            .find(|plugin| plugin.id == "workflow-engine")
            .unwrap()
            .config = json!({"max_concurrent_agents": 2, "max_wall_ms": 30_000});
        if let Some(timeout_ms) = tool_timeout_ms {
            profile
                .plugins
                .iter_mut()
                .find(|plugin| plugin.kind == "ternilo.tools.registry")
                .unwrap()
                .config = json!({"timeout_ms": timeout_ms});
        }
        let harness = Arc::new(
            HarnessSession::boot(&crate::catalog().unwrap(), &profile, environment)
                .await
                .unwrap(),
        );
        let (entries_tx, entries) = mpsc::unbounded_channel();
        harness
            .subagents()
            .unwrap()
            .register_backend(SubagentBackendRegistration {
                name: "controlled".to_owned(),
                backend: Arc::new(Backend {
                    entered: entries_tx,
                    next: Arc::new(AtomicU64::new(1)),
                }),
            })
            .await
            .unwrap();
        let input = format!(
            "/workflow {}",
            json!({
                "meta": {"name": "activity-test", "description": "Verify real workflow activity"},
                "script": script,
            })
        );
        let worker = Arc::clone(&harness);
        let running = tokio::spawn(async move { worker.run(run_id, input).await });
        Self {
            _directory: directory,
            harness,
            entries,
            events,
            running,
        }
    }

    async fn entry(&mut self) -> Started {
        tokio::select! {
            entry = self.entries.recv() => entry.expect("controlled backend remains registered"),
            result = &mut self.running => panic!("workflow ended before starting the expected child: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(3)) => panic!("workflow did not start the expected child; events: {:?}", self.harness.events().await),
        }
    }

    async fn parked(&mut self, expected: &[&AcceptedSubagentRun]) {
        let event = tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap();
        let AdmissionEvent::Park(mut dependencies) = event else {
            panic!("expected dependency park")
        };
        let mut expected = expected
            .iter()
            .map(|ticket| (*ticket).clone())
            .collect::<Vec<_>>();
        dependencies.sort_by(|a, b| a.run_id.as_str().cmp(b.run_id.as_str()));
        expected.sort_by(|a, b| a.run_id.as_str().cmp(b.run_id.as_str()));
        assert_eq!(dependencies, expected);
    }

    async fn resume(&mut self) {
        let event = tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap();
        let AdmissionEvent::Resume(reply) = event else {
            panic!("expected readmission")
        };
        assert!(!self.running.is_finished());
        reply.send(()).unwrap();
    }

    async fn finish(self) -> RunOutcome {
        let result = tokio::time::timeout(Duration::from_secs(3), self.running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.harness.shutdown().await.unwrap();
        result
    }
}

#[tokio::test]
async fn workflow_parallel_waits_for_all_live_leaves_and_preserves_buffered_order() {
    let mut fixture = Fixture::start(
        r#"
        parallel([
            task("alpha", #{provider: "controlled"}),
            task("beta", #{provider: "controlled"}),
            task("gamma", #{provider: "controlled"})
        ])
    "#,
        None,
    )
    .await;
    let first = fixture.entry().await;
    let second = fixture.entry().await;
    let (first, second) = if first.prompt == "alpha" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!((&*first.prompt, &*second.prompt), ("alpha", "beta"));
    first.accept();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.harness.events().await.iter().any(|event| {
                matches!(
                    event.kind,
                    ternilo_protocol::SessionEventKind::WorkflowAgentStarted { sequence: 1, .. }
                )
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first task reached its wait while the other task remains unaccepted");
    assert!(fixture.events.try_recv().is_err());
    second.accept();
    fixture.parked(&[&first.ticket, &second.ticket]).await;
    assert!(fixture.entries.try_recv().is_err());
    second.finish.send("second".to_owned()).unwrap();
    fixture.resume().await;
    fixture.parked(&[&first.ticket]).await;
    assert!(fixture.entries.try_recv().is_err());
    first.finish.send("first".to_owned()).unwrap();
    fixture.resume().await;
    let third = fixture.entry().await;
    assert_eq!(third.prompt, "gamma");
    third.accept();
    fixture.parked(&[&third.ticket]).await;
    third.finish.send("third".to_owned()).unwrap();
    fixture.resume().await;
    let result = fixture.finish().await;
    let output: serde_json::Value = serde_json::from_str(&result.answer).unwrap();
    assert_eq!(output["result"], json!(["first", "second", "third"]));
}

#[tokio::test]
async fn workflow_pipeline_readmits_each_item_without_a_cross_stage_barrier() {
    let mut fixture = Fixture::start(
        r#"
        pipeline(["alpha", "beta"], [
            stage("scan {{item}}", #{provider: "controlled"}),
            stage("verify {{prev}}", #{provider: "controlled"})
        ])
    "#,
        None,
    )
    .await;
    let first = fixture.entry().await;
    let second = fixture.entry().await;
    let (first, second) = if first.prompt == "scan alpha" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(first.prompt, "scan alpha");
    assert_eq!(second.prompt, "scan beta");
    first.accept();
    second.accept();
    fixture.parked(&[&first.ticket, &second.ticket]).await;
    first.finish.send("alpha scanned".to_owned()).unwrap();
    fixture.resume().await;
    let third = fixture.entry().await;
    assert_eq!(third.prompt, "verify alpha scanned");
    third.accept();
    fixture.parked(&[&second.ticket, &third.ticket]).await;
    second.finish.send("beta scanned".to_owned()).unwrap();
    fixture.resume().await;
    let fourth = fixture.entry().await;
    assert_eq!(fourth.prompt, "verify beta scanned");
    fourth.accept();
    fixture.parked(&[&third.ticket, &fourth.ticket]).await;
    third.finish.send("alpha verified".to_owned()).unwrap();
    fourth.finish.send("beta verified".to_owned()).unwrap();
    fixture.resume().await;
    let result = fixture.finish().await;
    let output: serde_json::Value = serde_json::from_str(&result.answer).unwrap();
    assert_eq!(output["result"], json!(["alpha verified", "beta verified"]));
}

#[tokio::test]
async fn workflow_tool_timeout_cancels_its_blocking_worker_and_child() {
    let mut fixture = Fixture::start(
        r#"agent("unfinished", #{provider: "controlled"})"#,
        Some(500),
    )
    .await;
    let child = fixture.entry().await;
    child.accept();
    fixture.parked(&[&child.ticket]).await;
    fixture.resume().await;
    tokio::time::timeout(Duration::from_secs(3), child.dropped)
        .await
        .unwrap()
        .unwrap();
    assert!(child.finish.is_closed());
    let result = fixture.finish().await;
    assert!(
        result.answer.contains("execution timeout"),
        "{}",
        result.answer
    );
}
