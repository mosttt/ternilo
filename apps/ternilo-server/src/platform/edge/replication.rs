use super::{
    AtomicU64, ConnectionId, ControlFrame, Duration, EXECUTOR_LEASE_TTL_MS, EdgeGateway,
    EdgeLiveNotification, ExecutorFrame, HarnessError, Ordering, RouteKey, SessionEvent,
    SessionEventKind, SessionLiveDirty, StreamExt, WebSocket, mpsc, now_ms,
};

impl EdgeGateway {
    #[expect(
        clippy::too_many_lines,
        reason = "Keep frame validation and connection dispatch in one protocol loop."
    )]
    pub(super) async fn receive_frames(
        &self,
        route: &RouteKey,
        connection_id: &ConnectionId,
        last_seen: &AtomicU64,
        mut stream: futures_util::stream::SplitStream<WebSocket>,
    ) {
        loop {
            let Ok(Some(message)) =
                tokio::time::timeout(Duration::from_secs(45), stream.next()).await
            else {
                break;
            };
            let Ok(message) = message else { break };
            if message.is_text() {
                let connected = self
                    .executors
                    .read()
                    .await
                    .get(route)
                    .filter(|executor| &executor.connection_id == connection_id)
                    .cloned();
                let Some(connected) = connected else {
                    break;
                };
                if self
                    .store
                    .require_node_credential(&connected.principal)
                    .await
                    .is_err()
                {
                    break;
                }
                let Ok(text) = message.as_str() else { break };
                let Ok(frame) = serde_json::from_str::<ExecutorFrame>(text) else {
                    break;
                };
                let Ok(now) = now_ms() else { break };
                if !matches!(
                    self.journal
                        .renew(route, &connected.lease, now, EXECUTOR_LEASE_TTL_MS)
                        .await,
                    Ok(true)
                ) {
                    break;
                }
                if self
                    .store
                    .mark_executor_seen(&route.tenant_id, &route.executor_id, now)
                    .await
                    .is_err()
                {
                    break;
                }
                last_seen.store(now, Ordering::Relaxed);
                match frame {
                    ExecutorFrame::Heartbeat { .. } => {
                        if self.dispatch_available(route).await.is_err() {
                            break;
                        }
                        self.pending
                            .lock()
                            .await
                            .retain(|_, call| !call.sender.is_closed());
                    }
                    ExecutorFrame::Reply { reply } => {
                        if let Err(error) = self.accept_reply(route, &connected, reply).await {
                            eprintln!("accept Node {} command reply: {error}", route.executor_id);
                            break;
                        }
                    }
                    ExecutorFrame::UploadSyncStarted { scope, stream_id } => {
                        if scope != connected.scope {
                            break;
                        }
                        let result = self
                            .journal
                            .begin_upload_sync(
                                &self.store,
                                route,
                                &connected.lease,
                                &scope,
                                &stream_id,
                                now,
                            )
                            .await;
                        if !acknowledge_uploads(&connected.sender, stream_id, result).await {
                            break;
                        }
                    }
                    ExecutorFrame::AcceptedUploads { batch } => {
                        if batch.scope != connected.scope || batch.validate().is_err() {
                            break;
                        }
                        let result = self
                            .journal
                            .merge_uploads(&self.store, route, &connected.lease, &batch, now)
                            .await;
                        if !acknowledge_uploads(&connected.sender, batch.stream_id, result).await {
                            break;
                        }
                        let _ = self.live_notify.send(EdgeLiveNotification::Session {
                            tenant_id: route.tenant_id.clone(),
                            executor_id: route.executor_id.clone(),
                            node_session_id: None,
                            dirty: SessionLiveDirty::default(),
                            refresh_events: false,
                            workbench: true,
                            activity: None,
                        });
                    }
                    ExecutorFrame::EventBatch { batch } => {
                        let scope_matches = self
                            .executors
                            .read()
                            .await
                            .get(route)
                            .is_some_and(|executor| batch.scope == executor.scope);
                        if !scope_matches || batch.validate().is_err() {
                            break;
                        }
                        let (dirty, workbench) = edge_batch_dirty(&batch.events);
                        let synchronized = self
                            .journal
                            .merge_events(
                                &self.store,
                                route,
                                &connected.lease,
                                &batch.session_id,
                                &batch.events,
                                now,
                            )
                            .await;
                        let last_seq = match synchronized {
                            Ok(last_seq) => last_seq,
                            Err(error) => {
                                eprintln!(
                                    "synchronize Node {} session {} events: {error}",
                                    route.executor_id, batch.session_id
                                );
                                break;
                            }
                        };
                        if connected
                            .sender
                            .send(ControlFrame::EventsAcknowledged {
                                cursor: ternilo_transport::SessionCursor {
                                    session_id: batch.session_id.clone(),
                                    last_seq,
                                },
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                        self.event_notify.notify_waiters();
                        let _ = self.live_notify.send(EdgeLiveNotification::Session {
                            tenant_id: route.tenant_id.clone(),
                            executor_id: route.executor_id.clone(),
                            node_session_id: Some(batch.session_id),
                            dirty,
                            refresh_events: false,
                            workbench,
                            activity: None,
                        });
                    }
                    ExecutorFrame::LiveInvalidation { invalidation } => {
                        if invalidation
                            .session_id
                            .as_ref()
                            .is_some_and(|session_id| session_id.validate().is_err())
                            || invalidation.activity.as_ref().is_some_and(|activity| {
                                activity.session_id.validate().is_err()
                                    || invalidation.session_id.as_ref().is_some_and(|session_id| {
                                        session_id != &activity.session_id
                                    })
                            })
                        {
                            break;
                        }
                        let refresh_events = invalidation.dirty.events;
                        if self
                            .journal
                            .notify_live(route, &connected.lease, now)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        let _ = self.live_notify.send(EdgeLiveNotification::Session {
                            tenant_id: route.tenant_id.clone(),
                            executor_id: route.executor_id.clone(),
                            node_session_id: invalidation.session_id,
                            dirty: invalidation.dirty,
                            refresh_events,
                            workbench: invalidation.workbench,
                            activity: invalidation.activity,
                        });
                    }
                    // Model outputs require a corresponding Server-side pending
                    // invocation. No unsolicited result may become session data.
                    ExecutorFrame::ModelOutput { .. } | ExecutorFrame::Hello { .. } => break,
                }
            } else if !message.is_ping() && !message.is_pong() {
                break;
            }
        }
    }
}

pub(super) async fn acknowledge_uploads(
    sender: &mpsc::Sender<ControlFrame>,
    stream_id: String,
    result: Result<Option<u64>, HarnessError>,
) -> bool {
    match result {
        Ok(last_seq) => sender
            .send(ControlFrame::UploadsAcknowledged {
                stream_id,
                last_seq,
            })
            .await
            .is_ok(),
        Err(error) => {
            eprintln!("synchronize Node uploads: {error}");
            let _ = sender
                .send(ControlFrame::Shutdown {
                    reason: error.to_string(),
                })
                .await;
            false
        }
    }
}

pub(super) fn edge_batch_dirty(events: &[SessionEvent]) -> (SessionLiveDirty, bool) {
    let mut dirty = SessionLiveDirty::default();
    let mut workbench = false;
    for event in events {
        dirty.merge(SessionLiveDirty::for_event(&event.kind));
        workbench |= matches!(
            &event.kind,
            SessionEventKind::TurnStarted
                | SessionEventKind::TurnFinished { .. }
                | SessionEventKind::TurnFailed { .. }
                | SessionEventKind::TurnCancelled
                | SessionEventKind::SessionTitleGenerated { .. }
                | SessionEventKind::SubagentUpdated { .. }
        );
    }
    (dirty, workbench)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::RunId;

    #[test]
    fn edge_event_batches_keep_streaming_deltas_out_of_derived_reads() {
        let streaming = SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: RunId::new("run"),
            kind: SessionEventKind::AssistantMessageDelta {
                step: 1,
                delta: "chunk".to_owned(),
            },
        };
        let (dirty, workbench) = edge_batch_dirty(&[streaming]);
        assert!(dirty.events);
        assert!(dirty.metadata().is_empty());
        assert!(!workbench);

        let started = SessionEvent {
            seq: 1,
            occurred_at_ms: 2,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        };
        let (dirty, workbench) = edge_batch_dirty(&[started]);
        assert!(dirty.events && dirty.stats && dirty.projection);
        assert!(workbench);
    }
}
