use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use salvo_core::prelude::{Depot, Request, Response, Router, Scribe, handler};
use salvo_extra::websocket::{Message, WebSocket, WebSocketUpgrade};
use serde::{Deserialize, Serialize};
use ternilo_protocol::{ComputerModelRequest, HarnessError, ModelGatewayFrame, TenantId, UserId};
use ternilo_transport::{ControlFrame, ExecutorId, ModelRequestId};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message as ClientMessage, client::IntoClientRequest, protocol::WebSocketConfig},
};

use super::{ComputerModelEvent, ComputerModelStream};
use crate::{
    gateway_journal::{GatewayLease, PeerRoute, RouteKey},
    platform::{
        edge::{EdgeGateway, forwarding::validate_cluster_origin, now_ms, random_hex_128},
        http::ApiError,
        state::app_state,
    },
};

pub(in crate::platform::edge) const DOMAIN: &[u8] =
    b"ternilo-server-peer-v1\0WEBSOCKET\0/api/v1/internal/node/model\0";
const SIGNATURE: &str = "x-ternilo-peer-signature";
const MAX_MESSAGE: usize = 96 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::platform::edge) struct PeerModelRequest {
    request_id: String,
    target_instance_id: String,
    tenant_id: TenantId,
    executor_id: ExecutorId,
    fencing_token: u64,
    issued_at_ms: u64,
    owner_user_id: UserId,
    request: ComputerModelRequest,
}

impl EdgeGateway {
    pub(super) async fn forward_computer_model(
        &self,
        peer: PeerRoute,
        route: RouteKey,
        owner: &UserId,
        request: ComputerModelRequest,
    ) -> Result<ComputerModelStream, HarnessError> {
        let cluster = self.cluster.as_ref().ok_or_else(disconnected)?;
        self.store.require_node_credential(&peer.principal).await?;
        if peer.principal.scope.user_id != *owner {
            return Err(HarnessError::policy("source computer ownership changed"));
        }
        let input = PeerModelRequest {
            request_id: random_hex_128(),
            target_instance_id: peer.lease.owner_id,
            tenant_id: route.tenant_id,
            executor_id: route.executor_id,
            fencing_token: peer.lease.fencing_token,
            issued_at_ms: now_ms()?,
            owner_user_id: owner.clone(),
            request,
        };
        let bytes = serde_json::to_string(&input).map_err(|_| disconnected())?;
        let mut url = validate_cluster_origin(&peer.endpoint)?;
        url.set_path("/api/v1/internal/node/model");
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|()| disconnected())?;
        let mut upgrade = url
            .as_str()
            .into_client_request()
            .map_err(|_| disconnected())?;
        upgrade.headers_mut().insert(
            SIGNATURE,
            cluster
                .sign_domain(DOMAIN, bytes.as_bytes())
                .parse()
                .map_err(|_| disconnected())?,
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let (mut socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            connect_async_with_config(upgrade, Some(config), false),
        )
        .await
        .map_err(|_| disconnected())?
        .map_err(|_| disconnected())?;
        tokio::time::timeout(
            Duration::from_secs(10),
            socket.send(ClientMessage::text(bytes)),
        )
        .await
        .map_err(|_| disconnected())?
        .map_err(|_| disconnected())?;
        Ok(client_stream(socket, ModelRequestId::new(input.request_id)))
    }

    async fn accept_model_peer(
        &self,
        signature: &str,
        bytes: &[u8],
    ) -> Result<ComputerModelStream, HarnessError> {
        let cluster = self.cluster.as_ref().ok_or_else(disconnected)?;
        cluster.authenticate_domain(DOMAIN, signature, bytes)?;
        let input: PeerModelRequest = serde_json::from_slice(bytes)
            .map_err(|_| HarnessError::invalid("invalid Server peer model request"))?;
        input.request.validate()?;
        let now = now_ms()?;
        cluster
            .admit_identity(
                &input.request_id,
                &input.target_instance_id,
                input.issued_at_ms,
                &self.instance_id,
                now,
            )
            .await?;
        let route = RouteKey::new(input.tenant_id, input.executor_id);
        let lease = GatewayLease {
            owner_id: input.target_instance_id,
            fencing_token: input.fencing_token,
        };
        self.journal.check_lease(&route, &lease, now).await?;
        let source = self.connected(&route).await?;
        if source.lease.fencing_token != lease.fencing_token
            || source.lease.owner_id != lease.owner_id
        {
            return Err(HarnessError::conflict(
                "source computer connection changed before model dispatch",
            ));
        }
        self.start_local_computer_model(
            route,
            source,
            &input.owner_user_id,
            ModelRequestId::new(input.request_id),
            input.request,
        )
        .await
    }

    async fn serve_model_peer(self: Arc<Self>, signature: String, mut socket: WebSocket) {
        let Some(Ok(message)) = tokio::time::timeout(Duration::from_secs(10), socket.recv())
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let bytes = message.as_bytes();
        let mut call = match self.accept_model_peer(&signature, bytes).await {
            Ok(call) => call,
            Err(error) => {
                let event =
                    ComputerModelEvent::Output(Box::new(ModelGatewayFrame::Error { error }));
                if let Ok(bytes) = serde_json::to_string(&event) {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        socket.send(Message::text(bytes)),
                    )
                    .await;
                }
                return;
            }
        };
        let (mut sink, mut socket) = socket.split();
        let commands = call.commands.clone();
        let id = call.request_id.clone();
        let read = async {
            while let Some(Ok(message)) = socket.next().await {
                if message.is_close() {
                    break;
                }
                if !message.is_text() {
                    continue;
                }
                let Ok(command) = serde_json::from_slice::<ControlFrame>(message.as_bytes()) else {
                    break;
                };
                match &command {
                    ControlFrame::ModelRetryPermit { request_id, .. }
                    | ControlFrame::ModelCancel { request_id }
                        if *request_id == id => {}
                    _ => break,
                }
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(2), commands.send(command)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        };
        let write = async {
            while let Some(event) = call.events.recv().await {
                let terminal = event.is_terminal();
                let Ok(bytes) = serde_json::to_string(&event) else {
                    break;
                };
                if sink.send(Message::text(bytes)).await.is_err() || terminal {
                    break;
                }
            }
        };
        tokio::select! { () = read => {}, () = write => {} }
        call.cancel().await;
        if let Some(pending) = call.pending.take() {
            pending
                .lock()
                .expect("computer model calls")
                .remove(&call.request_id);
        }
    }
}

fn client_stream(
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request_id: ModelRequestId,
) -> ComputerModelStream {
    let (mut sink, mut socket) = socket.split();
    let (output, events) = mpsc::channel(64);
    let (commands, mut incoming) = mpsc::channel::<ControlFrame>(8);
    let task = tokio::spawn(async move {
        let read = async {
            while let Some(Ok(message)) = socket.next().await {
                if message.is_close() {
                    break;
                }
                if !message.is_text() {
                    continue;
                }
                let Ok(event) = serde_json::from_slice::<ComputerModelEvent>(&message.into_data())
                else {
                    break;
                };
                let terminal = event.is_terminal();
                if output.send(event).await.is_err() || terminal {
                    break;
                }
            }
        };
        let write = async {
            let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
            loop {
                let message = tokio::select! {
                    command = incoming.recv() => {
                        let Some(command) = command else { break; };
                        let Ok(bytes) = serde_json::to_string(&command) else { break; };
                        ClientMessage::text(bytes)
                    }
                    _ = heartbeat.tick() => ClientMessage::Ping(Vec::new().into()),
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(2), sink.send(message)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
        };
        tokio::select! { () = read => {}, () = write => {}, () = output.closed() => {} }
    });
    ComputerModelStream {
        request_id,
        events,
        commands,
        pending: None,
        peer_task: Some(task.abort_handle()),
    }
}

fn disconnected() -> HarnessError {
    HarnessError::unavailable(
        "source computer peer stream disconnected; the request was not replayed",
    )
}

pub(in crate::platform::edge) fn router() -> Router {
    Router::with_path("internal/node/model").get(connect_model)
}

#[handler]
async fn connect_model(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    let edge = Arc::clone(&app_state(depot).edge);
    let signature = request
        .headers()
        .get(SIGNATURE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if edge.cluster.is_none() || signature.is_none() {
        ApiError::unauthorized(HarnessError::policy(
            "Server peer authentication is required",
        ))
        .render(response);
        return;
    }
    let _ = WebSocketUpgrade::new()
        .max_frame_size(MAX_MESSAGE)
        .max_message_size(MAX_MESSAGE)
        .upgrade(request, response, move |socket| {
            edge.serve_model_peer(signature.expect("checked signature"), socket)
        })
        .await;
}
