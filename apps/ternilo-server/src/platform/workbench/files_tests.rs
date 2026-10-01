use std::{fmt::Write as _, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use salvo_core::{
    Service,
    http::StatusCode,
    prelude::Response,
    test::{ResponseExt as _, TestClient},
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use ternilo_cloud::{CloudSessionDraft, CloudSessionEventFeed, CloudSessionRecord, CloudStore};
use ternilo_control::{
    ControlStore, EdgeSessionMetadata, EdgeSessionRecord, InstanceMode, NativeRegistration,
    NativeSessionGrant, OidcPrincipal, ResourceKind, ResourcePermissions, SecretCipher,
    TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, Attachment, FileSourceStatus, OfflineFileSource, PermissionPreset, RunId,
    SessionEvent, SessionEventKind, SessionFileContent, SessionFilePage, SessionFileQuery,
    SessionId, SessionMode, TenantId, WorkspaceId,
};
use ternilo_transport::ExecutorId;

use crate::platform::{edge::EdgeGateway, http::now_ms, state::AppState};

#[path = "files_accepted_tests.rs"]
mod accepted_tests;

#[path = "archive_tests.rs"]
mod archive_tests;

#[path = "files_transport_tests.rs"]
mod transport_tests;

#[path = "files_tests/event_sync_tests.rs"]
mod event_sync_tests;

#[path = "files_tests/read_revocation_tests.rs"]
mod read_revocation_tests;

#[path = "files_tests/project_sharing_tests.rs"]
mod project_sharing_tests;

#[path = "files_tests/services_tests.rs"]
mod services_tests;

#[path = "files_tests/workspace_location_tests.rs"]
mod workspace_location_tests;

#[path = "files_tests/preset_view_tests.rs"]
mod preset_view_tests;

#[path = "files_tests/resource_ownership_tests.rs"]
mod resource_ownership_tests;

struct Fixture {
    state: AppState,
    service: Service,
    owner: NativeSessionGrant,
    tenant: TenantId,
    now: u64,
    _shutdown: tokio::sync::watch::Sender<bool>,
}

impl Fixture {
    async fn new(url: &str, schema_owner_url: Option<&str>) -> Self {
        let now = now_ms().unwrap();
        let store =
            ControlStore::connect(url, schema_owner_url, SecretCipher::from_key([83; 32]), 1)
                .await
                .unwrap();
        let owner = store
            .initialize_owner(
                &NativeRegistration {
                    email: "files-owner@example.test".to_owned(),
                    username: "files-owner".to_owned(),
                    password: "files-owner-test-password".to_owned(),
                },
                now,
            )
            .await
            .unwrap();
        store
            .set_instance_mode(&owner.session.user, InstanceMode::MultiUser, 1, now)
            .await
            .unwrap();
        let tenant = store
            .create_tenant(
                &owner.session.user,
                "files-team",
                "Files team",
                TenantQuota::default(),
                now,
            )
            .await
            .unwrap()
            .tenant_id;
        if let Some(owner_url) = schema_owner_url {
            let database = ternilo_storage::Database::connect(owner_url, 1)
                .await
                .unwrap();
            CloudStore::from_database(database.clone()).await.unwrap();
            crate::gateway_journal::GatewayJournal::open(database.clone())
                .await
                .unwrap();
            database.close().await;
        }
        let cloud = CloudStore::from_database(store.database().clone())
            .await
            .unwrap();
        let catalog = ternilo_cloud::catalog().unwrap();
        let policy = crate::platform::load_worker_policy(None, &catalog).unwrap();
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let state = AppState {
            cloud_events: CloudSessionEventFeed::from_database(store.database().clone())
                .await
                .unwrap(),
            edge: Arc::new(EdgeGateway::new(store.edge_store()).await.unwrap()),
            store,
            cloud,
            security: Arc::default(),
            setup_token_hash: None,
            managed_execution_enabled: false,
            shutdown: receiver,
            worker_policy: Arc::new(policy),
            catalog: Arc::new(catalog),
        };
        let service = Service::new(crate::platform::web_router(state.clone()));
        Self {
            state,
            service,
            owner,
            tenant,
            now,
            _shutdown: shutdown,
        }
    }

    async fn session(&self, name: &str) -> CloudSessionRecord {
        let user = &self.owner.session.user;
        let project = self
            .state
            .store
            .create_project(user, &self.tenant, name, self.now)
            .await
            .unwrap();
        let workspace = self
            .state
            .store
            .create_cloud_workspace(user, &self.tenant, &project.project_id, name, self.now)
            .await
            .unwrap();
        self.state
            .cloud
            .create_session(
                CloudSessionDraft {
                    project_id: project.project_id,
                    workspace_id: workspace.workspace_id,
                    session_id: Some(SessionId::new(name)),
                    agent_id: AgentId::new("files-agent"),
                    title: name.to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 32768,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &self.tenant,
                &user.user_id,
                self.now,
            )
            .await
            .unwrap()
    }

    async fn edge_session(&self, name: &str) -> EdgeSessionRecord {
        self.edge_session_with_credential(name).await.0
    }

    async fn edge_session_with_credential(
        &self,
        name: &str,
    ) -> (EdgeSessionRecord, ternilo_control::NodeCredentialGrant) {
        let user = &self.owner.session.user;
        let executor = ExecutorId::new(format!("executor-{name}"));
        let enrollment = self
            .state
            .store
            .create_owned_enrollment(
                user,
                &self.tenant,
                None,
                executor.clone(),
                Duration::from_secs(60),
                self.now,
            )
            .await
            .unwrap();
        let credential = self
            .state
            .store
            .consume_enrollment(&enrollment.token, self.now + 1)
            .await
            .unwrap();
        let project = self
            .state
            .store
            .create_project(user, &self.tenant, name, self.now)
            .await
            .unwrap();
        let workspace = self
            .state
            .store
            .create_local_workspace(
                user,
                &self.tenant,
                &project.project_id,
                name,
                (
                    &executor,
                    &WorkspaceId::new(format!("node-workspace-{name}")),
                ),
                self.now,
            )
            .await
            .unwrap();
        let session = self
            .state
            .store
            .create_edge_session_mapping(
                user,
                &self.tenant,
                &workspace.workspace_id,
                &executor,
                &SessionId::new(format!("node-session-{name}")),
                Some(&SessionId::new(name)),
                EdgeSessionMetadata {
                    server_model: None,
                    parent_session_id: None,
                    subagent: None,
                    title: name.to_owned(),
                    archived_at_ms: Some(self.now + 2),
                    blank: false,
                    permissions: PermissionPreset::ReadOnly,
                    model: json!({"kind": "profile_default"}),
                    agent_preset: "standard".to_owned(),
                    preset_plugins: vec![],
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                    created_at_ms: self.now,
                    updated_at_ms: self.now + 2,
                },
                self.now + 2,
            )
            .await
            .unwrap();
        (session, credential)
    }

    async fn collaborator(&self) -> NativeSessionGrant {
        let user = self
            .state
            .store
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://files.test.invalid".to_owned(),
                    subject: "collaborator".to_owned(),
                    email: None,
                    display_name: Some("Files collaborator".to_owned()),
                },
                "test-collaborator",
                self.now,
            )
            .await
            .unwrap();
        self.state
            .store
            .set_membership(
                &self.owner.session.user,
                &self.tenant,
                &user.user_id,
                TenantRole::Member,
                self.now,
            )
            .await
            .unwrap();
        self.state
            .store
            .create_browser_session(user, self.now)
            .await
            .unwrap()
    }

    async fn get(&self, path: &str, account: &NativeSessionGrant, tenant: &TenantId) -> Response {
        TestClient::get(format!("http://server.test/api/v1{path}"))
            .add_header(
                "Authorization",
                format!("Bearer {}", account.access_token),
                true,
            )
            .add_header("x-ternilo-tenant", tenant.as_str(), true)
            .send(&self.service)
            .await
    }

    async fn page(&self, path: &str, account: &NativeSessionGrant) -> (SessionFilePage, Value) {
        let mut response = self.get(path, account, &self.tenant).await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let body: Value = response.take_json().await.unwrap();
        (serde_json::from_value(body.clone()).unwrap(), body)
    }

    async fn content(
        &self,
        session: &SessionId,
        id: &str,
        account: &NativeSessionGrant,
    ) -> Vec<u8> {
        let mut response = self
            .get(
                &format!("/sessions/{session}/files/{id}/content"),
                account,
                &self.tenant,
            )
            .await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let body: SessionFileContent = response.take_json().await.unwrap();
        STANDARD.decode(body.content_base64).unwrap()
    }

    async fn event(&self, session: &CloudSessionRecord, seq: i64, kind: SessionEventKind) {
        let event = SessionEvent {
            seq: seq.try_into().unwrap(),
            occurred_at_ms: self.now + u64::try_from(seq).unwrap(),
            run_id: RunId::new("files-run"),
            kind,
        };
        let mut tx = self
            .state
            .cloud
            .database()
            .owner_transaction(&self.tenant, &self.owner.session.user.user_id)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cloud_session_events (tenant_id, session_id, seq, run_id, event, writer_fencing_token, created_at_ms) VALUES ($1,$2,$3,$4,$5,1,$6)")
            .bind(self.tenant.as_str()).bind(session.session_id.as_str()).bind(seq).bind(event.run_id.as_str())
            .bind(serde_json::to_string(&event).unwrap()).bind(i64::try_from(event.occurred_at_ms).unwrap())
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn upload(&self, session: &CloudSessionRecord, seq: i64, name: &str, content: &str) {
        self.event(
            session,
            seq,
            SessionEventKind::UserMessage {
                provenance: None,
                content: "Inspect the uploaded file".to_owned(),
                display_content: None,
                source: None,
                references: vec![],
                attachments: vec![Attachment {
                    name: name.to_owned(),
                    media_type: "text/plain".to_owned(),
                    content: content.to_owned(),
                }],
            },
        )
        .await;
    }

    async fn generated(&self, session: &CloudSessionRecord, seq: i64, bytes: &[u8]) -> String {
        let digest =
            Sha256::digest(bytes)
                .iter()
                .fold(String::with_capacity(64), |mut digest, byte| {
                    write!(digest, "{byte:02x}").expect("writing to a String cannot fail");
                    digest
                });
        let mut tx = self
            .state
            .cloud
            .database()
            .owner_transaction(&self.tenant, &self.owner.session.user.user_id)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cloud_attachment_objects (tenant_id, workspace_id, digest, content, created_at_ms) VALUES ($1,$2,$3,$4,$5)")
            .bind(self.tenant.as_str()).bind(session.workspace_id.as_str()).bind(&digest).bind(bytes)
            .bind(i64::try_from(self.now).unwrap()).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        self.event(
            session,
            seq,
            SessionEventKind::DeliverableProduced {
                path: "out/report.zip".to_owned(),
                operation: "write".to_owned(),
                attachment: Attachment {
                    name: "report.zip".to_owned(),
                    media_type: "application/zip".to_owned(),
                    content: format!("ternilo-attachment://sha256/{digest}"),
                },
            },
        )
        .await;
        digest
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify filtering, exact file versions, and pagination as one inventory lifecycle."
)]
async fn inventory_contract(fixture: &Fixture) {
    let session = fixture.session("versioned-files").await;
    let literal = "data:text/plain;base64,dGhpcyBpcyBsaXRlcmFs\r\n";
    fixture.upload(&session, 1, "literal.txt", literal).await;
    let previous = [0, 255, 80, 75, 3, 4, 128];
    let latest = [0, 254, 80, 75, 3, 4, 129];
    let first_digest = fixture.generated(&session, 2, &previous).await;
    let latest_digest = fixture.generated(&session, 3, &latest).await;
    let (first, raw) = fixture.page("/files?limit=1", &fixture.owner).await;
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].id, "generated-3-0");
    assert_eq!(first.items[0].path.as_deref(), Some("out/report.zip"));
    assert!(!first.items[0].session_archived);
    let serialized = raw.to_string();
    for secret in [
        literal,
        first_digest.as_str(),
        latest_digest.as_str(),
        "content_base64",
        "ternilo-attachment://",
    ] {
        assert!(
            !serialized.contains(secret),
            "inventory must contain metadata only"
        );
    }
    let mut query = SessionFileQuery {
        limit: 1,
        cursor: first.next_cursor,
        ..SessionFileQuery::default()
    };
    let second = super::file_page(
        &fixture.state,
        &fixture.owner.session.user,
        &fixture.tenant,
        &query,
    )
    .await
    .unwrap();
    assert_eq!(second.items[0].id, "generated-2-0");
    query.cursor = second.next_cursor;
    let third = super::file_page(
        &fixture.state,
        &fixture.owner.session.user,
        &fixture.tenant,
        &query,
    )
    .await
    .unwrap();
    assert_eq!(third.items[0].id, "upload-1-0");
    assert!(third.next_cursor.is_none());
    let (filtered, _) = fixture
        .page(
            &format!(
                "/files?workspace_id={}&kind=generated&query=REPORT",
                session.workspace_id
            ),
            &fixture.owner,
        )
        .await;
    assert_eq!(filtered.items.len(), 2);
    let (uploads, _) = fixture
        .page(
            &format!("/files?session_id={}&kind=upload", session.session_id),
            &fixture.owner,
        )
        .await;
    assert_eq!(uploads.items.len(), 1);
    assert_eq!(
        fixture
            .content(&session.session_id, "upload-1-0", &fixture.owner)
            .await,
        literal.as_bytes()
    );
    assert_eq!(
        fixture
            .content(&session.session_id, "generated-2-0", &fixture.owner)
            .await,
        previous
    );
    assert_eq!(
        fixture
            .content(&session.session_id, "generated-3-0", &fixture.owner)
            .await,
        latest
    );
    for path in [
        "/files?limit=0",
        "/files?cursor=broken",
        "/files?kind=unknown",
    ] {
        assert_eq!(
            fixture
                .get(path, &fixture.owner, &fixture.tenant)
                .await
                .status_code,
            Some(StatusCode::BAD_REQUEST)
        );
    }
    keyset_ties_contract(fixture).await;
}

async fn keyset_ties_contract(fixture: &Fixture) {
    let alpha = fixture.session("keyset-alpha").await;
    let zeta = fixture.session("keyset-zeta").await;
    for (session, label) in [(&alpha, "alpha"), (&zeta, "zeta")] {
        fixture
            .event(
                session,
                1,
                SessionEventKind::UserMessage {
                    provenance: None,
                    content: "Uploads sharing the same event timestamp".to_owned(),
                    display_content: None,
                    source: None,
                    references: vec![],
                    attachments: (0..2)
                        .map(|index| Attachment {
                            name: format!("keyset-tie-{label}-{index}.txt"),
                            media_type: "text/plain".to_owned(),
                            content: format!("{label} attachment {index}"),
                        })
                        .collect(),
                },
            )
            .await;
    }
    let expected = [
        (&zeta.session_id, 1_u32, "keyset-tie-zeta-1.txt"),
        (&zeta.session_id, 0, "keyset-tie-zeta-0.txt"),
        (&alpha.session_id, 1, "keyset-tie-alpha-1.txt"),
        (&alpha.session_id, 0, "keyset-tie-alpha-0.txt"),
    ];
    let mut query = SessionFileQuery {
        query: Some("keyset-tie-".to_owned()),
        limit: 1,
        ..SessionFileQuery::default()
    };
    for (page_index, (session, attachment_index, name)) in expected.iter().enumerate() {
        let page = super::file_page(
            &fixture.state,
            &fixture.owner.session.user,
            &fixture.tenant,
            &query,
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
        let item = &page.items[0];
        assert_eq!(&item.session_id, *session);
        assert_eq!(item.event_seq, Some(1));
        assert_eq!(item.attachment_index, *attachment_index);
        assert_eq!(item.id, format!("upload-1-{attachment_index}"));
        assert_eq!(item.name, *name);
        assert_eq!(item.occurred_at_ms, fixture.now + 1);
        assert_eq!(page.next_cursor.is_some(), page_index + 1 < expected.len());
        query.cursor = page.next_cursor;
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify grant, archive, cross-resource denial, and revocation in one authorization lifecycle."
)]
async fn authorization_contract(fixture: &Fixture) {
    let session = fixture.session("shared-files").await;
    let other = fixture.session("private-files").await;
    let collaborator = fixture.collaborator().await;
    fixture
        .upload(&session, 41, "shared.txt", "shared payload")
        .await;
    fixture
        .upload(&other, 42, "private.txt", "private payload")
        .await;
    let path = format!("/sessions/{}/files/upload-41-0/content", session.session_id);
    let (before, _) = fixture.page("/files", &collaborator).await;
    assert!(before.items.is_empty());
    assert_eq!(
        fixture
            .get(&path, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &collaborator.session.user.user_id,
            Some(ResourcePermissions {
                view: true,
                ..ResourcePermissions::default()
            }),
            fixture.now,
        )
        .await
        .unwrap();
    let (shared, _) = fixture.page("/files", &collaborator).await;
    assert_eq!(shared.items.len(), 1);
    assert_eq!(shared.items[0].session_id, session.session_id);
    assert_eq!(
        fixture
            .content(&session.session_id, "upload-41-0", &collaborator)
            .await,
        b"shared payload"
    );
    let wrong_session = format!("/sessions/{}/files/upload-41-0/content", other.session_id);
    assert_eq!(
        fixture
            .get(&wrong_session, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
    assert_eq!(
        fixture
            .get(&wrong_session, &fixture.owner, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    assert_eq!(
        fixture
            .get(
                &path,
                &collaborator,
                &collaborator.session.personal_tenant_id
            )
            .await
            .status_code,
        Some(StatusCode::BAD_REQUEST)
    );
    let mut tx = fixture
        .state
        .cloud
        .database()
        .owner_transaction(&fixture.tenant, &fixture.owner.session.user.user_id)
        .await
        .unwrap();
    sqlx::query("UPDATE cloud_sessions SET archived_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2")
        .bind(fixture.tenant.as_str())
        .bind(session.session_id.as_str())
        .bind(i64::try_from(fixture.now + 100).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (archived, _) = fixture
        .page(
            &format!("/files?session_id={}", session.session_id),
            &collaborator,
        )
        .await;
    assert_eq!(archived.items.len(), 1);
    assert!(archived.items[0].session_archived);
    assert_eq!(
        fixture
            .content(&session.session_id, "upload-41-0", &collaborator)
            .await,
        b"shared payload"
    );
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            session.session_id.as_str(),
            &collaborator.session.user.user_id,
            None,
            fixture.now + 101,
        )
        .await
        .unwrap();
    let (revoked, _) = fixture.page("/files", &collaborator).await;
    assert!(revoked.items.is_empty());
    assert_eq!(
        fixture
            .get(&path, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
}

async fn large_inventory_contract(fixture: &Fixture) {
    let oldest = fixture.session("oldest-file-session").await;
    fixture
        .upload(
            &oldest,
            1,
            "oldest-visible.txt",
            "file before the workbench session window",
        )
        .await;
    let mut tx = fixture
        .state
        .cloud
        .database()
        .owner_transaction(&fixture.tenant, &fixture.owner.session.user.user_id)
        .await
        .unwrap();
    // Newer empty conversations must not push retained files out of the inventory.
    for index in 0..501_i64 {
        sqlx::query("INSERT INTO cloud_sessions (tenant_id,session_id,user_id,project_id,workspace_id,agent_id,title,state,created_at_ms,updated_at_ms) SELECT tenant_id,$3,user_id,project_id,workspace_id,agent_id,$3,'idle',$4,$4 FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2")
            .bind(fixture.tenant.as_str()).bind(oldest.session_id.as_str()).bind(format!("newer-{index}"))
            .bind(i64::try_from(fixture.now).unwrap() + index + 1).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let (page, _) = fixture
        .page("/files?query=oldest-visible&limit=1", &fixture.owner)
        .await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].session_id, oldest.session_id);
    assert_eq!(
        fixture
            .content(&oldest.session_id, "upload-1-0", &fixture.owner)
            .await,
        b"file before the workbench session window"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise offline discovery, cached metadata, session-only grants, and revocation together."
)]
async fn offline_sources_contract(fixture: &Fixture) {
    let shared = fixture.edge_session("offline-shared-files").await;
    let private = fixture.edge_session("offline-private-files").await;
    let collaborator = fixture.collaborator().await;
    let expected = vec![OfflineFileSource {
        workspace_id: shared.workspace_id.clone(),
        executor_id: shared.executor_id.to_string(),
    }];
    let filtered_path = format!(
        "/files?session_id={}&query=not-cached-anywhere",
        shared.session_id
    );
    let (empty, _) = fixture.page(&filtered_path, &fixture.owner).await;
    assert!(empty.items.is_empty());
    assert_eq!(empty.offline_sources, expected);
    let (private_page, _) = fixture.page("/files", &collaborator).await;
    assert!(private_page.offline_sources.is_empty());
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            shared.session_id.as_str(),
            &collaborator.session.user.user_id,
            Some(ResourcePermissions {
                view: true,
                ..ResourcePermissions::default()
            }),
            fixture.now + 3,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .state
            .store
            .list_accessible_workspaces(&collaborator.session.user, &fixture.tenant)
            .await
            .unwrap()
            .is_empty()
    );
    for path in ["/files", filtered_path.as_str()] {
        let (page, raw) = fixture.page(path, &collaborator).await;
        assert!(page.items.is_empty());
        assert_eq!(page.offline_sources, expected);
        assert!(!raw.to_string().contains(private.executor_id.as_str()));
    }
    let event = SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now,
        run_id: RunId::new("offline-node-run"),
        kind: SessionEventKind::UserMessage {
            provenance: None,
            content: "A retained Node upload".to_owned(),
            display_content: None,
            source: None,
            references: vec![],
            attachments: vec![Attachment {
                name: "cached-node.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                content: "private Node file bytes".to_owned(),
            }],
        },
    };
    fixture
        .state
        .store
        .edge_store()
        .merge_events(
            &fixture.tenant,
            &shared.executor_id,
            &shared.node_session_id,
            &[event],
        )
        .await
        .unwrap();
    let (cached, raw) = fixture
        .page("/files?query=cached-node", &collaborator)
        .await;
    assert_eq!(cached.items.len(), 1);
    assert_eq!(cached.items[0].session_id, shared.session_id);
    assert_eq!(cached.items[0].id, "upload-0-0");
    assert!(cached.items[0].session_archived);
    assert_eq!(cached.items[0].source_status, FileSourceStatus::Offline);
    assert_eq!(cached.offline_sources, expected);
    assert!(!raw.to_string().contains("private Node file bytes"));
    let content_path = format!("/sessions/{}/files/upload-0-0/content", shared.session_id);
    assert_eq!(
        fixture
            .get(&content_path, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    fixture
        .state
        .store
        .set_resource_share(
            &fixture.owner.session.user,
            &fixture.tenant,
            ResourceKind::Session,
            shared.session_id.as_str(),
            &collaborator.session.user.user_id,
            None,
            fixture.now + 4,
        )
        .await
        .unwrap();
    for path in ["/files", filtered_path.as_str()] {
        let (revoked, _) = fixture.page(path, &collaborator).await;
        assert!(revoked.items.is_empty());
        assert!(revoked.offline_sources.is_empty());
    }
    assert_eq!(
        fixture
            .get(&content_path, &collaborator, &fixture.tenant)
            .await
            .status_code,
        Some(StatusCode::FORBIDDEN)
    );
}

#[tokio::test]
async fn sqlite_files_route_serves_the_spa_without_authentication() {
    let fixture = Fixture::new("sqlite::memory:", None).await;
    let mut response = TestClient::get("http://server.test/files")
        .send(&fixture.service)
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    assert!(
        response
            .take_string()
            .await
            .unwrap()
            .contains("id=\"root\"")
    );
}

#[tokio::test]
async fn sqlite_offline_files_report_authorized_sources_without_cached_matches() {
    offline_sources_contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
async fn sqlite_file_inventory_keeps_metadata_and_exact_versions_separate() {
    inventory_contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
async fn sqlite_file_access_requires_a_live_resource_grant_including_archived_sessions() {
    authorization_contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
async fn sqlite_file_inventory_is_not_limited_by_the_workbench_session_window() {
    large_inventory_contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
#[ignore = "requires an isolated empty PostgreSQL database in TERNILO_FILES_TEST_POSTGRES_DATABASE_URL"]
async fn postgres_file_inventory_and_download_enforce_the_same_contract() {
    let url = std::env::var("TERNILO_FILES_TEST_POSTGRES_DATABASE_URL").unwrap();
    let schema_owner = std::env::var("TERNILO_FILES_TEST_POSTGRES_SCHEMA_OWNER_URL").ok();
    let fixture = Fixture::new(&url, schema_owner.as_deref()).await;
    inventory_contract(&fixture).await;
    authorization_contract(&fixture).await;
    large_inventory_contract(&fixture).await;
    offline_sources_contract(&fixture).await;
    accepted_tests::accepted_upload_contract(&fixture).await;
}

#[path = "files_tests/account_delivery_tests.rs"]
mod account_delivery_tests;
