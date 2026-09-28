use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt as _, StreamExt as _};
use salvo_core::conn::tcp::TcpAcceptor;
use ternilo_protocol::{ErrorCode, LIVE_PROTOCOL_VERSION, LiveClientFrame, LiveServerFrame};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use super::*;

type LiveSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(address: SocketAddr, grant: &NativeSessionGrant) -> LiveSocket {
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/live"))
        .await
        .unwrap();
    let hello = LiveClientFrame::Hello {
        protocol_version: LIVE_PROTOCOL_VERSION,
        bearer_token: Some(grant.access_token.clone()),
        tenant_id: Some(grant.session.personal_tenant_id.clone()),
    };
    socket
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut socket).await,
        LiveServerFrame::Ready { .. }
    ));
    assert!(matches!(
        next_frame(&mut socket).await,
        LiveServerFrame::Workbench { .. }
    ));
    socket
}

async fn next_frame(socket: &mut LiveSocket) -> LiveServerFrame {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match socket
                .next()
                .await
                .expect("live connection remains open")
                .unwrap()
            {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Close(frame) => panic!("unexpected live close: {frame:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("live frame arrives before the five-second authorization interval")
}

async fn assert_connected(socket: &mut LiveSocket) {
    socket.send(Message::Text("{}".into())).await.unwrap();
    loop {
        match next_frame(socket).await {
            LiveServerFrame::Error {
                code: ErrorCode::InvalidInput,
                ..
            } => break,
            LiveServerFrame::Workbench { .. } | LiveServerFrame::Activity { .. } => {}
            frame => panic!("expected application response from retained connection: {frame:?}"),
        }
    }
}

async fn assert_revoked(socket: &mut LiveSocket) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match next_frame(socket).await {
                LiveServerFrame::Error {
                    code: ErrorCode::PolicyDenied,
                    subscription_id: None,
                    ..
                } => break,
                LiveServerFrame::Workbench { .. } | LiveServerFrame::Activity { .. } => {}
                frame => panic!("expected revoked credential rejection: {frame:?}"),
            }
        }
        let message = socket.next().await;
        assert!(matches!(message, Some(Ok(Message::Close(_))) | None));
    })
    .await
    .expect("revoked connection closes before periodic authorization");
}

#[tokio::test]
async fn session_revocation_reauthenticates_existing_live_connections_and_keeps_valid_sessions() {
    let fixture = Fixture::new().await;
    let victim = fixture
        .state
        .store
        .create_browser_session(fixture.member.session.user.clone(), now_ms().unwrap())
        .await
        .unwrap();
    let retained = fixture
        .state
        .store
        .create_browser_session(fixture.member.session.user.clone(), now_ms().unwrap())
        .await
        .unwrap();
    let victim_id = fixture.list(&victim).await["sessions"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let actor_id = fixture.list(&fixture.member).await["sessions"][0]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move {
        server.try_serve(router).await.unwrap();
    });
    let mut actor_socket = connect(address, &fixture.member).await;
    let mut victim_socket = connect(address, &victim).await;
    let mut retained_socket = connect(address, &retained).await;
    let mut owner_socket = connect(address, &fixture.owner).await;
    let response = fixture
        .request(
            "DELETE",
            &format!("sessions/{victim_id}"),
            Some(&fixture.member.access_token),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_revoked(&mut victim_socket).await;
    assert_connected(&mut actor_socket).await;
    assert_connected(&mut retained_socket).await;
    assert_connected(&mut owner_socket).await;
    let response = fixture
        .request(
            "POST",
            "sessions/revoke-others",
            Some(&fixture.member.access_token),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_revoked(&mut retained_socket).await;
    assert_connected(&mut actor_socket).await;
    assert_connected(&mut owner_socket).await;
    let response = fixture
        .request(
            "DELETE",
            &format!("sessions/{actor_id}"),
            Some(&fixture.member.access_token),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_revoked(&mut actor_socket).await;
    assert_connected(&mut owner_socket).await;
    owner_socket.close(None).await.unwrap();
    fixture.close().await;
    handle.stop_graceful(Some(Duration::from_secs(2)));
    serving.await.unwrap();
}
