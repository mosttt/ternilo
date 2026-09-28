use std::{future::Future, pin::Pin, sync::Arc, task::Poll, time::Duration};

use ternilo_kernel::{
    ActivityBranch, ExecutionAdmission, HostPolicy, RunCancellation, SubagentAdmission,
    SubagentBackend, SubagentBackendContext, SubagentBackendRegistration, SubagentDriver,
    SubagentRunStart, SubagentSessionBinding, SubagentsClient,
};
use ternilo_protocol::{
    AcceptedSubagentRun, HarnessError, RunId, RunLimits, SessionEventKind, SessionId,
    SubagentSnapshot, SubagentStatus,
};
use tokio::sync::{Notify, mpsc, oneshot};

use super::LocalApplication;

#[derive(Clone)]
struct PendingBackend {
    entered: Arc<Notify>,
    dropped: Arc<Notify>,
}

struct NotifyOnDrop(Arc<Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl SubagentBackend for PendingBackend {
    fn create(&self, _: SubagentBackendContext) -> Result<Arc<dyn SubagentDriver>, HarnessError> {
        Ok(Arc::new(self.clone()))
    }
}

impl SubagentDriver for PendingBackend {
    fn run<'a>(
        &'a self,
        _: RunId,
        _: String,
        _: RunCancellation,
        _: Option<SubagentSessionBinding>,
        _: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let _dropped = NotifyOnDrop(Arc::clone(&self.dropped));
            self.entered.notify_one();
            // Hold startup before acknowledging it to the spawning caller.
            std::future::pending().await
        })
    }
}

#[derive(Debug)]
struct ControlledStart {
    admission: SubagentRunStart,
    finish: oneshot::Sender<Result<String, HarnessError>>,
}

#[derive(Clone)]
struct ControlledBackend(mpsc::UnboundedSender<ControlledStart>);

impl SubagentBackend for ControlledBackend {
    fn create(&self, _: SubagentBackendContext) -> Result<Arc<dyn SubagentDriver>, HarnessError> {
        Ok(Arc::new(self.clone()))
    }
}

impl SubagentDriver for ControlledBackend {
    fn supports_followup(&self) -> bool {
        true
    }

    fn run<'a>(
        &'a self,
        _: RunId,
        _: String,
        _: RunCancellation,
        _: Option<SubagentSessionBinding>,
        admission: SubagentRunStart,
    ) -> Pin<Box<dyn Future<Output = Result<String, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let (finish, result) = oneshot::channel();
            self.0.send(ControlledStart { admission, finish }).unwrap();
            result.await.unwrap()
        })
    }
}

#[tokio::test]
async fn followup_wait_uses_its_new_admission_and_propagates_rejection() {
    let mut fixture = ManagedFixture::new().await;
    let subagents = &fixture.subagents;
    let entries = &mut fixture.entries;
    let spawning = subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("first-parent"),
        "First child task".to_owned(),
        None,
        true,
        ActivityBranch::untracked(),
    );
    tokio::pin!(spawning);
    let first = tokio::select! {
        result = &mut spawning => panic!("spawn returned before acknowledgement: {result:?}"),
        entry = entries.recv() => entry.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => panic!("first child did not enter"),
    };
    let ticket = SubagentAdmission::Scheduled(AcceptedSubagentRun {
        session_id: SessionId::new("scheduled-child"),
        run_id: RunId::new("first-accepted-run"),
    });
    first.admission.resolve(Ok(ticket.clone()));
    let child = spawning.await.unwrap();
    first.finish.send(Ok("first result".to_owned())).unwrap();
    assert_eq!(
        subagents
            .wait(
                child.subagent_id.clone(),
                3_000,
                ActivityBranch::untracked()
            )
            .await
            .unwrap()
            .status,
        SubagentStatus::Idle
    );

    let following = subagents.followup(
        RunId::new("second-parent"),
        child.subagent_id.clone(),
        "A separately accepted task".to_owned(),
        None,
    );
    tokio::pin!(following);
    let second = tokio::select! {
        result = &mut following => panic!("follow-up reused an earlier acknowledgement: {result:?}"),
        entry = entries.recv() => entry.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => panic!("follow-up did not enter"),
    };
    let waiting = subagents.wait(
        child.subagent_id.clone(),
        3_000,
        ActivityBranch::untracked(),
    );
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        result = &mut waiting => panic!("unacknowledged follow-up returned: {result:?}"),
        () = tokio::task::yield_now() => {},
    }
    second
        .admission
        .resolve(Err(HarnessError::policy("follow-up admission denied")));
    assert_eq!(
        following.await.unwrap_err().message,
        "follow-up admission denied"
    );
    assert_eq!(
        waiting.await.unwrap_err().message,
        "follow-up admission denied"
    );
    assert_eq!(first.admission.wait().await.unwrap(), ticket);
    second
        .finish
        .send(Err(HarnessError::policy("follow-up admission denied")))
        .unwrap();
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_existing_wait_returns_its_original_run_after_a_followup_starts() {
    let mut fixture = ManagedFixture::new().await;
    let subagents = &fixture.subagents;
    let entries = &mut fixture.entries;
    let spawning = subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("first-parent"),
        "First child task".to_owned(),
        None,
        true,
        ActivityBranch::untracked(),
    );
    tokio::pin!(spawning);
    let first = tokio::select! {
        result = &mut spawning => panic!("spawn returned before acknowledgement: {result:?}"),
        entry = entries.recv() => entry.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => panic!("first child did not enter"),
    };
    first
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(AcceptedSubagentRun {
            session_id: SessionId::new("scheduled-child"),
            run_id: RunId::new("first-accepted-run"),
        })));
    let child = spawning.await.unwrap();
    let old_wait = subagents.wait(
        child.subagent_id.clone(),
        10_000,
        ActivityBranch::untracked(),
    );
    tokio::pin!(old_wait);
    std::future::poll_fn(|context| {
        assert!(old_wait.as_mut().poll(context).is_pending());
        Poll::Ready(())
    })
    .await;
    first.finish.send(Ok("first result".to_owned())).unwrap();
    // Leave the old waiter unpolled while the child completes and accepts a follow-up.
    tokio::time::timeout(Duration::from_secs(3), async {
        while subagents
            .get(child.subagent_id.clone())
            .await
            .unwrap()
            .status
            != SubagentStatus::Idle
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let following = subagents.followup(
        RunId::new("second-parent"),
        child.subagent_id.clone(),
        "Second child task".to_owned(),
        None,
    );
    tokio::pin!(following);
    let second = tokio::select! {
        result = &mut following => panic!("follow-up returned before acknowledgement: {result:?}"),
        entry = entries.recv() => entry.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => panic!("follow-up did not enter"),
    };
    second
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(AcceptedSubagentRun {
            session_id: SessionId::new("scheduled-child"),
            run_id: RunId::new("second-accepted-run"),
        })));
    following.await.unwrap();
    let first_result = tokio::time::timeout(Duration::from_secs(3), &mut old_wait)
        .await
        .expect("the old waiter must not follow a newer child run")
        .unwrap();
    assert_eq!(first_result.output.as_deref(), Some("first result"));
    assert_eq!(
        subagents
            .get(child.subagent_id.clone())
            .await
            .unwrap()
            .status,
        SubagentStatus::Running
    );
    second.finish.send(Ok("second result".to_owned())).unwrap();
    assert_eq!(
        subagents
            .wait(child.subagent_id, 3_000, ActivityBranch::untracked())
            .await
            .unwrap()
            .output
            .as_deref(),
        Some("second result")
    );
    fixture.application.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_an_unacknowledged_spawn_retires_its_child_and_worker() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    tokio::fs::create_dir(&workspace).await.unwrap();
    let application = LocalApplication::open(
        crate::catalog().unwrap(),
        crate::local_profile(),
        HostPolicy::local(RunLimits::default()),
        temporary.path().join("data"),
    )
    .await
    .unwrap();
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id.as_str();
    let managed = application.addressable_session(session_id).await.unwrap();
    let subagents = managed.harness.subagents().unwrap();
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    subagents
        .register_backend(SubagentBackendRegistration {
            name: "pending-start".to_owned(),
            backend: Arc::new(PendingBackend {
                entered: Arc::clone(&entered),
                dropped: Arc::clone(&dropped),
            }),
        })
        .await
        .unwrap();
    let (child_id, waiting) = {
        let spawning = subagents.spawn_on(
            "pending-start".to_owned(),
            RunId::new("pending-parent"),
            "Wait for startup".to_owned(),
            None,
            true,
            ActivityBranch::untracked(),
        );
        tokio::pin!(spawning);
        tokio::select! {
            result = &mut spawning => panic!("spawn returned before acknowledgement: {result:?}"),
            () = entered.notified() => {},
            () = tokio::time::sleep(Duration::from_secs(3)) => panic!("child driver did not start"),
        }
        let children = subagents.list().await;
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, SubagentStatus::Running);
        let child_id = children[0].subagent_id.clone();
        let mut waiting =
            Box::pin(subagents.wait(child_id.clone(), 3_000, ActivityBranch::untracked()));
        std::future::poll_fn(|context| {
            assert!(waiting.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        (child_id, waiting)
    };
    assert_eq!(waiting.await.unwrap().status, SubagentStatus::Cancelled);
    tokio::time::timeout(Duration::from_secs(3), dropped.notified())
        .await
        .expect("cancelling spawn must stop its worker");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let events = application.events(session_id).await.unwrap();
            if events.iter().any(|event| matches!(
                &event.kind,
                SessionEventKind::SubagentUpdated { subagent }
                    if subagent.subagent_id == child_id && subagent.status == SubagentStatus::Cancelled
            )) {
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.expect("cancelled child must have a durable terminal status");
    assert!(subagents.list().await.is_empty());
    application.shutdown().await.unwrap();
}

struct ManagedFixture {
    application: LocalApplication,
    subagents: SubagentsClient,
    entries: mpsc::UnboundedReceiver<ControlledStart>,
    _directory: tempfile::TempDir,
}

impl ManagedFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        tokio::fs::create_dir(&workspace).await.unwrap();
        let application = LocalApplication::open(
            crate::catalog().unwrap(),
            crate::local_profile(),
            HostPolicy::local(RunLimits::default()),
            directory.path().join("data"),
        )
        .await
        .unwrap();
        let workspace = application
            .add_workspace(workspace.to_str().unwrap())
            .await
            .unwrap();
        let session = application
            .create_session(workspace.workspace_id, None, None)
            .await
            .unwrap();
        let managed = application
            .addressable_session(session.identity.session_id.as_str())
            .await
            .unwrap();
        let subagents = managed.harness.subagents().unwrap();
        let (entered, entries) = mpsc::unbounded_channel();
        subagents
            .register_backend(SubagentBackendRegistration {
                name: "controlled-start".to_owned(),
                backend: Arc::new(ControlledBackend(entered)),
            })
            .await
            .unwrap();
        Self {
            application,
            subagents,
            entries,
            _directory: directory,
        }
    }
}

#[derive(Debug)]
enum AdmissionTransition {
    Park(Vec<AcceptedSubagentRun>),
    Resume(oneshot::Sender<()>),
}

struct ControlledAdmission(mpsc::UnboundedSender<AdmissionTransition>);

impl ExecutionAdmission for ControlledAdmission {
    fn park<'a>(
        &'a self,
        dependencies: Vec<AcceptedSubagentRun>,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.0
                .send(AdmissionTransition::Park(dependencies))
                .unwrap();
            Ok(())
        })
    }

    fn resume<'a>(
        &'a self,
        _: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let (accepted, waiting) = oneshot::channel();
            self.0.send(AdmissionTransition::Resume(accepted)).unwrap();
            waiting
                .await
                .map_err(|_| HarnessError::execution("test resume acknowledgement dropped"))
        })
    }
}

fn managed_activity() -> (ActivityBranch, mpsc::UnboundedReceiver<AdmissionTransition>) {
    let (transitions, events) = mpsc::unbounded_channel();
    (
        ActivityBranch::managed(
            Arc::new(ControlledAdmission(transitions)),
            RunCancellation::new(),
        ),
        events,
    )
}

fn accepted_run(run: &str) -> AcceptedSubagentRun {
    AcceptedSubagentRun {
        session_id: SessionId::new("scheduled-child"),
        run_id: RunId::new(run),
    }
}

async fn bounded<T>(operation: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), operation)
        .await
        .expect("controlled operation did not settle")
}

async fn pending<T>(mut operation: Pin<&mut impl Future<Output = T>>) {
    std::future::poll_fn(|context| {
        assert!(
            operation.as_mut().poll(context).is_pending(),
            "the operation returned before its controlled acknowledgement"
        );
        Poll::Ready(())
    })
    .await;
}

async fn started(
    entries: &mut mpsc::UnboundedReceiver<ControlledStart>,
    operation: Pin<&mut impl Future<Output = Result<SubagentSnapshot, HarnessError>>>,
) -> ControlledStart {
    bounded(async {
        tokio::select! {
            result = operation => panic!("operation returned before child acceptance: {result:?}"),
            entry = entries.recv() => entry.unwrap(),
        }
    })
    .await
}

async fn parked(
    events: &mut mpsc::UnboundedReceiver<AdmissionTransition>,
    ticket: &AcceptedSubagentRun,
) {
    match bounded(events.recv()).await.unwrap() {
        AdmissionTransition::Park(dependencies) => assert_eq!(dependencies, vec![ticket.clone()]),
        event @ AdmissionTransition::Resume(_) => {
            panic!("expected parking on the accepted child, received {event:?}");
        }
    }
}

async fn resume_ack(
    events: &mut mpsc::UnboundedReceiver<AdmissionTransition>,
) -> oneshot::Sender<()> {
    match bounded(events.recv()).await.unwrap() {
        AdmissionTransition::Resume(accepted) => accepted,
        event @ AdmissionTransition::Park(_) => {
            panic!("expected foreground readmission, received {event:?}");
        }
    }
}

#[tokio::test]
async fn managed_wait_does_not_park_before_acceptance_or_after_rejection() {
    let mut fixture = ManagedFixture::new().await;
    let (activity, mut events) = managed_activity();
    let spawning = fixture.subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("parent"),
        "Await real admission".to_owned(),
        None,
        false,
        activity.clone(),
    );
    tokio::pin!(spawning);
    let entry = started(&mut fixture.entries, spawning.as_mut()).await;
    assert_eq!(activity.snapshot().waiting_branches, 0);
    assert_eq!(activity.snapshot().active_branches, 1);
    assert!(events.try_recv().is_err());

    let child = fixture.subagents.list().await.pop().unwrap();
    let (wait_activity, mut wait_events) = managed_activity();
    let waiting = fixture
        .subagents
        .wait(child.subagent_id, 3_000, wait_activity.clone());
    tokio::pin!(waiting);
    pending(waiting.as_mut()).await;
    assert_eq!(wait_activity.snapshot().waiting_branches, 0);
    assert!(wait_events.try_recv().is_err());
    entry
        .admission
        .resolve(Err(HarnessError::policy("admission refused")));
    assert_eq!(
        bounded(waiting).await.unwrap_err().message,
        "admission refused"
    );
    assert_eq!(
        bounded(spawning).await.unwrap_err().message,
        "admission refused"
    );
    assert!(events.try_recv().is_err());
    assert!(wait_events.try_recv().is_err());
    bounded(fixture.application.shutdown()).await.unwrap();
}

#[tokio::test]
async fn managed_foreground_spawn_waits_for_readmission_on_success_and_failure() {
    for fails in [false, true] {
        let mut fixture = ManagedFixture::new().await;
        let (activity, mut events) = managed_activity();
        let spawning = fixture.subagents.spawn_on(
            "controlled-start".to_owned(),
            RunId::new("parent"),
            "Wait in foreground".to_owned(),
            None,
            false,
            activity.clone(),
        );
        tokio::pin!(spawning);
        let entry = started(&mut fixture.entries, spawning.as_mut()).await;
        let ticket = accepted_run("foreground-run");
        entry
            .admission
            .resolve(Ok(SubagentAdmission::Scheduled(ticket.clone())));
        pending(spawning.as_mut()).await;
        parked(&mut events, &ticket).await;
        assert!(!activity.snapshot().admitted);
        entry
            .finish
            .send(if fails {
                Err(HarnessError::execution("child failed"))
            } else {
                Ok("child succeeded".to_owned())
            })
            .unwrap();
        let acknowledgement = bounded(async {
            tokio::select! {
                result = &mut spawning => panic!("foreground result escaped readmission: {result:?}"),
                acknowledgement = resume_ack(&mut events) => acknowledgement,
            }
        }).await;
        pending(spawning.as_mut()).await;
        acknowledgement.send(()).unwrap();
        let result = bounded(spawning).await.unwrap();
        assert_eq!(
            result.status,
            if fails {
                SubagentStatus::Failed
            } else {
                SubagentStatus::Idle
            }
        );
        assert!(activity.snapshot().admitted);
        bounded(fixture.application.shutdown()).await.unwrap();
    }
}

#[tokio::test]
async fn managed_background_spawn_stays_active_until_an_explicit_wait() {
    let mut fixture = ManagedFixture::new().await;
    let (activity, mut events) = managed_activity();
    let spawning = fixture.subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("parent"),
        "Work in background".to_owned(),
        None,
        true,
        activity.clone(),
    );
    tokio::pin!(spawning);
    let entry = started(&mut fixture.entries, spawning.as_mut()).await;
    let ticket = accepted_run("background-run");
    entry
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(ticket.clone())));
    let child = bounded(spawning).await.unwrap();
    assert_eq!(activity.snapshot().active_branches, 1);
    assert_eq!(activity.snapshot().waiting_branches, 0);
    assert!(events.try_recv().is_err());
    let waiting = fixture
        .subagents
        .wait(child.subagent_id, 3_000, activity.clone());
    tokio::pin!(waiting);
    pending(waiting.as_mut()).await;
    parked(&mut events, &ticket).await;
    entry
        .finish
        .send(Ok("background result".to_owned()))
        .unwrap();
    let acknowledgement = bounded(async {
        tokio::select! {
            result = &mut waiting => panic!("wait result escaped readmission: {result:?}"),
            acknowledgement = resume_ack(&mut events) => acknowledgement,
        }
    })
    .await;
    pending(waiting.as_mut()).await;
    acknowledgement.send(()).unwrap();
    assert_eq!(
        bounded(waiting).await.unwrap().output.as_deref(),
        Some("background result")
    );
    bounded(fixture.application.shutdown()).await.unwrap();
}

#[tokio::test]
async fn managed_wait_timeout_is_reported_only_after_readmission() {
    let mut fixture = ManagedFixture::new().await;
    let (activity, mut events) = managed_activity();
    let spawning = fixture.subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("parent"),
        "Keep running past a wait timeout".to_owned(),
        None,
        true,
        activity.clone(),
    );
    tokio::pin!(spawning);
    let entry = started(&mut fixture.entries, spawning.as_mut()).await;
    let ticket = accepted_run("long-running-child");
    entry
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(ticket.clone())));
    let child = bounded(spawning).await.unwrap();
    let waiting = fixture
        .subagents
        .wait(child.subagent_id.clone(), 100, activity.clone());
    tokio::pin!(waiting);
    pending(waiting.as_mut()).await;
    parked(&mut events, &ticket).await;
    let acknowledgement = bounded(async {
        tokio::select! {
            result = &mut waiting => panic!("timeout returned before foreground readmission: {result:?}"),
            acknowledgement = resume_ack(&mut events) => acknowledgement,
        }
    }).await;
    pending(waiting.as_mut()).await;
    assert!(!activity.snapshot().admitted);
    acknowledgement.send(()).unwrap();
    assert!(
        bounded(waiting)
            .await
            .unwrap_err()
            .message
            .contains("timed out waiting for subagent")
    );
    assert_eq!(
        fixture
            .subagents
            .get(child.subagent_id.clone())
            .await
            .unwrap()
            .status,
        SubagentStatus::Running
    );
    entry.finish.send(Ok("still usable".to_owned())).unwrap();
    assert_eq!(
        bounded(
            fixture
                .subagents
                .wait(child.subagent_id, 3_000, ActivityBranch::untracked())
        )
        .await
        .unwrap()
        .output
        .as_deref(),
        Some("still usable")
    );
    bounded(fixture.application.shutdown()).await.unwrap();
}

#[tokio::test]
async fn managed_direct_child_wait_never_parks() {
    let mut fixture = ManagedFixture::new().await;
    let (activity, mut events) = managed_activity();
    let spawning = fixture.subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("parent"),
        "Run a direct child".to_owned(),
        None,
        false,
        activity.clone(),
    );
    tokio::pin!(spawning);
    let entry = started(&mut fixture.entries, spawning.as_mut()).await;
    entry.admission.resolve(Ok(SubagentAdmission::Direct));
    pending(spawning.as_mut()).await;
    assert_eq!(activity.snapshot().active_branches, 1);
    assert_eq!(activity.snapshot().waiting_branches, 0);
    entry.finish.send(Ok("direct result".to_owned())).unwrap();
    assert_eq!(
        bounded(spawning).await.unwrap().output.as_deref(),
        Some("direct result")
    );
    assert!(events.try_recv().is_err());
    bounded(fixture.application.shutdown()).await.unwrap();
}

#[tokio::test]
async fn managed_old_wait_keeps_its_accepted_run_while_followup_is_running() {
    let mut fixture = ManagedFixture::new().await;
    let (activity, mut events) = managed_activity();
    let spawning = fixture.subagents.spawn_on(
        "controlled-start".to_owned(),
        RunId::new("parent"),
        "First run".to_owned(),
        None,
        true,
        activity.clone(),
    );
    tokio::pin!(spawning);
    let first = started(&mut fixture.entries, spawning.as_mut()).await;
    let first_ticket = accepted_run("first-run");
    first
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(first_ticket.clone())));
    let child = bounded(spawning).await.unwrap();
    let old_wait = fixture
        .subagents
        .wait(child.subagent_id.clone(), 3_000, activity.clone());
    tokio::pin!(old_wait);
    pending(old_wait.as_mut()).await;
    parked(&mut events, &first_ticket).await;
    first.finish.send(Ok("first result".to_owned())).unwrap();
    bounded(async {
        while fixture
            .subagents
            .get(child.subagent_id.clone())
            .await
            .unwrap()
            .status
            != SubagentStatus::Idle
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    // A separate caller starts the next command while the original waiter is still parked.
    let following = fixture.subagents.followup(
        RunId::new("next-parent"),
        child.subagent_id.clone(),
        "Second run".to_owned(),
        None,
    );
    tokio::pin!(following);
    let second = started(&mut fixture.entries, following.as_mut()).await;
    second
        .admission
        .resolve(Ok(SubagentAdmission::Scheduled(accepted_run("second-run"))));
    bounded(following).await.unwrap();
    let acknowledgement = bounded(async {
        tokio::select! {
            result = &mut old_wait => panic!("old result escaped readmission: {result:?}"),
            acknowledgement = resume_ack(&mut events) => acknowledgement,
        }
    })
    .await;
    pending(old_wait.as_mut()).await;
    acknowledgement.send(()).unwrap();
    assert_eq!(
        bounded(old_wait).await.unwrap().output.as_deref(),
        Some("first result")
    );
    assert_eq!(
        fixture
            .subagents
            .get(child.subagent_id.clone())
            .await
            .unwrap()
            .status,
        SubagentStatus::Running
    );
    assert!(
        events.try_recv().is_err(),
        "the old waiter must not park on the new command's ticket"
    );
    second.finish.send(Ok("second result".to_owned())).unwrap();
    bounded(
        fixture
            .subagents
            .wait(child.subagent_id, 3_000, ActivityBranch::untracked()),
    )
    .await
    .unwrap();
    bounded(fixture.application.shutdown()).await.unwrap();
}
