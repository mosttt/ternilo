use std::{
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use ternilo_kernel::RunCancellation;
use ternilo_protocol::HarnessError;
use wasmtime::Engine;

pub(super) struct WasmCancellation {
    completion: Option<Sender<()>>,
    watcher: Option<JoinHandle<()>>,
}

impl WasmCancellation {
    pub(super) fn watch(
        engine: Engine,
        cancellation: RunCancellation,
    ) -> Result<Self, HarnessError> {
        let (completion, receiver) = mpsc::channel();
        let watcher = thread::Builder::new()
            .name("ternilo-wasm-cancel".to_owned())
            .spawn(move || {
                while matches!(
                    receiver.recv_timeout(Duration::from_millis(10)),
                    Err(RecvTimeoutError::Timeout)
                ) {
                    if cancellation.is_cancelled() {
                        engine.increment_epoch();
                        break;
                    }
                }
            })
            .map_err(|error| {
                HarnessError::execution(format!("start WASM cancellation watcher: {error}"))
            })?;
        Ok(Self {
            completion: Some(completion),
            watcher: Some(watcher),
        })
    }
}

impl Drop for WasmCancellation {
    fn drop(&mut self) {
        self.completion.take();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}
