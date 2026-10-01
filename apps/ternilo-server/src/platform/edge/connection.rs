use super::{
    ApiError, Arc, AtomicU64, ConnectedExecutor, ConnectionId, ControlFrame, Depot, Duration,
    EXECUTOR_LEASE_TTL_MS, EXECUTOR_PROTOCOL_VERSION, EdgeGateway, EdgeLiveNotification,
    ExecutorCapability, ExecutorFrame, ExecutorHello, ExecutorId, ExecutorKind, GatewayLease,
    HEARTBEAT_INTERVAL_MS, HarnessError, Message, NodePrincipal, Request, Response, RouteKey,
    Router, Scribe, SinkExt, StatusCode, StreamExt, TenantId, WebSocket, WebSocketUpgrade,
    app_state, bearer_token, control_now_ms, handler, mpsc, now_ms,
};

impl EdgeGateway {
    pub(crate) fn notify_computer_changed(&self, tenant: &TenantId, executor: &ExecutorId) {
        self.event_notify.notify_waiters();
        let _ = self.live_notify.send(EdgeLiveNotification::Rescan {
            tenant_id: tenant.clone(),
            executor_id: executor.clone(),
        });
    }

    pub(crate) async fn serve(self: Arc<Self>, principal: NodePrincipal, mut socket: WebSocket) {
        let Some((hello, connection_id, now, lease)) =
            self.handshake(&principal, &mut socket).await
        else {
            let _ = socket.close().await;
            return;
        };
        let route = RouteKey::new(
            principal.scope.tenant_id.clone(),
            principal.executor_id.clone(),
        );
        let last_seen = Arc::new(AtomicU64::new(now));
        let (sink, stream) = socket.split();
        let (sender, receiver) = mpsc::channel::<ControlFrame>(256);
        let connection = ConnectedExecutor {
            principal: principal.clone(),
            hello,
            scope: principal.scope,
            connection_id: connection_id.clone(),
            sender,
            lease,
        };
        if let Some(previous) = self
            .executors
            .write()
            .await
            .insert(route.clone(), connection)
        {
            if let Err(error) = self.journal.release(&route, &previous.lease).await {
                eprintln!("release replaced Node connection: {error}");
            }
            let _ = previous.sender.try_send(ControlFrame::Shutdown {
                reason: "this node established a newer connection".to_owned(),
            });
            self.fail_connection_calls(&route, &previous.connection_id)
                .await;
        }
        let Ok(connected) = self.connected(&route).await else {
            self.disconnect(
                &route.tenant_id,
                &route.executor_id,
                "node credential is invalid or revoked",
            )
            .await;
            return;
        };
        if connected.connection_id != connection_id {
            return;
        }
        if let Some(cluster) = &self.cluster
            && let Err(error) = self
                .journal
                .publish_peer(
                    &route,
                    &connected.lease,
                    &cluster.origin,
                    &connected.principal,
                    now,
                )
                .await
        {
            eprintln!("publish Node peer route: {error}");
            self.disconnect(
                &route.tenant_id,
                &route.executor_id,
                "Server peer route could not be published",
            )
            .await;
            return;
        }
        self.event_notify.notify_waiters();
        let _ = self.live_notify.send(EdgeLiveNotification::Rescan {
            tenant_id: route.tenant_id.clone(),
            executor_id: route.executor_id.clone(),
        });
        let writer = tokio::spawn(websocket_writer(sink, receiver));
        if let Err(error) = self.dispatch_available(&route).await {
            eprintln!("dispatch recovered Node commands: {error}");
        }
        self.receive_frames(&route, &connection_id, &last_seen, stream)
            .await;
        writer.abort();
        let _ = writer.await;
        self.remove_executor(&route, &connection_id).await;
    }

    pub(crate) async fn is_connected(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> bool {
        let route = RouteKey::new(tenant_id.clone(), executor_id.clone());
        if self.executors.read().await.contains_key(&route) {
            return self.connected(&route).await.is_ok();
        }
        if self.cluster.is_none() {
            return false;
        }
        match self
            .journal
            .peer_route(&route, now_ms().unwrap_or(u64::MAX))
            .await
        {
            Ok(Some(peer)) if peer.lease.owner_id != self.instance_id => self
                .store
                .require_node_credential(&peer.principal)
                .await
                .is_ok(),
            _ => false,
        }
    }

    pub(crate) async fn online_executor_ids(
        &self,
        tenant: &TenantId,
    ) -> Result<Vec<ExecutorId>, HarnessError> {
        let mut online = Vec::new();
        for id in self.journal.leased_executor_ids(tenant, now_ms()?).await? {
            if self.is_connected(tenant, &id).await {
                online.push(id);
            }
        }
        Ok(online)
    }

    pub(crate) async fn disconnect_account(&self, user_id: &ternilo_protocol::UserId) {
        let routes: Vec<_> = self
            .executors
            .read()
            .await
            .iter()
            .filter(|(_, connection)| connection.scope.user_id == *user_id)
            .map(|(route, _)| route.clone())
            .collect();
        for route in routes {
            self.disconnect(
                &route.tenant_id,
                &route.executor_id,
                "the computer owner's account is no longer active",
            )
            .await;
        }
    }

    pub(crate) async fn disconnect(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        reason: &str,
    ) {
        let route = RouteKey::new(tenant_id.clone(), executor_id.clone());
        let connection = self.executors.write().await.remove(&route);
        if let Some(connection) = connection {
            if let Err(error) = self.journal.release(&route, &connection.lease).await {
                eprintln!("release revoked Node connection: {error}");
            }
            let _ = connection.sender.try_send(ControlFrame::Shutdown {
                reason: reason.to_owned(),
            });
            self.fail_connection_calls(&route, &connection.connection_id)
                .await;
            self.event_notify.notify_waiters();
            let _ = self.live_notify.send(EdgeLiveNotification::Rescan {
                tenant_id: route.tenant_id,
                executor_id: route.executor_id,
            });
        }
    }

    pub(crate) async fn shutdown(&self) {
        let connections = std::mem::take(&mut *self.executors.write().await);
        self.pending.lock().await.clear();
        for (route, connection) in connections {
            let _ = connection.sender.try_send(ControlFrame::Shutdown {
                reason: "Ternilo Server is shutting down".to_owned(),
            });
            if let Err(error) = self.journal.release(&route, &connection.lease).await {
                eprintln!("release Node connection during shutdown: {error}");
            }
        }
    }

    pub(super) async fn connected(
        &self,
        route: &RouteKey,
    ) -> Result<ConnectedExecutor, HarnessError> {
        let connected = self
            .executors
            .read()
            .await
            .get(route)
            .cloned()
            .ok_or_else(|| {
                HarnessError::unavailable(
                    "selected Ternilo node is offline; reconnect it and retry the request",
                )
            })?;
        self.store
            .require_node_credential(&connected.principal)
            .await?;
        self.journal
            .check_lease(route, &connected.lease, now_ms()?)
            .await?;
        Ok(connected)
    }

    pub(super) async fn handshake(
        &self,
        principal: &NodePrincipal,
        socket: &mut WebSocket,
    ) -> Option<(ExecutorHello, ConnectionId, u64, GatewayLease)> {
        self.store.require_node_credential(principal).await.ok()?;
        let message = tokio::time::timeout(Duration::from_secs(10), socket.recv())
            .await
            .ok()??
            .ok()?;
        let text = message.as_str().ok()?;
        let ExecutorFrame::Hello { hello } = serde_json::from_str::<ExecutorFrame>(text).ok()?
        else {
            return None;
        };
        if hello.validate().is_err()
            || hello.executor_id != principal.executor_id
            || hello.executor_kind != ExecutorKind::EdgeNode
            || !hello
                .capabilities
                .contains(&ExecutorCapability::ApplicationRpc)
            || !hello
                .capabilities
                .contains(&ExecutorCapability::SessionEventDelta)
            || !hello
                .capabilities
                .contains(&ExecutorCapability::LiveInvalidations)
        {
            return None;
        }
        let now = now_ms().ok()?;
        self.store
            .register_executor(&principal.scope.tenant_id, &hello, now)
            .await
            .ok()?;
        let connection_id = ConnectionId::new(self.next_identifier("connection", now));
        let event_cursors = self
            .store
            .event_cursors(&principal.scope.tenant_id, &hello.executor_id)
            .await
            .ok()?;
        let route = RouteKey::new(
            principal.scope.tenant_id.clone(),
            principal.executor_id.clone(),
        );
        let lease = self
            .journal
            .acquire(&route, &self.instance_id, now, EXECUTOR_LEASE_TTL_MS)
            .await
            .ok()??;
        let welcome = ControlFrame::Welcome {
            protocol_version: EXECUTOR_PROTOCOL_VERSION,
            connection_id: connection_id.clone(),
            scope: principal.scope.clone(),
            heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
            event_cursors,
        };
        let Ok(message) = serde_json::to_string(&welcome) else {
            let _ = self.journal.release(&route, &lease).await;
            return None;
        };
        if socket.send(Message::text(message)).await.is_err() {
            let _ = self.journal.release(&route, &lease).await;
            return None;
        }
        if let Err(error) = self
            .handshake_uploads(principal, &route, &lease, socket)
            .await
        {
            if let Ok(message) = serde_json::to_string(&ControlFrame::Shutdown {
                reason: error.to_string(),
            }) {
                let _ = socket.send(Message::text(message)).await;
            }
            let _ = self.journal.release(&route, &lease).await;
            return None;
        }
        Some((hello, connection_id, now, lease))
    }

    pub(super) async fn handshake_uploads(
        &self,
        principal: &NodePrincipal,
        route: &RouteKey,
        lease: &GatewayLease,
        socket: &mut WebSocket,
    ) -> Result<(), HarnessError> {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.recv())
            .await
            .map_err(|_| HarnessError::invalid("Node did not identify its upload data directory"))?
            .ok_or_else(|| HarnessError::invalid("Node closed before upload synchronization"))?
            .map_err(|error| {
                HarnessError::execution(format!("read Node upload handshake: {error}"))
            })?;
        let text = message
            .as_str()
            .map_err(|_| HarnessError::invalid("expected Node upload handshake text frame"))?;
        let ExecutorFrame::UploadSyncStarted { scope, stream_id } = serde_json::from_str(text)
            .map_err(|error| {
                HarnessError::invalid(format!("decode Node upload handshake: {error}"))
            })?
        else {
            return Err(HarnessError::invalid(
                "expected upload data directory handshake before Node traffic",
            ));
        };
        if scope != principal.scope {
            return Err(HarnessError::policy(
                "Node upload handshake scope differs from enrollment",
            ));
        }
        let last_seq = self
            .journal
            .begin_upload_sync(&self.store, route, lease, &scope, &stream_id, now_ms()?)
            .await?;
        let frame = serde_json::to_string(&ControlFrame::UploadsAcknowledged {
            stream_id,
            last_seq,
        })
        .map_err(|error| {
            HarnessError::execution(format!("encode upload acknowledgement: {error}"))
        })?;
        socket.send(Message::text(frame)).await.map_err(|error| {
            HarnessError::execution(format!("send upload acknowledgement: {error}"))
        })
    }

    pub(super) async fn remove_executor(&self, route: &RouteKey, connection_id: &ConnectionId) {
        let removed = {
            let mut executors = self.executors.write().await;
            if executors
                .get(route)
                .is_some_and(|current| &current.connection_id == connection_id)
            {
                executors.remove(route)
            } else {
                None
            }
        };
        if let Some(connection) = removed {
            if let Err(error) = self.journal.release(route, &connection.lease).await {
                eprintln!("release disconnected Node: {error}");
            }
            self.fail_connection_calls(route, connection_id).await;
            self.event_notify.notify_waiters();
            let _ = self.live_notify.send(EdgeLiveNotification::Rescan {
                tenant_id: route.tenant_id.clone(),
                executor_id: route.executor_id.clone(),
            });
        }
    }

    pub(super) async fn fail_connection_calls(
        &self,
        route: &RouteKey,
        connection_id: &ConnectionId,
    ) {
        let keys = self
            .pending
            .lock()
            .await
            .iter()
            .filter(|(_, call)| {
                call.route == *route && call.connection_id.as_ref() == Some(connection_id)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.pending.lock().await.remove(&key);
        }
    }
}

pub(crate) fn router() -> Router {
    Router::with_path("executors/connect").get(connect)
}

#[handler]
pub(super) async fn connect(request: &mut Request, depot: &mut Depot, response: &mut Response) {
    let result = async {
        let token = bearer_token(request)?;
        let principal = app_state(depot)
            .store
            .authenticate_node(token, control_now_ms()?)
            .await
            .map_err(ApiError::unauthorized)?;
        let gateway = Arc::clone(&app_state(depot).edge);
        WebSocketUpgrade::new()
            // Retained files can contain 64 MiB before JSON/base64 encoding.
            .max_frame_size(96 * 1024 * 1024)
            .max_message_size(96 * 1024 * 1024)
            .upgrade(request, response, move |socket| {
                gateway.serve(principal, socket)
            })
            .await
            .map_err(|error| {
                ApiError::from(HarnessError::execution(format!(
                    "upgrade executor WebSocket: {error}"
                )))
            })?;
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = result {
        error.render(response);
    } else if response.status_code.is_none() {
        response.status_code(StatusCode::SWITCHING_PROTOCOLS);
    }
}

pub(super) async fn websocket_writer(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut receiver: mpsc::Receiver<ControlFrame>,
) {
    while let Some(frame) = receiver.recv().await {
        let Ok(text) = serde_json::to_string(&frame) else {
            break;
        };
        if sink.send(Message::text(text)).await.is_err() {
            break;
        }
    }
}
