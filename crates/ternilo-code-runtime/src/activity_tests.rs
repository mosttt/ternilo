use std::future::pending;

use ternilo_kernel::ExecutionAdmission;
use ternilo_protocol::{AcceptedSubagentRun, SessionId};
use tokio::sync::{mpsc, oneshot};

use super::*;

enum AdmissionEvent {
    Park(Vec<AcceptedSubagentRun>),
    Resume(oneshot::Sender<Result<(), HarnessError>>),
}

struct ControlledAdmission(mpsc::UnboundedSender<AdmissionEvent>);

impl ExecutionAdmission for ControlledAdmission {
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
            let (reply, response) = oneshot::channel();
            self.0.send(AdmissionEvent::Resume(reply)).unwrap();
            response.await.unwrap()
        })
    }
}

struct ControlledBinding {
    entered: mpsc::UnboundedSender<oneshot::Sender<()>>,
    continued: mpsc::UnboundedSender<()>,
}

impl CodeBindingHandler for ControlledBinding {
    fn call<'a>(
        &'a self,
        name: String,
        _: Value,
        activity: ActivityBranch,
    ) -> Pin<Box<dyn Future<Output = Result<Value, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if name == "after" {
                self.continued.send(()).unwrap();
                return Ok(json!("continued"));
            }
            assert_eq!(name, "wait");
            let (timeout, expired) = oneshot::channel();
            self.entered.send(timeout).unwrap();
            // Model an outer timeout that drops the dependency wait itself.
            tokio::select! {
                _ = expired => Err(HarnessError::execution("fixture timeout")),
                result = activity.wait_for(ticket(), pending()) => result,
            }
        })
    }
}

fn ticket() -> AcceptedSubagentRun {
    AcceptedSubagentRun {
        session_id: SessionId::new("code-child"),
        run_id: RunId::new("code-accepted-run"),
    }
}

struct Fixture {
    activity: ActivityBranch,
    cancellation: RunCancellation,
    events: mpsc::UnboundedReceiver<AdmissionEvent>,
    entered: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    continued: mpsc::UnboundedReceiver<()>,
    worker: tokio::task::JoinHandle<Result<CodeRunResult, HarnessError>>,
}

impl Fixture {
    fn start() -> Self {
        let (events_tx, events) = mpsc::unbounded_channel();
        let activity = ActivityBranch::managed(
            Arc::new(ControlledAdmission(events_tx)),
            RunCancellation::new(),
        );
        let cancellation = RunCancellation::new();
        let (entered_tx, entered) = mpsc::unbounded_channel();
        let (continued_tx, continued) = mpsc::unbounded_channel();
        let request = CodeRunRequest {
            program: r#"
                try { call_tool("wait", #{}); }
                catch (err) { tools::after(#{}); }
                "finished"
            "#
            .to_owned(),
            bindings: ["wait", "after"]
                .into_iter()
                .map(|name| CodeBindingSpec {
                    name: name.to_owned(),
                    description: name.to_owned(),
                    input_schema: json!({"type": "object"}),
                })
                .collect(),
            binding: Arc::new(ControlledBinding {
                entered: entered_tx,
                continued: continued_tx,
            }),
            cancellation: cancellation.clone(),
            activity: activity.clone(),
        };
        let worker = tokio::spawn(async move {
            let mut runtime = tests::runtime(100_000);
            runtime.config.max_wall_ms = 30_000;
            runtime.run_request(request).await
        });
        Self {
            activity,
            cancellation,
            events,
            entered,
            continued,
            worker,
        }
    }

    async fn parked(&mut self) -> oneshot::Sender<()> {
        let entered = tokio::time::timeout(Duration::from_secs(3), self.entered.recv())
            .await
            .unwrap()
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap();
        let AdmissionEvent::Park(dependencies) = event else {
            panic!("expected dependency park")
        };
        assert_eq!(dependencies, vec![ticket()]);
        assert_eq!(self.activity.snapshot().active_branches, 0);
        assert_eq!(self.activity.snapshot().waiting_branches, 1);
        entered
    }

    async fn resuming(&mut self) -> oneshot::Sender<Result<(), HarnessError>> {
        let event = tokio::time::timeout(Duration::from_secs(3), self.events.recv())
            .await
            .unwrap()
            .unwrap();
        let AdmissionEvent::Resume(reply) = event else {
            panic!("expected foreground resume")
        };
        assert!(!self.activity.snapshot().admitted);
        assert!(self.continued.try_recv().is_err());
        reply
    }
}

#[tokio::test]
async fn dropped_dependency_wait_resumes_before_rhai_catch_runs() {
    let mut fixture = Fixture::start();
    fixture.parked().await.send(()).unwrap();
    let resume = fixture.resuming().await;
    assert!(!fixture.worker.is_finished());
    resume.send(Ok(())).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), fixture.worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.failure, None);
    assert_eq!(result.value, Some(json!("finished")));
    assert_eq!(fixture.continued.recv().await, Some(()));
}

#[tokio::test]
async fn denied_readmission_terminates_rhai_instead_of_entering_catch() {
    let mut fixture = Fixture::start();
    fixture.parked().await.send(()).unwrap();
    fixture
        .resuming()
        .await
        .send(Err(HarnessError::policy("resume denied")))
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(3), fixture.worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("resume denied"));
    assert!(fixture.continued.try_recv().is_err());
}

#[tokio::test]
async fn dropping_runtime_future_keeps_the_blocking_worker_activity_until_exit() {
    let mut fixture = Fixture::start();
    let timeout = fixture.parked().await;
    fixture.worker.abort();
    assert!(fixture.worker.await.unwrap_err().is_cancelled());
    assert_eq!(fixture.activity.snapshot().active_branches, 0);
    assert_eq!(fixture.activity.snapshot().waiting_branches, 1);
    fixture.cancellation.cancel();
    // The test owns the admission channels after the outer runtime call is gone.
    let event = tokio::time::timeout(Duration::from_secs(3), fixture.events.recv())
        .await
        .unwrap()
        .unwrap();
    let AdmissionEvent::Resume(reply) = event else {
        panic!("expected resume during worker cleanup")
    };
    assert!(fixture.continued.try_recv().is_err());
    reply.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let state = fixture.activity.snapshot();
            if state.active_branches == 1 && state.waiting_branches == 0 && state.admitted {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("blocking worker releases its real activity after cancellation");
    assert!(fixture.continued.try_recv().is_err());
    drop(timeout);
}
