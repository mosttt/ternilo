use super::*;

async fn put(fixture: &Fixture, path: &str, account: &NativeSessionGrant, body: Value) -> Response {
    TestClient::put(format!("http://server.test/api/v1{path}"))
        .add_header(
            "Authorization",
            format!("Bearer {}", account.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", fixture.tenant.as_str(), true)
        .json(&body)
        .send(&fixture.service)
        .await
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify real authenticated HTTP handoff, immutable Node credentials, editor retention and private execution configuration together."
)]
async fn node_resource_management_handoff_preserves_node_identity_and_protects_execution_settings()
{
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture.edge_session_with_credential("ownership-node").await;
    let recipient = fixture.collaborator().await;
    let workspace = &session.workspace_id;
    let path = format!("/workspaces/{workspace}/sharing/ownership");
    let input = json!({"owner_user_id":recipient.session.user.user_id,
        "expected_owner_user_id":fixture.owner.session.user.user_id,"expected_revision":0,
        "retain_previous_owner":true});
    let mut response = put(&fixture, &path, &fixture.owner, input.clone()).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let result: Value = response.take_json().await.unwrap();
    assert_eq!(
        result["owner"]["user_id"],
        recipient.session.user.user_id.as_str()
    );
    assert_eq!(result["revision"], 1);
    let snapshot = format!("/workspaces/{workspace}/sharing");
    let mut response = fixture.get(&snapshot, &recipient, &fixture.tenant).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let body: Value = response.take_json().await.unwrap();
    assert_eq!(
        body["access"]["owner_user_id"],
        recipient.session.user.user_id.as_str()
    );
    assert_eq!(
        body["access"]["storage_user_id"],
        fixture.owner.session.user.user_id.as_str()
    );
    assert_eq!(body["access"]["is_owner"], true);
    assert_eq!(body["access"]["is_execution_owner"], false);
    let mut response = fixture
        .get(
            &format!("/workspaces/{workspace}"),
            &recipient,
            &fixture.tenant,
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let details: Value = response.take_json().await.unwrap();
    assert_eq!(
        details["workspace"]["owner_user_id"],
        recipient.session.user.user_id.as_str()
    );
    assert_eq!(
        details["workspace"]["storage_user_id"],
        fixture.owner.session.user.user_id.as_str()
    );
    let mut response = fixture
        .get(&snapshot, &fixture.owner, &fixture.tenant)
        .await;
    let former: Value = response.take_json().await.unwrap();
    assert_eq!(former["access"]["is_owner"], false);
    assert_eq!(former["access"]["can_manage_sharing"], false);
    assert_eq!(former["access"]["permissions"]["submit"], true);
    assert_eq!(
        fixture
            .get(
                &format!("{path}/candidates"),
                &fixture.owner,
                &fixture.tenant
            )
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        put(&fixture, &path, &recipient, input).await.status_code,
        Some(StatusCode::CONFLICT)
    );
    let rename = TestClient::patch(format!("http://server.test/api/v1/workspaces/{workspace}"))
        .add_header(
            "Authorization",
            format!("Bearer {}", recipient.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", fixture.tenant.as_str(), true)
        .json(&json!({"title":"Transferred Node workspace"}))
        .send(&fixture.service)
        .await;
    assert_eq!(rename.status_code, Some(StatusCode::OK));
    let current = fixture
        .state
        .store
        .find_accessible_edge_session(
            &recipient.session.user,
            &fixture.tenant,
            &session.session_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.executor_id, session.executor_id);
    assert_eq!(current.node_session_id, session.node_session_id);
    assert_eq!(current.owner_user_id, fixture.owner.session.user.user_id);
    let principal = fixture
        .state
        .store
        .authenticate_node(&credential.token, fixture.now + 20)
        .await
        .unwrap();
    assert_eq!(principal.scope.user_id, fixture.owner.session.user.user_id);
    let response = TestClient::post(format!(
        "http://server.test/api/v1/credentials?workspace_id={workspace}"
    ))
    .add_header(
        "Authorization",
        format!("Bearer {}", recipient.access_token),
        true,
    )
    .add_header("x-ternilo-tenant", fixture.tenant.as_str(), true)
    .json(&json!({"name":"PRIVATE_KEY","value":"must-not-be-installed"}))
    .send(&fixture.service)
    .await;
    assert_eq!(response.status_code, Some(StatusCode::FORBIDDEN));
    fixture
        .state
        .store
        .set_resource_share(
            &recipient.session.user,
            &fixture.tenant,
            ResourceKind::Workspace,
            workspace.as_str(),
            &fixture.owner.session.user.user_id,
            None,
            fixture.now + 21,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .get(&snapshot, &fixture.owner, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    fixture
        .state
        .store
        .transfer_resource_ownership(
            &recipient.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &ternilo_control::ResourceOwnershipTransfer {
                owner_user_id: fixture.owner.session.user.user_id.clone(),
                expected_owner_user_id: recipient.session.user.user_id.clone(),
                expected_revision: 1,
                retain_previous_owner: false,
            },
            fixture.now + 22,
        )
        .await
        .unwrap();
    fixture
        .state
        .store
        .transfer_resource_ownership(
            &fixture.owner.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &ternilo_control::ResourceOwnershipTransfer {
                owner_user_id: recipient.session.user.user_id.clone(),
                expected_owner_user_id: fixture.owner.session.user.user_id.clone(),
                expected_revision: 2,
                retain_previous_owner: false,
            },
            fixture.now + 23,
        )
        .await
        .unwrap();
    fixture
        .state
        .store
        .delete_edge_session_mapping(
            &recipient.session.user,
            &fixture.tenant,
            &session.session_id,
            &session.node_session_id,
            fixture.now + 24,
        )
        .await
        .unwrap();
    let mut tx = fixture
        .state
        .store
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_resource_ownership WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2")
        .bind(fixture.tenant.as_str()).bind(session.session_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(remaining, 0);
    tx.commit().await.unwrap();
    fixture.state.store.database().close().await;
}
