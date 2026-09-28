use std::time::Duration;

use ternilo_protocol::{RunId, SessionId};
use tokio::sync::{mpsc, oneshot};

use super::*;

struct Request {
    dependencies: Option<Vec<AcceptedSubagentRun>>,
    acknowledge: oneshot::Sender<Result<(), HarnessError>>,
}

struct Admission(mpsc::UnboundedSender<Request>);

impl Admission {
    async fn request(
        &self,
        dependencies: Option<Vec<AcceptedSubagentRun>>,
    ) -> Result<(), HarnessError> {
        let (acknowledge, reply) = oneshot::channel();
        self.0
            .send(Request {
                dependencies,
                acknowledge,
            })
            .unwrap();
        reply
            .await
            .map_err(|_| HarnessError::cancelled("test admission ended"))?
    }
}

impl ExecutionAdmission for Admission {
    fn park<'a>(
        &'a self,
        dependencies: Vec<AcceptedSubagentRun>,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.request(Some(dependencies)))
    }

    fn resume<'a>(
        &'a self,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(self.request(None))
    }
}

fn fixture() -> (ActivityBranch, mpsc::UnboundedReceiver<Request>) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        ActivityBranch::managed(Arc::new(Admission(sender)), RunCancellation::new()),
        receiver,
    )
}

fn ticket(name: &str) -> AcceptedSubagentRun {
    AcceptedSubagentRun {
        session_id: SessionId::new(name),
        run_id: RunId::new(name),
    }
}

async fn request(receiver: &mut mpsc::UnboundedReceiver<Request>) -> Request {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn snapshot(activity: &ActivityBranch, active: usize, waiting: usize) {
    let mut changed = activity.node.as_ref().unwrap().core.changed.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            changed.borrow_and_update();
            let state = activity.snapshot();
            if state.active_branches == active && state.waiting_branches == waiting {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

type ControlledWait = (
    oneshot::Sender<Result<u32, HarnessError>>,
    tokio::task::JoinHandle<Result<u32, HarnessError>>,
);

fn waiting(branch: ActivityBranch, name: &str) -> ControlledWait {
    let dependency = ticket(name);
    let (finish, completion) = oneshot::channel();
    let task = tokio::spawn(async move {
        branch
            .wait_for(dependency, async { completion.await.unwrap() })
            .await
    });
    (finish, task)
}

#[tokio::test]
async fn parallel_leaves_park_only_together_and_resume_before_delivering_results() {
    let (root, mut admission) = fixture();
    let first = root.delegate();
    let second = root.delegate();
    let (finish_first, first_task) = waiting(first.branch(), "first");
    snapshot(&root, 1, 1).await;
    assert!(
        admission.try_recv().is_err(),
        "one real runnable leaf retains admission"
    );
    let (finish_second, second_task) = waiting(second.branch(), "second");
    let parked = request(&mut admission).await;
    assert_eq!(
        parked.dependencies,
        Some(vec![ticket("first"), ticket("second")])
    );
    assert!(!root.snapshot().admitted);
    // Completion can race the park acknowledgement, but may not bypass a fresh resume.
    finish_first.send(Ok(1)).unwrap();
    snapshot(&root, 1, 1).await;
    assert!(!first_task.is_finished());
    parked.acknowledge.send(Ok(())).unwrap();
    let resumed = request(&mut admission).await;
    assert!(resumed.dependencies.is_none());
    assert!(!first_task.is_finished());
    resumed.acknowledge.send(Ok(())).unwrap();
    assert_eq!(first_task.await.unwrap().unwrap(), 1);
    first.finish().await.unwrap();
    // Returning a completed item must not deadlock a buffered parallel scheduler.
    let parked = request(&mut admission).await;
    assert_eq!(parked.dependencies, Some(vec![ticket("second")]));
    parked.acknowledge.send(Ok(())).unwrap();
    finish_second.send(Ok(2)).unwrap();
    let resumed = request(&mut admission).await;
    assert!(resumed.dependencies.is_none());
    resumed.acknowledge.send(Ok(())).unwrap();
    assert_eq!(second_task.await.unwrap().unwrap(), 2);
    second.finish().await.unwrap();
    assert_eq!(
        root.snapshot(),
        ActivitySnapshot {
            active_branches: 1,
            waiting_branches: 0,
            admitted: true
        }
    );
}

#[tokio::test]
async fn dropping_a_wait_requires_acknowledged_admission_before_catch_can_continue() {
    let (root, mut admission) = fixture();
    let scope = root.delegate();
    let (_finish, task) = waiting(scope.branch(), "cancelled-wait");
    request(&mut admission)
        .await
        .acknowledge
        .send(Ok(()))
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let resumed = request(&mut admission).await;
    assert!(resumed.dependencies.is_none());
    let continuation = {
        let branch = scope.branch();
        tokio::spawn(async move { branch.ensure_running().await })
    };
    tokio::task::yield_now().await;
    assert!(!continuation.is_finished());
    resumed.acknowledge.send(Ok(())).unwrap();
    continuation.await.unwrap().unwrap();
    scope.finish().await.unwrap();
}

#[tokio::test]
async fn dropping_a_parallel_branch_updates_parked_dependencies_without_a_false_resume() {
    let (root, mut admission) = fixture();
    let first = root.delegate();
    let second = root.delegate();
    let (_finish_first, first_task) = waiting(first.branch(), "first");
    let (finish_second, second_task) = waiting(second.branch(), "second");
    let parked = request(&mut admission).await;
    first_task.abort();
    drop(first);
    assert!(first_task.await.unwrap_err().is_cancelled());
    snapshot(&root, 0, 1).await;
    parked.acknowledge.send(Ok(())).unwrap();
    let updated = request(&mut admission).await;
    assert_eq!(updated.dependencies, Some(vec![ticket("second")]));
    updated.acknowledge.send(Ok(())).unwrap();
    finish_second.send(Ok(2)).unwrap();
    let resumed = request(&mut admission).await;
    assert!(resumed.dependencies.is_none());
    resumed.acknowledge.send(Ok(())).unwrap();
    assert_eq!(second_task.await.unwrap().unwrap(), 2);
    second.finish().await.unwrap();
}

#[tokio::test]
async fn detached_execution_keeps_its_branch_after_the_awaiting_wrapper_is_dropped() {
    let (root, mut admission) = fixture();
    let scope = root.delegate();
    let thread_branch = scope.branch();
    drop(scope);
    assert_eq!(root.snapshot().active_branches, 2);
    drop(root);
    assert_eq!(thread_branch.snapshot().active_branches, 1);
    assert!(admission.try_recv().is_err());
    drop(thread_branch);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), admission.recv())
            .await
            .unwrap()
            .is_none(),
        "the monitor cannot retain an ownership cycle"
    );
}

#[tokio::test]
async fn rejected_resumption_is_sticky_and_cannot_become_an_ordinary_tool_failure() {
    let (root, mut admission) = fixture();
    let scope = root.delegate();
    let (finish, task) = waiting(scope.branch(), "failed-wait");
    request(&mut admission)
        .await
        .acknowledge
        .send(Ok(()))
        .unwrap();
    finish
        .send(Err(HarnessError::execution("wait timed out")))
        .unwrap();
    let resumed = request(&mut admission).await;
    assert!(resumed.dependencies.is_none());
    resumed
        .acknowledge
        .send(Err(HarnessError::policy("run admission revoked")))
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap_err().message,
        "run admission revoked"
    );
    assert_eq!(
        scope.finish().await.unwrap_err().message,
        "run admission revoked"
    );
    assert_eq!(
        root.ensure_running().await.unwrap_err().message,
        "run admission revoked"
    );
    assert!(!root.snapshot().admitted);
}

#[tokio::test]
async fn empty_or_invalid_activity_does_not_release_foreground_capacity() {
    let (root, mut admission) = fixture();
    let result = root.wait_for(ticket(""), async { Ok(()) }).await;
    assert!(result.is_err());
    root.ensure_running().await.unwrap();
    drop(root);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), admission.recv())
            .await
            .unwrap()
            .is_none()
    );
}

struct OutputRequest {
    phase: ExecutionActivityPhase,
    acknowledge: oneshot::Sender<Result<(), HarnessError>>,
}

struct Output(mpsc::UnboundedSender<OutputRequest>);

impl ExecutionActivityOutput for Output {
    fn changed<'a>(
        &'a self,
        phase: ExecutionActivityPhase,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let (acknowledge, reply) = oneshot::channel();
            self.0.send(OutputRequest { phase, acknowledge }).unwrap();
            reply
                .await
                .map_err(|_| HarnessError::execution("test activity output ended"))?
        })
    }
}

fn output_fixture() -> (
    ActivityBranch,
    mpsc::UnboundedReceiver<Request>,
    mpsc::UnboundedReceiver<OutputRequest>,
) {
    let (admission, requests) = mpsc::unbounded_channel();
    let (output, observations) = mpsc::unbounded_channel();
    (
        ActivityBranch::managed_with_output(
            Arc::new(Admission(admission)),
            RunCancellation::new(),
            Arc::new(Output(output)),
        ),
        requests,
        observations,
    )
}

async fn observed(
    output: &mut mpsc::UnboundedReceiver<OutputRequest>,
    expected: ExecutionActivityPhase,
) -> oneshot::Sender<Result<(), HarnessError>> {
    let observation = tokio::time::timeout(Duration::from_secs(3), output.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observation.phase, expected);
    observation.acknowledge
}

#[tokio::test]
async fn activity_events_surround_admission_and_running_is_persisted_before_continuation() {
    let (root, mut admission, mut output) = output_fixture();
    let (finish, task) = waiting(root.clone(), "accepted-child");
    let park = request(&mut admission).await;
    assert_eq!(park.dependencies, Some(vec![ticket("accepted-child")]));
    assert!(
        output.try_recv().is_err(),
        "waiting is recorded only after the park acknowledgement"
    );
    park.acknowledge.send(Ok(())).unwrap();
    let waiting_event = observed(&mut output, ExecutionActivityPhase::WaitingForSubagents).await;
    finish.send(Ok(7)).unwrap();
    snapshot(&root, 1, 0).await;
    assert!(!task.is_finished());
    assert!(admission.try_recv().is_err());
    waiting_event.send(Ok(())).unwrap();
    let capacity_event = observed(&mut output, ExecutionActivityPhase::WaitingForCapacity).await;
    assert!(
        admission.try_recv().is_err(),
        "capacity wait is durable before requesting resume"
    );
    capacity_event.send(Ok(())).unwrap();
    let resume = request(&mut admission).await;
    assert!(resume.dependencies.is_none());
    assert!(output.try_recv().is_err());
    resume.acknowledge.send(Ok(())).unwrap();
    let running_event = observed(&mut output, ExecutionActivityPhase::Running).await;
    assert!(!root.snapshot().admitted);
    assert!(
        !task.is_finished(),
        "a host resume acknowledgement alone cannot release application work"
    );
    running_event.send(Ok(())).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        7
    );
    root.ensure_running().await.unwrap();
    assert!(root.snapshot().admitted);
}

#[tokio::test]
async fn a_failed_park_wakes_an_unfinished_child_wait_with_the_original_error() {
    let (root, mut admission) = fixture();
    let (_finish, task) = waiting(root.clone(), "not-finishing");
    let error = HarnessError::policy("dependency admission refused");
    request(&mut admission)
        .await
        .acknowledge
        .send(Err(error.clone()))
        .unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(failure.code, error.code);
    assert_eq!(failure.message, error.message);
    assert_eq!(
        root.ensure_running().await.unwrap_err().message,
        error.message
    );
    assert!(!root.snapshot().admitted);
}

#[tokio::test]
async fn failed_waiting_output_wakes_every_waiter_without_waiting_for_child_results() {
    let (root, mut admission, mut output) = output_fixture();
    let first = root.delegate();
    let second = root.delegate();
    let (_first_finish, first_task) = waiting(first.branch(), "first");
    let (_second_finish, second_task) = waiting(second.branch(), "second");
    request(&mut admission)
        .await
        .acknowledge
        .send(Ok(()))
        .unwrap();
    let error = HarnessError::execution("cannot persist execution activity");
    observed(&mut output, ExecutionActivityPhase::WaitingForSubagents)
        .await
        .send(Err(error.clone()))
        .unwrap();
    for task in [first_task, second_task] {
        let failure = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.code, error.code);
        assert_eq!(failure.message, error.message);
    }
    assert!(!root.snapshot().admitted);
    assert_eq!(
        root.ensure_running().await.unwrap_err().message,
        error.message
    );
    assert!(
        admission.try_recv().is_err(),
        "an output failure cannot silently request readmission"
    );
    drop(first);
    drop(second);
}

#[tokio::test]
async fn failed_capacity_or_running_output_never_admits_continuation() {
    for failed_phase in [
        ExecutionActivityPhase::WaitingForCapacity,
        ExecutionActivityPhase::Running,
    ] {
        let (root, mut admission, mut output) = output_fixture();
        let (finish, task) = waiting(root.clone(), "accepted-child");
        request(&mut admission)
            .await
            .acknowledge
            .send(Ok(()))
            .unwrap();
        observed(&mut output, ExecutionActivityPhase::WaitingForSubagents)
            .await
            .send(Ok(()))
            .unwrap();
        finish.send(Ok(9)).unwrap();
        let capacity_event =
            observed(&mut output, ExecutionActivityPhase::WaitingForCapacity).await;
        let failed_output = if failed_phase == ExecutionActivityPhase::Running {
            capacity_event.send(Ok(())).unwrap();
            let resume = request(&mut admission).await;
            assert!(resume.dependencies.is_none());
            resume.acknowledge.send(Ok(())).unwrap();
            observed(&mut output, ExecutionActivityPhase::Running).await
        } else {
            assert!(admission.try_recv().is_err());
            capacity_event
        };
        assert!(!root.snapshot().admitted);
        assert!(!task.is_finished());
        let error = HarnessError::execution("execution state storage failed");
        failed_output.send(Err(error.clone())).unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.code, error.code);
        assert_eq!(failure.message, error.message);
        assert!(!root.snapshot().admitted);
        assert_eq!(
            root.ensure_running().await.unwrap_err().message,
            error.message
        );
    }
}
