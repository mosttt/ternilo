use salvo_core::test::{ResponseExt, TestClient};
use ternilo_cloud::CloudSessionDraft;
use ternilo_control::{
    InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind, ResourcePermissions,
};
use ternilo_protocol::{
    Profile, RunId, SessionMode, SessionSubmissionRequest, SubmissionContent, SubmissionDelivery,
};

use super::*;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify direct HTTP read and stop permissions across an administrator's grant and revocation lifecycle."
)]
async fn cloud_http_requires_resource_grants_even_for_an_administrator() {
    let now = now_ms().unwrap();
    let store = ControlStore::connect("sqlite::memory:", None, SecretCipher::from_key([13; 32]), 1)
        .await
        .unwrap();
    let owner = store
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".to_owned(),
                username: "owner".to_owned(),
                password: "owner-password-123".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    let instance = owner.session.instance;
    let owner = owner.session.user;
    store
        .set_instance_mode(&owner, InstanceMode::MultiUser, instance.revision, now)
        .await
        .unwrap();
    let tenant = store
        .create_tenant(
            &owner,
            "shared-team",
            "Shared team",
            TenantQuota::default(),
            now,
        )
        .await
        .unwrap()
        .tenant_id;
    let project_id = store.list_projects(&owner, &tenant).await.unwrap()[0]
        .project_id
        .clone();
    let administrator = store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://identity.example".to_owned(),
                subject: "administrator".to_owned(),
                email: None,
                display_name: None,
            },
            "test-administrator",
            now,
        )
        .await
        .unwrap();
    store
        .set_account_role(
            &owner,
            &administrator.user_id,
            ternilo_control::PlatformRole::Admin,
            1,
            now,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &owner,
            &tenant,
            &administrator.user_id,
            TenantRole::Admin,
            now,
        )
        .await
        .unwrap();
    let workspace = store
        .create_cloud_workspace(&owner, &tenant, &project_id, "Private workspace", now)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(store.database().clone())
        .await
        .unwrap();
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(SessionId::new("private-session")),
                agent_id: AgentId::new("agent"),
                title: "Private session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: vec![],
                mode: SessionMode::Execute,
            },
            &tenant,
            &owner.user_id,
            now,
        )
        .await
        .unwrap();
    let catalog = ternilo_cloud::catalog().unwrap();
    let policy = load_worker_policy(None, &catalog).unwrap();
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id,
                workspace_id: workspace.workspace_id,
                agent_id: session.agent_id,
                session_id: session.session_id.clone(),
                run_id: Some(RunId::new("private-run")),
                limits: policy.maximum_limits,
                permissions: PermissionPreset::WorkspaceWrite,
                mode: SessionMode::Execute,
                profile: Profile::default(),
                input: "Private task".to_owned(),
                references: vec![],
                reference_contexts: vec![],
                attachments: vec![],
                reserved_model_tokens: 100,
            },
            tenant.clone(),
            owner.user_id.clone(),
            owner.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let request = SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: vec![],
        attachments: vec![],
    };
    cloud
        .enqueue_session_submission_as(&owner.user_id, &compiled, &request, now)
        .await
        .unwrap();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let state = AppState {
        cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
            .await
            .unwrap(),
        edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
        store: store.clone(),
        cloud: cloud.clone(),
        security: Arc::default(),
        setup_token_hash: None,
        managed_execution_enabled: true,
        shutdown: receiver,
        worker_policy: Arc::new(policy),
        catalog: Arc::new(catalog),
    };
    let router = Router::with_path("tenants/{tenant_id}")
        .hoop(affix_state::inject(state.clone()))
        .hoop(affix_state::inject(administrator.clone()))
        .push(
            Router::with_path("runs").get(list_cloud_runs).push(
                Router::with_path("{run_id}")
                    .get(get_cloud_run)
                    .delete(cancel_cloud_run),
            ),
        )
        .push(
            Router::with_path("sessions")
                .get(list_cloud_sessions)
                .push(Router::with_path("{session_id}/events").get(cloud_session_events)),
        );
    let service = salvo_core::Service::new(router);
    let base = format!("http://server.test/tenants/{tenant}");
    let run_path = format!("{base}/runs/private-run");
    let events_path = format!("{base}/sessions/private-session/events");
    assert_eq!(
        TestClient::get(&run_path).send(&service).await.status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        TestClient::get(&events_path)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        TestClient::delete(&run_path)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    for (path, key) in [("runs", "runs"), ("sessions", "sessions")] {
        let data: Value = TestClient::get(format!("{base}/{path}"))
            .send(&service)
            .await
            .take_json()
            .await
            .unwrap();
        assert_eq!(data[key].as_array().unwrap().len(), 0);
    }
    let view = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    store
        .set_resource_share(
            &owner,
            &tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &administrator.user_id,
            Some(view),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        TestClient::get(&run_path).send(&service).await.status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        TestClient::get(&events_path)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_eq!(
        TestClient::delete(&run_path)
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    let data: Value = TestClient::get(format!("{base}/runs"))
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(data["runs"].as_array().unwrap().len(), 1);
    store
        .set_resource_share(
            &owner,
            &tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &administrator.user_id,
            Some(ResourcePermissions { stop: true, ..view }),
            now,
        )
        .await
        .unwrap();
    let data: Value = TestClient::delete(&run_path)
        .send(&service)
        .await
        .take_json()
        .await
        .unwrap();
    assert_eq!(data["state"], "cancelled");
    store
        .set_resource_share(
            &owner,
            &tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &administrator.user_id,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        TestClient::get(&run_path).send(&service).await.status_code,
        Some(StatusCode::FORBIDDEN)
    );
    shutdown.send_replace(true);
    state.edge.shutdown().await;
}
