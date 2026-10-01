use super::{
    AppState, ApplicationOperation, BTreeMap, CloudLiveNotification, CloudSessionRecord,
    CloudSessionState, ControlUser, EdgeLiveNotification, EdgeSessionRecord, ExecutorId,
    HarnessError, LiveServerFrame, SessionId, SessionLiveActivity, TenantId, WebSocket,
    load_state_filtered, send_frame,
};

pub(super) async fn handle_notification(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    notification: CloudLiveNotification,
    socket: &mut WebSocket,
    workbench_revision: &mut u64,
    online_only: bool,
) -> Result<(), HarnessError> {
    match notification {
        CloudLiveNotification::Session {
            tenant_id: notified_tenant,
            user_id: _,
            session_id,
            activity,
            workbench,
            ..
        } if notified_tenant == *tenant_id => {
            if (activity || workbench)
                && let Some(session_id) = session_id.as_ref()
                && let Some(activity) = load_activity(state, actor, tenant_id, session_id).await?
            {
                send_frame(socket, &LiveServerFrame::Activity { activity }).await?;
            }
            if workbench {
                send_workbench(
                    state,
                    actor,
                    tenant_id,
                    socket,
                    workbench_revision,
                    online_only,
                )
                .await?;
            }
        }
        CloudLiveNotification::Workbench {
            tenant_id: notified_tenant,
            user_id: _,
        } if notified_tenant == *tenant_id => {
            send_workbench(
                state,
                actor,
                tenant_id,
                socket,
                workbench_revision,
                online_only,
            )
            .await?;
        }
        CloudLiveNotification::Rescan => {
            send_workbench(
                state,
                actor,
                tenant_id,
                socket,
                workbench_revision,
                online_only,
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn handle_edge_notification(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    notification: EdgeLiveNotification,
    socket: &mut WebSocket,
    workbench_revision: &mut u64,
    online_only: bool,
) -> Result<(), HarnessError> {
    match notification {
        EdgeLiveNotification::ResourcesChanged {
            tenant_id: notified_tenant,
        }
        | EdgeLiveNotification::Rescan {
            tenant_id: notified_tenant,
            ..
        } if notified_tenant == *tenant_id => {
            send_workbench(
                state,
                actor,
                tenant_id,
                socket,
                workbench_revision,
                online_only,
            )
            .await?;
        }
        EdgeLiveNotification::Session {
            tenant_id: notified_tenant,
            executor_id,
            node_session_id,
            workbench,
            activity,
            ..
        } if notified_tenant == *tenant_id => {
            if let Some(activity) = activity
                && (!online_only || state.edge.is_connected(tenant_id, &executor_id).await)
                && let Some(mapping) = state
                    .store
                    .list_accessible_edge_sessions_on_executors(
                        actor,
                        tenant_id,
                        std::slice::from_ref(&executor_id),
                    )
                    .await?
                    .into_iter()
                    .find(|mapping| {
                        mapping.executor_id == executor_id
                            && mapping.node_session_id == activity.session_id
                            && node_session_id
                                .as_ref()
                                .is_none_or(|session_id| session_id == &mapping.node_session_id)
                    })
            {
                send_frame(
                    socket,
                    &LiveServerFrame::Activity {
                        activity: SessionLiveActivity {
                            session_id: mapping.session_id,
                            running: activity.running,
                            execution: activity.running.then_some(activity.execution).flatten(),
                            updated_at_ms: activity.updated_at_ms,
                        },
                    },
                )
                .await?;
            }
            if workbench {
                send_workbench(
                    state,
                    actor,
                    tenant_id,
                    socket,
                    workbench_revision,
                    online_only,
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn send_workbench(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    socket: &mut WebSocket,
    revision: &mut u64,
    online_only: bool,
) -> Result<(), HarnessError> {
    let (workbench, activity) = load_workbench(state, actor, tenant_id, online_only).await?;
    *revision = revision.saturating_add(1);
    send_frame(
        socket,
        &LiveServerFrame::Workbench {
            revision: *revision,
            state: workbench,
            activity,
        },
    )
    .await
}

pub(super) async fn load_workbench(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    online_only: bool,
) -> Result<(serde_json::Value, Vec<SessionLiveActivity>), HarnessError> {
    let workbench =
        serde_json::to_value(load_state_filtered(state, actor, tenant_id, online_only).await?)
            .map_err(|error| HarnessError::execution(format!("encode live Workbench: {error}")))?;
    let mut activity = state
        .cloud
        .list_accessible_sessions(tenant_id, &actor.user_id, 500)
        .await?
        .into_iter()
        .map(session_activity)
        .collect::<Vec<_>>();
    activity.extend(load_edge_activities(state, actor, tenant_id, online_only).await?);
    Ok((workbench, activity))
}

pub(super) async fn load_edge_activities(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    online_only: bool,
) -> Result<Vec<SessionLiveActivity>, HarnessError> {
    let mappings = if online_only {
        let ids = state.edge.online_executor_ids(tenant_id).await?;
        state
            .store
            .list_accessible_edge_sessions_on_executors(actor, tenant_id, &ids)
            .await?
    } else {
        state
            .store
            .list_accessible_edge_sessions(actor, tenant_id)
            .await?
    };
    let mut by_executor = BTreeMap::<ExecutorId, Vec<&EdgeSessionRecord>>::new();
    let mut activity = mappings
        .iter()
        .map(|mapping| {
            (
                mapping.session_id.clone(),
                SessionLiveActivity {
                    session_id: mapping.session_id.clone(),
                    running: false,
                    execution: None,
                    updated_at_ms: mapping.updated_at_ms,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for mapping in &mappings {
        by_executor
            .entry(mapping.executor_id.clone())
            .or_default()
            .push(mapping);
    }
    for (executor_id, executor_mappings) in by_executor {
        if !state.edge.is_connected(tenant_id, &executor_id).await {
            continue;
        }
        let Ok(values) = state
            .edge
            .call(
                tenant_id,
                &executor_id,
                ApplicationOperation::LiveActivities,
            )
            .await
        else {
            continue;
        };
        let node_activity =
            serde_json::from_value::<Vec<SessionLiveActivity>>(values).map_err(|error| {
                HarnessError::execution(format!("decode Node live activities: {error}"))
            })?;
        for item in node_activity {
            item.session_id.validate()?;
            if let Some(mapping) = executor_mappings
                .iter()
                .find(|mapping| mapping.node_session_id == item.session_id)
            {
                activity.insert(
                    mapping.session_id.clone(),
                    SessionLiveActivity {
                        session_id: mapping.session_id.clone(),
                        running: item.running,
                        execution: item.running.then_some(item.execution).flatten(),
                        updated_at_ms: item.updated_at_ms,
                    },
                );
            }
        }
    }
    Ok(activity.into_values().collect())
}

pub(super) async fn load_activity(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    session_id: &SessionId,
) -> Result<Option<SessionLiveActivity>, HarnessError> {
    Ok(state
        .cloud
        .find_accessible_session(tenant_id, &actor.user_id, session_id)
        .await?
        .map(session_activity))
}

pub(super) fn session_activity(session: CloudSessionRecord) -> SessionLiveActivity {
    SessionLiveActivity {
        session_id: session.session_id,
        running: matches!(
            session.state,
            CloudSessionState::Queued | CloudSessionState::Running
        ),
        execution: matches!(
            session.state,
            CloudSessionState::Queued | CloudSessionState::Running
        )
        .then_some(session.execution)
        .flatten(),
        updated_at_ms: session.updated_at_ms,
    }
}
