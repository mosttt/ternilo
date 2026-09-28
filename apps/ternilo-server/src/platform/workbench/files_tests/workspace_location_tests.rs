use std::time::Duration;

use futures_util::SinkExt as _;
use salvo_core::{conn::tcp::TcpAcceptor, http::StatusCode, test::ResponseExt as _};
use serde_json::{Value, json};
use ternilo_control::{ResourceKind, ResourcePermissions};
use ternilo_transport::{
    ApplicationOperation, CommandReply, ControlFrame, ExecutorCommandBody, ExecutorFrame,
};
use tokio_tungstenite::tungstenite::Message;

use super::{
    Fixture,
    transport_tests::{connect_upload_node, next_frame},
};

#[tokio::test]
async fn workspace_location_requires_view_and_machine_ownership_without_starting_cloud_execution() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let node = fixture.edge_session("private-location").await;
    let cloud = fixture.session("cloud-location").await;
    let collaborator = fixture.collaborator().await;
    let resource = format!("/workspaces/{}/location", node.workspace_id);
    let mut offline = fixture
        .get(&resource, &fixture.owner, &fixture.tenant)
        .await;
    assert_eq!(offline.status_code, Some(StatusCode::OK));
    assert_eq!(offline.headers().get("cache-control").unwrap(), "no-store");
    assert_eq!(
        offline.take_json::<Value>().await.unwrap(),
        json!({
            "status": "offline", "path": null, "home": null, "created_at_ms": null,
        })
    );
    let denied = fixture.get(&resource, &collaborator, &fixture.tenant).await;
    assert_eq!(denied.status_code, Some(StatusCode::FORBIDDEN));
    assert_eq!(denied.headers().get("cache-control").unwrap(), "no-store");
    for configure in [false, true] {
        fixture
            .state
            .store
            .set_resource_share(
                &fixture.owner.session.user,
                &fixture.tenant,
                ResourceKind::Workspace,
                node.workspace_id.as_str(),
                &collaborator.session.user.user_id,
                Some(ResourcePermissions {
                    view: true,
                    submit: configure,
                    stop: configure,
                    configure,
                }),
                fixture.now,
            )
            .await
            .unwrap();
        let mut forbidden = fixture.get(&resource, &collaborator, &fixture.tenant).await;
        assert_eq!(
            forbidden.status_code,
            Some(StatusCode::FORBIDDEN),
            "shared access must not expose the machine owner's directory"
        );
        assert_eq!(
            forbidden.headers().get("cache-control").unwrap(),
            "no-store"
        );
        assert!(
            forbidden
                .take_json::<Value>()
                .await
                .unwrap()
                .get("path")
                .is_none()
        );
    }
    let mut unavailable = fixture
        .get(
            &format!("/workspaces/{}/location", cloud.workspace_id),
            &fixture.owner,
            &fixture.tenant,
        )
        .await;
    assert_eq!(unavailable.status_code, Some(StatusCode::OK));
    assert_eq!(
        unavailable.headers().get("cache-control").unwrap(),
        "no-store"
    );
    assert_eq!(
        unavailable.take_json::<Value>().await.unwrap(),
        json!({
            "status": "unavailable", "path": null, "home": null, "created_at_ms": null,
        })
    );
    assert!(
        fixture
            .state
            .cloud
            .list_runs(&fixture.tenant, 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify authenticated path delivery, opaque mapping, persistence, and disconnect in one real Node connection."
)]
async fn workspace_location_crosses_the_authenticated_node_connection_without_persisting_paths() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let (session, credential) = fixture.edge_session_with_credential("location-node").await;
    let workspace_before = fixture
        .state
        .store
        .resolve_accessible_workspace(
            &fixture.owner.session.user,
            &fixture.tenant,
            &session.workspace_id,
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = salvo_core::Server::new(TcpAcceptor::try_from(listener).unwrap());
    let handle = server.handle();
    let router = crate::platform::web_router(fixture.state.clone());
    let serving = tokio::spawn(async move { server.try_serve(router).await.unwrap() });
    let (mut socket, frame) = connect_upload_node(
        address,
        &session.executor_id,
        &credential,
        "abcdef0123456789abcdef0123456789",
    )
    .await;
    assert!(matches!(frame, ControlFrame::UploadsAcknowledged { .. }));
    tokio::time::timeout(Duration::from_secs(3), async {
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
    let get_location = async {
        let response = reqwest::Client::new()
            .get(format!(
                "http://{address}/api/v1/workspaces/{}/location",
                session.workspace_id
            ))
            .bearer_auth(&fixture.owner.access_token)
            .header("x-ternilo-tenant", fixture.tenant.as_str())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
        response.json::<Value>().await.unwrap()
    };
    let reply = async {
        let ControlFrame::Command { command } = next_frame(&mut socket).await else {
            panic!("expected a workspace location request")
        };
        assert_eq!(
            command.body,
            ExecutorCommandBody::Application {
                request: ApplicationOperation::WorkspaceLocation {
                    workspace_id: workspace_before.executor_workspace_id.clone().unwrap()
                },
            }
        );
        socket.send(Message::Text(serde_json::to_string(&ExecutorFrame::Reply {
            reply: CommandReply::success(command.command_id.clone(), crate::platform::http::now_ms().unwrap(), json!({
                "path": "/home/private-owner/projects/location-proof", "home": "/home/private-owner", "created_at_ms": 123,
            })),
        }).unwrap().into())).await.unwrap();
        command.command_id
    };
    let (location, command_id) = tokio::join!(get_location, reply);
    assert_eq!(
        location,
        json!({
            "status": "available", "path": "/home/private-owner/projects/location-proof", "home": "/home/private-owner", "created_at_ms": 123,
        })
    );
    let mut tx = fixture
        .state
        .store
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let persisted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM gateway_commands WHERE tenant_id=$1 AND command_id=$2",
    )
    .bind(fixture.tenant.as_str())
    .bind(command_id.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        persisted, 0,
        "location commands and replies must remain ephemeral"
    );
    tx.commit().await.unwrap();
    let workspace_after = fixture
        .state
        .store
        .resolve_accessible_workspace(
            &fixture.owner.session.user,
            &fixture.tenant,
            &session.workspace_id,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(workspace_after).unwrap(),
        serde_json::to_value(&workspace_before).unwrap()
    );
    assert!(
        !serde_json::to_string(&workspace_before)
            .unwrap()
            .contains("private-owner")
    );
    socket.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
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
    let mut offline = fixture
        .get(
            &format!("/workspaces/{}/location", session.workspace_id),
            &fixture.owner,
            &fixture.tenant,
        )
        .await;
    assert_eq!(
        offline.take_json::<Value>().await.unwrap(),
        json!({
            "status": "offline", "path": null, "home": null, "created_at_ms": null,
        }),
        "disconnect must not return a retained copy of the private path"
    );
    handle.stop_graceful(Some(Duration::from_secs(1)));
    serving.await.unwrap();
}
