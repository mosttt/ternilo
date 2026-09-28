use super::*;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::{
    InputAuthor, InputProvenance, RunLimits, SessionEventKind, SubmissionId, TenantId, UserId,
};

async fn gateway(
    listener: tokio::net::TcpListener,
    application: Arc<LocalApplication>,
    command: ExecutorCommand,
    session_id: SessionId,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert!(matches!(
        socket.next().await.unwrap().unwrap(),
        Message::Text(_)
    ));
    for frame in [
        ControlFrame::Welcome {
            protocol_version: EXECUTOR_PROTOCOL_VERSION,
            connection_id: ternilo_transport::ConnectionId::new("lifecycle-connection"),
            scope: command.scope.clone(),
            heartbeat_interval_ms: 60_000,
            event_cursors: Vec::new(),
        },
        ControlFrame::Command {
            command: Box::new(command),
        },
    ] {
        socket
            .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while application
            .pending_questions(Some(session_id.as_str()))
            .await
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the command must be running before disconnect");
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlFrame::Shutdown {
                reason: "disconnect without cancelling accepted work".to_owned(),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    while let Some(Ok(frame)) = socket.next().await {
        if matches!(frame, Message::Close(_)) {
            break;
        }
    }
}

pub(super) async fn test_application(
    data: &std::path::Path,
    workspace: &std::path::Path,
) -> (Arc<LocalApplication>, SessionId) {
    let application = Arc::new(
        LocalApplication::open(
            ternilo_local::catalog().unwrap(),
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data.to_owned(),
        )
        .await
        .unwrap(),
    );
    let workspace = application
        .add_workspace(workspace.to_str().unwrap())
        .await
        .unwrap();
    let session = application
        .create_session(workspace.workspace_id, None, None)
        .await
        .unwrap();
    (application, session.identity.session_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_commands_survive_disconnect_and_finish_durable_replies_before_shutdown() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (application, session_id) = test_application(data.path(), workspace.path()).await;
    let scope = ExecutorScope {
        tenant_id: TenantId::new("tenant"),
        user_id: UserId::new("owner"),
    };
    let command = ExecutorCommand {
        command_id: CommandId::new("accepted-before-disconnect"),
        input_provenance: Some(InputProvenance {
            input_id: SubmissionId::new("gateway-input"),
            run_id: None,
            author: InputAuthor::Account {
                user_id: scope.user_id.clone(),
                username: "owner".to_owned(),
            },
        }),
        scope: scope.clone(),
        issued_at_ms: now_ms().unwrap(),
        expires_at_ms: now_ms().unwrap() + 60_000,
        body: ExecutorCommandBody::Application {
            request: ApplicationOperation::SessionTurn {
                session_id: session_id.clone(),
                run_id: Some("lifecycle-run".to_owned()),
                input: "/ask Wait for shutdown?".to_owned(),
                attachments: Vec::new(),
            },
        },
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("ws://{}/executor/connect", listener.local_addr().unwrap());
    let gateway = tokio::spawn(gateway(
        listener,
        Arc::clone(&application),
        command.clone(),
        session_id.clone(),
    ));
    let reply_path = data.path().join("node-transport.sqlite3");
    let replies = Arc::new(ReplyCache::open(reply_path.clone()).await.unwrap());
    let commands = TaskTracker::new();
    connect_once(ConnectionConfig {
        gateway_url: &address,
        token: "test-token",
        executor_id: &ExecutorId::new("node"),
        instance_nonce: "first-instance",
        catalog_revision: ternilo_local::catalog().unwrap().revision(),
        application: Arc::clone(&application),
        replies: Arc::clone(&replies),
        commands: commands.clone(),
    })
    .await
    .unwrap();
    gateway.await.unwrap();
    assert_eq!(
        commands.len(),
        1,
        "disconnect must retain ownership of accepted commands"
    );
    assert_eq!(
        application
            .pending_questions(Some(session_id.as_str()))
            .await
            .len(),
        1
    );
    commands.close();
    tokio::time::timeout(Duration::from_secs(5), async {
        application.shutdown().await.unwrap();
        commands.wait().await;
    })
    .await
    .expect("shutdown must cancel the question and drain its command reply");
    let replies::ReplyClaim::Completed(reply) = replies.claim(&command).await.unwrap() else {
        panic!("accepted command must have a completed reply before the service exits")
    };
    let restored = ReplyCache::open(reply_path).await.unwrap();
    let recovered = execute_command(Arc::clone(&application), &scope, command, &restored).await;
    assert_eq!(
        serde_json::to_value(recovered).unwrap(),
        serde_json::to_value(reply).unwrap()
    );
    let events = application.events(session_id.as_str()).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
            .count(),
        1
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnCancelled))
    );
}
