use std::{fmt::Write as _, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt as _, StreamExt as _};
use salvo_core::conn::tcp::TcpAcceptor;
use sha2::{Digest as _, Sha256};
use ternilo_protocol::{
    Attachment, FileSourceStatus, RunId, SessionEvent, SessionEventKind, SessionFileContent,
};
use ternilo_transport::{
    ApplicationOperation, CommandReply, ControlFrame, EXECUTOR_PROTOCOL_VERSION,
    ExecutorCapability, ExecutorCommandBody, ExecutorFrame, ExecutorHello, ExecutorKind,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest as _},
};

use super::Fixture;

type NodeSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) async fn next_frame(socket: &mut NodeSocket) -> ControlFrame {
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .expect("Node gateway frame timed out")
        .expect("Node gateway closed")
        .expect("Node gateway frame failed");
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise authenticated Node transport, a large retained file, and nonpersistent delivery end to end."
)]
async fn node_file_download_crosses_the_default_websocket_frame_limit_without_caching() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture
        .edge_session_with_credential("large-node-file")
        .await;
    let original = (0_u8..=255)
        .cycle()
        .take(13 * 1024 * 1024 + 17)
        .collect::<Vec<_>>();
    let expected_digest = Sha256::digest(&original);
    let digest_hex = expected_digest
        .iter()
        .fold(String::with_capacity(64), |mut digest, byte| {
            write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
            digest
        });
    fixture
        .state
        .store
        .edge_store()
        .merge_events(
            &fixture.tenant,
            &session.executor_id,
            &session.node_session_id,
            &[SessionEvent {
                seq: 0,
                occurred_at_ms: fixture.now,
                run_id: RunId::new("large-file-run"),
                kind: SessionEventKind::DeliverableProduced {
                    path: "out/large-retained.bin".to_owned(),
                    operation: "write".to_owned(),
                    attachment: Attachment {
                        name: "large-retained.bin".to_owned(),
                        media_type: "application/octet-stream".to_owned(),
                        content: format!("ternilo-attachment://sha256/{digest_hex}"),
                    },
                },
            }],
        )
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let mut request = format!("ws://{address}/api/v1/executors/connect")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", credential.token).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::Hello {
                hello: ExecutorHello {
                    protocol_version: EXECUTOR_PROTOCOL_VERSION,
                    executor_id: session.executor_id.clone(),
                    executor_kind: ExecutorKind::EdgeNode,
                    instance_nonce: "large-file-test".to_owned(),
                    catalog_revision: "large-file-test".to_owned(),
                    capabilities: [
                        ExecutorCapability::ApplicationRpc,
                        ExecutorCapability::SessionEventDelta,
                        ExecutorCapability::LiveInvalidations,
                    ]
                    .into_iter()
                    .collect(),
                },
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(next_frame(&mut socket).await, ControlFrame::Welcome { scope, .. } if scope == credential.scope)
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::UploadSyncStarted {
                scope: credential.scope.clone(),
                stream_id: "1234567890abcdef1234567890abcdef".to_owned(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut socket).await,
        ControlFrame::UploadsAcknowledged { last_seq: None, .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture
            .state
            .edge
            .is_connected(&fixture.tenant, &session.executor_id)
            .await
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (page, raw) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &fixture.owner,
        )
        .await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "generated-0-0");
    assert_eq!(page.items[0].source_status, FileSourceStatus::Online);
    assert!(page.items[0].session_archived);
    assert!(page.offline_sources.is_empty());
    assert!(!raw.to_string().contains("content_base64"));

    let download = async {
        let response = reqwest::Client::new()
            .get(format!(
                "http://{address}/api/v1/sessions/{}/files/generated-0-0/content",
                session.session_id
            ))
            .bearer_auth(&fixture.owner.access_token)
            .header("x-ternilo-tenant", fixture.tenant.as_str())
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        response.json::<SessionFileContent>().await.unwrap()
    };
    let respond = async {
        let ControlFrame::Command { command } = next_frame(&mut socket).await else {
            panic!("expected a session file request")
        };
        assert_eq!(command.scope, credential.scope);
        assert_eq!(
            command.body,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionFileContent {
                    session_id: session.node_session_id.clone(),
                    file_id: "generated-0-0".to_owned(),
                },
            }
        );
        let reply = ExecutorFrame::Reply {
            reply: CommandReply::success(
                command.command_id.clone(),
                crate::platform::http::now_ms().unwrap(),
                serde_json::to_value(SessionFileContent {
                    name: "large-retained.bin".to_owned(),
                    media_type: "application/octet-stream".to_owned(),
                    content_base64: STANDARD.encode(&original),
                })
                .unwrap(),
            ),
        };
        let text = serde_json::to_string(&reply).unwrap();
        assert!(text.len() > 16 * 1024 * 1024);
        socket.send(Message::Text(text.into())).await.unwrap();
        command.command_id.clone()
    };
    let (content, command_id) = tokio::join!(download, respond);
    assert_eq!(content.name, "large-retained.bin");
    assert_eq!(content.media_type, "application/octet-stream");
    let received = STANDARD.decode(content.content_base64).unwrap();
    assert_eq!(received.len(), original.len());
    assert_eq!(Sha256::digest(&received), expected_digest);
    assert_eq!(received, original);
    let mut tx = fixture
        .state
        .cloud
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let persisted = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM gateway_commands WHERE tenant_id=$1 AND command_id=$2",
    )
    .bind(fixture.tenant.as_str())
    .bind(command_id.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        persisted, 0,
        "file download commands and replies must stay ephemeral"
    );
    let objects = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM cloud_attachment_objects WHERE tenant_id=$1",
    )
    .bind(fixture.tenant.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        objects, 0,
        "Node file bytes must not enter Cloud object storage"
    );
    tx.commit().await.unwrap();
    socket.close(None).await.unwrap();
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep authenticated upload sync, offline browsing, reconnect and dataset identity fencing in one real WebSocket contract."
)]
async fn accepted_node_uploads_survive_disconnect_and_fence_a_replaced_data_directory() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture
        .edge_session_with_credential("queued-node-file")
        .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let stream_id = "1234567890abcdef1234567890abcdef";
    let (mut socket, ack) =
        connect_upload_node(address, &session.executor_id, &credential, stream_id).await;
    assert!(matches!(
        ack,
        ControlFrame::UploadsAcknowledged { last_seq: None, .. }
    ));
    let batch = ternilo_transport::AcceptedUploadBatch {
        scope: credential.scope.clone(),
        stream_id: stream_id.to_owned(),
        after_seq: None,
        changes: vec![ternilo_protocol::AcceptedUploadChange {
            seq: 1,
            session_id: session.node_session_id.clone(),
            kind: ternilo_protocol::AcceptedUploadChangeKind::UploadAccepted {
                upload: ternilo_protocol::AcceptedUploadMetadata {
                    submission_id: ternilo_protocol::SubmissionId::new("queued-submission"),
                    attachment_index: 0,
                    created_at_ms: fixture.now,
                    submitted_run_id: RunId::new("not-started-run"),
                    name: "accepted-before-execution.txt".to_owned(),
                    media_type: "text/plain".to_owned(),
                },
            },
        }],
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::AcceptedUploads {
                batch: batch.clone(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut socket).await,
        ControlFrame::UploadsAcknowledged {
            last_seq: Some(1),
            ..
        }
    ));
    let url = format!("/files?session_id={}", session.session_id);
    let (page, raw) = fixture.page(&url, &fixture.owner).await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].id, "submission-queued-submission-0");
    assert_eq!(page.items[0].event_seq, None);
    assert_eq!(page.items[0].source_status, FileSourceStatus::Online);
    assert!(!raw.to_string().contains("content"));
    assert!(!raw.to_string().contains("sha256"));
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .state
            .edge
            .is_connected(&fixture.tenant, &session.executor_id)
            .await
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (page, _) = fixture.page(&url, &fixture.owner).await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source_status, FileSourceStatus::Offline);
    assert_eq!(page.offline_sources.len(), 1);

    // A new local data root cannot dispatch commands or reuse the old mapping.
    let (mut rejected, frame) = connect_upload_node(
        address,
        &session.executor_id,
        &credential,
        "abcdef1234567890abcdef1234567890",
    )
    .await;
    assert!(
        matches!(frame, ControlFrame::Shutdown { reason } if reason.contains("data identity changed"))
    );
    assert!(
        !fixture
            .state
            .edge
            .is_connected(&fixture.tenant, &session.executor_id)
            .await
    );
    let _ = rejected.close(None).await;
    let (mut resumed, ack) =
        connect_upload_node(address, &session.executor_id, &credential, stream_id).await;
    assert!(matches!(
        ack,
        ControlFrame::UploadsAcknowledged {
            last_seq: Some(1),
            ..
        }
    ));
    resumed
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::AcceptedUploads {
                batch: batch.clone(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut resumed).await,
        ControlFrame::UploadsAcknowledged {
            last_seq: Some(1),
            ..
        }
    ));
    assert_eq!(fixture.page(&url, &fixture.owner).await.0.items.len(), 1);
    let stale_event = SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now,
        run_id: RunId::new("old-generated-run"),
        kind: SessionEventKind::DeliverableProduced {
            path: "out/deleted.txt".to_owned(),
            operation: "write".to_owned(),
            attachment: Attachment {
                name: "deleted.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                content: format!("ternilo-attachment://sha256/{}", "a".repeat(64)),
            },
        },
    };
    fixture
        .state
        .store
        .edge_store()
        .merge_events(
            &fixture.tenant,
            &session.executor_id,
            &session.node_session_id,
            std::slice::from_ref(&stale_event),
        )
        .await
        .unwrap();
    assert_eq!(fixture.page(&url, &fixture.owner).await.0.items.len(), 2);
    let deletion = ternilo_transport::AcceptedUploadBatch {
        after_seq: Some(1),
        changes: vec![ternilo_protocol::AcceptedUploadChange {
            seq: 2,
            session_id: session.node_session_id.clone(),
            kind: ternilo_protocol::AcceptedUploadChangeKind::SessionDeleted,
        }],
        ..batch
    };
    resumed
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::AcceptedUploads { batch: deletion })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut resumed).await,
        ControlFrame::UploadsAcknowledged {
            last_seq: Some(2),
            ..
        }
    ));
    assert!(fixture.page(&url, &fixture.owner).await.0.items.is_empty());
    resumed
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::EventBatch {
                batch: ternilo_transport::EventBatch {
                    scope: credential.scope.clone(),
                    session_id: session.node_session_id.clone(),
                    after_seq: None,
                    events: vec![stale_event],
                },
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    match next_frame(&mut resumed).await {
        ControlFrame::EventsAcknowledged { cursor } => {
            assert_eq!(cursor.session_id, session.node_session_id);
            assert_eq!(cursor.last_seq, None);
        }
        other => panic!("expected an event acknowledgement after deletion, received {other:?}"),
    }
    resumed
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::UploadSyncStarted {
                scope: credential.scope.clone(),
                stream_id: stream_id.to_owned(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut resumed).await,
        ControlFrame::UploadsAcknowledged {
            last_seq: Some(2),
            ..
        }
    ));
    assert!(fixture.page(&url, &fixture.owner).await.0.items.is_empty());
    resumed.close(None).await.unwrap();
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}

pub(super) async fn connect_upload_node(
    address: std::net::SocketAddr,
    executor_id: &ternilo_transport::ExecutorId,
    credential: &ternilo_control::NodeCredentialGrant,
    stream_id: &str,
) -> (NodeSocket, ControlFrame) {
    let mut request = format!("ws://{address}/api/v1/executors/connect")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", credential.token).parse().unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::Hello {
                hello: ExecutorHello {
                    protocol_version: EXECUTOR_PROTOCOL_VERSION,
                    executor_id: executor_id.clone(),
                    executor_kind: ExecutorKind::EdgeNode,
                    instance_nonce: "accepted-upload-test".to_owned(),
                    catalog_revision: "test-catalog".to_owned(),
                    capabilities: [
                        ExecutorCapability::ApplicationRpc,
                        ExecutorCapability::SessionEventDelta,
                        ExecutorCapability::LiveInvalidations,
                    ]
                    .into_iter()
                    .collect(),
                },
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        next_frame(&mut socket).await,
        ControlFrame::Welcome { .. }
    ));
    socket
        .send(Message::Text(
            serde_json::to_string(&ExecutorFrame::UploadSyncStarted {
                scope: credential.scope.clone(),
                stream_id: stream_id.to_owned(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let frame = next_frame(&mut socket).await;
    (socket, frame)
}
