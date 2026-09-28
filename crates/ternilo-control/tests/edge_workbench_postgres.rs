#[path = "support/edge_usage.rs"]
mod edge_usage;

#[path = "support/edge_models.rs"]
mod edge_models;

#[path = "support/edge_history.rs"]
mod edge_history;

#[path = "support/edge_provenance.rs"]
mod edge_provenance;

#[path = "support/edge_uploads.rs"]
mod edge_uploads;

#[path = "support/postgres.rs"]
mod postgres_runtime;

use std::{collections::BTreeSet, time::Duration};

use serde_json::json;
use sqlx::Executor;
use ternilo_control::{
    ControlStore, ControlUser, EdgeSessionMetadata, InstanceMode, NativeRegistration,
    OidcPrincipal, ResourceKind, ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    ErrorCode, HarnessError, PermissionPreset, RunId, SessionEvent, SessionEventKind, SessionId,
    SessionMode, TenantId, WorkspaceId,
};
use ternilo_transport::{
    EXECUTOR_PROTOCOL_VERSION, ExecutorCapability, ExecutorHello, ExecutorId, ExecutorKind,
};

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_edge_workbench_enforces_placement_isolation_and_offline_lifecycle() {
    let (admin_url, runtime_url, store) = prepare_store().await;
    edge_contract(store, &admin_url, &runtime_url).await;
}

#[tokio::test]
async fn sqlite_edge_workbench_follows_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("edge.sqlite3").display()
    );
    let store = ControlStore::connect(&url, None, SecretCipher::from_key([31; 32]), 4)
        .await
        .unwrap();
    edge_contract(store, &url, &url).await;
}

async fn edge_contract(store: ControlStore, admin_url: &str, runtime_url: &str) {
    let fixture = create_fixture(&store).await;
    let mapping = create_mappings(&store, &fixture).await;
    assert_mapping_ownership(&store, &fixture, &mapping).await;
    assert_mapping_conflicts(&store, &fixture, &mapping).await;
    assert_event_cache(&store, &fixture, &mapping).await;
    edge_usage::verify(&store, &fixture).await;
    edge_uploads::contract(&store, &fixture).await;
    assert_ungrouped_lifecycle(&store, &fixture, &mapping).await;
    assert_shared_mapping_contract(&store, &fixture, &mapping).await;
    assert_group_mapping_contract(&store, &fixture, &mapping).await;
    edge_provenance::contract(&store, &fixture, &mapping).await;
    edge_models::contract(&store, &fixture, &mapping).await;
    assert_offline_reopen(store, admin_url, runtime_url, &fixture, &mapping).await;
}

struct EdgeFixture {
    now: u64,
    alice: ControlUser,
    bob: ControlUser,
    tenant_a: TenantId,
    tenant_b: TenantId,
    executor_a: ExecutorId,
    executor_b: ExecutorId,
    workspace_a: WorkspaceId,
    workspace_b: WorkspaceId,
}

struct MappingFixture {
    browser_session: SessionId,
    node_session: SessionId,
}

async fn prepare_store() -> (String, String, ControlStore) {
    let admin_url = std::env::var("TERNILO_TEST_DATABASE_URL")
        .expect("TERNILO_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_control_test"),
        "integration test refuses a database URL without ternilo_control_test"
    );
    let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    postgres_runtime::prepare_role(&admin, "ternilo_edge_runtime_test", "ternilo-edge-password")
        .await;
    let migration_store =
        ControlStore::connect(&admin_url, None, SecretCipher::from_key([31; 32]), 2)
            .await
            .unwrap();
    migration_store.health().await.unwrap();
    migration_store.database().close().await;
    admin.close().await;

    let mut runtime_url = admin_url
        .parse::<sqlx::any::AnyConnectOptions>()
        .unwrap()
        .database_url;
    runtime_url
        .set_username("ternilo_edge_runtime_test")
        .unwrap();
    runtime_url
        .set_password(Some("ternilo-edge-password"))
        .unwrap();
    let runtime_url = runtime_url.to_string();
    let store = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([31; 32]),
        4,
    )
    .await
    .unwrap();
    (admin_url, runtime_url, store)
}

async fn create_fixture(store: &ControlStore) -> EdgeFixture {
    let now = 1_900_000_000_000;
    let alice = user(store, "alice-edge", now).await;
    let bob = user(store, "bob-edge", now + 1).await;
    let tenant_a = store
        .create_tenant(
            &alice,
            "edge-tenant-a",
            "Edge Tenant A",
            TenantQuota::default(),
            now + 2,
        )
        .await
        .unwrap();
    let tenant_b = store
        .create_tenant(
            &bob,
            "edge-tenant-b",
            "Edge Tenant B",
            TenantQuota::default(),
            now + 3,
        )
        .await
        .unwrap();
    store
        .set_membership(
            &alice,
            &tenant_a.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 4,
        )
        .await
        .unwrap();
    let project_a = store
        .list_projects(&alice, &tenant_a.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let project_b = store
        .list_projects(&bob, &tenant_b.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let executor_a = enroll(
        store,
        &alice,
        &tenant_a.tenant_id,
        &project_a.project_id,
        "alice-node",
        now + 5,
    )
    .await;
    let executor_b = enroll(
        store,
        &bob,
        &tenant_b.tenant_id,
        &project_b.project_id,
        "bob-node",
        now + 6,
    )
    .await;
    let workspace_a = store
        .create_local_workspace(
            &alice,
            &tenant_a.tenant_id,
            &project_a.project_id,
            "Alice Laptop",
            (&executor_a, &WorkspaceId::new("node-workspace-a")),
            now + 7,
        )
        .await
        .unwrap();
    let workspace_b = store
        .create_local_workspace(
            &bob,
            &tenant_b.tenant_id,
            &project_b.project_id,
            "Bob Laptop",
            (&executor_b, &WorkspaceId::new("node-workspace-b")),
            now + 8,
        )
        .await
        .unwrap();

    EdgeFixture {
        now,
        alice,
        bob,
        tenant_a: tenant_a.tenant_id,
        tenant_b: tenant_b.tenant_id,
        executor_a,
        executor_b,
        workspace_a: workspace_a.workspace_id,
        workspace_b: workspace_b.workspace_id,
    }
}

async fn create_mappings(store: &ControlStore, fixture: &EdgeFixture) -> MappingFixture {
    let browser_session = SessionId::new("browser-session");
    let mapping_a = store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-session-a"),
            Some(&browser_session),
            metadata("Alice cached title", None, fixture.now + 9),
            fixture.now + 9,
        )
        .await
        .unwrap();
    // Browser IDs are scoped by tenant; the same opaque ID in another tenant
    // resolves to Bob's independent Node mapping.
    store
        .create_edge_session_mapping(
            &fixture.bob,
            &fixture.tenant_b,
            &fixture.workspace_b,
            &fixture.executor_b,
            &SessionId::new("node-session-b"),
            Some(&browser_session),
            metadata("Bob cached title", None, fixture.now + 10),
            fixture.now + 10,
        )
        .await
        .unwrap();
    MappingFixture {
        browser_session,
        node_session: mapping_a.node_session_id,
    }
}

async fn assert_mapping_ownership(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    assert_eq!(
        store
            .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
            .await
            .unwrap()
            .unwrap()
            .node_session_id
            .as_str(),
        "node-session-a"
    );
    assert_eq!(
        store
            .find_owned_edge_session(&fixture.bob, &fixture.tenant_b, &mapping.browser_session)
            .await
            .unwrap()
            .unwrap()
            .node_session_id
            .as_str(),
        "node-session-b"
    );
    // Bob is a member of tenant A, but cannot observe or mutate Alice's
    // owner-scoped mapping. Missing and foreign IDs are intentionally equal.
    assert!(
        store
            .find_owned_edge_session(&fixture.bob, &fixture.tenant_a, &mapping.browser_session)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .resolve_owned_workspace(&fixture.bob, &fixture.tenant_a, &fixture.workspace_a)
            .await
            .is_err()
    );
}

async fn assert_mapping_conflicts(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    assert!(
        store
            .create_edge_session_mapping(
                &fixture.bob,
                &fixture.tenant_a,
                &fixture.workspace_a,
                &fixture.executor_a,
                &SessionId::new("foreign-node-session"),
                Some(&SessionId::new("foreign-browser-session")),
                metadata("foreign", None, fixture.now + 11),
                fixture.now + 11,
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_edge_session_mapping(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.workspace_a,
                &fixture.executor_a,
                &SessionId::new("different-node-session"),
                Some(&mapping.browser_session),
                metadata("duplicate browser", None, fixture.now + 12),
                fixture.now + 12,
            )
            .await
            .is_err()
    );
    assert!(
        store
            .create_edge_session_mapping(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.workspace_a,
                &fixture.executor_a,
                &mapping.node_session,
                Some(&SessionId::new("different-browser-session")),
                metadata("duplicate node", None, fixture.now + 13),
                fixture.now + 13,
            )
            .await
            .is_err()
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify event replay, conflict rejection, and generated titles against one canonical stream."
)]
async fn assert_event_cache(store: &ControlStore, fixture: &EdgeFixture, mapping: &MappingFixture) {
    let edge = store.edge_store();
    edge.register_executor(
        &fixture.tenant_a,
        &hello(fixture.executor_a.clone()),
        fixture.now + 14,
    )
    .await
    .unwrap();
    let local_only_session = SessionId::new("node-local-only-session");
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &local_only_session,
        &[SessionEvent {
            seq: 0,
            occurred_at_ms: fixture.now + 15,
            run_id: RunId::new("local-only-run"),
            kind: SessionEventKind::TurnStarted,
        }],
    )
    .await
    .unwrap();
    assert!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &local_only_session)
            .await
            .unwrap()
            .is_empty(),
        "events from Node-local sessions must not enter the Control cache"
    );
    let first = SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now + 15,
        run_id: RunId::new("edge-run"),
        kind: SessionEventKind::TurnStarted,
    };
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        std::slice::from_ref(&first),
    )
    .await
    .unwrap();
    assert_eq!(
        edge.event_cursors(&fixture.tenant_a, &fixture.executor_a)
            .await
            .unwrap(),
        vec![ternilo_transport::SessionCursor {
            session_id: mapping.node_session.clone(),
            last_seq: Some(0),
        }]
    );
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        std::slice::from_ref(&first),
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
        )
        .await
        .unwrap()
        .len(),
        1,
        "replayed event batches must remain idempotent"
    );
    assert!(
        edge.merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            &[SessionEvent {
                seq: 2,
                occurred_at_ms: fixture.now + 17,
                run_id: RunId::new("edge-run"),
                kind: SessionEventKind::TurnFinished {
                    answer: "skipped".to_owned(),
                    finish_reason: ternilo_protocol::TurnFinishReason::Completed,
                },
            }],
        )
        .await
        .is_err(),
        "a reconnect batch may not skip the durable cursor"
    );
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &[SessionEvent {
            seq: 1,
            occurred_at_ms: fixture.now + 16,
            run_id: RunId::new("edge-run"),
            kind: SessionEventKind::TurnFinished {
                answer: "reconnected".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        edge.event_cursors(&fixture.tenant_a, &fixture.executor_a)
            .await
            .unwrap()[0]
            .last_seq,
        Some(1)
    );
    let cached = store
        .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached.last_event_seq, Some(1));
    assert_eq!(cached.metadata.title, "Alice cached title");
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &[SessionEvent {
            seq: 2,
            occurred_at_ms: fixture.now + 18,
            run_id: RunId::new("edge-run"),
            kind: SessionEventKind::SessionTitleGenerated {
                title: "Late generated title".to_owned(),
            },
        }],
    )
    .await
    .unwrap();
    let manually_named = store
        .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manually_named.metadata.title, "Alice cached title");

    let mut pending = manually_named.metadata;
    "New session".clone_into(&mut pending.title);
    pending.updated_at_ms = fixture.now + 19;
    store
        .update_edge_session_metadata(
            &fixture.alice,
            &fixture.tenant_a,
            &mapping.browser_session,
            &mapping.node_session,
            pending,
            fixture.now + 19,
        )
        .await
        .unwrap();
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &[SessionEvent {
            seq: 3,
            occurred_at_ms: fixture.now + 20,
            run_id: RunId::new("edge-run-2"),
            kind: SessionEventKind::SessionTitleGenerated {
                title: "Generated edge title".to_owned(),
            },
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
            .await
            .unwrap()
            .unwrap()
            .metadata
            .title,
        "Generated edge title"
    );
    edge_history::verify(store, fixture, mapping).await;
}

async fn assert_ungrouped_lifecycle(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    store
        .unregister_owned_workspace(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            fixture.now + 16,
        )
        .await
        .unwrap();
    assert!(
        store
            .list_workspaces(&fixture.alice, &fixture.tenant_a)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .resolve_owned_workspace(&fixture.alice, &fixture.tenant_a, &fixture.workspace_a)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .resolve_owned_session_workspace(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.workspace_a,
            )
            .await
            .unwrap()
            .workspace_id,
        fixture.workspace_a,
    );
    assert!(
        store
            .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
            .await
            .unwrap()
            .is_some()
    );
    // A fork inherits the immutable placement and remains valid after its
    // Workspace registration has moved to the Ungrouped presentation bucket.
    store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-session-child"),
            Some(&SessionId::new("browser-session-child")),
            metadata(
                "Fork after unregister",
                Some(mapping.browser_session.clone()),
                fixture.now + 17,
            ),
            fixture.now + 17,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_owned_edge_sessions(&fixture.alice, &fixture.tenant_a)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise shared placement, canonical refresh, owner preservation and revocation against one durable Node mapping."
)]
async fn assert_shared_mapping_contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    let now = fixture.now + 30;
    let bootstrap = store
        .initialize_owner(
            &NativeRegistration {
                email: "instance-owner@example.test".to_owned(),
                username: "instance-owner".to_owned(),
                password: "instance-owner-password".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    store
        .set_instance_mode(
            &bootstrap.session.user,
            InstanceMode::MultiUser,
            bootstrap.session.instance.revision,
            now + 1,
        )
        .await
        .unwrap();
    let view = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    let submit = ResourcePermissions {
        submit: true,
        ..view
    };
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Session,
            mapping.browser_session.as_str(),
            &fixture.bob.user_id,
            Some(view),
            now + 2,
        )
        .await
        .unwrap();
    clear_stored_share_permissions(store, fixture, "session", mapping.browser_session.as_str())
        .await;
    assert!(
        store
            .list_accessible_edge_sessions(&fixture.bob, &fixture.tenant_a)
            .await
            .unwrap()
            .is_empty()
    );
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Session,
            mapping.browser_session.as_str(),
            &fixture.bob.user_id,
            Some(view),
            now + 2,
        )
        .await
        .unwrap();
    let shared = store
        .find_accessible_edge_session(&fixture.bob, &fixture.tenant_a, &mapping.browser_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared.owner_user_id, fixture.alice.user_id);
    assert!(
        store
            .find_owned_edge_session(&fixture.bob, &fixture.tenant_a, &mapping.browser_session,)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .list_accessible_edge_sessions(&fixture.bob, &fixture.tenant_a,)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .list_accessible_workspaces(&fixture.bob, &fixture.tenant_a,)
            .await
            .unwrap()
            .is_empty()
    );
    // A session grant preserves its immutable binding even after unregistering
    // the workspace; it does not grant access to the workspace itself.
    assert_eq!(
        store
            .resolve_accessible_session_workspace(
                &fixture.bob,
                &fixture.tenant_a,
                &mapping.browser_session,
            )
            .await
            .unwrap()
            .workspace_id,
        fixture.workspace_a
    );
    assert_eq!(
        store
            .resolve_accessible_workspace(&fixture.bob, &fixture.tenant_a, &fixture.workspace_a,)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let cached = store
        .update_edge_session_metadata(
            &fixture.bob,
            &fixture.tenant_a,
            &mapping.browser_session,
            &mapping.node_session,
            shared.metadata,
            now + 3,
        )
        .await
        .unwrap();
    assert_eq!(cached.owner_user_id, fixture.alice.user_id);
    let child = SessionId::new("shared-fork");
    let node_child = SessionId::new("node-shared-fork");
    assert_eq!(
        store
            .create_edge_session_mapping(
                &fixture.bob,
                &fixture.tenant_a,
                &fixture.workspace_a,
                &fixture.executor_a,
                &node_child,
                Some(&child),
                metadata(
                    "Shared fork",
                    Some(mapping.browser_session.clone()),
                    now + 4
                ),
                now + 4,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Session,
            mapping.browser_session.as_str(),
            &fixture.bob.user_id,
            Some(submit),
            now + 5,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .create_edge_session_mapping(
                &fixture.bob,
                &fixture.tenant_a,
                &fixture.workspace_b,
                &fixture.executor_b,
                &node_child,
                Some(&child),
                metadata(
                    "Forged placement",
                    Some(mapping.browser_session.clone()),
                    now + 6
                ),
                now + 6,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let fork = store
        .create_edge_session_mapping(
            &fixture.bob,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &node_child,
            Some(&child),
            metadata(
                "Shared fork",
                Some(mapping.browser_session.clone()),
                now + 7,
            ),
            now + 7,
        )
        .await
        .unwrap();
    assert_eq!(fork.owner_user_id, fixture.alice.user_id);
    assert_eq!(fork.workspace_id, fixture.workspace_a);
    let fork_access = store
        .resource_access(
            &fixture.bob,
            &fixture.tenant_a,
            ResourceKind::Session,
            child.as_str(),
        )
        .await
        .unwrap();
    assert!(!fork_access.is_owner);
    assert_eq!(fork_access.permissions, submit);
    assert!(
        store
            .find_accessible_edge_session(&fixture.bob, &fixture.tenant_a, &child,)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .delete_edge_session_mapping(
                &fixture.bob,
                &fixture.tenant_a,
                &mapping.browser_session,
                &mapping.node_session,
                now + 8,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );

    let project = store
        .resolve_owned_session_workspace(&fixture.alice, &fixture.tenant_a, &fixture.workspace_a)
        .await
        .unwrap()
        .project_id;
    let workspace = store
        .create_local_workspace(
            &fixture.alice,
            &fixture.tenant_a,
            &project,
            "Shared Node workspace",
            (
                &fixture.executor_a,
                &WorkspaceId::new("node-shared-workspace"),
            ),
            now + 9,
        )
        .await
        .unwrap();
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Workspace,
            workspace.workspace_id.as_str(),
            &fixture.bob.user_id,
            Some(view),
            now + 10,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_accessible_workspace(&fixture.bob, &fixture.tenant_a, &workspace.workspace_id,)
            .await
            .unwrap()
            .owner_user_id,
        fixture.alice.user_id
    );
    assert_eq!(
        store
            .list_accessible_workspaces(&fixture.bob, &fixture.tenant_a,)
            .await
            .unwrap()
            .len(),
        1
    );
    let session_id = SessionId::new("shared-workspace-session");
    let node_session_id = SessionId::new("node-shared-workspace-session");
    assert_eq!(
        store
            .create_edge_session_mapping(
                &fixture.bob,
                &fixture.tenant_a,
                &workspace.workspace_id,
                &fixture.executor_a,
                &node_session_id,
                Some(&session_id),
                metadata("New session", None, now + 11),
                now + 11,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Workspace,
            workspace.workspace_id.as_str(),
            &fixture.bob.user_id,
            Some(submit),
            now + 12,
        )
        .await
        .unwrap();
    let created = store
        .create_edge_session_mapping(
            &fixture.bob,
            &fixture.tenant_a,
            &workspace.workspace_id,
            &fixture.executor_a,
            &node_session_id,
            Some(&session_id),
            metadata("New session", None, now + 13),
            now + 13,
        )
        .await
        .unwrap();
    assert_eq!(created.owner_user_id, fixture.alice.user_id);
    // The adapter has already authorized the Node operation. A View grant can
    // refresh canonical metadata without pretending that the reader owns it.
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Workspace,
            workspace.workspace_id.as_str(),
            &fixture.bob.user_id,
            Some(view),
            now + 14,
        )
        .await
        .unwrap();
    let generated = store
        .apply_generated_edge_title(
            &fixture.bob,
            &fixture.tenant_a,
            &session_id,
            &node_session_id,
            "Generated shared title",
            now + 15,
        )
        .await
        .unwrap();
    assert_eq!(generated.metadata.title, "Generated shared title");
    assert_eq!(generated.owner_user_id, fixture.alice.user_id);
    let updated = store
        .update_edge_session_metadata(
            &fixture.bob,
            &fixture.tenant_a,
            &session_id,
            &node_session_id,
            generated.metadata,
            now + 16,
        )
        .await
        .unwrap();
    assert_eq!(updated.owner_user_id, fixture.alice.user_id);
    clear_stored_share_permissions(store, fixture, "workspace", workspace.workspace_id.as_str())
        .await;
    assert!(
        store
            .list_accessible_workspaces(&fixture.bob, &fixture.tenant_a)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .list_accessible_edge_sessions(&fixture.bob, &fixture.tenant_a)
            .await
            .unwrap()
            .iter()
            .any(|session| session.session_id == session_id)
    );
    for (kind, id) in [
        (ResourceKind::Session, mapping.browser_session.as_str()),
        (ResourceKind::Workspace, workspace.workspace_id.as_str()),
    ] {
        store
            .set_resource_share(
                &fixture.alice,
                &fixture.tenant_a,
                kind,
                id,
                &fixture.bob.user_id,
                None,
                now + 17,
            )
            .await
            .unwrap();
    }
    assert!(
        store
            .find_accessible_edge_session(&fixture.bob, &fixture.tenant_a, &session_id,)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.find_accessible_edge_session(
        &fixture.bob, &fixture.tenant_a, &mapping.browser_session,
    ).await.unwrap().is_none());
    assert_eq!(
        store
            .update_edge_session_metadata(
                &fixture.bob,
                &fixture.tenant_a,
                &session_id,
                &node_session_id,
                updated.metadata,
                now + 18,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        store
            .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &session_id,)
            .await
            .unwrap()
            .is_some()
    );
    let audit = store
        .list_audit(&fixture.alice, &fixture.tenant_a, 100)
        .await
        .unwrap();
    for id in [&child, &session_id] {
        assert!(
            audit
                .iter()
                .any(|entry| entry.action == "edge_session.create"
                    && entry.resource_id == id.as_str()
                    && entry.actor_user_id.as_ref() == Some(&fixture.bob.user_id)
                    && entry.metadata["owner_user_id"] == fixture.alice.user_id.as_str())
        );
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise group-derived edge forks, dynamic access, and source cleanup on both database backends."
)]
async fn assert_group_mapping_contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    let now = fixture.now + 100;
    let owner = &fixture.alice;
    let member = &fixture.bob;
    let tenant = &fixture.tenant_a;
    let group = store
        .create_permission_group(
            owner,
            tenant,
            &ternilo_control::GroupInput {
                name: "Edge contributors".to_owned(),
                description: None,
            },
            now,
        )
        .await
        .unwrap();
    store
        .set_permission_group_member(
            owner,
            tenant,
            &group.group_id,
            &member.user_id,
            true,
            now + 1,
        )
        .await
        .unwrap();
    let parent = SessionId::new("group-parent");
    store
        .create_edge_session_mapping(
            owner,
            tenant,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-group-parent"),
            Some(&parent),
            metadata(
                "Group parent",
                Some(mapping.browser_session.clone()),
                now + 2,
            ),
            now + 2,
        )
        .await
        .unwrap();
    store
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            parent.as_str(),
            &group.group_id,
            Some(ResourcePermissions::OWNER),
            now + 3,
        )
        .await
        .unwrap();
    let child = SessionId::new("group-fork");
    store
        .create_edge_session_mapping(
            member,
            tenant,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-group-fork"),
            Some(&child),
            metadata("Group fork", Some(parent.clone()), now + 4),
            now + 4,
        )
        .await
        .unwrap();
    let grandchild = SessionId::new("group-fork-again");
    store
        .create_edge_session_mapping(
            member,
            tenant,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-group-fork-again"),
            Some(&grandchild),
            metadata("Group fork again", Some(child.clone()), now + 5),
            now + 5,
        )
        .await
        .unwrap();
    let visible = store
        .list_accessible_edge_sessions(member, tenant)
        .await
        .unwrap();
    assert!(visible.iter().any(|session| session.session_id == child));
    assert!(
        visible
            .iter()
            .any(|session| session.session_id == grandchild)
    );
    let access = store
        .resource_access(member, tenant, ResourceKind::Session, grandchild.as_str())
        .await
        .unwrap();
    assert!(access.sources.iter().any(|source| source.kind
        == ternilo_control::ResourceAccessSourceKind::Fork
        && source.resource_id == parent.as_str()));
    let page = store
        .list_resource_shares(
            owner,
            tenant,
            ResourceKind::Session,
            child.as_str(),
            &ternilo_control::PageQuery::default(),
        )
        .await
        .unwrap();
    assert!(page.shares[0].inherited);
    let read = ResourcePermissions {
        view: true,
        ..Default::default()
    };
    store
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            parent.as_str(),
            &group.group_id,
            Some(read),
            now + 6,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resource_access(member, tenant, ResourceKind::Session, grandchild.as_str())
            .await
            .unwrap()
            .permissions,
        read
    );
    store
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            child.as_str(),
            &member.user_id,
            Some(read),
            now + 6,
        )
        .await
        .unwrap();
    store
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            parent.as_str(),
            &group.group_id,
            Some(ResourcePermissions::OWNER),
            now + 6,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resource_access(member, tenant, ResourceKind::Session, child.as_str())
            .await
            .unwrap()
            .permissions,
        read,
        "the owner's explicit view grant must replace the child's inherited execution permission"
    );
    assert!(
        store
            .resource_access(member, tenant, ResourceKind::Session, grandchild.as_str())
            .await
            .unwrap()
            .permissions
            .submit,
        "converting one child does not change other fork references"
    );
    let grants = store
        .list_resource_shares(
            owner,
            tenant,
            ResourceKind::Session,
            child.as_str(),
            &ternilo_control::PageQuery::default(),
        )
        .await
        .unwrap();
    assert!(!grants.shares[0].inherited);
    store
        .set_permission_group_member(
            owner,
            tenant,
            &group.group_id,
            &member.user_id,
            false,
            now + 7,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resource_access(member, tenant, ResourceKind::Session, child.as_str())
            .await
            .unwrap()
            .permissions,
        read,
        "the owner's independent grant survives group removal"
    );
    assert!(
        store
            .find_accessible_edge_session(member, tenant, &grandchild)
            .await
            .unwrap()
            .is_none()
    );
    store
        .set_permission_group_member(
            owner,
            tenant,
            &group.group_id,
            &member.user_id,
            true,
            now + 8,
        )
        .await
        .unwrap();
    assert!(
        store
            .find_accessible_edge_session(member, tenant, &grandchild)
            .await
            .unwrap()
            .is_none(),
        "rejoining a group must not resurrect removed fork references"
    );
    store
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            parent.as_str(),
            &group.group_id,
            Some(ResourcePermissions::OWNER),
            now + 9,
        )
        .await
        .unwrap();
    let last = SessionId::new("group-fork-before-delete");
    store
        .create_edge_session_mapping(
            member,
            tenant,
            &fixture.workspace_a,
            &fixture.executor_a,
            &SessionId::new("node-group-fork-before-delete"),
            Some(&last),
            metadata("Last group fork", Some(parent.clone()), now + 10),
            now + 10,
        )
        .await
        .unwrap();
    store
        .delete_edge_session_mapping(
            owner,
            tenant,
            &parent,
            &SessionId::new("node-group-parent"),
            now + 11,
        )
        .await
        .unwrap();
    assert!(
        store
            .find_accessible_edge_session(member, tenant, &last)
            .await
            .unwrap()
            .is_none(),
        "deleting the source must revoke derived group access"
    );
    assert!(
        store
            .find_owned_edge_session(owner, tenant, &last)
            .await
            .unwrap()
            .is_some()
    );
}

async fn clear_stored_share_permissions(
    store: &ControlStore,
    fixture: &EdgeFixture,
    kind: &str,
    id: &str,
) {
    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    sqlx::query("UPDATE control_resource_shares SET permissions_json=$1 WHERE tenant_id=$2 AND resource_kind=$3 AND resource_id=$4 AND grantee_user_id=$5")
        .bind(ternilo_storage::Json(ResourcePermissions::default())).bind(fixture.tenant_a.as_str())
        .bind(kind).bind(id).bind(fixture.bob.user_id.as_str())
        .execute(&mut *transaction).await.unwrap();
    transaction.commit().await.unwrap();
}

async fn assert_offline_reopen(
    store: ControlStore,
    admin_url: &str,
    runtime_url: &str,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
) {
    // Dropping and reopening the Control store models a fully offline Node and
    // a Control restart: resource metadata and the last cursor remain durable.
    drop(store);
    let reopened = ControlStore::connect(
        runtime_url,
        Some(admin_url),
        SecretCipher::from_key([31; 32]),
        2,
    )
    .await
    .unwrap();
    edge_uploads::assert_reopened(&reopened, fixture).await;
    edge_provenance::assert_reopened(&reopened, fixture).await;
    let offline = reopened
        .find_owned_edge_session(&fixture.alice, &fixture.tenant_a, &mapping.browser_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(offline.metadata.title, "Generated edge title");
    assert_eq!(offline.last_event_seq, Some(1210));
    assert_eq!(
        reopened
            .edge_store()
            .events(
                &fixture.tenant_a,
                &fixture.executor_a,
                &offline.node_session_id,
            )
            .await
            .unwrap()
            .len(),
        1211
    );
}

async fn user(store: &ControlStore, subject: &str, now_ms: u64) -> ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: subject.to_owned(),
                email: Some(format!("{subject}@example.com")),
                display_name: Some(subject.to_owned()),
            },
            &format!("test-{subject}"),
            now_ms,
        )
        .await
        .unwrap()
}

async fn enroll(
    store: &ControlStore,
    actor: &ControlUser,
    tenant_id: &TenantId,
    project_id: &str,
    executor_id: &str,
    now_ms: u64,
) -> ExecutorId {
    let executor_id = ExecutorId::new(executor_id);
    let grant = store
        .create_enrollment(
            actor,
            tenant_id,
            Some(project_id),
            executor_id.clone(),
            Duration::from_mins(5),
            now_ms,
        )
        .await
        .unwrap();
    let credential = store
        .consume_enrollment(&grant.token, now_ms + 1)
        .await
        .unwrap();
    store
        .authenticate_node(&credential.token, now_ms + 2)
        .await
        .unwrap();
    executor_id
}

fn metadata(title: &str, parent_session_id: Option<SessionId>, now_ms: u64) -> EdgeSessionMetadata {
    EdgeSessionMetadata {
        server_model: None,
        parent_session_id,
        subagent: None,
        title: title.to_owned(),
        archived_at_ms: None,
        blank: false,
        permissions: PermissionPreset::WorkspaceWrite,
        model: json!({ "provider": "profile_default" }),
        agent_preset: "standard".to_owned(),
        preset_plugins: Vec::new(),
        profile_plugins: Vec::new(),
        mode: SessionMode::Execute,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

fn hello(executor_id: ExecutorId) -> ExecutorHello {
    ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id,
        executor_kind: ExecutorKind::EdgeNode,
        instance_nonce: "postgres-edge-test".to_owned(),
        catalog_revision: "test-catalog".to_owned(),
        capabilities: BTreeSet::from([
            ExecutorCapability::ApplicationRpc,
            ExecutorCapability::SessionEventDelta,
        ]),
    }
}

#[test]
fn edge_metadata_rejects_inconsistent_timestamps() {
    let invalid = EdgeSessionMetadata {
        created_at_ms: 20,
        updated_at_ms: 19,
        ..metadata("invalid", None, 20)
    };
    assert_eq!(
        invalid.validate().unwrap_err().code,
        HarnessError::invalid("x").code
    );
}
