use super::{
    EVENT_CHUNK_SIZE, HarnessError, LivePendingQuestion, LiveServerFrame, LocalApplication,
    SessionId, SessionLiveMetadata, SessionLiveReadMask, SessionProjectionSnapshot, SessionStats,
    mpsc,
};

pub(super) async fn send_canonical_events(
    application: &LocalApplication,
    outgoing: &mpsc::Sender<LiveServerFrame>,
    subscription_id: u64,
    session_id: &SessionId,
    after_seq: &mut Option<u64>,
    reset: bool,
    send_empty: bool,
) -> Result<(), HarnessError> {
    let events = application
        .events_after(session_id.as_str(), *after_seq)
        .await?;
    if events.is_empty() {
        if send_empty {
            outgoing
                .send(LiveServerFrame::EventBatch {
                    subscription_id,
                    session_id: session_id.clone(),
                    reset,
                    complete: true,
                    events: Vec::new(),
                    next_seq: (*after_seq).map_or(0, |sequence| sequence.saturating_add(1)),
                })
                .await
                .map_err(|_| HarnessError::execution("live connection closed"))?;
        }
        return Ok(());
    }

    let chunk_count = events.len().div_ceil(EVENT_CHUNK_SIZE);
    for (index, chunk) in events.chunks(EVENT_CHUNK_SIZE).enumerate() {
        let next_seq = chunk.last().map_or(0, |event| event.seq.saturating_add(1));
        outgoing
            .send(LiveServerFrame::EventBatch {
                subscription_id,
                session_id: session_id.clone(),
                reset: reset && index == 0,
                complete: index + 1 == chunk_count,
                events: chunk.to_vec(),
                next_seq,
            })
            .await
            .map_err(|_| HarnessError::execution("live connection closed"))?;
    }
    *after_seq = events.last().map(|event| event.seq);
    Ok(())
}

pub(super) async fn send_metadata(
    application: &LocalApplication,
    outgoing: &mpsc::Sender<LiveServerFrame>,
    subscription_id: u64,
    session_id: &SessionId,
    read: SessionLiveReadMask,
) -> Result<(), HarnessError> {
    let metadata = read_metadata(application, session_id, read).await?;
    outgoing
        .send(LiveServerFrame::SessionMetadata {
            subscription_id,
            session_id: session_id.clone(),
            metadata,
        })
        .await
        .map_err(|_| HarnessError::execution("live connection closed"))
}

pub(super) async fn read_metadata(
    application: &LocalApplication,
    session_id: &SessionId,
    read: SessionLiveReadMask,
) -> Result<SessionLiveMetadata, HarnessError> {
    let mut metadata = SessionLiveMetadata {
        read,
        ..SessionLiveMetadata::default()
    };
    if read.inbox {
        metadata.inbox = Some(application.session_inbox(session_id.as_str()).await?);
    }
    if read.stats || read.projection {
        let projection = application.session_projection(session_id.clone()).await?;
        if read.stats {
            metadata.stats = Some(stats_from_projection(&projection)?);
        }
        if read.projection {
            metadata.projection = Some(projection);
        }
    }
    if read.questions {
        metadata.questions = Some(
            application
                .pending_questions(Some(session_id.as_str()))
                .await
                .into_iter()
                .map(|pending| LivePendingQuestion {
                    session_id: pending.session_id,
                    question: pending.question,
                })
                .collect(),
        );
    }
    if read.profile {
        metadata.profile = Some(
            application
                .effective_session_profile(session_id.as_str())
                .await?,
        );
    }
    if read.agent_team {
        metadata.agent_team = Some(application.agent_team_snapshot(session_id.as_str()).await?);
    }
    Ok(metadata)
}

pub(super) fn stats_from_projection(
    projection: &SessionProjectionSnapshot,
) -> Result<SessionStats, HarnessError> {
    serde_json::from_value(
        projection
            .values
            .get("stats")
            .cloned()
            .ok_or_else(|| HarnessError::execution("stats projection is not registered"))?,
    )
    .map_err(|error| HarnessError::execution(format!("decode stats projection: {error}")))
}
