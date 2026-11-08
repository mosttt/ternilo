use ternilo_cloud::{CloudSessionDraft, CloudSessionRecord, CloudStore};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{AgentId, PermissionPreset, SessionMode, TenantId};

struct Fixture {
    _directory: tempfile::TempDir,
    control: ControlStore,
    cloud: CloudStore,
    admin: ControlUser,
    owner: ControlUser,
    reader: ControlUser,
    session: CloudSessionRecord,
}

async fn member(
    control: &ControlStore,
    admin: &ControlUser,
    tenant: &TenantId,
    name: &str,
) -> ControlUser {
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://archive.example".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            100,
        )
        .await
        .unwrap();
    control
        .set_membership(admin, tenant, &user.user_id, TenantRole::Member, 100)
        .await
        .unwrap();
    user
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("archive.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([51; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    let bootstrap = control
        .initialize_owner(
            &NativeRegistration {
                email: "admin@archive.example".to_owned(),
                username: "archive-admin".to_owned(),
                password: "archive-test-password".to_owned(),
            },
            100,
        )
        .await
        .unwrap();
    let admin = bootstrap.session.user;
    control
        .set_instance_mode(
            &admin,
            InstanceMode::MultiUser,
            bootstrap.session.instance.revision,
            100,
        )
        .await
        .unwrap();
    let tenant = control
        .create_tenant(&admin, "archives", "Archives", TenantQuota::default(), 100)
        .await
        .unwrap();
    let owner = member(&control, &admin, &tenant.tenant_id, "archive-owner").await;
    let reader = member(&control, &admin, &tenant.tenant_id, "archive-reader").await;
    let project = control
        .list_projects(&owner, &tenant.tenant_id)
        .await
        .unwrap()
        .remove(0);
    let workspace = control
        .create_cloud_workspace(
            &owner,
            &tenant.tenant_id,
            &project.project_id,
            "Archive workspace",
            100,
        )
        .await
        .unwrap();
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id,
                workspace_id: workspace.workspace_id,
                session_id: None,
                agent_id: AgentId::new("archive-agent"),
                title: "Private archive".to_owned(),
                permissions: PermissionPreset::ReadOnly,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Plan,
            },
            &tenant.tenant_id,
            &owner.user_id,
            101,
        )
        .await
        .unwrap();
    Fixture {
        _directory: directory,
        control,
        cloud,
        admin,
        owner,
        reader,
        session,
    }
}

#[tokio::test]
async fn archive_restore_is_owner_only_atomic_idempotent_and_never_recreates_deleted_rows() {
    let fixture = fixture().await;
    let tenant = &fixture.session.tenant_id;
    let session_id = &fixture.session.session_id;
    let mut archived = fixture
        .cloud
        .archive_session(tenant, &fixture.owner.user_id, session_id, 102)
        .await
        .unwrap();
    for user in [&fixture.admin, &fixture.reader] {
        assert!(
            fixture
                .cloud
                .list_accessible_archived_sessions(tenant, &user.user_id, 100)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .cloud
                .restore_session(tenant, &user.user_id, session_id, 103)
                .await
                .is_err()
        );
    }
    assert!(
        fixture
            .cloud
            .restore_session(
                &TenantId::new("other-space"),
                &fixture.owner.user_id,
                session_id,
                103
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, &fixture.owner.user_id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let (first, second) = tokio::join!(
        fixture
            .cloud
            .restore_session(tenant, &fixture.owner.user_id, session_id, 103),
        fixture
            .cloud
            .restore_session(tenant, &fixture.owner.user_id, session_id, 104),
    );
    archived.archived_at_ms = None;
    assert_eq!(first.unwrap(), archived);
    assert_eq!(second.unwrap(), archived);
    assert!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, &fixture.owner.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .cloud
            .list_accessible_sessions(tenant, &fixture.owner.user_id, 100)
            .await
            .unwrap(),
        vec![archived]
    );
    fixture
        .cloud
        .archive_session(tenant, &fixture.owner.user_id, session_id, 105)
        .await
        .unwrap();
    fixture
        .cloud
        .delete_session(tenant, session_id, &fixture.owner.user_id, 106)
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .restore_session(tenant, &fixture.owner.user_id, session_id, 107)
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, &fixture.owner.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn archived_listing_respects_sharing_and_restore_preserves_grants() {
    let fixture = fixture().await;
    let tenant = &fixture.session.tenant_id;
    let session_id = &fixture.session.session_id;
    fixture
        .control
        .set_resource_share(
            &fixture.owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &fixture.reader.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                stop: true,
                configure: true,
            }),
            102,
        )
        .await
        .unwrap();
    let access = fixture
        .control
        .resource_access(
            &fixture.reader,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    fixture
        .cloud
        .archive_session(tenant, &fixture.owner.user_id, session_id, 103)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, &fixture.reader.user_id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        fixture
            .cloud
            .restore_session(tenant, &fixture.reader.user_id, session_id, 104)
            .await
            .is_err()
    );
    fixture
        .cloud
        .restore_session(tenant, &fixture.owner.user_id, session_id, 105)
        .await
        .unwrap();
    let restored_access = fixture
        .control
        .resource_access(
            &fixture.reader,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(restored_access).unwrap(),
        serde_json::to_value(access).unwrap()
    );
    fixture
        .cloud
        .archive_session(tenant, &fixture.owner.user_id, session_id, 106)
        .await
        .unwrap();
    fixture
        .control
        .set_resource_share(
            &fixture.owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &fixture.reader.user_id,
            None,
            107,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, &fixture.reader.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Verify archive, restart/restore and single-record deletion on the same delegated family."
)]
async fn archive_family_keeps_forks_independent_and_restore_does_not_resume_queues() {
    use ternilo_protocol::{
        SessionId, SubagentId, SubagentSessionMetadata, SubagentTranscriptKind,
    };
    let fixture = fixture().await;
    let tenant = &fixture.session.tenant_id;
    let owner = &fixture.owner.user_id;
    let metadata = |id: &str| SubagentSessionMetadata {
        role: None,
        subagent_id: SubagentId::new(id),
        provider: "in-process".into(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    let root = &fixture.session.session_id;
    let child = fixture
        .cloud
        .create_canonical_subagent_session(
            tenant,
            owner,
            root,
            &SessionId::new("archive-child"),
            metadata("child"),
            "Child".into(),
            102,
        )
        .await
        .unwrap();
    let grandchild = fixture
        .cloud
        .create_canonical_subagent_session(
            tenant,
            owner,
            &child.session_id,
            &SessionId::new("archive-grandchild"),
            metadata("grandchild"),
            "Grandchild".into(),
            103,
        )
        .await
        .unwrap();
    // Seed an ordinary branch with the same parent pointer but no delegation metadata.
    // Actual fork history and execution-family isolation have their own contract tests.
    let fork = fixture
        .cloud
        .create_canonical_subagent_session(
            tenant,
            owner,
            &child.session_id,
            &SessionId::new("ordinary-branch"),
            metadata("branch"),
            "Independent branch".into(),
            104,
        )
        .await
        .unwrap();
    let mut tx = fixture
        .cloud
        .database()
        .tenant_transaction(tenant)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE cloud_sessions SET subagent_metadata=NULL WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(tenant.as_str())
    .bind(fork.session_id.as_str())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let fork_child = fixture
        .cloud
        .create_canonical_subagent_session(
            tenant,
            owner,
            &fork.session_id,
            &SessionId::new("fork-child"),
            metadata("fork-child"),
            "Independent child".into(),
            105,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .archive_session(tenant, &fixture.reader.user_id, root, 106)
            .await
            .is_err()
    );
    fixture
        .cloud
        .archive_session(tenant, owner, root, 107)
        .await
        .unwrap();
    for id in [root, &child.session_id, &grandchild.session_id] {
        assert!(
            fixture
                .cloud
                .get_session(tenant, id)
                .await
                .unwrap()
                .archived_at_ms
                .is_some()
        );
        assert!(
            fixture
                .cloud
                .session_inbox(tenant, owner, id)
                .await
                .unwrap()
                .paused
        );
    }
    for id in [&fork.session_id, &fork_child.session_id] {
        assert!(
            fixture
                .cloud
                .get_session(tenant, id)
                .await
                .unwrap()
                .archived_at_ms
                .is_none()
        );
    }
    assert!(
        fixture
            .cloud
            .create_canonical_subagent_session(
                tenant,
                owner,
                root,
                &SessionId::new("late-child"),
                metadata("late"),
                "Late child".into(),
                108
            )
            .await
            .is_err()
    );
    fixture
        .cloud
        .restore_session(tenant, owner, root, 109)
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .list_accessible_archived_sessions(tenant, owner, 100)
            .await
            .unwrap()
            .is_empty()
    );
    for id in [root, &child.session_id, &grandchild.session_id] {
        assert!(
            fixture
                .cloud
                .session_inbox(tenant, owner, id)
                .await
                .unwrap()
                .paused
        );
    }
    fixture
        .cloud
        .archive_session(tenant, owner, root, 110)
        .await
        .unwrap();
    fixture
        .cloud
        .delete_session(tenant, &child.session_id, owner, 111)
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .get_session(tenant, &child.session_id)
            .await
            .is_err()
    );
    let surviving = fixture
        .cloud
        .get_session(tenant, &grandchild.session_id)
        .await
        .unwrap();
    assert!(surviving.archived_at_ms.is_some());
    assert!(surviving.parent_session_id.is_none());
    assert!(surviving.subagent.is_none());
    assert!(
        fixture
            .cloud
            .get_session(tenant, &fork.session_id)
            .await
            .unwrap()
            .parent_session_id
            .is_none()
    );
}
