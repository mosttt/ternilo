use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use ternilo_cloud::WorkerModelFrame;
use ternilo_kernel::RunCancellation;
use ternilo_protocol::HarnessError;
use tokio::sync::{mpsc, watch};

use super::access::AcceptedModelCall;

#[derive(Clone)]
pub(super) struct StopReason {
    pub error: HarnessError,
    pub code: &'static str,
}

pub(super) type SharedStopReason = Arc<Mutex<Option<StopReason>>>;

pub(super) struct RequestMonitor {
    done: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    reason: SharedStopReason,
}

impl RequestMonitor {
    pub(super) fn start(
        call: Arc<AcceptedModelCall>,
        frames: mpsc::Sender<WorkerModelFrame>,
        cancellation: RunCancellation,
    ) -> Self {
        let (done, mut receiver) = watch::channel(false);
        let reason = Arc::new(Mutex::new(None));
        let recorded_reason = Arc::clone(&reason);
        let task = tokio::spawn(async move {
            let mut shutdown = call.state.shutdown.clone();
            let period = Duration::from_secs(2);
            let mut check = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let failure = loop {
                if *shutdown.borrow() {
                    break StopReason {
                        error: HarnessError::cancelled("Server is stopping"),
                        code: "server_stopping",
                    };
                }
                tokio::select! {
                    _ = receiver.changed() => return,
                    () = frames.closed() => break StopReason { error: HarnessError::cancelled("client disconnected during the model request"), code: "client_disconnected" },
                    _ = shutdown.changed() => break StopReason { error: HarnessError::cancelled("Server is stopping"), code: "server_stopping" },
                    _ = check.tick() => {
                        if let Err(error) = call.check().await { break StopReason { error, code: "request_authority_changed" }; }
                    }
                }
            };
            recorded_reason
                .lock()
                .expect("model monitor mutex")
                .get_or_insert(failure);
            cancellation.cancel();
        });
        Self { done, task, reason }
    }

    pub(super) fn reason(&self) -> SharedStopReason {
        Arc::clone(&self.reason)
    }

    pub(super) async fn finish(self) -> Option<StopReason> {
        self.done.send_replace(true);
        let _ = self.task.await;
        self.reason.lock().expect("model monitor mutex").clone()
    }
}
