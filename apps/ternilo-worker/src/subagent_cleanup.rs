use std::{
    collections::BTreeMap,
    mem,
    sync::{Arc, Mutex},
};

use ternilo_kernel::{RunCancellation, SubagentRunStart};
use ternilo_protocol::{HarnessError, RunId, SessionId};
use tokio::{sync::Notify, task::JoinSet};

use crate::PipeHostClient;

#[derive(Default)]
pub(crate) struct SubagentCleanup {
    state: Mutex<CleanupState>,
    changed: Notify,
}

#[derive(Default)]
struct CleanupState {
    next_guard: u64,
    active: BTreeMap<u64, TrackedRun>,
    pending_retention: Vec<TrackedRun>,
    parent_succeeded: Option<bool>,
    tasks: JoinSet<Result<(), HarnessError>>,
}

struct TrackedRun {
    client: PipeHostClient,
    session_id: SessionId,
    run_id: RunId,
    start: SubagentRunStart,
    cancellation: RunCancellation,
}

impl TrackedRun {
    fn can_preserve(&self) -> bool {
        self.start.is_preserved() && !self.cancellation.is_cancelled()
    }

    fn cancel(self, tasks: &mut JoinSet<Result<(), HarnessError>>) {
        tasks.spawn(async move {
            self.client
                .cancel_subagent_run(self.session_id, self.run_id)
                .await
        });
    }
}

impl SubagentCleanup {
    pub(crate) fn track(
        self: &Arc<Self>,
        client: PipeHostClient,
        session_id: SessionId,
        run_id: RunId,
        start: SubagentRunStart,
        cancellation: RunCancellation,
    ) -> SubagentRunGuard {
        let mut state = self.state.lock().expect("Subagent cleanup lock poisoned");
        let id = state.next_guard;
        state.next_guard += 1;
        state.active.insert(
            id,
            TrackedRun {
                client,
                session_id,
                run_id,
                start,
                cancellation,
            },
        );
        SubagentRunGuard {
            owner: Arc::clone(self),
            id: Some(id),
        }
    }

    pub(crate) fn preserve_delivered(&self) {
        let state = self.state.lock().expect("Subagent cleanup lock poisoned");
        for run in state.active.values() {
            if !run.cancellation.is_cancelled() {
                let _ = run.start.preserve_delivered();
            }
        }
    }

    pub(crate) fn finish_parent(&self, succeeded: bool) {
        let mut state = self.state.lock().expect("Subagent cleanup lock poisoned");
        state.parent_succeeded = Some(succeeded);
        for run in mem::take(&mut state.pending_retention) {
            if !succeeded || !run.can_preserve() {
                run.cancel(&mut state.tasks);
            }
        }
        self.changed.notify_one();
    }

    pub(crate) async fn drain(&self) -> Result<(), HarnessError> {
        let mut failure = None;
        loop {
            let notified = self.changed.notified();
            let (active, pending_retention, mut tasks) = {
                let mut state = self.state.lock().expect("Subagent cleanup lock poisoned");
                (
                    state.active.len(),
                    state.pending_retention.len(),
                    mem::take(&mut state.tasks),
                )
            };
            if active == 0 && pending_retention == 0 && tasks.is_empty() {
                return failure.map_or(Ok(()), Err);
            }
            if tasks.is_empty() {
                notified.await;
                continue;
            }
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result.unwrap_or_else(|error| {
                    Err(HarnessError::execution(format!(
                        "cloud Subagent cleanup task failed: {error}"
                    )))
                }) {
                    failure.get_or_insert(error);
                }
            }
        }
    }
}

pub(crate) struct SubagentRunGuard {
    owner: Arc<SubagentCleanup>,
    id: Option<u64>,
}

impl SubagentRunGuard {
    pub(crate) fn disarm(&mut self) {
        if let Some(id) = self.id.take() {
            self.owner
                .state
                .lock()
                .expect("Subagent cleanup lock poisoned")
                .active
                .remove(&id);
            self.owner.changed.notify_one();
        }
    }
}

impl Drop for SubagentRunGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        let mut state = self
            .owner
            .state
            .lock()
            .expect("Subagent cleanup lock poisoned");
        let run = state
            .active
            .remove(&id)
            .expect("Subagent run guard is registered");
        if run.can_preserve() {
            match state.parent_succeeded {
                Some(true) => {}
                None => state.pending_retention.push(run),
                Some(false) => run.cancel(&mut state.tasks),
            }
        } else {
            run.cancel(&mut state.tasks);
        }
        drop(state);
        self.owner.changed.notify_one();
    }
}
