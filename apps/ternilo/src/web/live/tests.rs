use super::session_data::read_metadata;
use super::subscriptions::next_dirty;
use super::*;
use ternilo_local::{LocalInvalidationCategory, LocalSessionUpdate};
use ternilo_protocol::{
    RunId, SessionEventKind, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
    TenantId,
};
use tokio_tungstenite::tungstenite::Message as ClientMessage;

fn boot_token(html: &str) -> String {
    let marker = "window.__TERNILO_BOOT__ = ";
    let json = html
        .split_once(marker)
        .expect("boot marker")
        .1
        .split_once(";</script>")
        .expect("boot script end")
        .0;
    serde_json::from_str::<serde_json::Value>(json).unwrap()["apiToken"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn send_frame<S>(socket: &mut S, frame: LiveClientFrame)
where
    S: futures_util::Sink<ClientMessage> + Unpin,
    S::Error: std::fmt::Debug,
{
    socket
        .send(ClientMessage::text(serde_json::to_string(&frame).unwrap()))
        .await
        .unwrap();
}

async fn receive_frame<S>(socket: &mut S) -> LiveServerFrame
where
    S: futures_util::Stream<Item = Result<ClientMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let message = tokio::time::timeout(std::time::Duration::from_secs(3), socket.next())
        .await
        .expect("live frame timeout")
        .expect("live socket closed")
        .expect("live socket error");
    serde_json::from_str(message.to_text().expect("text live frame"))
        .expect("valid live server frame")
}

#[test]
fn hello_uses_local_token_version_and_tenant() {
    let hello = |token: &str, tenant: Option<&str>, version| LiveClientFrame::Hello {
        protocol_version: version,
        bearer_token: Some(token.to_owned()),
        tenant_id: tenant.map(TenantId::new),
    };
    validate_hello(
        "token",
        &hello("token", Some("local"), LIVE_PROTOCOL_VERSION),
    )
    .unwrap();
    validate_hello("token", &hello("token", None, LIVE_PROTOCOL_VERSION)).unwrap();
    assert!(
        validate_hello(
            "token",
            &hello("wrong", Some("local"), LIVE_PROTOCOL_VERSION)
        )
        .is_err()
    );
    assert!(
        validate_hello(
            "token",
            &hello("token", Some("other"), LIVE_PROTOCOL_VERSION)
        )
        .is_err()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn local_live_reads_terminal_inbox_when_cancelled_occurrence_finishes_before_observation() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id;

    // These receivers intentionally remain unread until the occurrence has
    // reached its terminal state, matching an append -> finish window in
    // which the browser never observed the transient active run.
    let mut unobserved_events = application.subscribe_events();
    let mut unobserved_invalidations = application.subscribe_invalidations();
    let mut completion_events = application.subscribe_events();
    let mut completion_invalidations = application.subscribe_invalidations();
    let run_id = RunId::new("unobserved-cancelled-occurrence");
    application
        .submit_session(
            session_id.as_str(),
            SessionSubmissionRequest {
                delivery: SubmissionDelivery::Queue,
                run_id: Some(run_id.clone()),
                content: SubmissionContent::Prompt {
                    input: "/ask Wait for cancellation?".to_owned(),
                },
                references: Vec::new(),
                attachments: Vec::new(),
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if !application
                .pending_questions(Some(session_id.as_str()))
                .await
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    while completion_events.try_recv().is_ok() {}
    while completion_invalidations.try_recv().is_ok() {}
    application
        .cancel_turn(session_id.as_str(), run_id.as_str())
        .await
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notification = completion_invalidations.recv().await.unwrap();
            if notification.session_id.as_deref() == Some(session_id.as_str())
                && notification.category == LocalInvalidationCategory::Inbox
            {
                break;
            }
        }
    })
    .await
    .expect("cancel did not persist the inbox pause");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notification = completion_events.recv().await.unwrap();
            if notification.event.run_id == run_id
                && matches!(notification.event.kind, SessionEventKind::TurnCancelled)
            {
                break;
            }
        }
    })
    .await
    .expect("cancelled turn did not reach the canonical event log");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let notification = completion_invalidations.recv().await.unwrap();
            if notification.session_id.as_deref() == Some(session_id.as_str())
                && notification.category == LocalInvalidationCategory::Inbox
            {
                break;
            }
        }
    })
    .await
    .expect("terminal turn did not settle the canonical inbox");

    assert!(
        application
            .events(session_id.as_str())
            .await
            .unwrap()
            .iter()
            .any(|event| event.run_id == run_id
                && matches!(event.kind, SessionEventKind::TurnCancelled))
    );
    let dirty = next_dirty(
        &session_id,
        SessionLiveReadMask {
            inbox: true,
            ..SessionLiveReadMask::default()
        },
        &mut unobserved_events,
        &mut unobserved_invalidations,
    )
    .await
    .unwrap();
    assert!(dirty.events);
    assert!(dirty.inbox);
    let metadata = read_metadata(
        application.as_ref(),
        &session_id,
        SessionLiveReadMask {
            inbox: true,
            ..SessionLiveReadMask::default()
        },
    )
    .await
    .unwrap();
    let inbox = metadata.inbox.unwrap();
    assert_eq!(inbox.active_run_id, None);
    assert!(inbox.paused);
    assert!(inbox.items.is_empty());

    application.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn local_websocket_delivers_baseline_incremental_metadata_activity_and_unsubscribe() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = crate::open_local_application(
        ternilo_local::local_profile(),
        data_dir.path().to_path_buf(),
    )
    .await
    .unwrap();
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let first = application
        .create_session(workspace.workspace_id.clone(), None, None)
        .await
        .unwrap();
    let second = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let first_id = first.identity.session_id.clone();
    let second_id = second.identity.session_id.clone();

    let listener = super::super::bind_loopback("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = {
        let application = Arc::clone(&application);
        tokio::spawn(async move {
            super::super::serve_application_on_listener(listener, application).await
        })
    };
    let base = format!("http://{address}");
    let token = boot_token(&reqwest::get(&base).await.unwrap().text().await.unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/live"))
        .await
        .unwrap();
    send_frame(
        &mut socket,
        LiveClientFrame::Hello {
            protocol_version: LIVE_PROTOCOL_VERSION,
            bearer_token: Some(token),
            tenant_id: Some(TenantId::new("local")),
        },
    )
    .await;
    assert!(matches!(
        receive_frame(&mut socket).await,
        LiveServerFrame::Ready { .. }
    ));
    let LiveServerFrame::Workbench {
        revision,
        state,
        activity,
    } = receive_frame(&mut socket).await
    else {
        panic!("expected workbench baseline")
    };
    assert_eq!(revision, 1);
    assert_eq!(state["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(activity.len(), 2);
    assert!(activity.iter().all(|activity| !activity.running));

    send_frame(
        &mut socket,
        LiveClientFrame::Subscribe {
            subscription_id: 1,
            session_id: first_id.clone(),
            after_seq: None,
            metadata: SessionLiveReadMask::all(),
        },
    )
    .await;
    let LiveServerFrame::EventBatch {
        subscription_id,
        reset,
        complete,
        events,
        next_seq,
        ..
    } = receive_frame(&mut socket).await
    else {
        panic!("expected empty event baseline")
    };
    assert_eq!(subscription_id, 1);
    assert!(reset);
    assert!(complete);
    assert!(events.is_empty());
    assert_eq!(next_seq, 0);
    let LiveServerFrame::SessionMetadata {
        subscription_id,
        metadata,
        ..
    } = receive_frame(&mut socket).await
    else {
        panic!("expected metadata baseline")
    };
    assert_eq!(subscription_id, 1);
    assert_eq!(metadata.read, SessionLiveReadMask::all());
    assert!(metadata.inbox.is_some());
    assert!(metadata.stats.is_some());
    assert!(metadata.projection.is_some());
    assert!(metadata.questions.is_some());
    assert!(metadata.profile.is_some());
    assert!(metadata.agent_team.is_some());

    let running = {
        let application = Arc::clone(&application);
        let session_id = first_id.clone();
        tokio::spawn(async move {
            application
                .run_turn(session_id.as_str(), None, "/ask Continue?".to_owned())
                .await
        })
    };
    let mut delivered = Vec::new();
    let mut saw_running = false;
    let mut saw_stats = false;
    let mut saw_projection = false;
    let mut question_id = None;
    while question_id.is_none() || !saw_running {
        match receive_frame(&mut socket).await {
            LiveServerFrame::EventBatch {
                subscription_id: 1,
                events,
                next_seq,
                ..
            } => {
                if let Some(last) = events.last() {
                    assert_eq!(next_seq, last.seq + 1);
                }
                delivered.extend(events);
            }
            LiveServerFrame::Activity { activity } if activity.session_id == first_id => {
                saw_running |= activity.running;
            }
            LiveServerFrame::SessionMetadata {
                subscription_id: 1,
                metadata,
                ..
            } => {
                saw_stats |= metadata.read.stats;
                saw_projection |= metadata.read.projection;
                if let Some(question) = metadata.questions.and_then(|questions| {
                    questions
                        .into_iter()
                        .find(|pending| !pending.question.id.is_empty())
                }) {
                    question_id = Some(question.question.id);
                }
            }
            _ => {}
        }
    }
    application
        .answer_question(ternilo_protocol::UserAnswer {
            question_id: question_id.unwrap(),
            selected: Vec::new(),
            custom: Some("Yes".to_owned()),
        })
        .await
        .unwrap();
    running.await.unwrap().unwrap();

    let mut saw_stopped = false;
    let mut saw_questions_cleared = false;
    while !saw_stopped
        || !saw_questions_cleared
        || !delivered
            .iter()
            .any(|event: &ternilo_protocol::SessionEvent| {
                matches!(event.kind, SessionEventKind::TurnFinished { .. })
            })
    {
        match receive_frame(&mut socket).await {
            LiveServerFrame::EventBatch {
                subscription_id: 1,
                events,
                next_seq,
                ..
            } => {
                if let Some(last) = events.last() {
                    assert_eq!(next_seq, last.seq + 1);
                }
                delivered.extend(events);
            }
            LiveServerFrame::Activity { activity } if activity.session_id == first_id => {
                saw_stopped |= !activity.running;
            }
            LiveServerFrame::SessionMetadata {
                subscription_id: 1,
                metadata,
                ..
            } => {
                saw_stats |= metadata.read.stats;
                saw_projection |= metadata.read.projection;
                saw_questions_cleared |= metadata.questions.as_ref().is_some_and(Vec::is_empty);
            }
            _ => {}
        }
    }
    assert!(saw_stats);
    assert!(saw_projection);
    assert!(
        delivered
            .windows(2)
            .all(|events| events[1].seq == events[0].seq + 1)
    );

    application
        .update_session(
            first_id.as_str(),
            LocalSessionUpdate {
                title: Some("Live renamed".to_owned()),
                ..LocalSessionUpdate::default()
            },
        )
        .await
        .unwrap();
    let mut saw_profile = false;
    let mut saw_workbench_revision = false;
    while !saw_profile || !saw_workbench_revision {
        match receive_frame(&mut socket).await {
            LiveServerFrame::SessionMetadata {
                subscription_id: 1,
                metadata,
                ..
            } if metadata.read.profile => saw_profile = true,
            LiveServerFrame::Workbench {
                revision, state, ..
            } if revision > 1 => {
                saw_workbench_revision = state["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|session| session["title"] == "Live renamed");
            }
            _ => {}
        }
    }

    send_frame(
        &mut socket,
        LiveClientFrame::Unsubscribe { subscription_id: 1 },
    )
    .await;
    send_frame(
        &mut socket,
        LiveClientFrame::Subscribe {
            subscription_id: 2,
            session_id: second_id,
            after_seq: None,
            metadata: SessionLiveReadMask::default(),
        },
    )
    .await;
    loop {
        if matches!(
            receive_frame(&mut socket).await,
            LiveServerFrame::EventBatch {
                subscription_id: 2,
                ..
            }
        ) {
            break;
        }
    }
    application
        .run_turn(
            first_id.as_str(),
            None,
            "/code \"after-unsubscribe\"".to_owned(),
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(300);
    while let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, socket.next()).await {
        let frame: LiveServerFrame = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert!(!matches!(
            frame,
            LiveServerFrame::EventBatch {
                subscription_id: 1,
                ..
            } | LiveServerFrame::SessionMetadata {
                subscription_id: 1,
                ..
            }
        ));
    }

    let _ = socket.close(None).await;
    server.abort();
    let _ = server.await;
    application.shutdown().await.unwrap();
}

#[test]
fn event_chunks_preserve_after_seq_and_next_seq() {
    let events = (0..300)
        .map(|seq| ternilo_protocol::SessionEvent {
            seq,
            occurred_at_ms: seq,
            run_id: ternilo_protocol::RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        })
        .collect::<Vec<_>>();
    assert_eq!(events.chunks(EVENT_CHUNK_SIZE).count(), 2);
    assert_eq!(events[255].seq + 1, 256);
    assert_eq!(events[299].seq + 1, 300);
}

#[test]
fn switched_subscription_drops_old_generation_frames() {
    let old = LiveServerFrame::EventBatch {
        subscription_id: 1,
        session_id: SessionId::new("old"),
        reset: false,
        complete: true,
        events: Vec::new(),
        next_seq: 0,
    };
    let current = LiveServerFrame::EventBatch {
        subscription_id: 2,
        session_id: SessionId::new("current"),
        reset: false,
        complete: true,
        events: Vec::new(),
        next_seq: 0,
    };
    assert!(!frame_matches_active_subscription(&old, 2));
    assert!(frame_matches_active_subscription(&current, 2));
}
