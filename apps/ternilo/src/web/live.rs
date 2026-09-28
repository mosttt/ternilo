use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_util::{SinkExt, StreamExt};
use salvo_core::prelude::{Depot, Request, Response, Router, handler};
use salvo_extra::websocket::{Message, WebSocket, WebSocketUpgrade};
use ternilo_local::{
    LocalApplication, LocalEventNotification, LocalInvalidationCategory,
    LocalInvalidationNotification,
};
use ternilo_protocol::{
    ErrorCode, HarnessError, LIVE_PROTOCOL_VERSION, LiveClientFrame, LivePendingQuestion,
    LiveServerFrame, SessionId, SessionLiveActivity, SessionLiveDirty, SessionLiveMetadata,
    SessionLiveReadMask, SessionProjectionSnapshot, SessionStats,
};
use tokio::sync::{broadcast, mpsc};

use super::app_state;

const EVENT_CHUNK_SIZE: usize = 256;
const OUTGOING_CAPACITY: usize = 256;

pub(super) fn router() -> Router {
    Router::with_path("api/v1/live").get(browser_live)
}

#[handler]
async fn browser_live(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    let shared = Arc::clone(&app_state(depot).shared);
    if let Err(error) = WebSocketUpgrade::new()
        .upgrade(request, response, move |socket| async move {
            let serving = serve_browser_live(
                Arc::clone(&shared.application),
                shared.api_token.clone(),
                socket,
            );
            if let Some(service) = &shared.service {
                tokio::select! {
                    () = serving => {},
                    () = service.http_shutdown.cancelled() => {},
                }
            } else {
                serving.await;
            }
        })
        .await
    {
        response.render(error);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the connection handshake and subscription lifecycle in one protocol loop."
)]
async fn serve_browser_live(
    application: Arc<LocalApplication>,
    api_token: String,
    mut socket: WebSocket,
) {
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), socket.recv()).await;
    let hello = match first {
        Ok(Some(Ok(message))) if message.is_text() => message
            .as_str()
            .ok()
            .and_then(|text| serde_json::from_str::<LiveClientFrame>(text).ok()),
        _ => None,
    };
    let result = hello
        .ok_or_else(|| HarnessError::invalid("first live frame must be Hello"))
        .and_then(|frame| validate_hello(&api_token, &frame));
    if let Err(error) = result {
        let _ = send_direct_error(&mut socket, None, error).await;
        let _ = socket.close().await;
        return;
    }

    let (sink, mut stream) = socket.split();
    let (outgoing, receiver) = mpsc::channel::<LiveServerFrame>(OUTGOING_CAPACITY);
    let active_subscription = Arc::new(AtomicU64::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(live_writer(
        sink,
        receiver,
        Arc::clone(&active_subscription),
    ));
    if outgoing
        .send(LiveServerFrame::Ready {
            protocol_version: LIVE_PROTOCOL_VERSION,
        })
        .await
        .is_err()
    {
        tasks.shutdown().await;
        return;
    }
    tasks.spawn(serve_workbench(Arc::clone(&application), outgoing.clone()));
    let mut subscription = tokio::task::JoinSet::new();

    while let Some(message) = stream.next().await {
        let Ok(message) = message else { break };
        if !message.is_text() {
            if !message.is_ping() && !message.is_pong() {
                break;
            }
            continue;
        }
        let frame = message
            .as_str()
            .ok()
            .and_then(|text| serde_json::from_str::<LiveClientFrame>(text).ok());
        match frame {
            Some(LiveClientFrame::Subscribe {
                subscription_id,
                session_id,
                after_seq,
                metadata,
            }) if subscription_id != 0 && session_id.validate().is_ok() => {
                active_subscription.store(subscription_id, Ordering::Release);
                subscription.shutdown().await;
                let application = Arc::clone(&application);
                let outgoing = outgoing.clone();
                subscription.spawn(async move {
                    if let Err(error) = serve_subscription(
                        application,
                        outgoing.clone(),
                        subscription_id,
                        session_id,
                        after_seq,
                        metadata,
                    )
                    .await
                    {
                        let _ = outgoing
                            .send(LiveServerFrame::Error {
                                subscription_id: Some(subscription_id),
                                code: error.code,
                                message: error.message,
                            })
                            .await;
                    }
                });
            }
            Some(LiveClientFrame::Unsubscribe { subscription_id }) => {
                if active_subscription.load(Ordering::Acquire) == subscription_id {
                    active_subscription.store(0, Ordering::Release);
                    subscription.shutdown().await;
                }
            }
            Some(LiveClientFrame::Hello { .. } | LiveClientFrame::Subscribe { .. }) | None => {
                let _ = outgoing
                    .send(LiveServerFrame::Error {
                        subscription_id: None,
                        code: ErrorCode::InvalidInput,
                        message: "invalid live client frame".to_owned(),
                    })
                    .await;
            }
        }
    }

    active_subscription.store(0, Ordering::Release);
    subscription.shutdown().await;
    tasks.shutdown().await;
}

fn validate_hello(api_token: &str, frame: &LiveClientFrame) -> Result<(), HarnessError> {
    let LiveClientFrame::Hello {
        protocol_version,
        bearer_token,
        tenant_id,
    } = frame
    else {
        return Err(HarnessError::invalid("first live frame must be Hello"));
    };
    if *protocol_version != LIVE_PROTOCOL_VERSION {
        return Err(HarnessError::invalid(format!(
            "unsupported live protocol version {protocol_version}"
        )));
    }
    if bearer_token.as_deref() != Some(api_token) {
        return Err(HarnessError::policy("invalid Local live bearer token"));
    }
    if tenant_id
        .as_ref()
        .is_some_and(|tenant_id| tenant_id.as_str() != "local")
    {
        return Err(HarnessError::policy(
            "Local live tenant must be omitted or equal to local",
        ));
    }
    Ok(())
}

async fn send_direct_error(
    socket: &mut WebSocket,
    subscription_id: Option<u64>,
    error: HarnessError,
) -> Result<(), salvo_core::Error> {
    let frame = LiveServerFrame::Error {
        subscription_id,
        code: error.code,
        message: error.message,
    };
    let text = serde_json::to_string(&frame).map_err(salvo_core::Error::other)?;
    socket.send(Message::text(text)).await
}

async fn live_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut receiver: mpsc::Receiver<LiveServerFrame>,
    active_subscription: Arc<AtomicU64>,
) {
    while let Some(frame) = receiver.recv().await {
        if !frame_matches_active_subscription(&frame, active_subscription.load(Ordering::Acquire)) {
            continue;
        }
        let Ok(text) = serde_json::to_string(&frame) else {
            break;
        };
        if sink.send(Message::text(text)).await.is_err() {
            break;
        }
    }
}

fn frame_matches_active_subscription(frame: &LiveServerFrame, active: u64) -> bool {
    match frame {
        LiveServerFrame::EventBatch {
            subscription_id, ..
        }
        | LiveServerFrame::SessionMetadata {
            subscription_id, ..
        }
        | LiveServerFrame::Error {
            subscription_id: Some(subscription_id),
            ..
        } => *subscription_id == active,
        LiveServerFrame::Ready { .. }
        | LiveServerFrame::Workbench { .. }
        | LiveServerFrame::Activity { .. }
        | LiveServerFrame::Error {
            subscription_id: None,
            ..
        } => true,
    }
}

mod workbench;
use workbench::serve_workbench;
mod subscriptions;
use subscriptions::serve_subscription;
mod session_data;

#[cfg(test)]
mod tests;
