use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::gateway_journal::{GatewayJournal, GatewayLease, RouteKey};
use futures_util::{SinkExt, StreamExt};
use salvo_extra::websocket::{Message, WebSocket};
use serde_json::Value;
use ternilo_control::{ControlUser, EdgeStore, NodePrincipal};
use ternilo_protocol::{
    HarnessError, InputAuthor, InputProvenance, RunId, SessionEvent, SessionEventKind, SessionId,
    SessionLiveActivity, SessionLiveDirty, SubmissionId, TenantId,
};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandOutcome, CommandReply, ConnectionId, ControlFrame,
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorCommand, ExecutorCommandBody,
    ExecutorFrame, ExecutorHello, ExecutorId, ExecutorKind, ExecutorScope,
};
use tokio::sync::{Mutex, Notify, RwLock, broadcast, mpsc, oneshot};

use crate::platform::{
    http::{ApiError, bearer_token, now_ms as control_now_ms},
    state::app_state,
};
use salvo_core::{
    http::StatusCode,
    prelude::{Depot, Request, Response, Router, Scribe, handler},
};
use salvo_extra::websocket::WebSocketUpgrade;

mod cluster_feed;
#[cfg(test)]
mod cluster_tests;
mod commands;
mod connection;
mod forwarding;
mod model_forwarding;
#[cfg(test)]
mod resource_lock_tests;
pub(crate) use model_forwarding::{ComputerModelEvent, ComputerModelStream};
mod replication;
pub(crate) use forwarding::{router as peer_router, validate_cluster_origin};

pub(crate) use connection::router;

const HEARTBEAT_INTERVAL_MS: u64 = 15_000;
const EXECUTOR_LEASE_TTL_MS: u64 = 60_000;
const COMMAND_DISPATCH_TTL_MS: u64 = 45_000;
const COMMAND_BATCH_LIMIT: u32 = 64;

#[derive(Clone)]
struct ConnectedExecutor {
    principal: NodePrincipal,
    hello: ExecutorHello,
    scope: ExecutorScope,
    connection_id: ConnectionId,
    sender: mpsc::Sender<ControlFrame>,
    lease: GatewayLease,
}

struct PendingCall {
    route: RouteKey,
    connection_id: Option<ConnectionId>,
    sender: oneshot::Sender<CommandReply>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum EdgeLiveNotification {
    ResourcesChanged {
        tenant_id: TenantId,
    },
    Session {
        tenant_id: TenantId,
        executor_id: ExecutorId,
        node_session_id: Option<SessionId>,
        dirty: SessionLiveDirty,
        refresh_events: bool,
        workbench: bool,
        activity: Option<SessionLiveActivity>,
    },
    Rescan {
        tenant_id: TenantId,
        executor_id: ExecutorId,
    },
}

pub(crate) struct EdgeGateway {
    store: EdgeStore,
    journal: GatewayJournal,
    instance_id: String,
    executors: RwLock<BTreeMap<RouteKey, ConnectedExecutor>>,
    resource_locks: Mutex<BTreeMap<RouteKey, Arc<Mutex<Option<Instant>>>>>,
    pending: Mutex<BTreeMap<(TenantId, CommandId), PendingCall>>,
    model_calls: Arc<
        std::sync::Mutex<
            BTreeMap<ternilo_transport::ModelRequestId, model_forwarding::PendingModel>,
        >,
    >,
    event_notify: Arc<Notify>,
    live_notify: broadcast::Sender<EdgeLiveNotification>,
    next_id: AtomicU64,
    cluster: Option<forwarding::ClusterForwarder>,
    cluster_feed: Option<tokio::task::JoinHandle<()>>,
}

impl EdgeGateway {
    pub(crate) async fn new(store: EdgeStore) -> Result<Self, HarnessError> {
        let (live_notify, _) = broadcast::channel(1_024);
        let journal = GatewayJournal::open(store.database().clone()).await?;
        Ok(Self {
            store,
            journal,
            instance_id: random_hex_128(),
            executors: RwLock::new(BTreeMap::new()),
            resource_locks: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeMap::new()),
            model_calls: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            event_notify: Arc::new(Notify::new()),
            live_notify,
            next_id: AtomicU64::new(1),
            cluster: None,
            cluster_feed: None,
        })
    }

    pub(crate) async fn with_cluster(
        mut self,
        origin: &str,
        key: &str,
    ) -> Result<Self, HarnessError> {
        self.cluster = Some(forwarding::ClusterForwarder::new(origin, key)?);
        self.cluster_feed = Some(cluster_feed::start(&self).await?);
        Ok(self)
    }

    pub(crate) async fn health(&self) -> Result<(), HarnessError> {
        self.store.health().await
    }

    pub(super) async fn connected_executors(&self) -> usize {
        self.executors.read().await.len()
    }

    pub(crate) fn subscribe_live(&self) -> broadcast::Receiver<EdgeLiveNotification> {
        self.live_notify.subscribe()
    }

    pub(crate) fn notify_resource_change(&self, tenant_id: &TenantId) {
        let _ = self
            .live_notify
            .send(EdgeLiveNotification::ResourcesChanged {
                tenant_id: tenant_id.clone(),
            });
    }

    async fn resource_lock(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> Arc<Mutex<Option<Instant>>> {
        self.resource_locks
            .lock()
            .await
            .entry(RouteKey::new(tenant_id.clone(), executor_id.clone()))
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    pub(crate) async fn lock_resources(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> tokio::sync::OwnedMutexGuard<Option<Instant>> {
        let mut guard = self
            .resource_lock(tenant_id, executor_id)
            .await
            .lock_owned()
            .await;
        // Mutations invalidate an overlapping discovery's result.
        *guard = None;
        guard
    }

    /// Concurrent readers can share a completed discovery; a later read refreshes again.
    pub(crate) async fn lock_discovery(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
    ) -> Option<tokio::sync::OwnedMutexGuard<Option<Instant>>> {
        let requested = Instant::now();
        let guard = self
            .resource_lock(tenant_id, executor_id)
            .await
            .lock_owned()
            .await;
        if guard.is_some_and(|completed| completed > requested) {
            None
        } else {
            Some(guard)
        }
    }

    pub(crate) async fn cached_events(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        self.store.events(tenant_id, executor_id, session_id).await
    }

    pub(crate) async fn cached_event_delta(
        &self,
        tenant_id: &TenantId,
        executor_id: &ExecutorId,
        session_id: &SessionId,
        after_seq: Option<u64>,
        wait: Duration,
    ) -> Result<Vec<SessionEvent>, HarnessError> {
        let wait = wait.min(Duration::from_secs(30));
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.event_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let events = self
                .store
                .events_after(tenant_id, executor_id, session_id, after_seq)
                .await?;
            if !events.is_empty() || wait.is_zero() {
                return Ok(events);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(Vec::new());
            }
        }
    }

    fn next_identifier(&self, prefix: &str, now: u64) -> String {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{}-{now}-{sequence}", self.instance_id)
    }
}

impl Drop for EdgeGateway {
    fn drop(&mut self) {
        if let Some(task) = &self.cluster_feed {
            task.abort();
        }
    }
}

fn random_hex_128() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes
        .iter()
        .fold(String::with_capacity(32), |mut value, byte| {
            use std::fmt::Write as _;
            write!(value, "{byte:02x}").expect("writing to a String cannot fail");
            value
        })
}

fn now_ms() -> Result<u64, HarnessError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock is invalid: {error}")))?
        .as_millis();
    u64::try_from(milliseconds).map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}
