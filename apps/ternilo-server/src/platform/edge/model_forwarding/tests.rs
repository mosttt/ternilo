use super::*;
use crate::platform::edge::model_forwarding::peer::DOMAIN;
use ternilo_protocol::{
    ComputerModelAttempt, ComputerModelRequest, ModelGatewayFrame, ModelRequest,
    ProviderModelDefaults, ProviderProtocol, UserId,
};

fn request() -> ComputerModelRequest {
    ComputerModelRequest {
        provider_id: "source".into(),
        model: "source-model".into(),
        protocol: ProviderProtocol::OpenAiChatCompletions,
        defaults: ProviderModelDefaults {
            context_window: 32_000,
            max_output_tokens: 128,
            reasoning: None,
        },
        reasoning_effort: None,
        max_attempts: 2,
        request: ModelRequest {
            run_id: RunId::new("remote-run"),
            system_prompt: "private source prompt".into(),
            messages: vec![],
            tools: vec![],
            step: 1,
        },
    }
}

#[tokio::test]
async fn model_peer_authentication_is_bound_to_protocol_identity_and_body() {
    let cluster = super::super::forwarding::ClusterForwarder::new(
        "http://127.0.0.1:4321",
        &STANDARD.encode([5; 32]),
    )
    .unwrap();
    let body = b"private model request";
    let signature = cluster.sign_domain(DOMAIN, body);
    cluster
        .authenticate_domain(DOMAIN, &signature, body)
        .unwrap();
    assert!(cluster.authenticate(&signature, body).is_err());
    assert!(
        cluster
            .authenticate_domain(DOMAIN, &cluster.sign(body), body)
            .is_err()
    );
    assert!(
        cluster
            .authenticate_domain(DOMAIN, &signature, b"changed model request")
            .is_err()
    );
    let id = random_hex_128();
    let now = now_ms().unwrap();
    assert!(
        cluster
            .admit_identity(&id, "other", now, "owner", now)
            .await
            .is_err()
    );
    assert!(
        cluster
            .admit_identity(&id, "owner", now - 30_001, "owner", now)
            .await
            .is_err()
    );
    cluster
        .admit_identity(&id, "owner", now, "owner", now)
        .await
        .unwrap();
    assert_eq!(
        cluster
            .admit_identity(&id, "owner", now, "owner", now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise real peer streaming, source fencing, retry permission, cancellation and disconnect as one invocation lifecycle."
)]
async fn computer_model_stream_crosses_servers_and_cancels_without_replaying() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    fixture
        .connection
        .hello
        .capabilities
        .insert(ExecutorCapability::ModelForwarding);
    fixture
        .edge
        .executors
        .write()
        .await
        .insert(fixture.route.clone(), fixture.connection.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    fixture
        .edge
        .journal
        .publish_peer(
            &fixture.route,
            &fixture.connection.lease,
            &origin,
            &fixture.connection.principal,
            now_ms().unwrap(),
        )
        .await
        .unwrap();
    let (service, _shutdown) = fixture.service().await;
    let server =
        salvo_core::Server::new(salvo_core::conn::tcp::TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let serving = tokio::spawn(async move { server.try_serve(service).await.unwrap() });
    let origin_gateway = EdgeGateway::new(fixture.store.edge_store())
        .await
        .unwrap()
        .with_cluster("http://127.0.0.1:4322", &fixture.key)
        .await
        .unwrap();
    let mut stream = origin_gateway
        .start_computer_model(
            &fixture.route.tenant_id,
            &fixture.route.executor_id,
            &fixture.user.user_id,
            request(),
        )
        .await
        .unwrap();
    let id = stream.request_id.clone();
    let ControlFrame::ModelRequest {
        request_id,
        scope,
        request: source_request,
    } = tokio::time::timeout(Duration::from_secs(5), fixture.incoming.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected source model request");
    };
    assert_eq!(request_id, id);
    assert_eq!(scope.user_id, fixture.user.user_id);
    assert_eq!(*source_request, request());
    let emit = |connection: &ConnectedExecutor, event| {
        fixture
            .edge
            .receive_model_event(&fixture.route, connection, &id, event);
    };
    let mut stale = fixture.connection.clone();
    stale.connection_id = ConnectionId::new("replaced-connection");
    emit(
        &stale,
        ComputerModelEvent::Output(Box::new(ModelGatewayFrame::Delta {
            delta: "stale".into(),
        })),
    );
    assert!(stream.events.try_recv().is_err());
    emit(
        &fixture.connection,
        ComputerModelEvent::Attempt(ComputerModelAttempt::Started { attempt: 1 }),
    );
    emit(
        &fixture.connection,
        ComputerModelEvent::Output(Box::new(ModelGatewayFrame::ReasoningDelta {
            delta: "source thinking".into(),
        })),
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), stream.events.recv())
            .await
            .unwrap(),
        Some(ComputerModelEvent::Attempt(ComputerModelAttempt::Started {
            attempt: 1
        }))
    ));
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(5), stream.events.recv()).await.unwrap(), Some(ComputerModelEvent::Output(frame)) if matches!(*frame, ModelGatewayFrame::ReasoningDelta { ref delta } if delta == "source thinking"))
    );
    stream
        .permit_retry(2, Some(HarnessError::policy("source grant revoked")))
        .await
        .unwrap();
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(5), fixture.incoming.recv()).await.unwrap(), Some(ControlFrame::ModelRetryPermit { request_id, attempt: 2, error: Some(_) }) if request_id == id)
    );
    stream.cancel().await;
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(5), fixture.incoming.recv()).await.unwrap(), Some(ControlFrame::ModelCancel { request_id }) if request_id == id)
    );
    emit(
        &fixture.connection,
        ComputerModelEvent::Attempt(ComputerModelAttempt::Finished {
            attempt: 1,
            http_status: Some(200),
            usage: None,
            upstream_request_id: None,
            error_code: Some(ErrorCode::Cancelled),
        }),
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), stream.events.recv())
            .await
            .unwrap(),
        Some(ComputerModelEvent::Attempt(
            ComputerModelAttempt::Finished { .. }
        ))
    ));
    drop(stream);
    assert!(
        matches!(tokio::time::timeout(Duration::from_secs(5), fixture.incoming.recv()).await.unwrap(), Some(ControlFrame::ModelCancel { request_id }) if request_id == id)
    );
    assert!(fixture.edge.model_calls.lock().unwrap().is_empty());
    assert!(
        origin_gateway
            .start_computer_model(
                &fixture.route.tenant_id,
                &fixture.route.executor_id,
                &UserId::new("another-owner"),
                request()
            )
            .await
            .is_err()
    );
    assert!(fixture.incoming.try_recv().is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM gateway_commands")
            .fetch_one(fixture.store.database().pool())
            .await
            .unwrap(),
        0
    );
    // Losing the source connection terminates the stream instead of redelivering the prompt.
    let mut stream = origin_gateway
        .start_computer_model(
            &fixture.route.tenant_id,
            &fixture.route.executor_id,
            &fixture.user.user_id,
            request(),
        )
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), fixture.incoming.recv())
            .await
            .unwrap(),
        Some(ControlFrame::ModelRequest { .. })
    ));
    fixture
        .edge
        .fail_model_connection(&fixture.route, &fixture.connection.connection_id);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), stream.events.recv())
            .await
            .unwrap()
            .is_none()
    );
    drop(stream);
    handle.stop_graceful(None);
    serving.await.unwrap();
    origin_gateway.shutdown().await;
    fixture.edge.shutdown().await;
}
