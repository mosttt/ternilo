use super::*;

fn run() -> StartedRun {
    let envelope: ternilo_cloud::ExecutionEnvelope =
        serde_json::from_str(include_str!("../../../examples/execution-envelope.json")).unwrap();
    StartedRun {
        claim: CloudRunClaim {
            provenance: None,
            tenant_id: envelope.spec.metadata.tenant_id.clone(),
            actor_user_id: envelope.spec.metadata.user_id.clone(),
            authorization_session_id: envelope.spec.metadata.session_id.clone(),
            run_id: envelope.spec.metadata.run_id.clone(),
            session_id: envelope.spec.metadata.session_id.clone(),
            workspace_use: ternilo_cloud::WorkspaceUseTicket {
                storage_id: "test-storage".to_owned(),
                root_id: "test-root".to_owned(),
                tenant_id: envelope.spec.metadata.tenant_id.clone(),
                workspace_id: envelope.spec.metadata.workspace_id.clone(),
                family_id: "test-family".to_owned(),
                worker_id: "test-worker".to_owned(),
                worker_generation: 1,
                occupation_epoch: 1,
                run_id: envelope.spec.metadata.run_id.clone(),
                lease_token: 7,
            },
            lease_token: 7,
            spec_digest: [0; 32],
            spec: envelope.spec,
        },
        fencing_token: 9,
        prior_events: Vec::new(),
    }
}

fn dependency() -> AcceptedSubagentRun {
    AcceptedSubagentRun {
        session_id: SessionId::new("accepted-child"),
        run_id: RunId::new("accepted-child-run"),
    }
}

async fn server(
    replies: Vec<(u16, serde_json::Value)>,
) -> (WorkerClient, tokio::task::JoinHandle<Vec<WorkerRpcRequest>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, reply) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            assert!(headers.starts_with("POST /internal/worker/v1/rpc "));
            requests.push(serde_json::from_value(body).unwrap());
            let encoded = serde_json::to_vec(&reply).unwrap();
            let reason = if status == 200 { "OK" } else { "Forbidden" };
            let headers = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                encoded.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(&encoded).await.unwrap();
        }
        requests
    });
    let client = WorkerClient::new(&origin, "fixture-token".to_owned()).unwrap();
    *client.identity.write().unwrap() = Some(request().identity);
    (client, task)
}

#[tokio::test]
async fn admission_rpc_preserves_canonical_lease_revisions_and_cleanup_generation() {
    let (client, requests) = server(vec![
        (200, serde_json::json!({"type": "parked", "revision": 5})),
        (200, serde_json::json!({"type": "resumed", "admission": {"status": "pending"}})),
        (200, serde_json::json!({"type": "resumed", "admission": {"status": "ready", "admission_epoch": 2}})),
        (200, serde_json::json!({"type": "unit"})),
    ]).await;
    let run = run();
    assert_eq!(
        client.park_run(&run, 11, vec![dependency()]).await.unwrap(),
        5
    );
    assert_eq!(
        client.resume_run(&run, 12, 5).await.unwrap(),
        RunAdmission::Pending
    );
    assert_eq!(
        client.resume_run(&run, 12, 5).await.unwrap(),
        RunAdmission::Ready { admission_epoch: 2 }
    );
    client.release_resident(&run, 3).await.unwrap();
    let requests = requests.await.unwrap();
    assert_eq!(requests.len(), 4);
    for rpc in &requests {
        assert_eq!(rpc.identity, request().identity);
        let lease = match &rpc.request {
            WorkerRequest::ParkRun {
                run,
                activity_revision,
                dependencies,
            } => {
                assert_eq!(*activity_revision, 11);
                assert_eq!(dependencies, &[dependency()]);
                run
            }
            WorkerRequest::ResumeRun {
                run,
                activity_revision,
                parked_revision,
            } => {
                assert_eq!((*activity_revision, *parked_revision), (12, 5));
                run
            }
            WorkerRequest::ReleaseResident {
                run,
                worker_generation,
            } => {
                assert_eq!(*worker_generation, 3);
                run
            }
            other => panic!("unexpected RPC: {other:?}"),
        };
        assert_eq!(lease, &RunLease::from(&run));
    }
}

#[tokio::test]
async fn admission_and_cleanup_rpc_propagate_server_rejections() {
    let failure = HarnessError::policy("execution generation was rejected");
    let body = serde_json::json!({"error": failure});
    let (client, requests) =
        server(vec![(403, body.clone()), (403, body.clone()), (403, body)]).await;
    let run = run();
    assert_eq!(
        client
            .park_run(&run, 1, vec![dependency()])
            .await
            .unwrap_err(),
        failure
    );
    assert_eq!(client.resume_run(&run, 2, 1).await.unwrap_err(), failure);
    assert_eq!(client.release_resident(&run, 3).await.unwrap_err(), failure);
    assert_eq!(requests.await.unwrap().len(), 3);
}

#[tokio::test]
async fn malformed_admission_replies_never_grant_execution_or_silently_release_resources() {
    let (client, requests) = server(vec![
        (200, serde_json::json!({"type": "parked", "revision": 0})),
        (200, serde_json::json!({"type": "resumed", "admission": {"status": "ready", "admission_epoch": 0}})),
        (200, serde_json::json!({"type": "count", "count": 0})),
    ]).await;
    let run = run();
    for error in [
        client
            .park_run(&run, 1, vec![dependency()])
            .await
            .unwrap_err(),
        client.resume_run(&run, 2, 1).await.unwrap_err(),
        client.release_resident(&run, 3).await.unwrap_err(),
    ] {
        assert_eq!(error, unexpected_reply());
    }
    assert_eq!(requests.await.unwrap().len(), 3);
}
