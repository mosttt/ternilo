mod access;
mod config;
mod devices;
mod discovery;
pub(crate) use devices::public_router as device_router;
mod gateway;
mod grants;
mod groups;
mod maintenance;
mod reconciliation;
mod rotation;
mod traffic;
pub(crate) mod usage;

#[cfg(test)]
mod management_tests;

use salvo_core::prelude::Router;
use salvo_extra::size_limiter::max_size;

use super::{auth, identity::no_store};

pub(crate) fn administration_router() -> Router {
    Router::with_path("api/v1/admin/models")
        .hoop(auth::user_auth)
        .hoop(no_store)
        .hoop(max_size(256 * 1024))
        .push(config::providers_router())
        .push(config::publications_router())
        .push(groups::router())
        .push(grants::router())
        .push(
            Router::with_path("requests")
                .get(access::admin_requests)
                .push(
                    Router::with_path("{request_id}/reconciliations").get(reconciliation::records),
                )
                .push(
                    Router::with_path("{request_id}/attempts/{attempt}/reconcile")
                        .post(reconciliation::reconcile),
                ),
        )
        .push(Router::with_path("usage").get(access::admin_usage))
        .push(traffic::router())
}

pub(crate) fn access_router() -> Router {
    Router::with_path("api/v1/model-access")
        .hoop(auth::user_auth)
        .hoop(no_store)
        .hoop(max_size(16 * 1024))
        .push(Router::with_path("catalog").get(access::catalog))
        .push(devices::review_router())
        .push(devices::devices_router())
        .push(
            Router::with_path("keys")
                .get(access::keys)
                .post(access::create_key)
                .push(Router::with_path("{key_id}").delete(access::revoke_key)),
        )
        .push(Router::with_path("requests").get(access::requests))
        .push(Router::with_path("usage").get(access::user_usage))
        .push(Router::with_path("traffic").get(traffic::own))
}

pub(crate) use gateway::router as gateway_router;
pub(crate) use maintenance::start as start_maintenance;
