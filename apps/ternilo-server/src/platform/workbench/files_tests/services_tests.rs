use salvo_core::{
    Service,
    http::StatusCode,
    prelude::Response,
    test::{ResponseExt as _, TestClient},
};
use serde_json::{Value, json};
use ternilo_control::{NativeSessionGrant, ResourceKind, ResourcePermissions};
use ternilo_protocol::SessionId;

use super::Fixture;

async fn control(
    fixture: &Fixture,
    session: &SessionId,
    account: &NativeSessionGrant,
    action: &str,
) -> Response {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        TestClient::post(format!(
            "http://server.test/api/v1/sessions/{session}/services/lsp%3Afixture/{action}"
        ))
        .add_header(
            "Authorization",
            format!("Bearer {}", account.access_token),
            true,
        )
        .add_header("x-ternilo-tenant", fixture.tenant.as_str(), true)
        .send(&fixture.service),
    )
    .await
    .expect("idle service control must not dispatch and wait for a Worker")
}

async fn assert_access(
    fixture: &Fixture,
    session: &SessionId,
    account: &NativeSessionGrant,
    expected: [StatusCode; 3],
) {
    let mut listed = fixture
        .get(
            &format!("/sessions/{session}/services"),
            account,
            &fixture.tenant,
        )
        .await;
    let body = listed.take_json::<Value>().await.unwrap();
    assert_eq!(
        listed.status_code,
        Some(expected[0]),
        "service listing for {session}: {body}"
    );
    if expected[0] == StatusCode::OK {
        assert_eq!(body, json!([]));
    }
    for (action, expected) in [("start", expected[1]), ("stop", expected[2])] {
        let mut response = control(fixture, session, account, action).await;
        let body = response.take_json::<Value>().await.unwrap();
        assert_eq!(
            response.status_code,
            Some(expected),
            "service {action} for {session}: {body}"
        );
    }
}

async fn shared_service_permissions(fixture: &Fixture, kind: ResourceKind) {
    let name = match kind {
        ResourceKind::Project => {
            unreachable!("this fixture exercises direct session and workspace grants")
        }
        ResourceKind::Session => "session-services",
        ResourceKind::Workspace => "workspace-services",
    };
    let session = fixture.session(name).await;
    let collaborator = fixture.collaborator().await;
    let resource_id = match kind {
        ResourceKind::Project => {
            unreachable!("this fixture exercises direct session and workspace grants")
        }
        ResourceKind::Session => session.session_id.as_str(),
        ResourceKind::Workspace => session.workspace_id.as_str(),
    };
    assert_access(
        fixture,
        &session.session_id,
        &collaborator,
        [StatusCode::FORBIDDEN; 3],
    )
    .await;
    let forbidden = StatusCode::FORBIDDEN;
    let idle = StatusCode::CONFLICT;
    for (submit, stop, configure, start_status, stop_status) in [
        (false, false, false, forbidden, forbidden),
        (true, false, false, idle, forbidden),
        (false, true, false, forbidden, idle),
        (false, false, true, forbidden, forbidden),
    ] {
        fixture
            .state
            .store
            .set_resource_share(
                &fixture.owner.session.user,
                &fixture.tenant,
                kind,
                resource_id,
                &collaborator.session.user.user_id,
                Some(ResourcePermissions {
                    view: true,
                    submit,
                    stop,
                    configure,
                }),
                fixture.now,
            )
            .await
            .unwrap();
        assert_access(
            fixture,
            &session.session_id,
            &collaborator,
            [StatusCode::OK, start_status, stop_status],
        )
        .await;
    }
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &fixture.tenant,
            kind,
            resource_id,
            &collaborator.session.user.user_id,
            None,
            fixture.now + 1,
        )
        .await
        .unwrap();
    assert_access(
        fixture,
        &session.session_id,
        &collaborator,
        [StatusCode::FORBIDDEN; 3],
    )
    .await;
    assert_access(
        fixture,
        &session.session_id,
        &fixture.owner,
        [StatusCode::OK, idle, idle],
    )
    .await;
}

#[tokio::test]
async fn idle_service_controls_enforce_independent_permissions_without_creating_execution() {
    let mut fixture = Fixture::new("sqlite::memory:", None).await;
    fixture.state.managed_execution_enabled = true;
    fixture.service = Service::new(crate::platform::web_router(fixture.state.clone()));
    for kind in [ResourceKind::Session, ResourceKind::Workspace] {
        shared_service_permissions(&fixture, kind).await;
    }
    assert!(
        fixture
            .state
            .cloud
            .list_runs(&fixture.tenant, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let mut transaction = fixture
        .state
        .cloud
        .database()
        .owner_transaction(&fixture.tenant, &fixture.owner.session.user.user_id)
        .await
        .unwrap();
    let commands: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_session_commands WHERE tenant_id=$1")
            .bind(fixture.tenant.as_str())
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(
        commands, 0,
        "service inspection and rejected idle controls must not enqueue Worker commands"
    );
    transaction.commit().await.unwrap();
}
