use super::session_data::{send_canonical_events, send_metadata};
use super::{
    Arc, HarnessError, LiveServerFrame, LocalApplication, LocalEventNotification,
    LocalInvalidationNotification, SessionId, SessionLiveDirty, SessionLiveReadMask, broadcast,
    mpsc,
};

pub(super) async fn serve_subscription(
    application: Arc<LocalApplication>,
    outgoing: mpsc::Sender<LiveServerFrame>,
    subscription_id: u64,
    session_id: SessionId,
    mut after_seq: Option<u64>,
    metadata_mask: SessionLiveReadMask,
) -> Result<(), HarnessError> {
    let mut events = application.subscribe_events();
    let mut invalidations = application.subscribe_invalidations();
    let reset = after_seq.is_none();

    send_canonical_events(
        application.as_ref(),
        &outgoing,
        subscription_id,
        &session_id,
        &mut after_seq,
        reset,
        true,
    )
    .await?;
    if !metadata_mask.is_empty() {
        send_metadata(
            application.as_ref(),
            &outgoing,
            subscription_id,
            &session_id,
            metadata_mask,
        )
        .await?;
    }

    while let Some(dirty) =
        next_dirty(&session_id, metadata_mask, &mut events, &mut invalidations).await
    {
        if dirty.events {
            send_canonical_events(
                application.as_ref(),
                &outgoing,
                subscription_id,
                &session_id,
                &mut after_seq,
                false,
                false,
            )
            .await?;
        }
        let read = intersect_read_mask(metadata_mask, dirty.metadata());
        if !read.is_empty() {
            send_metadata(
                application.as_ref(),
                &outgoing,
                subscription_id,
                &session_id,
                read,
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn next_dirty(
    session_id: &SessionId,
    metadata_mask: SessionLiveReadMask,
    events: &mut broadcast::Receiver<LocalEventNotification>,
    invalidations: &mut broadcast::Receiver<LocalInvalidationNotification>,
) -> Option<SessionLiveDirty> {
    loop {
        let mut dirty = SessionLiveDirty::default();
        let closed = tokio::select! {
            event = events.recv() => merge_event_result(event, session_id, metadata_mask, &mut dirty),
            invalidation = invalidations.recv() => {
                merge_invalidation_result(invalidation, session_id, metadata_mask, &mut dirty)
            }
        };
        if closed {
            return None;
        }
        if drain_dirty(session_id, metadata_mask, events, invalidations, &mut dirty) {
            return None;
        }
        if !dirty.is_empty() {
            return Some(dirty);
        }
    }
}

pub(super) fn merge_event_result(
    result: Result<LocalEventNotification, broadcast::error::RecvError>,
    session_id: &SessionId,
    metadata_mask: SessionLiveReadMask,
    dirty: &mut SessionLiveDirty,
) -> bool {
    match result {
        Ok(notification) if notification.session_id == session_id.as_str() => {
            dirty.merge(notification.session_dirty());
            false
        }
        Ok(_) => false,
        Err(broadcast::error::RecvError::Lagged(_)) => {
            dirty.merge(dirty_for_recovery(metadata_mask, true));
            false
        }
        Err(broadcast::error::RecvError::Closed) => true,
    }
}

pub(super) fn merge_invalidation_result(
    result: Result<LocalInvalidationNotification, broadcast::error::RecvError>,
    session_id: &SessionId,
    metadata_mask: SessionLiveReadMask,
    dirty: &mut SessionLiveDirty,
) -> bool {
    match result {
        Ok(notification)
            if notification
                .session_id
                .as_deref()
                .is_none_or(|target| target == session_id.as_str()) =>
        {
            if let Some(notification_dirty) = notification.category.session_dirty() {
                dirty.merge(notification_dirty);
            }
            false
        }
        Ok(_) => false,
        Err(broadcast::error::RecvError::Lagged(_)) => {
            dirty.merge(dirty_for_recovery(metadata_mask, false));
            false
        }
        Err(broadcast::error::RecvError::Closed) => true,
    }
}

pub(super) fn drain_dirty(
    session_id: &SessionId,
    metadata_mask: SessionLiveReadMask,
    events: &mut broadcast::Receiver<LocalEventNotification>,
    invalidations: &mut broadcast::Receiver<LocalInvalidationNotification>,
    dirty: &mut SessionLiveDirty,
) -> bool {
    loop {
        match events.try_recv() {
            Ok(notification) if notification.session_id == session_id.as_str() => {
                dirty.merge(notification.session_dirty());
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                dirty.merge(dirty_for_recovery(metadata_mask, true));
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => return true,
        }
    }
    loop {
        match invalidations.try_recv() {
            Ok(notification)
                if notification
                    .session_id
                    .as_deref()
                    .is_none_or(|target| target == session_id.as_str()) =>
            {
                if let Some(notification_dirty) = notification.category.session_dirty() {
                    dirty.merge(notification_dirty);
                }
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                dirty.merge(dirty_for_recovery(metadata_mask, false));
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => return true,
        }
    }
    false
}

pub(super) fn dirty_for_recovery(read: SessionLiveReadMask, events: bool) -> SessionLiveDirty {
    SessionLiveDirty {
        events,
        inbox: read.inbox,
        stats: read.stats,
        projection: read.projection,
        questions: read.questions,
        profile: read.profile,
        agent_team: read.agent_team,
    }
}

pub(super) fn intersect_read_mask(
    subscribed: SessionLiveReadMask,
    dirty: SessionLiveReadMask,
) -> SessionLiveReadMask {
    SessionLiveReadMask {
        inbox: subscribed.inbox && dirty.inbox,
        stats: subscribed.stats && dirty.stats,
        projection: subscribed.projection && dirty.projection,
        questions: subscribed.questions && dirty.questions,
        profile: subscribed.profile && dirty.profile,
        agent_team: subscribed.agent_team && dirty.agent_team,
    }
}
