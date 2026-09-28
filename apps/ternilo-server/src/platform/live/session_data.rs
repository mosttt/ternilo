use super::{
    AppState, CloudAdapter, ControlUser, EVENT_BATCH_LIMIT, EdgeAdapter, EdgeSessionRecord,
    HarnessError, LivePendingQuestion, LiveServerFrame, SessionEvent, SessionId,
    SessionLiveMetadata, SessionLiveReadMask, TenantId, error_frame, mpsc,
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_edge_event_batches(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription_id: u64,
    session: &EdgeSessionRecord,
    cursor: &mut Option<u64>,
    refresh: bool,
    force_empty: bool,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    let adapter = EdgeAdapter::new(state, actor, tenant_id);
    let mut reset = force_empty && cursor.is_none();
    let mut events = if refresh {
        let connected = state
            .edge
            .is_connected(tenant_id, &session.executor_id)
            .await;
        let mut events = adapter.events(session).await?;
        if let Some(after) = *cursor {
            if events.last().is_some_and(|event| event.seq < after)
                || (events.is_empty() && connected)
            {
                *cursor = None;
                reset = true;
            } else {
                events.retain(|event| event.seq > after);
            }
        }
        events
    } else {
        adapter.live_event_delta(session, *cursor).await?
    };

    if validate_event_page(*cursor, &events).is_err() {
        events = adapter.events(session).await?;
        *cursor = None;
        reset = true;
        validate_event_page(None, &events).map_err(|()| {
            HarnessError::conflict("Edge Session journal has a non-contiguous live cursor")
        })?;
    }
    if events.is_empty() {
        if force_empty || reset {
            outgoing
                .send(LiveServerFrame::EventBatch {
                    subscription_id,
                    session_id: session.session_id.clone(),
                    reset,
                    complete: true,
                    events,
                    next_seq: next_sequence(*cursor),
                })
                .await
                .map_err(|_| HarnessError::cancelled("live connection closed"))?;
        }
        return Ok(());
    }

    let chunks = events.chunks(EVENT_BATCH_LIMIT as usize);
    let chunk_count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let events = chunk.to_vec();
        *cursor = events.last().map(|event| event.seq);
        outgoing
            .send(LiveServerFrame::EventBatch {
                subscription_id,
                session_id: session.session_id.clone(),
                reset: reset && index == 0,
                complete: index + 1 == chunk_count,
                next_seq: next_sequence(*cursor),
                events,
            })
            .await
            .map_err(|_| HarnessError::cancelled("live connection closed"))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_edge_metadata(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    subscription_id: u64,
    session: &EdgeSessionRecord,
    reads: SessionLiveReadMask,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    if !state
        .edge
        .is_connected(tenant_id, &session.executor_id)
        .await
    {
        return Ok(());
    }
    let frame = match EdgeAdapter::new(state, actor, tenant_id)
        .live_metadata(session, reads)
        .await
    {
        Ok(metadata) => LiveServerFrame::SessionMetadata {
            subscription_id,
            session_id: session.session_id.clone(),
            metadata,
        },
        Err(error) => error_frame(Some(subscription_id), error),
    };
    outgoing
        .send(frame)
        .await
        .map_err(|_| HarnessError::cancelled("live connection closed"))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_event_batches(
    state: &AppState,
    tenant_id: &TenantId,
    subscription_id: u64,
    session_id: &SessionId,
    cursor: &mut Option<u64>,
    mut reset: bool,
    force_empty: bool,
    outgoing: &mpsc::Sender<LiveServerFrame>,
) -> Result<(), HarnessError> {
    let mut sent = false;
    let mut repaired_gap = false;
    loop {
        let events = state
            .cloud
            .session_events(tenant_id, session_id, *cursor, EVENT_BATCH_LIMIT)
            .await?;
        if events.is_empty() {
            if force_empty || sent {
                outgoing
                    .send(LiveServerFrame::EventBatch {
                        subscription_id,
                        session_id: session_id.clone(),
                        reset: reset && !sent,
                        complete: true,
                        events,
                        next_seq: next_sequence(*cursor),
                    })
                    .await
                    .map_err(|_| HarnessError::cancelled("live connection closed"))?;
            }
            return Ok(());
        }
        if validate_event_page(*cursor, &events).is_err() {
            if repaired_gap {
                return Err(HarnessError::conflict(
                    "cloud session journal has a non-contiguous live cursor",
                ));
            }
            *cursor = None;
            reset = true;
            sent = false;
            repaired_gap = true;
            continue;
        }
        *cursor = events.last().map(|event| event.seq);
        let complete = events.len() < EVENT_BATCH_LIMIT as usize;
        outgoing
            .send(LiveServerFrame::EventBatch {
                subscription_id,
                session_id: session_id.clone(),
                reset: reset && !sent,
                complete,
                next_seq: next_sequence(*cursor),
                events,
            })
            .await
            .map_err(|_| HarnessError::cancelled("live connection closed"))?;
        sent = true;
        if complete {
            return Ok(());
        }
    }
}

pub(super) async fn read_metadata(
    state: &AppState,
    actor: &ControlUser,
    tenant_id: &TenantId,
    session_id: &SessionId,
    read: SessionLiveReadMask,
) -> Result<SessionLiveMetadata, HarnessError> {
    let session = state
        .cloud
        .find_accessible_session(tenant_id, &actor.user_id, session_id)
        .await?
        .ok_or_else(|| HarnessError::invalid("cloud session does not exist"))?;
    let adapter = CloudAdapter::new(state, actor, tenant_id);
    let plan = metadata_read_plan(read);
    let events = if plan.event_reads == 1 {
        Some(adapter.events(session_id).await?)
    } else {
        None
    };
    let profile = if plan.profile_reads == 1 {
        Some(CloudAdapter::profile(&session)?)
    } else {
        None
    };
    let mut metadata = SessionLiveMetadata {
        read,
        ..SessionLiveMetadata::default()
    };
    if read.inbox {
        metadata.inbox = Some(adapter.inbox(&session).await?);
    }
    if read.stats {
        metadata.stats = Some(CloudAdapter::stats_from_events(
            events.as_deref().expect("metadata plan loads stats events"),
        )?);
    }
    if read.projection {
        metadata.projection = Some(
            adapter.projection_from_events(
                &session,
                events
                    .as_deref()
                    .expect("metadata plan loads projection events"),
                profile
                    .as_ref()
                    .expect("metadata plan loads projection profile")
                    .clone(),
            )?,
        );
    }
    if read.questions {
        metadata.questions = Some(
            state
                .cloud
                .pending_questions(tenant_id, &actor.user_id, session_id)
                .await?
                .into_iter()
                .map(|question| LivePendingQuestion {
                    session_id: question.session_id,
                    question: question.question,
                })
                .collect(),
        );
    }
    if read.profile {
        metadata.profile = profile;
    }
    if read.agent_team {
        metadata.agent_team = Some(
            state
                .cloud
                .agent_team_snapshot(tenant_id, &actor.user_id, session_id)
                .await?,
        );
    }
    Ok(metadata)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MetadataReadPlan {
    pub(super) event_reads: u8,
    pub(super) profile_reads: u8,
}

pub(super) const fn metadata_read_plan(read: SessionLiveReadMask) -> MetadataReadPlan {
    MetadataReadPlan {
        event_reads: if read.stats || read.projection { 1 } else { 0 },
        profile_reads: if read.projection || read.profile {
            1
        } else {
            0
        },
    }
}

pub(super) fn validate_event_page(
    after_seq: Option<u64>,
    events: &[SessionEvent],
) -> Result<(), ()> {
    let mut expected = next_sequence(after_seq);
    for event in events {
        if event.seq != expected {
            return Err(());
        }
        expected = expected.saturating_add(1);
    }
    Ok(())
}

pub(super) const fn next_sequence(after_seq: Option<u64>) -> u64 {
    match after_seq {
        Some(sequence) => sequence.saturating_add(1),
        None => 0,
    }
}
