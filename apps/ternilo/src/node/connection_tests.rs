use super::*;
use crate::service::ServeOptions as Args;
use clap::{CommandFactory, Parser};
use std::net::SocketAddr;
use ternilo_kernel::HostPolicy;
use ternilo_protocol::RunLimits;

fn run_limits(max_steps: u32) -> RunLimits {
    RunLimits {
        max_steps,
        ..RunLimits::default()
    }
}

#[test]
fn successful_http_response_explains_that_the_gateway_path_is_missing() {
    let response = tokio_tungstenite::tungstenite::http::Response::builder()
        .status(200)
        .body(Some(Vec::new()))
        .unwrap();
    let error = gateway_connect_error(
        "ws://127.0.0.1:4321",
        &WebSocketError::Http(Box::new(response)),
    );
    let message = error.to_string();
    assert!(message.contains("returned HTTP 200 instead of a WebSocket upgrade"));
    assert!(message.contains("standalone Relay: /executor/connect"));
    assert!(message.contains("Control: /api/v1/executors/connect"));
}

#[test]
fn node_defaults_to_unlimited_steps_and_accepts_an_explicit_limit() {
    let default = Args::try_parse_from([
        "ternilo-node",
        "--gateway-url",
        "wss://relay.example.com/executor/connect",
        "--token",
        "test-token",
    ])
    .unwrap();
    assert_eq!(run_limits(default.max_steps).max_steps, 0);

    let limited = Args::try_parse_from([
        "ternilo-node",
        "--gateway-url",
        "wss://relay.example.com/executor/connect",
        "--token",
        "test-token",
        "--max-steps",
        "17",
    ])
    .unwrap();
    assert_eq!(run_limits(limited.max_steps).max_steps, 17);
}

#[test]
fn node_cli_accepts_only_gateway_connection_flags() {
    let args = Args::try_parse_from([
        "ternilo-node",
        "--gateway-url",
        "ws://127.0.0.1:4321/executor/connect",
        "--token",
        "test-token",
        "--allow-insecure-gateway",
    ])
    .unwrap();
    assert_eq!(
        args.gateway_url.as_deref(),
        Some("ws://127.0.0.1:4321/executor/connect")
    );
    assert!(args.allow_insecure_gateway);

    assert!(matches!(
        Args::try_parse_from([
            "ternilo-node",
            "--relay-url",
            "wss://relay.example.com/executor/connect",
        ]),
        Err(error) if error.kind() == clap::error::ErrorKind::UnknownArgument
    ));
    assert!(matches!(
        Args::try_parse_from([
            "ternilo-node",
            "--gateway-url",
            "ws://127.0.0.1:4321/executor/connect",
            "--token",
            "test-token",
            "--allow-insecure-relay",
        ]),
        Err(error) if error.kind() == clap::error::ErrorKind::UnknownArgument
    ));
    assert!(matches!(
        Args::try_parse_from([
            "ternilo-node",
            "--gateway-url",
            "wss://control.example.com/api/v1/executors/connect",
            "--token-env",
            "TERNILO_LOCAL_TOKEN",
        ]),
        Err(error) if error.kind() == clap::error::ErrorKind::UnknownArgument
    ));
}

#[test]
fn node_token_environment_value_is_hidden_from_help() {
    let command = Args::command();
    let token = command
        .get_arguments()
        .find(|argument| argument.get_id() == "token")
        .expect("missing Node token argument");
    assert_eq!(
        token.get_env(),
        Some(std::ffi::OsStr::new("TERNILO_LOCAL_TOKEN"))
    );
    assert!(token.is_hide_env_values_set());
}

#[test]
fn node_deployment_parameters_read_environment_and_cli_wins() {
    const CHILD_ENV: &str = "TERNILO_LOCAL_ARGS_TEST_CHILD";
    if std::env::var(CHILD_ENV).as_deref() == Ok("1") {
        let from_env = Args::try_parse_from(["ternilo-node"]).unwrap();
        assert_eq!(
            from_env.gateway_url.as_deref(),
            Some("wss://env.example.com/api/v1/executors/connect")
        );
        assert_eq!(from_env.token.as_deref(), Some("env-token"));
        assert_eq!(from_env.node_id, "env-node");
        assert_eq!(
            from_env.profile_layers,
            ["/env/one.json", "/env/two.json"].map(PathBuf::from)
        );
        assert_eq!(from_env.data_dir, Some(PathBuf::from("/env/data")));
        assert_eq!(
            from_env.listen,
            "127.0.0.1:4322".parse::<SocketAddr>().unwrap()
        );
        assert!(from_env.no_local_web);
        assert_eq!(from_env.max_steps, 23);
        assert!(!from_env.allow_insecure_gateway);

        let from_cli = Args::try_parse_from([
            "ternilo-node",
            "--gateway-url",
            "ws://127.0.0.1:4323/executor/connect",
            "--token",
            "cli-token",
            "--node-id",
            "cli-node",
            "--profile",
            "/cli/one.json",
            "--profile",
            "/cli/two.json",
            "--data-dir",
            "/cli/data",
            "--listen",
            "127.0.0.1:4324",
            "--no-local-web=false",
            "--max-steps",
            "29",
            "--allow-insecure-gateway",
        ])
        .unwrap();
        assert_eq!(
            from_cli.gateway_url.as_deref(),
            Some("ws://127.0.0.1:4323/executor/connect")
        );
        assert_eq!(from_cli.token.as_deref(), Some("cli-token"));
        assert_eq!(from_cli.node_id, "cli-node");
        assert_eq!(
            from_cli.profile_layers,
            ["/cli/one.json", "/cli/two.json"].map(PathBuf::from)
        );
        assert_eq!(from_cli.data_dir, Some(PathBuf::from("/cli/data")));
        assert_eq!(
            from_cli.listen,
            "127.0.0.1:4324".parse::<SocketAddr>().unwrap()
        );
        assert!(!from_cli.no_local_web);
        assert_eq!(from_cli.max_steps, 29);
        assert!(from_cli.allow_insecure_gateway);
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("node::connection_tests::node_deployment_parameters_read_environment_and_cli_wins")
        .arg("--exact")
        .env(CHILD_ENV, "1")
        .env(
            "TERNILO_LOCAL_GATEWAY_URL",
            "wss://env.example.com/api/v1/executors/connect",
        )
        .env("TERNILO_LOCAL_TOKEN", "env-token")
        .env("TERNILO_LOCAL_NODE_ID", "env-node")
        .env("TERNILO_LOCAL_PROFILES", "/env/one.json,/env/two.json")
        .env("TERNILO_LOCAL_DATA_DIR", "/env/data")
        .env("TERNILO_LOCAL_LISTEN", "127.0.0.1:4322")
        .env("TERNILO_LOCAL_NO_LOCAL_WEB", "true")
        .env("TERNILO_LOCAL_MAX_STEPS", "23")
        .env("TERNILO_LOCAL_ALLOW_INSECURE_GATEWAY", "false")
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep setup, protocol actions and assertions together for this integration scenario."
)]
async fn node_registers_and_dispatches_application_rpc_over_a_real_websocket() {
    async fn exchange_application_command(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        command: ExecutorCommand,
    ) -> Value {
        socket
            .send(Message::Text(
                serde_json::to_string(&ControlFrame::Command {
                    command: Box::new(command.clone()),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        loop {
            let frame = socket.next().await.unwrap().unwrap();
            let Message::Text(frame) = frame else {
                continue;
            };
            let frame: ExecutorFrame = serde_json::from_str(frame.as_str()).unwrap();
            if let ExecutorFrame::EventBatch { batch } = &frame {
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlFrame::EventsAcknowledged {
                            cursor: SessionCursor {
                                session_id: batch.session_id.clone(),
                                last_seq: batch.events.last().map(|event| event.seq),
                            },
                        })
                        .unwrap()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            if let ExecutorFrame::Reply { reply } = frame {
                assert_eq!(reply.command_id, command.command_id);
                return match reply.outcome {
                    ternilo_transport::CommandOutcome::Ok { value } => value,
                    ternilo_transport::CommandOutcome::Error { error } => {
                        panic!("application command failed: {error}")
                    }
                };
            }
        }
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("ws://{}/executor/connect", listener.local_addr().unwrap());
    let workspace_dir = tempfile::tempdir().unwrap();
    let workspace_path = workspace_dir.path().to_str().unwrap().to_owned();
    let scope = ExecutorScope {
        tenant_id: ternilo_protocol::TenantId::new("tenant"),
        user_id: ternilo_protocol::UserId::new("user"),
    };
    let gateway = tokio::spawn({
        let scope = scope.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = socket.next().await.unwrap().unwrap();
            let Message::Text(hello) = hello else {
                panic!("expected node hello")
            };
            let hello: ExecutorFrame = serde_json::from_str(hello.as_str()).unwrap();
            let ExecutorFrame::Hello { hello } = hello else {
                panic!("expected hello frame")
            };
            assert_eq!(hello.executor_kind, ExecutorKind::EdgeNode);
            assert!(
                hello
                    .capabilities
                    .contains(&ExecutorCapability::ApplicationRpc)
            );

            socket
                .send(Message::Text(
                    serde_json::to_string(&ControlFrame::Welcome {
                        protocol_version: EXECUTOR_PROTOCOL_VERSION,
                        connection_id: ternilo_transport::ConnectionId::new("connection-test"),
                        scope: scope.clone(),
                        heartbeat_interval_ms: 60_000,
                        event_cursors: Vec::new(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();

            let Message::Text(started) = socket.next().await.unwrap().unwrap() else {
                panic!("expected upload stream handshake")
            };
            let ExecutorFrame::UploadSyncStarted {
                scope: upload_scope,
                stream_id,
            } = serde_json::from_str(started.as_str()).unwrap()
            else {
                panic!("expected upload stream start")
            };
            assert_eq!(upload_scope, scope);
            socket
                .send(Message::Text(
                    serde_json::to_string(&ControlFrame::UploadsAcknowledged {
                        stream_id,
                        last_seq: None,
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();

            let command = |id: &str, request| ExecutorCommand {
                input_provenance: Some(ternilo_protocol::InputProvenance {
                    run_id: None,
                    input_id: ternilo_protocol::SubmissionId::new(format!("input-{id}")),
                    author: ternilo_protocol::InputAuthor::Account {
                        user_id: ternilo_protocol::UserId::new("actual-submitter"),
                        username: "requester".to_owned(),
                    },
                }),
                command_id: CommandId::new(id),
                scope: scope.clone(),
                issued_at_ms: now_ms().unwrap(),
                expires_at_ms: now_ms().unwrap() + 60_000,
                body: ExecutorCommandBody::Application { request },
            };
            let workspace: ternilo_local::Workspace = serde_json::from_value(
                exchange_application_command(
                    &mut socket,
                    command(
                        "command-workspace-create",
                        ApplicationOperation::WorkspaceCreate {
                            path: workspace_path,
                        },
                    ),
                )
                .await,
            )
            .unwrap();
            let session: ternilo_local::LocalSession = serde_json::from_value(
                exchange_application_command(
                    &mut socket,
                    command(
                        "command-session-create",
                        ApplicationOperation::SessionCreate {
                            workspace_id: workspace.workspace_id,
                            session_id: None,
                            agent_id: None,
                            agent_preset: None,
                            permissions: None,
                        },
                    ),
                )
                .await,
            )
            .unwrap();
            let session_id = session.identity.session_id;
            let outcome: ternilo_protocol::RunOutcome = serde_json::from_value(
                exchange_application_command(
                    &mut socket,
                    command(
                        "command-session-turn",
                        ApplicationOperation::SessionTurn {
                            session_id: session_id.clone(),
                            run_id: Some("node-ws-run".to_owned()),
                            input: "/code \"node rpc ready\"".to_owned(),
                            attachments: Vec::new(),
                        },
                    ),
                )
                .await,
            )
            .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&outcome.answer).unwrap()["result"],
                "node rpc ready"
            );
            assert_eq!(outcome.tool_calls, 1);

            let events: Vec<ternilo_protocol::SessionEvent> = serde_json::from_value(
                exchange_application_command(
                    &mut socket,
                    command(
                        "command-session-events",
                        ApplicationOperation::SessionEvents {
                            session_id,
                            after_seq: None,
                        },
                    ),
                )
                .await,
            )
            .unwrap();
            assert!(events.iter().any(|event| matches!(
                &event.kind,
                ternilo_protocol::SessionEventKind::TurnFinished { answer, .. }
                    if serde_json::from_str::<serde_json::Value>(answer).unwrap()["result"] == "node rpc ready"
            )));
            socket
                .send(Message::Text(
                    serde_json::to_string(&ControlFrame::Shutdown {
                        reason: "fixture complete".to_owned(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }
    });

    let data = tempfile::tempdir().unwrap();
    let catalog = ternilo_local::catalog().unwrap();
    let catalog_revision = catalog.revision().to_owned();
    let application = Arc::new(
        LocalApplication::open(
            catalog,
            ternilo_local::local_profile(),
            HostPolicy::local(RunLimits::default()),
            data.path().to_path_buf(),
        )
        .await
        .unwrap(),
    );
    let replies = Arc::new(
        ReplyCache::open(data.path().join("node-transport.sqlite3"))
            .await
            .unwrap(),
    );
    let executor_id = ExecutorId::new("fixture-node");
    let commands = TaskTracker::new();
    connect_once(ConnectionConfig {
        gateway_url: &gateway_url,
        token: "fixture-token",
        executor_id: &executor_id,
        instance_nonce: "fixture-instance",
        catalog_revision: &catalog_revision,
        application: Arc::clone(&application),
        replies,
        commands: commands.clone(),
    })
    .await
    .unwrap();
    gateway.await.unwrap();
    application.shutdown().await.unwrap();
    commands.close();
    commands.wait().await;
}
