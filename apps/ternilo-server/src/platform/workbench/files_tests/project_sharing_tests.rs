use super::*;
use sqlx::Executor as _;
use ternilo_protocol::{SessionSearchFilters, SessionSearchRequest};

#[expect(
    clippy::too_many_lines,
    reason = "Keep Cloud discovery, content, search, archive and inherited revocation in one HTTP-backed permission lifecycle."
)]
async fn contract(fixture: &Fixture) {
    let session = fixture.session("project-cloud").await;
    let reader = fixture.collaborator().await;
    let owner = &fixture.owner.session.user;
    let actor = &reader.session.user;
    let tenant = &fixture.tenant;
    fixture
        .upload(&session, 0, "project.txt", "project-file-proof")
        .await;
    fixture
        .state
        .store
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Project,
            &session.project_id,
            &actor.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                ..Default::default()
            }),
            fixture.now,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .state
            .cloud
            .list_accessible_sessions(tenant, &actor.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let search = |query: &str| SessionSearchRequest {
        query: query.to_owned(),
        session_id: None,
        workspace_id: None,
        filters: SessionSearchFilters::default(),
        limit: 100,
    };
    assert!(
        fixture
            .state
            .cloud
            .search_sessions(tenant, &actor.user_id, search("Inspect"))
            .await
            .unwrap()
            .is_empty()
    );
    fixture
        .state
        .store
        .set_workspace_project_sharing(owner, tenant, &session.workspace_id, true, fixture.now + 1)
        .await
        .unwrap();
    let visible = fixture
        .state
        .cloud
        .list_accessible_sessions(tenant, &actor.user_id, 100)
        .await
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].session_id, session.session_id);
    assert!(
        !fixture
            .state
            .cloud
            .search_sessions(tenant, &actor.user_id, search("Inspect"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !fixture
            .state
            .cloud
            .search_sessions(tenant, &actor.user_id, search("project-cloud"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .content(&session.session_id, "upload-0-0", &reader)
            .await,
        b"project-file-proof"
    );
    let (files, _) = fixture.page("/files", &reader).await;
    assert_eq!(files.items.len(), 1);
    assert!(
        fixture
            .state
            .cloud
            .archive_session(tenant, &actor.user_id, &session.session_id, fixture.now + 2)
            .await
            .is_err(),
        "project submit does not grant ownership actions"
    );
    fixture
        .state
        .cloud
        .archive_session(tenant, &owner.user_id, &session.session_id, fixture.now + 3)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state
            .cloud
            .list_accessible_archived_sessions(tenant, &actor.user_id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    let preview = format!("/sessions/{}/archive-history", session.session_id);
    assert_eq!(
        fixture.get(&preview, &reader, tenant).await.status_code,
        Some(StatusCode::OK)
    );
    fixture
        .state
        .store
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Project,
            &session.project_id,
            &actor.user_id,
            None,
            fixture.now + 4,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .state
            .cloud
            .list_accessible_archived_sessions(tenant, &actor.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .state
            .cloud
            .search_sessions(tenant, &actor.user_id, search("Inspect"))
            .await
            .unwrap()
            .is_empty()
    );
    let mut denied = fixture.get(&preview, &reader, tenant).await;
    assert!(matches!(
        denied.status_code,
        Some(StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN)
    ));
    assert!(
        !denied
            .take_string()
            .await
            .unwrap()
            .contains("project-file-proof")
    );
}

#[tokio::test]
async fn cloud_project_inheritance_controls_discovery_files_search_and_archives() {
    contract(&Fixture::new("sqlite::memory:", None).await).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_project_files_search_and_archives_preserve_runtime_authorization() {
    let url = std::env::var("TERNILO_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("ternilo_control_test"));
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    sqlx::raw_sql(
        "DO $$ BEGIN
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='ternilo_runtime') THEN
            CREATE ROLE ternilo_runtime NOLOGIN;
        END IF;
        END $$;
        DROP ROLE IF EXISTS project_files_runtime;
        CREATE ROLE project_files_runtime LOGIN PASSWORD 'project-files-password';
        GRANT ternilo_runtime TO project_files_runtime;
        REVOKE CREATE ON SCHEMA public FROM PUBLIC;
        GRANT USAGE ON SCHEMA public TO project_files_runtime;",
    )
    .execute(&admin)
    .await
    .unwrap();
    let mut runtime = reqwest::Url::parse(&url).unwrap();
    runtime.set_username("project_files_runtime").unwrap();
    runtime
        .set_password(Some("project-files-password"))
        .unwrap();
    let fixture = Fixture::new(runtime.as_str(), Some(&url)).await;
    contract(&fixture).await;
    fixture.state.store.database().close().await;
    admin.close().await;
}
