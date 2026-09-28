use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

use ternilo_protocol::{AcceptedSubagentRun, ExecutionActivityPhase, HarnessError};
use tokio::sync::{Notify, watch};

use crate::RunCancellation;

/// The host releases only foreground admission; resident resources remain owned.
pub trait ExecutionAdmission: Send + Sync + 'static {
    fn park<'a>(
        &'a self,
        dependencies: Vec<AcceptedSubagentRun>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;

    fn resume<'a>(
        &'a self,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

/// Persist public activity state before admitting subsequent execution.
pub trait ExecutionActivityOutput: Send + Sync + 'static {
    fn changed<'a>(
        &'a self,
        phase: ExecutionActivityPhase,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>>;
}

struct NoopExecutionActivityOutput;

impl ExecutionActivityOutput for NoopExecutionActivityOutput {
    fn changed<'a>(
        &'a self,
        _: ExecutionActivityPhase,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivitySnapshot {
    pub active_branches: usize,
    pub waiting_branches: usize,
    pub admitted: bool,
}

/// Clones refer to one execution branch, not to additional concurrent work.
#[derive(Clone, Default)]
pub struct ActivityBranch {
    node: Option<Arc<BranchNode>>,
}

impl fmt::Debug for ActivityBranch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActivityBranch")
            .field("id", &self.node.as_ref().map(|node| node.id))
            .finish_non_exhaustive()
    }
}

struct BranchNode {
    id: u64,
    core: Arc<ActivityCore>,
}

struct ActivityCore {
    state: Mutex<ActivityState>,
    changed: watch::Sender<u64>,
    ready: Notify,
    cancellation: RunCancellation,
    lifetime: RunCancellation,
}

struct ActivityState {
    next_id: u64,
    branches: BTreeMap<u64, BranchState>,
    admitted: bool,
    failure: Option<HarnessError>,
}

#[derive(Default)]
struct BranchState {
    delegations: usize,
    waiting: Option<AcceptedSubagentRun>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DesiredAdmission {
    Finished,
    Active,
    Parked(Vec<AcceptedSubagentRun>),
}

impl ActivityState {
    fn snapshot(&self) -> ActivitySnapshot {
        let mut snapshot = ActivitySnapshot {
            active_branches: 0,
            waiting_branches: 0,
            admitted: self.admitted,
        };
        for branch in self
            .branches
            .values()
            .filter(|branch| branch.delegations == 0)
        {
            if branch.waiting.is_some() {
                snapshot.waiting_branches += 1;
            } else {
                snapshot.active_branches += 1;
            }
        }
        snapshot
    }

    fn desired(&self) -> DesiredAdmission {
        if self.branches.is_empty() {
            return DesiredAdmission::Finished;
        }
        let snapshot = self.snapshot();
        if snapshot.active_branches != 0 || snapshot.waiting_branches == 0 {
            return DesiredAdmission::Active;
        }
        let mut dependencies = self
            .branches
            .values()
            .filter(|branch| branch.delegations == 0)
            .filter_map(|branch| branch.waiting.clone())
            .collect::<Vec<_>>();
        dependencies.sort_by(|a, b| {
            (a.session_id.as_str(), a.run_id.as_str())
                .cmp(&(b.session_id.as_str(), b.run_id.as_str()))
        });
        dependencies.dedup();
        DesiredAdmission::Parked(dependencies)
    }
}

impl ActivityCore {
    fn mutate<T>(&self, change: impl FnOnce(&mut ActivityState) -> T) -> T {
        let result = {
            let mut state = self.state.lock().expect("activity state lock poisoned");
            let result = change(&mut state);
            // Close the local gate before an asynchronous park can race a returning leaf.
            if matches!(state.desired(), DesiredAdmission::Parked(_)) {
                state.admitted = false;
            }
            result
        };
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.ready.notify_waiters();
        result
    }

    fn fail(&self, error: HarnessError) {
        self.mutate(|state| {
            state.failure = Some(error);
            state.admitted = false;
        });
    }

    async fn failed(&self) -> HarnessError {
        loop {
            let ready = self.ready.notified();
            if let Some(error) = self
                .state
                .lock()
                .expect("activity state lock poisoned")
                .failure
                .clone()
            {
                return error;
            }
            ready.await;
        }
    }
}

impl Drop for ActivityCore {
    fn drop(&mut self) {
        self.lifetime.cancel();
    }
}

impl Drop for BranchNode {
    fn drop(&mut self) {
        self.core.mutate(|state| {
            state.branches.remove(&self.id);
        });
    }
}

impl ActivityBranch {
    #[must_use]
    pub fn untracked() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn managed(admission: Arc<dyn ExecutionAdmission>, cancellation: RunCancellation) -> Self {
        Self::managed_with_output(
            admission,
            cancellation,
            Arc::new(NoopExecutionActivityOutput),
        )
    }

    #[must_use]
    pub fn managed_with_output(
        admission: Arc<dyn ExecutionAdmission>,
        cancellation: RunCancellation,
        output: Arc<dyn ExecutionActivityOutput>,
    ) -> Self {
        let (changed, receiver) = watch::channel(0);
        let core = Arc::new(ActivityCore {
            state: Mutex::new(ActivityState {
                next_id: 1,
                branches: BTreeMap::from([(0, BranchState::default())]),
                admitted: true,
                failure: None,
            }),
            changed,
            ready: Notify::new(),
            cancellation,
            lifetime: RunCancellation::new(),
        });
        tokio::spawn(drive_admission(
            Arc::downgrade(&core),
            receiver,
            admission,
            output,
            core.lifetime.clone(),
            core.cancellation.clone(),
        ));
        Self {
            node: Some(Arc::new(BranchNode { id: 0, core })),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> ActivitySnapshot {
        self.node.as_ref().map_or(
            ActivitySnapshot {
                active_branches: 1,
                waiting_branches: 0,
                admitted: true,
            },
            |node| {
                node.core
                    .state
                    .lock()
                    .expect("activity state lock poisoned")
                    .snapshot()
            },
        )
    }

    pub async fn ensure_running(&self) -> Result<(), HarnessError> {
        let Some(node) = &self.node else {
            return Ok(());
        };
        loop {
            let ready = node.core.ready.notified();
            node.core.cancellation.check()?;
            {
                let state = node
                    .core
                    .state
                    .lock()
                    .expect("activity state lock poisoned");
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.admitted {
                    return Ok(());
                }
            }
            tokio::select! {
                () = ready => {},
                () = node.core.cancellation.cancelled() => return node.core.cancellation.check(),
            }
        }
    }

    /// Enter only when actually transferring execution, not when queueing future work.
    #[must_use]
    pub fn delegate(&self) -> ActivityDelegation {
        let child = self.node.as_ref().map_or_else(Self::untracked, |node| {
            let id = node.core.mutate(|state| {
                let id = state.next_id;
                state.next_id = id.checked_add(1).expect("activity identifier exhausted");
                state
                    .branches
                    .get_mut(&node.id)
                    .expect("live activity branch")
                    .delegations += 1;
                state.branches.insert(id, BranchState::default());
                id
            });
            Self {
                node: Some(Arc::new(BranchNode {
                    id,
                    core: Arc::clone(&node.core),
                })),
            }
        });
        ActivityDelegation {
            parent: self.clone(),
            child: Some(child),
            released: false,
        }
    }

    /// The caller supplies the exact accepted command and keeps its timeout inside this future.
    pub async fn wait_for<T>(
        &self,
        dependency: AcceptedSubagentRun,
        waiting: impl Future<Output = Result<T, HarnessError>>,
    ) -> Result<T, HarnessError> {
        dependency.validate()?;
        self.ensure_running().await?;
        let guard = ActivityWait::new(self.clone(), dependency);
        let result = if let Some(node) = &self.node {
            tokio::select! {
                biased;
                error = node.core.failed() => Err(error),
                () = node.core.cancellation.cancelled() => Err(HarnessError::cancelled("execution activity cancelled")),
                result = waiting => result,
            }
        } else {
            waiting.await
        };
        drop(guard);
        self.ensure_running().await?;
        result
    }
}

pub struct ActivityDelegation {
    parent: ActivityBranch,
    child: Option<ActivityBranch>,
    released: bool,
}

impl ActivityDelegation {
    #[must_use]
    pub fn branch(&self) -> ActivityBranch {
        self.child.as_ref().expect("live delegation").clone()
    }

    pub async fn finish(mut self) -> Result<(), HarnessError> {
        self.child
            .as_ref()
            .expect("live delegation")
            .ensure_running()
            .await?;
        self.release();
        Ok(())
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.child.take();
        if let Some(node) = &self.parent.node {
            node.core.mutate(|state| {
                state
                    .branches
                    .get_mut(&node.id)
                    .expect("live parent branch")
                    .delegations -= 1;
            });
        }
    }
}

#[cfg(test)]
mod tests;

impl Drop for ActivityDelegation {
    fn drop(&mut self) {
        self.release();
    }
}

struct ActivityWait {
    branch: ActivityBranch,
}

impl ActivityWait {
    fn new(branch: ActivityBranch, dependency: AcceptedSubagentRun) -> Self {
        if let Some(node) = &branch.node {
            node.core.mutate(|state| {
                state
                    .branches
                    .get_mut(&node.id)
                    .expect("live waiting branch")
                    .waiting = Some(dependency);
            });
        }
        Self { branch }
    }
}

impl Drop for ActivityWait {
    fn drop(&mut self) {
        if let Some(node) = &self.branch.node {
            node.core.mutate(|state| {
                state
                    .branches
                    .get_mut(&node.id)
                    .expect("live waiting branch")
                    .waiting = None;
            });
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep acknowledged admission transitions and uncancellable event commits in one ownership loop."
)]
async fn drive_admission(
    core: Weak<ActivityCore>,
    mut changed: watch::Receiver<u64>,
    admission: Arc<dyn ExecutionAdmission>,
    output: Arc<dyn ExecutionActivityOutput>,
    lifetime: RunCancellation,
    cancellation: RunCancellation,
) {
    let mut held = true;
    let mut parked_dependencies = Vec::new();
    let mut phase = ExecutionActivityPhase::Running;
    loop {
        changed.borrow_and_update();
        let Some(current) = core.upgrade() else {
            return;
        };
        let desired = current
            .state
            .lock()
            .expect("activity state lock poisoned")
            .desired();
        drop(current);
        let transition = match desired {
            DesiredAdmission::Finished => return,
            DesiredAdmission::Parked(dependencies)
                if held || dependencies != parked_dependencies =>
            {
                let result = tokio::select! {
                    () = lifetime.cancelled() => return,
                    () = cancellation.cancelled() => return,
                    result = admission.park(dependencies.clone(), cancellation.clone()) => result,
                };
                parked_dependencies = dependencies;
                held = false;
                Some(match result {
                    Ok(()) => {
                        if lifetime.is_cancelled() || cancellation.is_cancelled() {
                            return;
                        }
                        publish_phase(
                            output.as_ref(),
                            &mut phase,
                            ExecutionActivityPhase::WaitingForSubagents,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                })
            }
            DesiredAdmission::Active if !held => {
                if lifetime.is_cancelled() || cancellation.is_cancelled() {
                    return;
                }
                let prepared = publish_phase(
                    output.as_ref(),
                    &mut phase,
                    ExecutionActivityPhase::WaitingForCapacity,
                )
                .await;
                if let Err(error) = prepared {
                    if let Some(current) = core.upgrade() {
                        current.fail(error);
                    }
                    return;
                }
                let result = tokio::select! {
                    () = lifetime.cancelled() => return,
                    () = cancellation.cancelled() => return,
                    result = admission.resume(cancellation.clone()) => result,
                };
                held = true;
                Some(match result {
                    Ok(()) => {
                        if lifetime.is_cancelled() || cancellation.is_cancelled() {
                            return;
                        }
                        publish_phase(output.as_ref(), &mut phase, ExecutionActivityPhase::Running)
                            .await
                    }
                    Err(error) => Err(error),
                })
            }
            DesiredAdmission::Active => {
                if let Some(current) = core.upgrade() {
                    let mut state = current.state.lock().expect("activity state lock poisoned");
                    if matches!(state.desired(), DesiredAdmission::Active) {
                        state.admitted = true;
                    }
                    drop(state);
                    current.ready.notify_waiters();
                }
                None
            }
            DesiredAdmission::Parked(_) => None,
        };
        if let Some(result) = transition {
            if let Err(error) = result {
                if let Some(current) = core.upgrade() {
                    current.fail(error);
                }
                return;
            }
            continue;
        }
        tokio::select! {
            () = lifetime.cancelled() => return,
            () = cancellation.cancelled() => return,
            result = changed.changed() => if result.is_err() { return; },
        }
    }
}

async fn publish_phase(
    output: &dyn ExecutionActivityOutput,
    current: &mut ExecutionActivityPhase,
    phase: ExecutionActivityPhase,
) -> Result<(), HarnessError> {
    if *current != phase {
        // Once output starts, complete the commit even if this activity is cancelled or dropped.
        // Sessions owns sequence allocation and must finish publishing an already-written record.
        output.changed(phase).await?;
        *current = phase;
    }
    Ok(())
}
