use super::*;
use futures_util::SinkExt as _;
use salvo_core::conn::tcp::TcpAcceptor;
use ternilo_protocol::{ErrorCode, SessionEventPage, SessionHistoryQuery};
use ternilo_transport::{
    ApplicationOperation, CommandReply, ControlFrame, ExecutorCommandBody, ExecutorFrame,
};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep authenticated delayed reads, revocation and owner-cache assertions in one ordered lifecycle."
)]
async fn revoking_a_share_during_a_node_read_withholds_history_and_event_deltas() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture
        .edge_session_with_credential("delayed-history")
        .await;
    let collaborator = fixture.collaborator().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let (mut socket, _) = super::transport_tests::connect_upload_node(
        address,
        &session.executor_id,
        &credential,
        "1234567890abcdef1234567890abcdef",
    )
    .await;
    let events = vec![SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now,
        run_id: RunId::new("private-history"),
        kind: SessionEventKind::UserMessage {
            content: "private delayed result".to_owned(),
            provenance: None,
            display_content: None,
            source: None,
            references: vec![],
            attachments: vec![],
        },
    }];
    for read in ["history", "events", "delta"] {
        fixture
            .state
            .store
            .set_resource_share(
                &fixture.owner.session.user,
                &fixture.tenant,
                ResourceKind::Session,
                session.session_id.as_str(),
                &collaborator.session.user.user_id,
                Some(ResourcePermissions {
                    view: true,
                    ..ResourcePermissions::default()
                }),
                fixture.now,
            )
            .await
            .unwrap();
        let adapter = crate::platform::workbench::EdgeAdapter::new(
            &fixture.state,
            &collaborator.session.user,
            &fixture.tenant,
        );
        let reading = async {
            match read {
                "history" => adapter
                    .history(&session, SessionHistoryQuery::default())
                    .await
                    .map(|page| page.events),
                "events" => adapter.events(&session).await,
                _ => adapter.refresh_event_delta(&session, None).await,
            }
        };
        let revoke_and_reply = async {
            let ControlFrame::Command { command } =
                super::transport_tests::next_frame(&mut socket).await
            else {
                panic!("expected the in-flight Node read");
            };
            assert!(matches!(
                &command.body,
                ExecutorCommandBody::Application {
                    request: ApplicationOperation::SessionHistory { .. }
                        | ApplicationOperation::SessionEvents { .. }
                }
            ));
            fixture
                .state
                .store
                .set_resource_share(
                    &fixture.owner.session.user,
                    &fixture.tenant,
                    ResourceKind::Session,
                    session.session_id.as_str(),
                    &collaborator.session.user.user_id,
                    None,
                    fixture.now + 1,
                )
                .await
                .unwrap();
            let value = if read == "history" {
                serde_json::to_value(SessionEventPage {
                    events: events.clone(),
                    next_before_seq: None,
                })
                .unwrap()
            } else {
                serde_json::to_value(&events).unwrap()
            };
            socket
                .send(Message::Text(
                    serde_json::to_string(&ExecutorFrame::Reply {
                        reply: CommandReply::success(
                            command.command_id.clone(),
                            now_ms().unwrap(),
                            value,
                        ),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        };
        let (result, ()) = tokio::join!(reading, revoke_and_reply);
        let error =
            result.expect_err("revoked readers must not receive the delayed private payload");
        assert_eq!(error.code, ErrorCode::PolicyDenied, "{read}");
        assert!(!error.message.contains("private delayed result"));
        let owner = crate::platform::workbench::EdgeAdapter::new(
            &fixture.state,
            &fixture.owner.session.user,
            &fixture.tenant,
        );
        if read == "events" {
            assert_eq!(
                owner.live_event_delta(&session, None).await.unwrap(),
                events
            );
            assert_eq!(
                adapter
                    .live_event_delta(&session, None)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::PolicyDenied
            );
        }
    }
    socket.close(None).await.unwrap();
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}
