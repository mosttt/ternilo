use super::session_data::{
    read_metadata, send_edge_event_batches, send_edge_metadata, send_event_batches,
};
use super::{
    AppState, CloudLiveNotification, CloudSessionRecord, ControlUser, EdgeLiveNotification,
    EdgeSessionRecord, HarnessError, LiveServerFrame, PlacementResolver, SessionId,
    SessionLiveDirty, SessionLiveReadMask, SessionTarget, TenantId, broadcast, error_frame, mpsc,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_subscription(
    state: AppState,
    actor: ControlUser,
    tenant_id: TenantId,
    subscription_id: u64,
    session_id: SessionId,
    after_seq: Option<u64>,
    reads: SessionLiveReadMask,
    outgoing: mpsc::Sender<LiveServerFrame>,
) {
    if let Err(error) = subscription_loop(
        &state,
        &actor,
        &tenant_id,
        subscription_id,
        &session_id,
        after_seq,
        reads,
        &outgoing,
    )
    .await
    {
        let _ = outgoing
            .send(error_frame(Some(subscription_id), error))
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn subscription_loop(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription_id: u64,
    session_id: &SessionId,
    after_seq: Option<u64>,
    reads: SessionLiveReadMask,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    let target = PlacementResolver::new(state, actor, tenant_id)
        .session(session_id)
        .await?;
    match target {
        SessionTarget::Cloud(session) => {
            cloud_subscription_loop(
                state,
                actor,
                tenant_id,
                subscription_id,
                session_id,
                session,
                after_seq,
                reads,
                outgoing,
            )
            .await
        }
        SessionTarget::Edge(session) => {
            edge_subscription_loop(
                state,
                actor,
                tenant_id,
                subscription_id,
                session,
                after_seq,
                reads,
                outgoing,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn cloud_subscription_loop(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription_id: u64,
    session_id: &SessionId,
    session: CloudSessionRecord,
    after_seq: Option<u64>,
    reads: SessionLiveReadMask,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    // Subscribe before baseline reads; any racing commit remains queued for
    // canonical repair from PostgreSQL.
    let mut notifications = state.cloud_events.subscribe();

    let mut cursor =
        if after_seq.is_some_and(|after| session.last_seq.is_none_or(|last| after > last)) {
            None
        } else {
            after_seq
        };
    let reset = cursor != after_seq || after_seq.is_none();
    send_event_batches(
        state,
        tenant_id,
        subscription_id,
        session_id,
        &mut cursor,
        reset,
        true,
        outgoing,
    )
    .await?;
    if !reads.is_empty() {
        outgoing
            .send(LiveServerFrame::SessionMetadata {
                subscription_id,
                session_id: session_id.clone(),
                metadata: read_metadata(state, actor, tenant_id, session_id, reads).await?,
            })
            .await
            .map_err(|_| HarnessError::cancelled("live connection closed"))?;
    }

    loop {
        let notification = notifications.recv().await;
        let mut dirty = match notification {
            Ok(notification) => {
                matching_dirty(notification, tenant_id, &session.user_id, session_id)
            }
            Err(broadcast::error::RecvError::Lagged(_)) => Some(all_dirty()),
            Err(broadcast::error::RecvError::Closed) => {
                return Err(HarnessError::unavailable(
                    "cloud live notification feed closed",
                ));
            }
        };
        while let Ok(notification) = notifications.try_recv() {
            if let Some(next) =
                matching_dirty(notification, tenant_id, &session.user_id, session_id)
            {
                if let Some(current) = dirty.as_mut() {
                    current.merge(next);
                } else {
                    dirty = Some(next);
                }
            }
        }
        let Some(dirty) = dirty else { continue };
        if dirty.events {
            send_event_batches(
                state,
                tenant_id,
                subscription_id,
                session_id,
                &mut cursor,
                false,
                false,
                outgoing,
            )
            .await?;
        }
        let metadata_reads = intersect_reads(reads, dirty.metadata());
        if !metadata_reads.is_empty() {
            outgoing
                .send(LiveServerFrame::SessionMetadata {
                    subscription_id,
                    session_id: session_id.clone(),
                    metadata: read_metadata(state, actor, tenant_id, session_id, metadata_reads)
                        .await?,
                })
                .await
                .map_err(|_| HarnessError::cancelled("live connection closed"))?;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn edge_subscription_loop(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription_id: u64,
    session: EdgeSessionRecord,
    after_seq: Option<u64>,
    reads: SessionLiveReadMask,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    // Subscribe before the Node/cache baseline so reconnect and commit races
    // remain queued for a canonical Edge read.
    let mut notifications = state.edge.subscribe_live();
    let mut cursor = after_seq;
    send_edge_event_batches(
        state,
        actor,
        tenant_id,
        subscription_id,
        &session,
        &mut cursor,
        true,
        true,
        outgoing,
    )
    .await?;
    if !reads.is_empty() {
        send_edge_metadata(
            state,
            actor,
            tenant_id,
            subscription_id,
            &session,
            reads,
            outgoing,
        )
        .await?;
    }

    loop {
        let notification = notifications.recv().await;
        let (mut dirty, mut refresh_events) = match notification {
            Ok(notification) => matching_edge_dirty(notification, tenant_id, &session),
            Err(broadcast::error::RecvError::Lagged(_)) => Some((all_dirty(), true)),
            Err(broadcast::error::RecvError::Closed) => {
                return Err(HarnessError::unavailable(
                    "Edge live notification feed closed",
                ));
            }
        }
        .unwrap_or_default();
        while let Ok(notification) = notifications.try_recv() {
            if let Some((next, next_refresh)) =
                matching_edge_dirty(notification, tenant_id, &session)
            {
                dirty.merge(next);
                refresh_events |= next_refresh;
            }
        }
        if dirty.is_empty() && !refresh_events {
            continue;
        }
        if dirty.events {
            send_edge_event_batches(
                state,
                actor,
                tenant_id,
                subscription_id,
                &session,
                &mut cursor,
                refresh_events,
                false,
                outgoing,
            )
            .await?;
        }
        let metadata_reads = intersect_reads(reads, dirty.metadata());
        if !metadata_reads.is_empty() {
            send_edge_metadata(
                state,
                actor,
                tenant_id,
                subscription_id,
                &session,
                metadata_reads,
                outgoing,
            )
            .await?;
        }
    }
}

pub(super) fn matching_dirty(
    notification: CloudLiveNotification,
    tenant_id: &TenantId,
    user_id: &ternilo_protocol::UserId,
    session_id: &SessionId,
) -> Option<SessionLiveDirty> {
    match notification {
        CloudLiveNotification::Session {
            tenant_id: notified_tenant,
            user_id: notified_user,
            session_id: notified_session,
            dirty,
            ..
        } if notified_tenant == *tenant_id
            && notified_user
                .as_ref()
                .is_none_or(|notified| notified == user_id)
            && notified_session
                .as_ref()
                .is_none_or(|notified| notified == session_id) =>
        {
            Some(dirty)
        }
        CloudLiveNotification::Rescan => Some(all_dirty()),
        _ => None,
    }
}

pub(super) fn matching_edge_dirty(
    notification: EdgeLiveNotification,
    tenant_id: &TenantId,
    session: &EdgeSessionRecord,
) -> Option<(SessionLiveDirty, bool)> {
    match notification {
        EdgeLiveNotification::ResourcesChanged {
            tenant_id: notified_tenant,
        } if notified_tenant == *tenant_id => Some((all_dirty(), true)),
        EdgeLiveNotification::Session {
            tenant_id: notified_tenant,
            executor_id,
            node_session_id,
            dirty,
            refresh_events,
            ..
        } if notified_tenant == *tenant_id
            && executor_id == session.executor_id
            && node_session_id
                .as_ref()
                .is_none_or(|notified| notified == &session.node_session_id) =>
        {
            Some((dirty, refresh_events))
        }
        EdgeLiveNotification::Rescan {
            tenant_id: notified_tenant,
            executor_id,
        } if notified_tenant == *tenant_id && executor_id == session.executor_id => {
            Some((all_dirty(), true))
        }
        _ => None,
    }
}

pub(super) fn intersect_reads(
    requested: SessionLiveReadMask,
    dirty: SessionLiveReadMask,
) -> SessionLiveReadMask {
    SessionLiveReadMask {
        inbox: requested.inbox && dirty.inbox,
        stats: requested.stats && dirty.stats,
        projection: requested.projection && dirty.projection,
        questions: requested.questions && dirty.questions,
        profile: requested.profile && dirty.profile,
        agent_team: requested.agent_team && dirty.agent_team,
    }
}

pub(super) const fn all_dirty() -> SessionLiveDirty {
    SessionLiveDirty {
        events: true,
        inbox: true,
        stats: true,
        projection: true,
        questions: true,
        profile: true,
        agent_team: true,
    }
}
