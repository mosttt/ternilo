use std::{collections::BTreeSet, time::Duration};

use futures_util::{StreamExt, stream};
use serde::Deserialize;
use ternilo_control::{ControlUser, NodeSessionResource, NodeWorkspaceResource};
use ternilo_protocol::{HarnessError, TenantId, WorkspaceId};
use ternilo_transport::{ApplicationOperation, ExecutorId};

use crate::platform::{http::now_ms, state::AppState};

use super::{EdgeAdapter, types::NodeSessionSnapshot};

#[derive(Deserialize)]
struct NodeSnapshot {
    workspaces: Vec<NodeWorkspace>,
    sessions: Vec<NodeSessionSnapshot>,
}

#[derive(Deserialize)]
struct NodeWorkspace {
    workspace_id: WorkspaceId,
    title: String,
}

pub(super) async fn discover(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
) -> Result<(), HarnessError> {
    let computers = state.store.list_owned_executors(user, tenant_id).await?;
    let mut discoveries = stream::iter(computers)
        .map(|computer| async move {
            if !state
                .edge
                .is_connected(tenant_id, &computer.executor_id)
                .await
            {
                return;
            }
            match tokio::time::timeout(
                Duration::from_secs(10),
                discover_computer(state, user, tenant_id, &computer.executor_id),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!(
                    "refresh resources from computer {}: {error}",
                    computer.executor_id
                ),
                Err(_) => eprintln!(
                    "resource refresh timed out for computer {}",
                    computer.executor_id
                ),
            }
        })
        .buffer_unordered(4);
    while discoveries.next().await.is_some() {}
    Ok(())
}

async fn discover_computer(
    state: &AppState,
    user: &ControlUser,
    tenant_id: &TenantId,
    executor_id: &ExecutorId,
) -> Result<(), HarnessError> {
    let resources = state.edge.lock_resources(tenant_id, executor_id).await;
    let value = state
        .edge
        .call_with_timeout(
            tenant_id,
            executor_id,
            ApplicationOperation::Snapshot,
            Duration::from_secs(5),
        )
        .await?;
    let snapshot: NodeSnapshot = serde_json::from_value(value).map_err(|error| {
        HarnessError::execution(format!("decode Node resource snapshot: {error}"))
    })?;
    let mut known = snapshot
        .workspaces
        .iter()
        .map(|workspace| workspace.workspace_id.clone())
        .collect::<BTreeSet<_>>();
    let mut workspaces = snapshot
        .workspaces
        .into_iter()
        .map(|workspace| NodeWorkspaceResource {
            workspace_id: workspace.workspace_id,
            title: workspace.title,
            registered: true,
        })
        .collect::<Vec<_>>();
    // Ungrouped sessions retain their immutable execution directory even when
    // the original sidebar workspace has been removed on the computer.
    for session in &snapshot.sessions {
        if known.insert(session.workspace_id.clone()) {
            workspaces.push(NodeWorkspaceResource {
                workspace_id: session.workspace_id.clone(),
                title: "Ungrouped".to_owned(),
                registered: false,
            });
        }
    }
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| NodeSessionResource {
            session_id: session.identity.session_id.clone(),
            workspace_id: session.workspace_id.clone(),
            metadata: session.metadata(session.parent_session_id.clone()),
        })
        .collect::<Vec<_>>();
    let imported = state
        .store
        .discover_owned_node_resources(
            user,
            tenant_id,
            executor_id,
            &workspaces,
            &sessions,
            now_ms()?,
        )
        .await?;
    drop(resources);
    if !imported.is_empty() {
        let mappings = state
            .store
            .list_owned_edge_sessions(user, tenant_id)
            .await?;
        let imported = imported.into_iter().collect::<BTreeSet<_>>();
        let adapter = EdgeAdapter::new(state, user, tenant_id);
        for mapping in mappings
            .iter()
            .filter(|mapping| imported.contains(&mapping.session_id))
        {
            // Initial events may have arrived before their resource mapping.
            // Fetch the canonical history once after installing that mapping.
            if let Err(error) = adapter.events(mapping).await {
                eprintln!("cache discovered session {}: {error}", mapping.session_id);
            }
        }
    }
    Ok(())
}
