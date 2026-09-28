use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use ternilo_cloud::WorkerModelFrame;
use ternilo_kernel::RunCancellation;
use ternilo_protocol::HarnessError;
use tokio::sync::{mpsc, watch};

use super::access::AcceptedModelCall;

pub(super) struct RequestMonitor {
    done: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    reason: Arc<Mutex<Option<HarnessError>>>,
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
                    break HarnessError::cancelled("Server is stopping");
                }
                tokio::select! {
                    _ = receiver.changed() => return,
                    () = frames.closed() => break HarnessError::cancelled("client disconnected during the model request"),
                    _ = shutdown.changed() => break HarnessError::cancelled("Server is stopping"),
                    _ = check.tick() => {
                        if let Err(error) = call.check().await { break error; }
                    }
                }
            };
            *recorded_reason.lock().expect("model monitor mutex") = Some(failure);
            cancellation.cancel();
        });
        Self { done, task, reason }
    }

    pub(super) async fn finish(self) -> Option<HarnessError> {
        self.done.send_replace(true);
        let _ = self.task.await;
        self.reason.lock().expect("model monitor mutex").clone()
    }
}
