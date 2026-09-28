use super::{
    Arc, BTreeMap, Duration, EventBatch, ExecutorFrame, ExecutorLiveInvalidation, ExecutorScope,
    HarnessError, LocalApplication, LocalEventNotification, LocalInvalidationCategory,
    LocalInvalidationNotification, SessionCursor, SessionId, broadcast, control_redaction, mpsc,
    now_ms,
};

pub(super) async fn pump_heartbeats(
    application: Arc<LocalApplication>,
    outgoing: mpsc::Sender<ExecutorFrame>,
    interval_ms: u64,
) {
    let interval = Duration::from_millis(interval_ms.clamp(1_000, 60_000));
    loop {
        tokio::time::sleep(interval).await;
        let sessions = application
            .snapshot()
            .await
            .sessions
            .into_iter()
            .map(|session| session.identity.session_id)
            .collect();
        let Ok(sent_at_ms) = now_ms() else {
            return;
        };
        if outgoing
            .send(ExecutorFrame::Heartbeat {
                sent_at_ms,
                active_sessions: sessions,
            })
            .await
            .is_err()
        {
            return;
        }
    }
}

pub(super) async fn pump_events(
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    initial_cursors: Vec<SessionCursor>,
    outgoing: mpsc::Sender<ExecutorFrame>,
    acknowledgements: mpsc::Receiver<SessionCursor>,
) {
    let notifications = application.subscribe_events();
    pump_events_from_notifications(
        application,
        scope,
        initial_cursors,
        outgoing,
        notifications,
        acknowledgements,
    )
    .await;
}

pub(super) async fn pump_invalidations(
    application: Arc<LocalApplication>,
    outgoing: mpsc::Sender<ExecutorFrame>,
) {
    let notifications = application.subscribe_invalidations();
    pump_invalidations_from_notifications(application, outgoing, notifications).await;
}

pub(super) async fn pump_invalidations_from_notifications(
    application: Arc<LocalApplication>,
    outgoing: mpsc::Sender<ExecutorFrame>,
    mut notifications: broadcast::Receiver<LocalInvalidationNotification>,
) {
    loop {
        let notification = match notifications.recv().await {
            Ok(notification) => notification,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                if send_invalidation_recovery(application.as_ref(), &outgoing)
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if notification.category == LocalInvalidationCategory::Events {
            continue;
        }
        let session_id = notification.session_id.map(SessionId::new);
        let activity = if notification.category == LocalInvalidationCategory::Activity {
            match session_id.as_ref() {
                Some(id) => application.live_activity(id.as_str()).await,
                None => None,
            }
        } else {
            None
        };
        let invalidation = ExecutorLiveInvalidation {
            session_id,
            dirty: notification.category.session_dirty().unwrap_or_default(),
            workbench: notification.category == LocalInvalidationCategory::Workbench,
            activity,
        };
        if outgoing
            .send(ExecutorFrame::LiveInvalidation { invalidation })
            .await
            .is_err()
        {
            return;
        }
    }
}

pub(super) async fn send_invalidation_recovery(
    application: &LocalApplication,
    outgoing: &mpsc::Sender<ExecutorFrame>,
) -> Result<(), ()> {
    outgoing
        .send(ExecutorFrame::LiveInvalidation {
            invalidation: ExecutorLiveInvalidation {
                session_id: None,
                dirty: ternilo_protocol::SessionLiveDirty::default(),
                workbench: true,
                activity: None,
            },
        })
        .await
        .map_err(|_| ())?;
    for activity in application.live_activities().await {
        outgoing
            .send(ExecutorFrame::LiveInvalidation {
                invalidation: ExecutorLiveInvalidation {
                    session_id: Some(activity.session_id.clone()),
                    dirty: ternilo_protocol::SessionLiveDirty {
                        events: true,
                        inbox: true,
                        stats: true,
                        projection: true,
                        questions: true,
                        profile: true,
                        agent_team: true,
                    },
                    workbench: false,
                    activity: Some(activity),
                },
            })
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

pub(super) async fn pump_events_from_notifications(
    application: Arc<LocalApplication>,
    scope: ExecutorScope,
    initial_cursors: Vec<SessionCursor>,
    outgoing: mpsc::Sender<ExecutorFrame>,
    mut notifications: broadcast::Receiver<LocalEventNotification>,
    mut acknowledgements: mpsc::Receiver<SessionCursor>,
) {
    let mut cursors = initial_cursors
        .into_iter()
        .map(|cursor| (cursor.session_id, cursor.last_seq))
        .collect::<BTreeMap<_, _>>();
    let mut workspace_paths = BTreeMap::new();
    if let Err(error) = catch_up_all_events(
        application.as_ref(),
        &scope,
        &outgoing,
        &mut cursors,
        &mut workspace_paths,
        &mut acknowledgements,
    )
    .await
    {
        eprintln!("synchronize session events: {error}");
        return;
    }
    loop {
        let result = match notifications.recv().await {
            Ok(notification) => {
                let session_id = SessionId::new(notification.session_id);
                if let Some(workspace_path) = workspace_paths.get(&session_id) {
                    catch_up_session_events(
                        application.as_ref(),
                        &scope,
                        &outgoing,
                        &mut cursors,
                        &session_id,
                        workspace_path,
                        &mut acknowledgements,
                    )
                    .await
                } else {
                    catch_up_all_events(
                        application.as_ref(),
                        &scope,
                        &outgoing,
                        &mut cursors,
                        &mut workspace_paths,
                        &mut acknowledgements,
                    )
                    .await
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                catch_up_all_events(
                    application.as_ref(),
                    &scope,
                    &outgoing,
                    &mut cursors,
                    &mut workspace_paths,
                    &mut acknowledgements,
                )
                .await
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if let Err(error) = result {
            eprintln!("synchronize session events: {error}");
            return;
        }
    }
}

pub(super) async fn catch_up_all_events(
    application: &LocalApplication,
    scope: &ExecutorScope,
    outgoing: &mpsc::Sender<ExecutorFrame>,
    cursors: &mut BTreeMap<SessionId, Option<u64>>,
    workspace_paths: &mut BTreeMap<SessionId, String>,
    acknowledgements: &mut mpsc::Receiver<SessionCursor>,
) -> Result<(), HarnessError> {
    let snapshot = application.snapshot().await;
    workspace_paths.clear();
    for session in snapshot.sessions {
        let session_id = session.identity.session_id;
        workspace_paths.insert(session_id.clone(), session.workspace_path.clone());
        catch_up_session_events(
            application,
            scope,
            outgoing,
            cursors,
            &session_id,
            &session.workspace_path,
            acknowledgements,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn catch_up_session_events(
    application: &LocalApplication,
    scope: &ExecutorScope,
    outgoing: &mpsc::Sender<ExecutorFrame>,
    cursors: &mut BTreeMap<SessionId, Option<u64>>,
    session_id: &SessionId,
    workspace_path: &str,
    acknowledgements: &mut mpsc::Receiver<SessionCursor>,
) -> Result<(), HarnessError> {
    let after_seq = cursors.get(session_id).copied().flatten();
    let Ok(mut events) = application
        .events_after(session_id.as_str(), after_seq)
        .await
    else {
        return Ok(());
    };
    if events.is_empty() {
        return Ok(());
    }
    control_redaction::events(&mut events, workspace_path)?;
    let last_seq = events.last().map(|event| event.seq);
    let batch = EventBatch {
        scope: scope.clone(),
        session_id: session_id.clone(),
        after_seq,
        events,
    };
    batch.validate()?;
    outgoing
        .send(ExecutorFrame::EventBatch { batch })
        .await
        .map_err(|_| HarnessError::execution("gateway event channel closed"))?;
    let acknowledged = tokio::time::timeout(Duration::from_secs(30), acknowledgements.recv())
        .await
        .map_err(|_| HarnessError::execution("gateway did not acknowledge the event delta"))?
        .ok_or_else(|| HarnessError::execution("gateway event acknowledgement channel closed"))?;
    if acknowledged.session_id != *session_id
        || acknowledged
            .last_seq
            .is_some_and(|sequence| Some(sequence) < last_seq)
    {
        return Err(HarnessError::invalid(
            "gateway acknowledged an unexpected event cursor",
        ));
    }
    cursors.insert(session_id.clone(), acknowledged.last_seq);
    Ok(())
}
