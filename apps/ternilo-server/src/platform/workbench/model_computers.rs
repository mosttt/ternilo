use crate::platform::{
    http::{ApiError, tenant_parameter},
    state::{actor, app_state},
};
use salvo_core::prelude::{Depot, Json, Request, handler};
use serde::Serialize;
use std::collections::BTreeMap;
use ternilo_control::{ControlAction, WorkspacePlacement};
use ternilo_protocol::{SessionId, WorkspaceId};
use ternilo_transport::ExecutorId;

#[derive(Serialize)]
pub(super) struct ModelComputer {
    executor_id: ExecutorId,
    name: String,
    connected: bool,
    can_configure: bool,
    workspace_id: Option<WorkspaceId>,
    session_id: Option<SessionId>,
}

#[handler]
pub(super) async fn list(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<Vec<ModelComputer>>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let state = app_state(depot);
    let user = actor(depot);
    let role = state
        .store
        .authorize(user, &tenant, ControlAction::TenantRead)
        .await?;
    let mut computers = BTreeMap::new();
    for record in state.store.list_owned_executors(user, &tenant).await? {
        if record.state == "revoked" {
            continue;
        }
        computers.insert(
            record.executor_id.clone(),
            ModelComputer {
                executor_id: record.executor_id,
                name: record.management.name,
                connected: false,
                can_configure: role.allows(ControlAction::RunReserve),
                workspace_id: None,
                session_id: None,
            },
        );
    }
    for workspace in state
        .store
        .list_accessible_workspaces(user, &tenant)
        .await?
    {
        if workspace.placement != WorkspacePlacement::LocalNode {
            continue;
        }
        if let Some(executor) = workspace.executor_id {
            computers.entry(executor.clone()).or_insert(ModelComputer {
                executor_id: executor,
                name: String::new(),
                connected: false,
                can_configure: false,
                workspace_id: Some(workspace.workspace_id),
                session_id: None,
            });
        }
    }
    for session in state
        .store
        .list_accessible_edge_sessions(user, &tenant)
        .await?
    {
        computers
            .entry(session.executor_id.clone())
            .or_insert(ModelComputer {
                executor_id: session.executor_id,
                name: String::new(),
                connected: false,
                can_configure: false,
                workspace_id: None,
                session_id: Some(session.session_id),
            });
    }
    let mut computers: Vec<_> = computers.into_values().collect();
    let names = state
        .store
        .computer_display_names(
            user,
            &tenant,
            &computers
                .iter()
                .map(|computer| computer.executor_id.clone())
                .collect::<Vec<_>>(),
        )
        .await?;
    for computer in &mut computers {
        if let Some(name) = names.get(&computer.executor_id) {
            computer.name.clone_from(name);
        }
        computer.connected = state
            .edge
            .is_connected(&tenant, &computer.executor_id)
            .await;
    }
    Ok(Json(computers))
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct UsageQuery {
    month: Option<String>,
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

#[handler]
pub(super) async fn usage(
    request: &mut Request,
    depot: &mut Depot,
) -> Result<Json<ternilo_control::ComputerProviderUsagePage>, ApiError> {
    let tenant = tenant_parameter(request)?;
    let executor = ExecutorId::new(crate::platform::http::path_parameter(
        request,
        "executor_id",
    )?);
    let query = request
        .parse_queries::<UsageQuery>()
        .map_err(crate::platform::http::invalid_request)?;
    let page = ternilo_control::PageQuery {
        query: query.query,
        cursor: query.cursor,
        limit: query
            .limit
            .unwrap_or_else(|| ternilo_control::PageQuery::default().limit),
    };
    Ok(Json(
        app_state(depot)
            .store
            .computer_provider_usage(
                actor(depot),
                &tenant,
                &executor,
                query.month.as_deref(),
                &page,
                crate::platform::http::now_ms()?,
            )
            .await?,
    ))
}
