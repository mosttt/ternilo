use std::sync::Arc;

use salvo_core::prelude::{Depot, Json, handler};
use ternilo_cloud::{CloudSessionEventFeed, CloudStore, WorkerPolicy};
use ternilo_control::{ControlStore, ControlUser};

use crate::platform::{edge::EdgeGateway, security::SecurityState};

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) store: ControlStore,
    pub(crate) cloud: CloudStore,
    pub(crate) cloud_events: CloudSessionEventFeed,
    pub(crate) edge: Arc<EdgeGateway>,
    pub(super) security: Arc<SecurityState>,
    pub(crate) setup_token_hash: Option<String>,
    pub(crate) managed_execution_enabled: bool,
    pub(crate) shutdown: tokio::sync::watch::Receiver<bool>,
    pub(crate) worker_policy: Arc<WorkerPolicy>,
    pub(crate) catalog: Arc<ternilo_kernel::Catalog>,
}

pub(crate) fn app_state(depot: &Depot) -> &AppState {
    depot
        .get_typed::<AppState>()
        .expect("application state middleware must run first")
}

pub(crate) fn actor(depot: &Depot) -> &ControlUser {
    depot
        .get_typed::<ControlUser>()
        .expect("user authentication middleware must run first")
}

#[handler]
pub(crate) fn me(depot: &mut Depot) -> Json<ControlUser> {
    Json(actor(depot).clone())
}
