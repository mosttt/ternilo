use super::{
    Arc, BTreeMap, HarnessError, LiveServerFrame, LocalApplication, LocalInvalidationCategory,
    SessionLiveActivity, broadcast, mpsc,
};

pub(super) async fn serve_workbench(
    application: Arc<LocalApplication>,
    outgoing: mpsc::Sender<LiveServerFrame>,
) {
    let mut events = application.subscribe_events();
    let mut invalidations = application.subscribe_invalidations();
    let mut revision = 1_u64;
    let Ok((state, activity)) = read_workbench(application.as_ref()).await else {
        return;
    };
    let mut last_activity = activity
        .iter()
        .cloned()
        .map(|activity| (activity.session_id.as_str().to_owned(), activity))
        .collect::<BTreeMap<_, _>>();
    if outgoing
        .send(LiveServerFrame::Workbench {
            revision,
            state,
            activity,
        })
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.updates_workbench() => {
                    if send_activity_if_changed(
                        application.as_ref(),
                        &event.session_id,
                        &outgoing,
                        &mut last_activity,
                    ).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    revision = revision.saturating_add(1);
                    match publish_workbench(
                        application.as_ref(),
                        &outgoing,
                        revision,
                        &mut last_activity,
                    ).await {
                        Ok(()) => {}
                        Err(_) => return,
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            invalidation = invalidations.recv() => match invalidation {
                Ok(invalidation) => match invalidation.category {
                    LocalInvalidationCategory::Workbench => {
                        revision = revision.saturating_add(1);
                        if publish_workbench(
                            application.as_ref(),
                            &outgoing,
                            revision,
                            &mut last_activity,
                        ).await.is_err() {
                            return;
                        }
                    }
                    LocalInvalidationCategory::Activity => {
                        if let Some(session_id) = invalidation.session_id
                            && send_activity_if_changed(
                                application.as_ref(),
                                &session_id,
                                &outgoing,
                                &mut last_activity,
                            ).await.is_err()
                        {
                            return;
                        }
                    }
                    _ => {}
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    revision = revision.saturating_add(1);
                    if publish_workbench(
                        application.as_ref(),
                        &outgoing,
                        revision,
                        &mut last_activity,
                    ).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

pub(super) async fn read_workbench(
    application: &LocalApplication,
) -> Result<(serde_json::Value, Vec<SessionLiveActivity>), HarnessError> {
    let state = serde_json::to_value(application.snapshot().await)
        .map_err(|error| HarnessError::execution(format!("serialize Local workbench: {error}")))?;
    let activity = application.live_activities().await;
    Ok((state, activity))
}

pub(super) async fn publish_workbench(
    application: &LocalApplication,
    outgoing: &mpsc::Sender<LiveServerFrame>,
    revision: u64,
    last_activity: &mut BTreeMap<String, SessionLiveActivity>,
) -> Result<(), HarnessError> {
    let (state, activity) = read_workbench(application).await?;
    *last_activity = activity
        .iter()
        .cloned()
        .map(|activity| (activity.session_id.as_str().to_owned(), activity))
        .collect();
    outgoing
        .send(LiveServerFrame::Workbench {
            revision,
            state,
            activity,
        })
        .await
        .map_err(|_| HarnessError::execution("live connection closed"))
}

pub(super) async fn send_activity_if_changed(
    application: &LocalApplication,
    session_id: &str,
    outgoing: &mpsc::Sender<LiveServerFrame>,
    last_activity: &mut BTreeMap<String, SessionLiveActivity>,
) -> Result<(), HarnessError> {
    let Some(activity) = application.live_activity(session_id).await else {
        return Ok(());
    };
    if last_activity.get(session_id) == Some(&activity) {
        return Ok(());
    }
    last_activity.insert(session_id.to_owned(), activity.clone());
    outgoing
        .send(LiveServerFrame::Activity { activity })
        .await
        .map_err(|_| HarnessError::execution("live connection closed"))
}
