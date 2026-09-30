use std::time::Duration;

use ternilo_cloud::{CloudLiveNotification, CloudSessionEventFeed, CloudStore};
use ternilo_control::{
    ControlStore, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::TenantId;
use tokio::sync::broadcast;

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

async fn changed(receiver: &mut broadcast::Receiver<CloudLiveNotification>, tenant: &TenantId) {
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .expect("another Server must observe the committed permission change")
            .unwrap(),
        CloudLiveNotification::ResourcesChanged {
            tenant_id: tenant.clone()
        },
    );
}

async fn quiet(receiver: &mut broadcast::Receiver<CloudLiveNotification>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(650), receiver.recv())
            .await
            .is_err()
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise real sharing mutations, independent feeds, rollback and canonical authorization together."
)]
async fn contract(url: &str, owner_url: Option<&str>) {
    let control = ControlStore::connect(url, owner_url, SecretCipher::from_key([59; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(url, owner_url, 4).await.unwrap();
    let owner = control
        .initialize_owner(
            &NativeRegistration {
                username: "resource-owner".into(),
                email: "resource-owner@example.test".into(),
                password: "resource-owner-password".into(),
            },
            1000,
        )
        .await
        .unwrap()
        .session
        .user;
    control
        .set_instance_mode(&owner, InstanceMode::MultiUser, 1, 1001)
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &owner,
            "resource-live",
            "Resource live",
            TenantQuota::default(),
            1002,
        )
        .await
        .unwrap()
        .tenant_id;
    let reader = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "resource-notifications".into(),
                subject: "reader".into(),
                email: None,
                display_name: None,
            },
            "reader",
            1003,
        )
        .await
        .unwrap();
    control
        .set_membership(&owner, &tenant, &reader.user_id, TenantRole::Member, 1004)
        .await
        .unwrap();
    let project = control
        .list_projects(&owner, &tenant)
        .await
        .unwrap()
        .remove(0);
    let workspace = control
        .create_cloud_workspace(
            &owner,
            &tenant,
            &project.project_id,
            "Private workspace",
            1005,
        )
        .await
        .unwrap();
    // Separate connection pools model two Server processes, not two subscribers
    // sharing an in-memory broadcast channel.
    let first = CloudSessionEventFeed::connect(url).await.unwrap();
    let second = CloudSessionEventFeed::connect(url).await.unwrap();
    let mut first_rx = first.subscribe();
    let mut second_rx = second.subscribe();
    let kind = ResourceKind::Workspace;
    let id = workspace.workspace_id.as_str();
    control
        .set_resource_share(
            &owner,
            &tenant,
            kind,
            id,
            &reader.user_id,
            Some(ResourcePermissions {
                view: true,
                ..Default::default()
            }),
            1006,
        )
        .await
        .unwrap();
    changed(&mut first_rx, &tenant).await;
    changed(&mut second_rx, &tenant).await;
    assert!(
        control
            .resource_access(&reader, &tenant, kind, id)
            .await
            .unwrap()
            .permissions
            .view
    );

    let mut transaction = cloud.database().tenant_transaction(&tenant).await.unwrap();
    sqlx::query("DELETE FROM control_resource_shares WHERE tenant_id=$1 AND resource_id=$2")
        .bind(tenant.as_str())
        .bind(id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    quiet(&mut second_rx).await;
    transaction.rollback().await.unwrap();
    quiet(&mut second_rx).await;
    assert!(
        control
            .resource_access(&reader, &tenant, kind, id)
            .await
            .unwrap()
            .permissions
            .view
    );

    control
        .set_resource_share(&owner, &tenant, kind, id, &reader.user_id, None, 1007)
        .await
        .unwrap();
    changed(&mut first_rx, &tenant).await;
    changed(&mut second_rx, &tenant).await;
    assert!(
        !control
            .resource_access(&reader, &tenant, kind, id)
            .await
            .unwrap()
            .permissions
            .view
    );
    quiet(&mut first_rx).await;
    quiet(&mut second_rx).await;
    drop(first);
    drop(second);
    cloud.database().close().await;
}

#[tokio::test]
async fn sqlite_sharing_changes_reach_independent_servers_only_after_commit() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("resources.sqlite3").display()
    );
    contract(&url, None).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn restricted_postgres_sharing_changes_reach_independent_servers_only_after_commit() {
    let owner = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(owner.contains("ternilo_cloud_test"));
    let runtime = server_runtime::initialize(&owner, "ternilo_resource_live_test", [59; 32]).await;
    server_runtime::assert_scoped_without_schema_access(&runtime).await;
    contract(&runtime, Some(&owner)).await;
}
