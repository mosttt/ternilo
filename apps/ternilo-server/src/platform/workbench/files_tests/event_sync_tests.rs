use super::*;
use futures_util::{SinkExt as _, StreamExt as _};
use salvo_core::conn::tcp::TcpAcceptor;
use ternilo_transport::{ControlFrame, EventBatch, ExecutorFrame};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

type NodeSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_batch(socket: &mut NodeSocket, batch: EventBatch) -> Option<u64> {
    let session_id = batch.session_id.clone();
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::EventBatch { batch })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    match super::transport_tests::next_frame(socket).await {
        ControlFrame::EventsAcknowledged { cursor } => {
            assert_eq!(cursor.session_id, session_id);
            cursor.last_seq
        }
        other => panic!("expected a committed event cursor, received {other:?}"),
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep unmapped delivery, mapping, replay and tamper rejection in one authenticated transport scenario."
)]
async fn node_event_acknowledgements_track_persistence_instead_of_receipt() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (registered, credential) = fixture.edge_session_with_credential("event-sync").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let (mut socket, _) = super::transport_tests::connect_upload_node(
        address,
        &registered.executor_id,
        &credential,
        "1234567890abcdef1234567890abcdef",
    )
    .await;
    let session_id = SessionId::new("initially-private-session");
    let prefix = SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now,
        run_id: RunId::new("private-run"),
        kind: SessionEventKind::TurnStarted,
    };
    let mut batch = EventBatch {
        scope: credential.scope.clone(),
        session_id: session_id.clone(),
        after_seq: None,
        events: vec![prefix],
    };
    assert_eq!(send_batch(&mut socket, batch.clone()).await, None);
    assert!(
        fixture
            .state
            .store
            .edge_store()
            .events(&fixture.tenant, &registered.executor_id, &session_id)
            .await
            .unwrap()
            .is_empty()
    );
    fixture
        .state
        .store
        .create_edge_session_mapping(
            &fixture.owner.session.user,
            &fixture.tenant,
            &registered.workspace_id,
            &registered.executor_id,
            &session_id,
            None,
            registered.metadata.clone(),
            fixture.now + 3,
        )
        .await
        .unwrap();
    batch.events.push(SessionEvent {
        seq: 1,
        occurred_at_ms: fixture.now + 1,
        run_id: RunId::new("private-run"),
        kind: SessionEventKind::TurnCancelled,
    });
    assert_eq!(send_batch(&mut socket, batch.clone()).await, Some(1));
    assert_eq!(
        send_batch(&mut socket, batch.clone()).await,
        Some(1),
        "replay cannot append duplicates"
    );
    let persisted = fixture
        .state
        .store
        .edge_store()
        .events(&fixture.tenant, &registered.executor_id, &session_id)
        .await
        .unwrap();
    assert_eq!(persisted, batch.events);
    batch.events[0].kind = SessionEventKind::SessionTitleGenerated {
        title: "tampered prefix".to_owned(),
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::EventBatch { batch })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    assert!(
        !matches!(closed, Some(Ok(Message::Text(_)))),
        "a changed event cannot receive an acknowledgement"
    );
    assert_eq!(
        fixture
            .state
            .store
            .edge_store()
            .events(&fixture.tenant, &registered.executor_id, &session_id)
            .await
            .unwrap(),
        persisted
    );
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}
