use super::*;
use futures_util::SinkExt as _;
use salvo_core::conn::tcp::TcpAcceptor;
use ternilo_transport::{
    ApplicationOperation, CommandReply, ControlFrame, ExecutorCommandBody, ExecutorFrame,
};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn newly_created_session_does_not_reuse_an_older_inflight_snapshot() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (parent, credential) = fixture.edge_session_with_credential("discovery").await;
    let node = fixture
        .state
        .store
        .authenticate_node(&credential.token, fixture.now)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let (mut socket, _) = super::transport_tests::connect_upload_node(
        address,
        &parent.executor_id,
        &credential,
        "1234567890abcdef1234567890abcdef",
    )
    .await;
    let first_id = SessionId::new("first-child");
    let second_id = SessionId::new("new-child");
    let first = crate::platform::workbench::synchronize_node_model_session(
        &fixture.state,
        &node,
        &first_id,
    );
    tokio::pin!(first);
    let first_command = tokio::select! {
        result = &mut first => panic!("discovery finished before snapshot: {result:?}"),
        frame = super::transport_tests::next_frame(&mut socket) => frame,
    };
    // The child was created after the first Node snapshot was captured.
    let second = crate::platform::workbench::synchronize_node_model_session(
        &fixture.state,
        &node,
        &second_id,
    );
    tokio::pin!(second);
    assert!(futures_util::poll!(&mut second).is_pending());
    let session = |id: &SessionId| {
        json!({
            "identity": {"tenant_id":"local", "user_id":"local-user", "agent_id":"standard", "session_id":id},
            "workspace_id":"node-workspace-discovery", "workspace_path":"/fixture", "title":"Child",
            "parent_session_id":parent.node_session_id, "permissions":"read_only", "model":{"provider":"profile_default"},
            "agent_preset":"standard", "mode":"execute", "created_at_ms":fixture.now, "updated_at_ms":fixture.now,
            "subagent":{"subagent_id":id, "provider":"in-process", "transcript_kind":"conversation"}
        })
    };
    let reply = |frame, sessions| {
        let ControlFrame::Command { command } = frame else {
            panic!("expected snapshot command")
        };
        assert_eq!(
            command.body,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::Snapshot
            }
        );
        Message::Text(serde_json::to_string(&ExecutorFrame::Reply {
            reply: CommandReply::success(command.command_id, fixture.now, json!({
                "workspaces":[{"workspace_id":"node-workspace-discovery", "title":"Fixture"}], "sessions":sessions,
            })),
        }).unwrap().into())
    };
    socket
        .send(reply(first_command, vec![session(&first_id)]))
        .await
        .unwrap();
    let (first_result, second_command) = tokio::join!(first, async {
        tokio::select! {
            result = &mut second => panic!("new child incorrectly reused the old snapshot: {result:?}"),
            frame = super::transport_tests::next_frame(&mut socket) => frame,
        }
    });
    first_result.unwrap();
    socket
        .send(reply(
            second_command,
            vec![session(&first_id), session(&second_id)],
        ))
        .await
        .unwrap();
    second.await.unwrap();
    assert!(
        fixture
            .state
            .store
            .node_model_session_registered(&node, &second_id)
            .await
            .unwrap()
    );
    socket.close(None).await.unwrap();
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}
