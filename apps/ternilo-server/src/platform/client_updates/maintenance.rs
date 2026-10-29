use super::{
    ApiError, ClientUpdateInfo, Depot, ExecutorId, HarnessError, Json, Request, Router, TenantId,
    app_state, bearer_token, handler, invalid_request, now_ms,
};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, MAINTENANCE_POLL_MS, MAINTENANCE_PROTOCOL_VERSION,
    MaintenanceOperation, MaintenanceRequest, MaintenanceResponse,
};

pub(in crate::platform) fn router() -> Router {
    Router::with_path("api/v1/executors/maintenance/v1")
        .hoop(crate::platform::identity::no_store)
        .post(poll)
}
#[handler]
async fn poll(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<MaintenanceResponse>, ApiError> {
    let state = app_state(depot);
    let node = state
        .store
        .authenticate_node(bearer_token(request)?, now_ms()?)
        .await?;
    let body = request
        .parse_json::<MaintenanceRequest>()
        .await
        .map_err(invalid_request)?;
    let command = match body {
        MaintenanceRequest::Hello { hello } => {
            state
                .store
                .maintenance_hello(&node, &hello, now_ms()?)
                .await?;
            state
                .edge
                .notify_computer_changed(&node.scope.tenant_id, &node.executor_id);
            None
        }
        MaintenanceRequest::Poll {
            instance_nonce,
            info,
            reply,
        } => {
            let command = state
                .store
                .maintenance_poll(&node, &instance_nonce, &info, reply.as_ref(), now_ms()?)
                .await?;
            if reply.is_some() || info.progress.is_some() {
                state
                    .edge
                    .notify_computer_changed(&node.scope.tenant_id, &node.executor_id);
            }
            command
        }
    };
    Ok(Json(MaintenanceResponse {
        protocol_version: MAINTENANCE_PROTOCOL_VERSION,
        scope: node.scope,
        server_version: env!("CARGO_PKG_VERSION").into(),
        execution_protocol_version: EXECUTOR_PROTOCOL_VERSION,
        poll_after_ms: MAINTENANCE_POLL_MS,
        command,
    }))
}

pub(super) async fn call(
    state: &crate::platform::state::AppState,
    actor: &ternilo_control::ControlUser,
    tenant: &TenantId,
    executor: &ExecutorId,
    operation: MaintenanceOperation,
) -> Result<ClientUpdateInfo, HarnessError> {
    let id = state
        .store
        .enqueue_maintenance_command(actor, tenant, executor, operation, now_ms()?)
        .await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(reply) = state
            .store
            .take_maintenance_reply(tenant, executor, &id)
            .await?
        {
            return reply.info.ok_or_else(|| {
                HarnessError::unavailable(
                    reply
                        .error
                        .unwrap_or_else(|| "invalid maintenance response".into()),
                )
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(HarnessError::unavailable(
                "computer maintenance request timed out; check its connection and update status",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}
