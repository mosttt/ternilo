use super::events::pump_events_from_notifications;
use super::*;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::RunLimits;

#[tokio::test]
async fn event_pump_delivers_reconnect_suffix_and_recovers_from_broadcast_lag() {
    let data_dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let application = Arc::new(
        LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data_dir.path().to_path_buf(),
        )
        .await
        .unwrap(),
    );
    let workspace = application
        .add_workspace(workspace_dir.path().to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    let session_id = session.identity.session_id;
    application
        .record_command_feedback(session_id.as_str(), "first".to_owned())
        .await
        .unwrap();

    let (notification_sender, notification_receiver) = broadcast::channel(1);
    let (outgoing, mut frames) = mpsc::channel(4);
    let (acknowledgements, ack_receiver) = mpsc::channel(1);
    let pump = tokio::spawn(pump_events_from_notifications(
        Arc::clone(&application),
        ExecutorScope {
            tenant_id: ternilo_protocol::TenantId::new("tenant"),
            user_id: ternilo_protocol::UserId::new("user"),
        },
        vec![SessionCursor {
            session_id: session_id.clone(),
            last_seq: Some(0),
        }],
        outgoing,
        notification_receiver,
        ack_receiver,
    ));

    let batch = next_batch(&mut frames).await;
    assert_eq!(batch.after_seq, Some(0));
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    acknowledgements
        .send(SessionCursor {
            session_id: session_id.clone(),
            last_seq: Some(2),
        })
        .await
        .unwrap();

    let second = application
        .record_command_feedback(session_id.as_str(), "second".to_owned())
        .await
        .unwrap();
    for event in second.events.iter().take(2) {
        notification_sender
            .send(LocalEventNotification {
                session_id: session_id.as_str().to_owned(),
                event: event.clone(),
            })
            .unwrap();
    }

    let batch = next_batch(&mut frames).await;
    assert_eq!(batch.after_seq, Some(2));
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![3, 4, 5]
    );
    acknowledgements
        .send(SessionCursor {
            session_id,
            last_seq: Some(5),
        })
        .await
        .unwrap();

    drop(notification_sender);
    tokio::time::timeout(Duration::from_secs(2), pump)
        .await
        .unwrap()
        .unwrap();
    application.shutdown().await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise unmapped history, backpressure and lost acknowledgements against a durable local session."
)]
async fn event_pump_retains_unmapped_history_and_replays_after_a_lost_acknowledgement() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (application, session_id) =
        super::command_lifecycle_tests::test_application(data.path(), workspace.path()).await;
    application
        .record_command_feedback(session_id.as_str(), "private prefix".to_owned())
        .await
        .unwrap();
    let scope = ExecutorScope {
        tenant_id: ternilo_protocol::TenantId::new("tenant"),
        user_id: ternilo_protocol::UserId::new("user"),
    };
    let (notifications, receiver) = broadcast::channel(1);
    let (outgoing, mut frames) = mpsc::channel(1);
    let (acknowledgements, ack_receiver) = mpsc::channel(1);
    let pump = tokio::spawn(pump_events_from_notifications(
        Arc::clone(&application),
        scope.clone(),
        Vec::new(),
        outgoing,
        receiver,
        ack_receiver,
    ));
    let first = next_batch(&mut frames).await;
    assert_eq!(first.after_seq, None);
    assert_eq!(first.events.first().unwrap().seq, 0);
    let second = application
        .record_command_feedback(session_id.as_str(), "mapping created".to_owned())
        .await
        .unwrap();
    notifications
        .send(LocalEventNotification {
            session_id: session_id.as_str().to_owned(),
            event: second.events.last().unwrap().clone(),
        })
        .unwrap();
    assert!(
        frames.try_recv().is_err(),
        "the sender waits for durable acknowledgement"
    );
    acknowledgements
        .send(SessionCursor {
            session_id: session_id.clone(),
            last_seq: None,
        })
        .await
        .unwrap();
    let replayed = next_batch(&mut frames).await;
    assert_eq!(
        replayed.after_seq, None,
        "an unmapped session has no Server cursor"
    );
    assert_eq!(
        replayed.events,
        application.events(session_id.as_str()).await.unwrap()
    );
    let committed = replayed.events.last().unwrap().seq;
    acknowledgements
        .send(SessionCursor {
            session_id: session_id.clone(),
            last_seq: Some(committed),
        })
        .await
        .unwrap();
    let third = application
        .record_command_feedback(session_id.as_str(), "lost acknowledgement".to_owned())
        .await
        .unwrap();
    notifications
        .send(LocalEventNotification {
            session_id: session_id.as_str().to_owned(),
            event: third.events.last().unwrap().clone(),
        })
        .unwrap();
    let lost = next_batch(&mut frames).await;
    assert_eq!(lost.after_seq, Some(committed));
    assert_eq!(lost.events.first().unwrap().seq, committed + 1);
    drop(acknowledgements);
    tokio::time::timeout(Duration::from_secs(2), pump)
        .await
        .unwrap()
        .unwrap();
    let (outgoing, mut frames) = mpsc::channel(1);
    let (acknowledgements, ack_receiver) = mpsc::channel(1);
    let (notifications, receiver) = broadcast::channel(1);
    let pump = tokio::spawn(pump_events_from_notifications(
        Arc::clone(&application),
        scope,
        vec![SessionCursor {
            session_id: session_id.clone(),
            last_seq: Some(committed),
        }],
        outgoing,
        receiver,
        ack_receiver,
    ));
    let replayed = next_batch(&mut frames).await;
    assert_eq!(
        replayed.events, lost.events,
        "reconnecting replays only the unacknowledged suffix"
    );
    acknowledgements
        .send(SessionCursor {
            session_id,
            last_seq: replayed.events.last().map(|event| event.seq),
        })
        .await
        .unwrap();
    drop(notifications);
    tokio::time::timeout(Duration::from_secs(2), pump)
        .await
        .unwrap()
        .unwrap();
    application.shutdown().await.unwrap();
}

async fn next_batch(frames: &mut mpsc::Receiver<ExecutorFrame>) -> EventBatch {
    match tokio::time::timeout(Duration::from_secs(2), frames.recv())
        .await
        .unwrap()
        .unwrap()
    {
        ExecutorFrame::EventBatch { batch } => batch,
        _ => panic!("expected a session event batch"),
    }
}
