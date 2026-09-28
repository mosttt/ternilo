use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use salvo_core::{
    conn::tcp::TcpAcceptor,
    http::StatusCode,
    prelude::{Depot, Json, Response, Router, Server, Text, handler},
    server::ServerHandle,
};
use salvo_extra::affix_state;
use serde::Serialize;
use ternilo_cloud::CloudWorkerIdentity;
use ternilo_protocol::HarnessError;
use tokio::task::JoinHandle;

use crate::{client::WorkerClient, now_ms, workspace::enforce_workspace_free_space};

#[derive(Clone)]
struct HealthState {
    client: WorkerClient,
    identity: CloudWorkerIdentity,
    workspace_root: PathBuf,
    minimum_workspace_free_bytes: u64,
    serving: Arc<AtomicBool>,
}

pub(crate) struct HealthServer {
    pub(crate) address: SocketAddr,
    state: HealthState,
    handle: ServerHandle,
    task: JoinHandle<Result<(), HarnessError>>,
}

#[derive(Serialize)]
struct ReadinessSnapshot {
    status: &'static str,
    worker_id: String,
    lifecycle: &'static str,
    server: &'static str,
    worker_lease: &'static str,
    workspace: &'static str,
    lease_expires_at_ms: Option<u64>,
}

impl ReadinessSnapshot {
    fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

impl HealthState {
    async fn readiness(&self) -> ReadinessSnapshot {
        let server_ready = self.client.configuration().await.is_ok();
        let workspace_ready =
            enforce_workspace_free_space(&self.workspace_root, self.minimum_workspace_free_bytes)
                .await
                .is_ok();
        let lease_expires_at_ms = Some(self.client.lease_expires_at_ms());
        let worker_lease_ready = now_ms().is_ok_and(|now| self.client.lease_expires_at_ms() > now);
        readiness_snapshot(
            self.identity.worker_id.to_string(),
            [
                self.serving.load(Ordering::Relaxed),
                server_ready,
                worker_lease_ready,
                workspace_ready,
            ],
            lease_expires_at_ms,
        )
    }
}

impl HealthServer {
    pub(crate) async fn bind(
        listen: SocketAddr,
        client: WorkerClient,
        identity: CloudWorkerIdentity,
        workspace_root: PathBuf,
        minimum_workspace_free_bytes: u64,
    ) -> Result<Self, HarnessError> {
        let state = HealthState {
            client,
            identity,
            workspace_root,
            minimum_workspace_free_bytes,
            serving: Arc::new(AtomicBool::new(true)),
        };
        let app = router(state.clone());
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "bind cloud Worker health endpoint {listen}: {error}"
                ))
            })?;
        let address = listener.local_addr().map_err(|error| {
            HarnessError::execution(format!("read cloud Worker health address: {error}"))
        })?;
        let acceptor = TcpAcceptor::try_from(listener).map_err(|error| {
            HarnessError::execution(format!("create cloud Worker health acceptor: {error}"))
        })?;
        let server = Server::new(acceptor);
        let handle = server.handle();
        let task = tokio::spawn(async move {
            server.try_serve(app).await.map_err(|error| {
                HarnessError::execution(format!("cloud Worker health server: {error}"))
            })
        });
        Ok(Self {
            address,
            state,
            handle,
            task,
        })
    }

    pub(crate) fn mark_draining(&self) {
        self.state.serving.store(false, Ordering::Relaxed);
    }

    pub(crate) async fn stop(&mut self) {
        self.mark_draining();
        self.handle.stop_graceful(Some(Duration::from_secs(5)));
        let _ = (&mut self.task).await;
    }

    pub(crate) async fn join(
        &mut self,
    ) -> Result<Result<(), HarnessError>, tokio::task::JoinError> {
        (&mut self.task).await
    }
}

fn router(state: HealthState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("livez").get(liveness))
        .push(Router::with_path("readyz").get(readiness))
        .push(Router::with_path("metrics").get(metrics))
}

fn state(depot: &Depot) -> &HealthState {
    depot
        .get_typed::<HealthState>()
        .expect("cloud Worker health state middleware must run first")
}

#[handler]
fn liveness(depot: &mut Depot) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "live",
        "worker_id": state(depot).identity.worker_id,
    }))
}

#[handler]
async fn readiness(depot: &mut Depot, response: &mut Response) {
    let snapshot = state(depot).readiness().await;
    if !snapshot.is_ready() {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
    }
    response.render(Json(snapshot));
}

#[handler]
async fn metrics(depot: &mut Depot, response: &mut Response) {
    let snapshot = state(depot).readiness().await;
    if !snapshot.is_ready() {
        response.status_code(StatusCode::SERVICE_UNAVAILABLE);
    }
    response.render(Text::Plain(format!(
        concat!(
            "# HELP ternilo_cloud_worker_ready Whether Server, lease and workspace probes passed.\n",
            "# TYPE ternilo_cloud_worker_ready gauge\n",
            "ternilo_cloud_worker_ready {}\n",
            "# HELP ternilo_cloud_worker_lease_expires_at_ms Durable worker lease expiry in Unix milliseconds.\n",
            "# TYPE ternilo_cloud_worker_lease_expires_at_ms gauge\n",
            "ternilo_cloud_worker_lease_expires_at_ms {}\n",
        ),
        u8::from(snapshot.is_ready()),
        snapshot.lease_expires_at_ms.unwrap_or_default(),
    )));
}

const fn readiness_label(ready: bool) -> &'static str {
    if ready { "ready" } else { "unavailable" }
}

fn readiness_snapshot(
    worker_id: String,
    checks: [bool; 4],
    lease_expires_at_ms: Option<u64>,
) -> ReadinessSnapshot {
    let [serving, server_ready, worker_lease_ready, workspace_ready] = checks;
    let ready = checks.into_iter().all(|check| check);
    ReadinessSnapshot {
        status: if ready { "ready" } else { "not_ready" },
        worker_id,
        lifecycle: if serving { "serving" } else { "draining" },
        server: readiness_label(server_ready),
        worker_lease: readiness_label(worker_lease_ready),
        workspace: readiness_label(workspace_ready),
        lease_expires_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_labels_are_stable_for_operator_automation() {
        assert_eq!(readiness_label(true), "ready");
        assert_eq!(readiness_label(false), "unavailable");

        let ready = readiness_snapshot("worker-1".to_owned(), [true; 4], Some(42));
        assert!(ready.is_ready());
        assert_eq!(ready.status, "ready");
        assert_eq!(ready.lifecycle, "serving");
        assert_eq!(ready.lease_expires_at_ms, Some(42));

        for unavailable_check in 0..4 {
            let checks = std::array::from_fn(|index| index != unavailable_check);
            let unavailable = readiness_snapshot("worker-1".to_owned(), checks, Some(42));
            assert!(!unavailable.is_ready());
            assert_eq!(unavailable.status, "not_ready");
            if unavailable_check == 0 {
                assert_eq!(unavailable.lifecycle, "draining");
            }
        }
    }
}
