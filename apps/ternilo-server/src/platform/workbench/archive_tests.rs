use super::*;

async fn restore(fixture: &Fixture, session: &SessionId, account: &NativeSessionGrant) -> Response {
    TestClient::post(format!(
        "http://server.test/api/v1/sessions/{session}/restore"
    ))
    .add_header(
        "Authorization",
        format!("Bearer {}", account.access_token),
        true,
    )
    .add_header("x-ternilo-tenant", fixture.tenant.as_str(), true)
    .send(&fixture.service)
    .await
}

async fn archives(fixture: &Fixture, account: &NativeSessionGrant) -> Vec<Value> {
    let mut response = fixture
        .get("/sessions/archived", account, &fixture.tenant)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    response.take_json().await.unwrap()
}

async fn preview(fixture: &Fixture, session: &SessionId, account: &NativeSessionGrant) -> Response {
    fixture
        .get(
            &format!("/sessions/{session}/archive-events"),
            account,
            &fixture.tenant,
        )
        .await
}

#[tokio::test]
async fn cloud_archive_http_restores_once_and_rejects_deleted_sessions() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let session = fixture.session("archived-cloud").await;
    assert_eq!(
        preview(&fixture, &session.session_id, &fixture.owner)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    fixture
        .state
        .cloud
        .archive_session(
            &fixture.tenant,
            &fixture.owner.session.user.user_id,
            &session.session_id,
            fixture.now + 1,
        )
        .await
        .unwrap();
    assert_eq!(archives(&fixture, &fixture.owner).await.len(), 1);
    let mut history = preview(&fixture, &session.session_id, &fixture.owner).await;
    assert_eq!(history.status_code, Some(StatusCode::OK));
    assert_eq!(history.headers().get("cache-control").unwrap(), "no-store");
    assert!(history.take_json::<Vec<Value>>().await.unwrap().is_empty());
    let mut first = restore(&fixture, &session.session_id, &fixture.owner).await;
    assert_eq!(first.status_code, Some(StatusCode::OK));
    let first = first.take_json::<Value>().await.unwrap();
    assert!(first["archived_at_ms"].is_null());
    assert_eq!(first["identity"]["session_id"], session.session_id.as_str());
    let mut second = restore(&fixture, &session.session_id, &fixture.owner).await;
    assert_eq!(second.status_code, Some(StatusCode::OK));
    assert_eq!(second.take_json::<Value>().await.unwrap(), first);
    assert!(archives(&fixture, &fixture.owner).await.is_empty());
    assert_eq!(
        preview(&fixture, &session.session_id, &fixture.owner)
            .await
            .status_code,
        Some(StatusCode::CONFLICT)
    );
    fixture
        .state
        .cloud
        .delete_session(
            &fixture.tenant,
            &session.session_id,
            &fixture.owner.session.user.user_id,
            fixture.now + 2,
        )
        .await
        .unwrap();
    assert_ne!(
        restore(&fixture, &session.session_id, &fixture.owner)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
    assert_ne!(
        preview(&fixture, &session.session_id, &fixture.owner)
            .await
            .status_code,
        Some(StatusCode::OK)
    );
}

#[tokio::test]
async fn cloud_and_offline_node_archives_require_current_grants_and_owner_restore() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let cloud = fixture.session("private-cloud-archive").await;
    let edge = fixture.edge_session("private-node-archive").await;
    let collaborator = fixture.collaborator().await;
    fixture
        .state
        .cloud
        .archive_session(
            &fixture.tenant,
            &fixture.owner.session.user.user_id,
            &cloud.session_id,
            fixture.now + 1,
        )
        .await
        .unwrap();
    assert_eq!(archives(&fixture, &fixture.owner).await.len(), 2);
    assert!(archives(&fixture, &collaborator).await.is_empty());
    for session_id in [&cloud.session_id, &edge.session_id] {
        assert_ne!(
            preview(&fixture, session_id, &collaborator)
                .await
                .status_code,
            Some(StatusCode::OK)
        );
        assert_ne!(
            restore(&fixture, session_id, &collaborator)
                .await
                .status_code,
            Some(StatusCode::OK)
        );
        fixture
            .state
            .store
            .set_resource_share(
                &fixture.owner.session.user,
                &fixture.tenant,
                ResourceKind::Session,
                session_id.as_str(),
                &collaborator.session.user.user_id,
                Some(ResourcePermissions {
                    view: true,
                    submit: true,
                    stop: true,
                    configure: true,
                }),
                fixture.now + 2,
            )
            .await
            .unwrap();
        assert_eq!(
            restore(&fixture, session_id, &collaborator)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            preview(&fixture, session_id, &collaborator)
                .await
                .status_code,
            Some(StatusCode::OK)
        );
    }
    let shared = archives(&fixture, &collaborator).await;
    assert_eq!(shared.len(), 2);
    assert!(
        shared
            .iter()
            .all(|session| session["access"]["is_owner"] == false)
    );
    let active = fixture
        .get("/sessions", &collaborator, &fixture.tenant)
        .await
        .take_json::<Vec<Value>>()
        .await
        .unwrap();
    assert!(active.is_empty());
    let other_tenant = &fixture.owner.session.personal_tenant_id;
    let mut other = fixture
        .get("/sessions/archived", &fixture.owner, other_tenant)
        .await;
    assert_eq!(other.status_code, Some(StatusCode::OK));
    assert!(other.take_json::<Vec<Value>>().await.unwrap().is_empty());
}

#[tokio::test]
async fn offline_archive_preview_returns_cached_events_and_rechecks_shared_access() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let edge = fixture.edge_session("preview-offline").await;
    let collaborator = fixture.collaborator().await;
    let event: SessionEvent = serde_json::from_value(json!({
        "seq":0,"occurred_at_ms":fixture.now,"run_id":"preview-run","type":"user_message","content":"retained offline history",
    })).unwrap();
    fixture
        .state
        .store
        .edge_store()
        .merge_events(
            &fixture.tenant,
            &edge.executor_id,
            &edge.node_session_id,
            std::slice::from_ref(&event),
        )
        .await
        .unwrap();
    let mut response = preview(&fixture, &edge.session_id, &fixture.owner).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert_eq!(
        response.take_json::<Vec<SessionEvent>>().await.unwrap(),
        vec![event]
    );
    let share = Some(ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    });
    for permissions in [share, None] {
        fixture
            .state
            .store
            .set_resource_share(
                &fixture.owner.session.user,
                &fixture.tenant,
                ResourceKind::Session,
                edge.session_id.as_str(),
                &collaborator.session.user.user_id,
                permissions,
                fixture.now + 3,
            )
            .await
            .unwrap();
        let response = preview(&fixture, &edge.session_id, &collaborator).await;
        assert_eq!(
            response.status_code == Some(StatusCode::OK),
            permissions.is_some()
        );
    }
    let cross = fixture
        .get(
            &format!("/sessions/{}/archive-events", edge.session_id),
            &fixture.owner,
            &fixture.owner.session.personal_tenant_id,
        )
        .await;
    assert_ne!(cross.status_code, Some(StatusCode::OK));
    let session = fixture
        .state
        .store
        .find_accessible_edge_session(
            &fixture.owner.session.user,
            &fixture.tenant,
            &edge.session_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.metadata, edge.metadata);
}
