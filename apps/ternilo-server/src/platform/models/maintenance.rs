use std::time::Duration;

use ternilo_control::ControlStore;
use tokio::{sync::watch, task::JoinHandle};

use crate::platform::http::now_ms;

pub(crate) fn start(store: ControlStore, mut shutdown: watch::Receiver<bool>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = tick.tick() => {
                    let result = match now_ms() {
                        Ok(now) => store.expire_model_requests(now, 100).await,
                        Err(error) => Err(error),
                    };
                    if let Err(error) = result {
                        eprintln!("model request recovery failed: {error}");
                    }
                }
            }
        }
    })
}
