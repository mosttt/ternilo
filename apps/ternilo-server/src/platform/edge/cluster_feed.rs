use super::{EdgeGateway, EdgeLiveNotification, HarnessError, RouteKey, now_ms};
use std::{collections::BTreeMap, time::Duration};

// Coalesce each executor's changes rather than retaining one row per token.
pub(super) async fn start(edge: &EdgeGateway) -> Result<tokio::task::JoinHandle<()>, HarnessError> {
    let journal = edge.journal.clone();
    let instance = edge.instance_id.clone();
    let sender = edge.live_notify.clone();
    let events = edge.event_notify.clone();
    let mut previous = BTreeMap::<RouteKey, (i64, bool)>::new();
    let now = now_ms()?;
    for live in journal.live_routes().await? {
        previous.insert(
            live.route,
            (
                live.revision,
                live.expires_at_ms > i64::try_from(now).unwrap_or(i64::MAX),
            ),
        );
    }
    Ok(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut failed = false;
        loop {
            interval.tick().await;
            let (Ok(routes), Ok(now)) = (journal.live_routes().await, now_ms()) else {
                failed = true;
                continue;
            };
            for live in routes {
                let online = live.expires_at_ms > i64::try_from(now).unwrap_or(i64::MAX);
                let old = previous.insert(live.route.clone(), (live.revision, online));
                if failed
                    || (old != Some((live.revision, online))
                        && (live.owner_id != instance
                            || old.is_none_or(|(_, was_online)| was_online != online)))
                {
                    events.notify_waiters();
                    let _ = sender.send(EdgeLiveNotification::Rescan {
                        tenant_id: live.route.tenant_id,
                        executor_id: live.route.executor_id,
                    });
                }
            }
            failed = false;
        }
    }))
}
