#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Write as _,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use rand::random;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ternilo_local::{
    LocalApplication, LocalEventNotification, LocalInvalidationCategory,
    LocalInvalidationNotification, LocalSessionUpdate, ModelSelection,
};
use ternilo_protocol::{HarnessError, SessionId, SessionSearchRequest};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, ControlFrame, EXECUTOR_PROTOCOL_VERSION,
    EventBatch, ExecutorCapabilities, ExecutorCapability, ExecutorCommand, ExecutorCommandBody,
    ExecutorFrame, ExecutorHello, ExecutorId, ExecutorKind, ExecutorLiveInvalidation,
    ExecutorScope, SessionCursor,
};
use ternilo_transport_store::{NodeCommandClaim, TransportStore};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Error as WebSocketError, Message,
        client::IntoClientRequest,
        http::{HeaderValue, header::AUTHORIZATION},
    },
};
use tokio_util::task::TaskTracker;

#[path = "node/control_redaction.rs"]
mod control_redaction;

#[path = "node/model_gateway.rs"]
mod model_gateway;

mod application;
mod cleanup;
mod events;
mod replies;
pub(crate) use cleanup::server_binding;
#[path = "node/uploads.rs"]
mod uploads;
use events::{pump_events, pump_heartbeats, pump_invalidations};
use replies::{ReplyCache, execute_command};

/// Connect the shared local application to a remote gateway.
pub async fn connect(
    application: Arc<LocalApplication>,
    options: &crate::service::ServeOptions,
    commands: TaskTracker,
) -> Result<(), HarnessError> {
    let gateway_url = options
        .gateway_url
        .as_deref()
        .expect("gateway URL was validated");
    let token = options
        .token
        .as_deref()
        .expect("gateway token was validated");
    validate_gateway_url(gateway_url, options.allow_insecure_gateway)?;
    application.install_server_model_gateway(Arc::new(model_gateway::NodeModelGateway::new(
        &application,
        gateway_url,
        token,
    )?));
    let executor_id = ExecutorId::new(&options.node_id);
    executor_id.validate()?;
    let instance_nonce = random_hex_128();
    let catalog_revision = ternilo_local::catalog()?.revision().to_owned();
    let replies =
        Arc::new(ReplyCache::open(application.data_dir().join("node-transport.sqlite3")).await?);
    println!("Ternilo node {executor_id} connecting outbound to gateway");
    let connection = maintain_connection(
        gateway_url,
        token,
        &executor_id,
        &instance_nonce,
        &catalog_revision,
        Arc::clone(&application),
        replies,
        commands,
    );
    if application.account_server_binding().await.is_some() {
        let cleanup = cleanup::CleanupClient::new(gateway_url, token, &executor_id)?;
        tokio::select! {
            result = connection => result,
            result = cleanup.pump(Arc::clone(&application)) => result,
        }
    } else {
        connection.await
    }
}

#[allow(clippy::too_many_arguments)]
async fn maintain_connection(
    gateway_url: &str,
    token: &str,
    executor_id: &ExecutorId,
    instance_nonce: &str,
    catalog_revision: &str,
    application: Arc<LocalApplication>,
    replies: Arc<ReplyCache>,
    commands: TaskTracker,
) -> Result<(), HarnessError> {
    let mut retry = Duration::from_secs(1);
    loop {
        let connected_at = tokio::time::Instant::now();
        let result = connect_once(ConnectionConfig {
            gateway_url,
            token,
            executor_id,
            instance_nonce,
            catalog_revision,
            application: Arc::clone(&application),
            replies: Arc::clone(&replies),
            commands: commands.clone(),
        })
        .await;
        match result {
            Ok(()) => eprintln!("gateway connection closed; reconnecting"),
            Err(error) => eprintln!("gateway connection failed: {error}"),
        }
        if connected_at.elapsed() >= Duration::from_mins(1) {
            retry = Duration::from_secs(1);
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(Duration::from_secs(30));
    }
}

struct ConnectionConfig<'a> {
    gateway_url: &'a str,
    token: &'a str,
    executor_id: &'a ExecutorId,
    instance_nonce: &'a str,
    catalog_revision: &'a str,
    application: Arc<LocalApplication>,
    replies: Arc<ReplyCache>,
    commands: TaskTracker,
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep connection task lifetimes, acknowledgements and reconnect handling together."
)]
async fn connect_once(config: ConnectionConfig<'_>) -> Result<(), HarnessError> {
    if config.application.account_server_binding().await.is_some()
        && !cleanup::CleanupClient::new(config.gateway_url, config.token, config.executor_id)?
            .synchronize(&config.application)
            .await?
    {
        return Err(HarnessError::unavailable(
            "Node credential is restricted to the account cleanup channel",
        ));
    }
    let mut request = config
        .gateway_url
        .into_client_request()
        .map_err(|error| HarnessError::invalid(format!("invalid gateway URL: {error}")))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", config.token))
        .map_err(|_| HarnessError::invalid("node token is not a valid HTTP header value"))?;
    request.headers_mut().insert(AUTHORIZATION, authorization);
    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| gateway_connect_error(config.gateway_url, &error))?;
    let (scope, cursors, heartbeat_interval_ms) = introduce_node(&mut socket, &config).await?;
    config
        .application
        .set_directory_account_owner(scope.user_id.clone())?;

    println!(
        "Ternilo node {} connected for tenant={} user={}",
        config.executor_id, scope.tenant_id, scope.user_id
    );
    let (sink, mut stream) = socket.split();
    let (outgoing, receiver) = mpsc::channel::<ExecutorFrame>(256);
    let (upload_acks, upload_ack_receiver) = mpsc::channel(2);
    let (event_acks, event_ack_receiver) = mpsc::channel(1);
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(websocket_writer(sink, receiver));
    tasks.spawn(uploads::pump(
        Arc::clone(&config.application),
        scope.clone(),
        outgoing.clone(),
        upload_ack_receiver,
    ));
    tasks.spawn(pump_events(
        Arc::clone(&config.application),
        scope.clone(),
        cursors,
        outgoing.clone(),
        event_ack_receiver,
    ));
    tasks.spawn(pump_invalidations(
        Arc::clone(&config.application),
        outgoing.clone(),
    ));
    tasks.spawn(pump_heartbeats(
        Arc::clone(&config.application),
        outgoing.clone(),
        heartbeat_interval_ms,
    ));

    config.application.resume_server_schedules().await?;

    let read_result = async {
        loop {
            let message = tokio::select! {
                message = stream.next() => match message {
                    Some(message) => message,
                    None => break,
                },
                _ = tasks.join_next() => {
                    return Err(HarnessError::execution("gateway synchronization task ended; reconnecting"));
                }
            };
            let message = message
                .map_err(|error| HarnessError::execution(format!("read gateway frame: {error}")))?;
            match message {
                Message::Text(text) => {
                    let frame: ControlFrame =
                        serde_json::from_str(text.as_str()).map_err(|error| {
                            HarnessError::invalid(format!("decode gateway control frame: {error}"))
                        })?;
                    match frame {
                        ControlFrame::Command { command } => {
                            let application = Arc::clone(&config.application);
                            let replies = Arc::clone(&config.replies);
                            let outgoing = outgoing.clone();
                            let scope = scope.clone();
                            config.commands.spawn(async move {
                                let reply =
                                    execute_command(application, &scope, *command, &replies).await;
                                let _ = outgoing.send(ExecutorFrame::Reply { reply }).await;
                            });
                        }
                        ControlFrame::UploadsAcknowledged { stream_id, last_seq } => {
                            upload_acks.try_send((stream_id, last_seq)).map_err(|_| {
                                HarnessError::invalid("unexpected upload synchronization acknowledgement")
                            })?;
                        }
                        ControlFrame::EventsAcknowledged { cursor } => {
                            event_acks.try_send(cursor).map_err(|_| {
                                HarnessError::invalid("unexpected event synchronization acknowledgement")
                            })?;
                        }
                        ControlFrame::Shutdown { reason } => {
                            eprintln!("gateway requested disconnect: {reason}");
                            break;
                        }
                        ControlFrame::Welcome { .. } => {
                            return Err(HarnessError::invalid("gateway sent a second welcome frame"));
                        }
                    }
                }
                Message::Close(_) => break,
                Message::Binary(_) => {
                    return Err(HarnessError::invalid(format!(
                        "binary gateway frames are not supported by executor protocol v{EXECUTOR_PROTOCOL_VERSION}"
                    )));
                }
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Ok(())
    }
    .await;
    tasks.shutdown().await;
    read_result
}

fn gateway_connect_error(gateway_url: &str, error: &WebSocketError) -> HarnessError {
    if matches!(error, WebSocketError::Http(response) if response.status().is_success()) {
        return HarnessError::execution(format!(
            "connect gateway WebSocket: {gateway_url} returned HTTP 200 instead of a WebSocket upgrade; use the complete endpoint (standalone Relay: /executor/connect, Control: /api/v1/executors/connect)"
        ));
    }
    HarnessError::execution(format!("connect gateway WebSocket: {error}"))
}

async fn introduce_node<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    config: &ConnectionConfig<'_>,
) -> Result<(ExecutorScope, Vec<SessionCursor>, u64), HarnessError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    send_executor_frame(
        socket,
        &ExecutorFrame::Hello {
            hello: ExecutorHello {
                protocol_version: EXECUTOR_PROTOCOL_VERSION,
                executor_id: config.executor_id.clone(),
                executor_kind: ExecutorKind::EdgeNode,
                instance_nonce: config.instance_nonce.to_owned(),
                catalog_revision: config.catalog_revision.to_owned(),
                capabilities: ExecutorCapabilities::from([
                    ExecutorCapability::ApplicationRpc,
                    ExecutorCapability::PersistentSessions,
                    ExecutorCapability::WorkspaceFiles,
                    ExecutorCapability::InteractiveQuestions,
                    ExecutorCapability::LocalCredentials,
                    ExecutorCapability::SessionEventDelta,
                    ExecutorCapability::RunCancellation,
                    ExecutorCapability::ExtensionManagement,
                    ExecutorCapability::AuthorizationFlows,
                    ExecutorCapability::AgentPresets,
                    ExecutorCapability::SessionProjections,
                    ExecutorCapability::TelemetryDisclosure,
                    ExecutorCapability::Skills,
                    ExecutorCapability::SessionSteering,
                    ExecutorCapability::LiveInvalidations,
                ]),
            },
        },
    )
    .await?;
    let welcome = tokio::time::timeout(Duration::from_secs(10), receive_control_frame(socket))
        .await
        .map_err(|_| HarnessError::execution("gateway did not welcome node within 10 seconds"))??;
    match welcome {
        ControlFrame::Welcome {
            protocol_version,
            scope,
            event_cursors,
            heartbeat_interval_ms,
            ..
        } if protocol_version == EXECUTOR_PROTOCOL_VERSION => {
            scope.validate()?;
            send_executor_frame(
                socket,
                &ExecutorFrame::UploadSyncStarted {
                    scope: scope.clone(),
                    stream_id: config.application.accepted_upload_stream_id().to_owned(),
                },
            )
            .await?;
            Ok((scope, event_cursors, heartbeat_interval_ms))
        }
        ControlFrame::Welcome {
            protocol_version, ..
        } => Err(HarnessError::invalid(format!(
            "gateway selected unsupported protocol version {protocol_version}"
        ))),
        _ => Err(HarnessError::invalid("gateway first frame was not welcome")),
    }
}

async fn websocket_writer<S>(
    mut sink: futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, Message>,
    mut receiver: mpsc::Receiver<ExecutorFrame>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(frame) = receiver.recv().await {
        let Ok(text) = serde_json::to_string(&frame) else {
            break;
        };
        if sink.send(Message::Text(text.into())).await.is_err() {
            break;
        }
    }
}

async fn send_executor_frame<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    frame: &ExecutorFrame,
) -> Result<(), HarnessError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(frame)
        .map_err(|error| HarnessError::execution(format!("encode executor frame: {error}")))?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|error| HarnessError::execution(format!("send executor frame: {error}")))
}

async fn receive_control_frame<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Result<ControlFrame, HarnessError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = socket
            .next()
            .await
            .ok_or_else(|| HarnessError::execution("gateway closed before welcome"))?
            .map_err(|error| HarnessError::execution(format!("read gateway welcome: {error}")))?;
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).map_err(|error| {
                    HarnessError::invalid(format!("decode gateway welcome: {error}"))
                });
            }
            Message::Close(_) => {
                return Err(HarnessError::execution("gateway closed before welcome"));
            }
            Message::Binary(_) => {
                return Err(HarnessError::invalid(
                    "gateway welcome must be a JSON text frame",
                ));
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

pub(crate) fn validate_gateway_url(url: &str, allow_insecure: bool) -> Result<(), HarnessError> {
    if url.starts_with("wss://") {
        return Ok(());
    }
    if allow_insecure && url.starts_with("ws://") {
        return Ok(());
    }
    Err(HarnessError::policy(
        "gateway URL must use wss://; ws:// requires --allow-insecure-gateway",
    ))
}

fn random_hex_128() -> String {
    random::<[u8; 16]>()
        .into_iter()
        .fold(String::with_capacity(32), |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

fn now_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[cfg(test)]
mod application_tests;
#[cfg(test)]
mod command_lifecycle_tests;
#[cfg(test)]
mod connection_tests;
#[cfg(test)]
mod events_tests;
