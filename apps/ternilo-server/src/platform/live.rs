use std::{collections::BTreeMap, time::Duration};

use futures_util::StreamExt as _;
use salvo_core::prelude::{Depot, Request, Response, handler};
use salvo_extra::websocket::{Message, WebSocket, WebSocketUpgrade};
use ternilo_cloud::{CloudLiveNotification, CloudSessionRecord, CloudSessionState};
use ternilo_control::{
    ControlAction, ControlUser, EdgeSessionRecord, ResourceAction, ResourceKind,
};
use ternilo_protocol::{
    HarnessError, LIVE_PROTOCOL_VERSION, LiveClientFrame, LivePendingQuestion, LiveServerFrame,
    SessionEvent, SessionId, SessionLiveActivity, SessionLiveDirty, SessionLiveMetadata,
    SessionLiveReadMask, TenantId,
};
use ternilo_transport::{ApplicationOperation, ExecutorId};
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

use crate::platform::{
    edge::EdgeLiveNotification,
    state::{AppState, app_state},
    workbench::{CloudAdapter, EdgeAdapter, PlacementResolver, SessionTarget, load_state},
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_BATCH_LIMIT: u32 = 256;
const OUTGOING_CAPACITY: usize = 64;

struct ActiveSubscription {
    id: u64,
    session_id: SessionId,
    task: JoinHandle<()>,
}

#[derive(Debug, Eq, PartialEq)]
enum IncomingLiveMessage<'a> {
    Text(&'a str),
    Close,
    Ignore,
}

fn classify_incoming(message: &Message) -> IncomingLiveMessage<'_> {
    if message.is_close() {
        IncomingLiveMessage::Close
    } else if let Ok(text) = message.as_str() {
        IncomingLiveMessage::Text(text)
    } else {
        IncomingLiveMessage::Ignore
    }
}

#[handler]
pub(crate) async fn upgrade(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    let state = app_state(depot).clone();
    if let Err(error) = WebSocketUpgrade::new()
        .upgrade(request, response, move |socket| serve(state, socket))
        .await
    {
        response.render(error);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep authentication and subscription cleanup in one connection lifecycle."
)]
async fn serve(state: AppState, mut socket: WebSocket) {
    let mut shutdown = state.shutdown.clone();
    let (actor, tenant_id, token) = match authenticate(&state, &mut socket).await {
        Ok(scope) => scope,
        Err(error) => {
            let _ = send_frame(&mut socket, &error_frame(None, error)).await;
            let _ = socket.close().await;
            return;
        }
    };

    // Subscribe before reading the baseline so commits racing with that read
    // remain queued for canonical repair after Ready.
    let mut notifications = state.cloud_events.subscribe();
    let mut edge_notifications = state.edge.subscribe_live();
    let (workbench, activity) = match load_workbench(&state, &actor, &tenant_id).await {
        Ok(baseline) => baseline,
        Err(error) => {
            let _ = send_frame(&mut socket, &error_frame(None, error)).await;
            let _ = socket.close().await;
            return;
        }
    };
    if send_frame(
        &mut socket,
        &LiveServerFrame::Ready {
            protocol_version: LIVE_PROTOCOL_VERSION,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let mut workbench_revision = 1_u64;
    if send_frame(
        &mut socket,
        &LiveServerFrame::Workbench {
            revision: workbench_revision,
            state: workbench,
            activity,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let (outgoing, mut subscription_frames) = mpsc::channel(OUTGOING_CAPACITY);
    let mut subscription: Option<ActiveSubscription> = None;
    let mut last_subscription_id: Option<u64> = None;
    let mut authorization_check = tokio::time::interval(Duration::from_secs(5));
    authorization_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = authorization_check.tick() => {
                if let Err(error) = authorize_connection(&state, &token, &tenant_id).await {
                    let _ = send_frame(&mut socket, &error_frame(None, error)).await;
                    break;
                }
                if let Some(frame) = recheck_subscription(&state, &actor, &tenant_id, &mut subscription).await
                    && (send_frame(&mut socket, &frame).await.is_err()
                        || send_workbench(&state, &actor, &tenant_id, &mut socket, &mut workbench_revision).await.is_err())
                {
                    break;
                }
            }
            incoming = socket.next() => {
                let Some(Ok(message)) = incoming else { break };
                let text = match classify_incoming(&message) {
                    IncomingLiveMessage::Text(text) => text,
                    IncomingLiveMessage::Close => break,
                    IncomingLiveMessage::Ignore => continue,
                };
                let frame = match serde_json::from_str::<LiveClientFrame>(text) {
                    Ok(frame) => frame,
                    Err(error) => {
                        let frame = error_frame(None, HarnessError::invalid(format!(
                            "decode live client frame: {error}"
                        )));
                        if send_frame(&mut socket, &frame).await.is_err() { break }
                        continue;
                    }
                };
                match frame {
                    LiveClientFrame::Hello { .. } => {
                        let frame = error_frame(None, HarnessError::invalid(
                            "Hello is only valid as the first live frame",
                        ));
                        if send_frame(&mut socket, &frame).await.is_err() { break }
                    }
                    LiveClientFrame::Subscribe {
                        subscription_id,
                        session_id,
                        after_seq,
                        metadata,
                    } => {
                        if !valid_next_subscription_id(last_subscription_id, subscription_id) {
                            let frame = error_frame(None, HarnessError::invalid(
                                "live subscription_id must be non-zero and strictly increasing",
                            ));
                            if send_frame(&mut socket, &frame).await.is_err() { break }
                            continue;
                        }
                        cancel_subscription(&mut subscription, None);
                        last_subscription_id = Some(subscription_id);
                        let task = tokio::spawn(run_subscription(
                            state.clone(),
                            actor.clone(),
                            tenant_id.clone(),
                            subscription_id,
                            session_id.clone(),
                            after_seq,
                            metadata,
                            outgoing.clone(),
                        ));
                        subscription = Some(ActiveSubscription { id: subscription_id, session_id, task });
                    }
                    LiveClientFrame::Unsubscribe { subscription_id } => {
                        cancel_subscription(&mut subscription, Some(subscription_id));
                    }
                }
            }
            frame = subscription_frames.recv() => {
                let Some(frame) = frame else { break };
                let active_subscription_id = subscription.as_ref().map(|active| active.id);
                if !frame_matches_active_subscription(&frame, active_subscription_id) {
                    continue;
                }
                if let LiveServerFrame::EventBatch { session_id, .. }
                    | LiveServerFrame::SessionMetadata { session_id, .. } = &frame
                    && let Err(error) = require_session_view(&state, &actor, &tenant_id, session_id).await
                {
                    cancel_subscription(&mut subscription, None);
                    if send_frame(&mut socket, &error_frame(active_subscription_id, error)).await.is_err() { break }
                    continue;
                }
                if send_frame(&mut socket, &frame).await.is_err() { break }
            }
            notification = notifications.recv() => {
                if let Ok(CloudLiveNotification::Reauthenticate { user_id }) = &notification {
                    if user_id == &actor.user_id
                        && let Err(error) = authorize_connection(&state, &token, &tenant_id).await
                    {
                        let _ = send_frame(&mut socket, &error_frame(None, error)).await;
                        break;
                    }
                    continue;
                }
                let rescan = matches!(
                    notification,
                    Ok(CloudLiveNotification::Rescan)
                        | Err(broadcast::error::RecvError::Lagged(_))
                );
                let closed = matches!(notification, Err(broadcast::error::RecvError::Closed));
                if closed { break }
                if rescan {
                    if let Err(error) = authorize_connection(&state, &token, &tenant_id).await {
                        let _ = send_frame(&mut socket, &error_frame(None, error)).await;
                        break;
                    }
                    if let Some(frame) = recheck_subscription(&state, &actor, &tenant_id, &mut subscription).await
                        && send_frame(&mut socket, &frame).await.is_err()
                    {
                        break;
                    }
                    if send_workbench(
                        &state,
                        &actor,
                        &tenant_id,
                        &mut socket,
                        &mut workbench_revision,
                    ).await.is_err() { break }
                    continue;
                }
                let Ok(notification) = notification else { continue };
                if handle_notification(
                    &state,
                    &actor,
                    &tenant_id,
                    notification,
                    &mut socket,
                    &mut workbench_revision,
                ).await.is_err() { break }
            }
            notification = edge_notifications.recv() => {
                let notification = match notification {
                    Ok(notification) => notification,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(frame) = recheck_subscription(&state, &actor, &tenant_id, &mut subscription).await
                            && send_frame(&mut socket, &frame).await.is_err()
                        {
                            break;
                        }
                        if send_workbench(
                            &state,
                            &actor,
                            &tenant_id,
                            &mut socket,
                            &mut workbench_revision,
                        ).await.is_err() { break }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if matches!(&notification, EdgeLiveNotification::ResourcesChanged { tenant_id: changed } if changed == &tenant_id)
                    && let Some(frame) = recheck_subscription(&state, &actor, &tenant_id, &mut subscription).await
                    && send_frame(&mut socket, &frame).await.is_err()
                {
                    break;
                }
                if handle_edge_notification(
                    &state,
                    &actor,
                    &tenant_id,
                    notification,
                    &mut socket,
                    &mut workbench_revision,
                ).await.is_err() { break }
            }
        }
    }
    cancel_subscription(&mut subscription, None);
    let _ = socket.close().await;
}

async fn authenticate(
    state: &AppState,
    socket: &mut WebSocket,
) -> Result<(ControlUser, TenantId, zeroize::Zeroizing<String>), HarnessError> {
    let first = tokio::time::timeout(HELLO_TIMEOUT, socket.next())
        .await
        .map_err(|_| HarnessError::policy("live Hello timed out"))?
        .ok_or_else(|| HarnessError::policy("live connection closed before Hello"))?
        .map_err(|error| HarnessError::policy(format!("receive live Hello: {error}")))?;
    let text = first
        .as_str()
        .map_err(|_| HarnessError::invalid("live Hello must be a text frame"))?;
    let (token, tenant_id) = parse_hello(text)?;
    let actor = authorize_connection(state, &token, &tenant_id).await?;
    Ok((actor, tenant_id, zeroize::Zeroizing::new(token)))
}

async fn authorize_connection(
    state: &AppState,
    token: &str,
    tenant_id: &TenantId,
) -> Result<ControlUser, HarnessError> {
    let (user, _) = super::auth::authenticate_token(state, token).await?;
    let actor = state.store.identity_session(user).await?.user;
    state
        .store
        .authorize(&actor, tenant_id, ControlAction::TenantRead)
        .await?;
    Ok(actor)
}

fn parse_hello(text: &str) -> Result<(String, TenantId), HarnessError> {
    let frame = serde_json::from_str::<LiveClientFrame>(text)
        .map_err(|error| HarnessError::invalid(format!("decode live Hello: {error}")))?;
    let LiveClientFrame::Hello {
        protocol_version,
        bearer_token,
        tenant_id,
    } = frame
    else {
        return Err(HarnessError::invalid("the first live frame must be Hello"));
    };
    if protocol_version != LIVE_PROTOCOL_VERSION {
        return Err(HarnessError::composition(format!(
            "unsupported live protocol version {protocol_version}"
        )));
    }
    let token = bearer_token
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| HarnessError::policy("live bearer token is required"))?;
    let tenant_id = tenant_id.ok_or_else(|| HarnessError::policy("live tenant is required"))?;
    tenant_id.validate()?;
    Ok((token, tenant_id))
}

async fn require_session_view(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<(), HarnessError> {
    state
        .store
        .resource_access(actor, tenant_id, ResourceKind::Session, session_id.as_str())
        .await?
        .require(ResourceAction::View)
}

fn error_frame(subscription_id: Option<u64>, error: HarnessError) -> LiveServerFrame {
    LiveServerFrame::Error {
        subscription_id,
        code: error.code,
        message: error.message,
    }
}

const fn valid_next_subscription_id(previous: Option<u64>, next: u64) -> bool {
    next != 0
        && match previous {
            Some(previous) => next > previous,
            None => true,
        }
}

fn frame_matches_active_subscription(
    frame: &LiveServerFrame,
    active_subscription_id: Option<u64>,
) -> bool {
    let subscription_id = match frame {
        LiveServerFrame::EventBatch {
            subscription_id, ..
        }
        | LiveServerFrame::SessionMetadata {
            subscription_id, ..
        } => Some(*subscription_id),
        LiveServerFrame::Error {
            subscription_id, ..
        } => *subscription_id,
        LiveServerFrame::Ready { .. }
        | LiveServerFrame::Workbench { .. }
        | LiveServerFrame::Activity { .. } => None,
    };
    subscription_id.is_none_or(|subscription_id| Some(subscription_id) == active_subscription_id)
}

async fn recheck_subscription(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription: &mut Option<ActiveSubscription>,
) -> Option<LiveServerFrame> {
    let active = subscription.as_ref()?;
    let error = require_session_view(state, actor, tenant_id, &active.session_id)
        .await
        .err()?;
    let id = active.id;
    cancel_subscription(subscription, Some(id));
    Some(error_frame(Some(id), error))
}

fn cancel_subscription(subscription: &mut Option<ActiveSubscription>, expected_id: Option<u64>) {
    if subscription
        .as_ref()
        .is_some_and(|active| expected_id.is_none_or(|expected| expected == active.id))
        && let Some(active) = subscription.take()
    {
        active.task.abort();
    }
}

async fn send_frame(socket: &mut WebSocket, frame: &LiveServerFrame) -> Result<(), HarnessError> {
    let text = serde_json::to_string(frame)
        .map_err(|error| HarnessError::execution(format!("encode live server frame: {error}")))?;
    socket
        .send(Message::text(text))
        .await
        .map_err(|error| HarnessError::execution(format!("send live server frame: {error}")))
}

mod subscriptions;
use subscriptions::run_subscription;
mod session_data;
mod workbench;
use workbench::{handle_edge_notification, handle_notification, load_workbench, send_workbench};

#[cfg(test)]
mod tests;
