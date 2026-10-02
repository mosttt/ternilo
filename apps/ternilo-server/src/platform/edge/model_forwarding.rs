use super::{
    Arc, ConnectedExecutor, ConnectionId, ControlFrame, EdgeGateway, ExecutorCapability,
    ExecutorId, HarnessError, RouteKey, TenantId, mpsc, random_hex_128,
};
use std::{collections::BTreeMap, sync::Mutex};
use ternilo_protocol::{ComputerModelAttempt, ComputerModelRequest, ModelGatewayFrame, UserId};
use ternilo_transport::ModelRequestId;

pub(super) mod peer;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub(crate) enum ComputerModelEvent {
    Output(Box<ModelGatewayFrame>),
    Attempt(ComputerModelAttempt),
}

impl ComputerModelEvent {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Output(frame) if frame.is_terminal())
    }
}

pub(super) struct PendingModel {
    route: RouteKey,
    connection_id: ConnectionId,
    output: mpsc::Sender<ComputerModelEvent>,
}

pub(crate) struct ComputerModelStream {
    pub(crate) request_id: ModelRequestId,
    pub(crate) events: mpsc::Receiver<ComputerModelEvent>,
    commands: mpsc::Sender<ControlFrame>,
    pending: Option<Arc<Mutex<BTreeMap<ModelRequestId, PendingModel>>>>,
    peer_task: Option<tokio::task::AbortHandle>,
}

impl ComputerModelStream {
    pub(crate) async fn permit_retry(
        &self,
        attempt: u32,
        error: Option<HarnessError>,
    ) -> Result<(), HarnessError> {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.commands.send(ControlFrame::ModelRetryPermit {
                request_id: self.request_id.clone(),
                attempt,
                error,
            }),
        )
        .await
        .map_err(|_| HarnessError::unavailable("source computer stopped reading permissions"))?
        .map_err(|_| HarnessError::unavailable("source computer disconnected"))
    }

    pub(crate) async fn cancel(&self) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.commands.send(ControlFrame::ModelCancel {
                request_id: self.request_id.clone(),
            }),
        )
        .await;
    }
}

impl Drop for ComputerModelStream {
    fn drop(&mut self) {
        if let Some(task) = &self.peer_task {
            task.abort();
        }
        if let Some(pending) = &self.pending
            && pending
                .lock()
                .expect("computer model calls")
                .remove(&self.request_id)
                .is_some()
        {
            let _ = self.commands.try_send(ControlFrame::ModelCancel {
                request_id: self.request_id.clone(),
            });
        }
    }
}

impl EdgeGateway {
    pub(crate) async fn start_computer_model(
        &self,
        tenant: &TenantId,
        executor: &ExecutorId,
        owner: &UserId,
        request: ComputerModelRequest,
    ) -> Result<ComputerModelStream, HarnessError> {
        request.validate()?;
        let route = RouteKey::new(tenant.clone(), executor.clone());
        if self.executors.read().await.contains_key(&route) {
            let source = self.connected(&route).await?;
            return self
                .start_local_computer_model(
                    route,
                    source,
                    owner,
                    ModelRequestId::new(random_hex_128()),
                    request,
                )
                .await;
        }
        let peer = self
            .journal
            .peer_route(&route, super::now_ms()?)
            .await?
            .filter(|peer| peer.lease.owner_id != self.instance_id)
            .ok_or_else(|| HarnessError::unavailable("source computer has no active peer route"))?;
        self.forward_computer_model(peer, route, owner, request)
            .await
    }

    async fn start_local_computer_model(
        &self,
        route: RouteKey,
        source: ConnectedExecutor,
        owner: &UserId,
        request_id: ModelRequestId,
        request: ComputerModelRequest,
    ) -> Result<ComputerModelStream, HarnessError> {
        if source.scope.user_id != *owner
            || !source
                .hello
                .capabilities
                .contains(&ExecutorCapability::ModelForwarding)
        {
            return Err(HarnessError::policy(
                "the source computer cannot serve this model binding",
            ));
        }
        let (output, events) = mpsc::channel(64);
        self.model_calls
            .lock()
            .expect("computer model calls")
            .insert(
                request_id.clone(),
                PendingModel {
                    route,
                    connection_id: source.connection_id,
                    output,
                },
            );
        let stream = ComputerModelStream {
            request_id: request_id.clone(),
            events,
            commands: source.sender.clone(),
            pending: Some(Arc::clone(&self.model_calls)),
            peer_task: None,
        };
        source
            .sender
            .send(ControlFrame::ModelRequest {
                request_id,
                scope: source.scope,
                request: Box::new(request),
            })
            .await
            .map_err(|_| {
                HarnessError::unavailable("source computer disconnected before model delivery")
            })?;
        Ok(stream)
    }

    pub(super) fn receive_model_event(
        &self,
        route: &RouteKey,
        source: &ConnectedExecutor,
        id: &ModelRequestId,
        event: ComputerModelEvent,
    ) {
        let mut pending = self.model_calls.lock().expect("computer model calls");
        let Some(call) = pending.get(id) else {
            let _ = source.sender.try_send(ControlFrame::ModelCancel {
                request_id: id.clone(),
            });
            return;
        };
        if &call.route != route || call.connection_id != source.connection_id {
            return;
        }
        let terminal = matches!(&event, ComputerModelEvent::Output(frame) if frame.is_terminal());
        if call.output.try_send(event).is_err() {
            pending.remove(id);
            let _ = source.sender.try_send(ControlFrame::ModelCancel {
                request_id: id.clone(),
            });
        } else if terminal {
            pending.remove(id);
        }
    }

    pub(super) fn fail_model_connection(&self, route: &RouteKey, connection: &ConnectionId) {
        self.model_calls
            .lock()
            .expect("computer model calls")
            .retain(|_, call| &call.route != route || &call.connection_id != connection);
    }
}
