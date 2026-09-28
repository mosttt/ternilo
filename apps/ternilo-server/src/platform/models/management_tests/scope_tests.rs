use ternilo_control::{
    ModelGrantInput, ModelGrantSubject, ResourceKind, ResourcePermissions, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    DefaultModelSelection, PermissionPreset, SessionId, SessionMode, SessionSubmission,
    SubmissionId, TenantId, WorkspaceId,
};

use super::*;

struct ScopedFixture {
    base: Fixture,
    tenant: TenantId,
    workspace: WorkspaceId,
    session: SessionId,
    collaborator: NativeSessionGrant,
    owner_model: Value,
    alternate_model: Value,
    collaborator_model: Value,
}

impl ScopedFixture {
    #[expect(
        clippy::too_many_lines,
        reason = "Reuse the real account/model HTTP fixture and add explicit team resource ownership and shared configuration permissions."
    )]
    async fn new() -> Self {
        let mut base = Fixture::new().await;
        base.state.managed_execution_enabled = true;
        base.service = Service::new(crate::platform::web_router(base.state.clone()));
        base.publish().await;
        let response = base.request("POST", "/api/v1/admin/models/publications", &base.owner.access_token, Some(json!({
            "model_id":"alternate-model", "display_name":"Alternate model", "provider_id":"upstream", "upstream_model":"internal-model", "enabled":true,
        }))).await;
        assert_eq!(response.status_code, Some(StatusCode::OK));
        let collaborator = base.account("scope-collaborator", PlatformRole::User).await;
        let owner = &base.owner.session.user;
        let now = now_ms().unwrap();
        let mut grants = Vec::new();
        for user in [owner, &collaborator.session.user] {
            grants.push(
                base.state
                    .store
                    .save_model_grant(
                        owner,
                        None,
                        &ModelGrantInput {
                            name: format!("Allowance for {}", user.user_id),
                            subject: ModelGrantSubject::User {
                                id: user.user_id.as_str().to_owned(),
                            },
                            model_ids: vec![
                                "public-model".to_owned(),
                                "alternate-model".to_owned(),
                            ],
                            monthly_tokens: 1_000_000,
                            max_concurrent_requests: 4,
                            expires_at_ms: None,
                            allow_resource_sharing: true,
                        },
                        now,
                    )
                    .await
                    .unwrap(),
            );
        }
        let tenant = base
            .state
            .store
            .create_tenant(
                owner,
                "scope-team",
                "Scope team",
                TenantQuota::default(),
                now,
            )
            .await
            .unwrap()
            .tenant_id;
        base.state
            .store
            .set_membership(
                owner,
                &tenant,
                &collaborator.session.user.user_id,
                TenantRole::Member,
                now,
            )
            .await
            .unwrap();
        let project = base
            .state
            .store
            .list_projects(owner, &tenant)
            .await
            .unwrap()
            .remove(0);
        let workspace = base
            .state
            .store
            .create_cloud_workspace(owner, &tenant, &project.project_id, "Owned workspace", now)
            .await
            .unwrap()
            .workspace_id;
        let session = SessionId::new("model-scope-session");
        let fixture = Self {
            owner_model: selection(&grants[0].grant_id, "public-model"),
            alternate_model: selection(&grants[0].grant_id, "alternate-model"),
            collaborator_model: selection(&grants[1].grant_id, "public-model"),
            base,
            tenant,
            workspace,
            session,
            collaborator,
        };
        let created = fixture.json("POST", "/api/v1/sessions", &fixture.base.owner.access_token,
            Some(json!({"workspace_id":fixture.workspace,"session_id":fixture.session,"permissions":"workspace_write"})), StatusCode::CREATED).await;
        assert_eq!(created["identity"]["session_id"], fixture.session.as_str());
        for (kind, resource) in [
            (ResourceKind::Workspace, fixture.workspace.as_str()),
            (ResourceKind::Session, fixture.session.as_str()),
        ] {
            fixture
                .base
                .state
                .store
                .set_resource_share(
                    &fixture.base.owner.session.user,
                    &fixture.tenant,
                    kind,
                    resource,
                    &fixture.collaborator.session.user.user_id,
                    Some(ResourcePermissions::OWNER),
                    now_ms().unwrap(),
                )
                .await
                .unwrap();
        }
        fixture
    }

    async fn json(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        expected: StatusCode,
    ) -> Value {
        let url = format!("http://server.test{path}");
        let request = match method {
            "GET" => TestClient::get(url),
            "POST" => TestClient::post(url),
            "PUT" => TestClient::put(url),
            "PATCH" => TestClient::patch(url),
            _ => panic!("unsupported scoped test method"),
        }
        .add_header("Authorization", format!("Bearer {token}"), true)
        .add_header("x-ternilo-tenant", self.tenant.as_str(), true);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        let mut response = request.send(&self.base.service).await;
        let status = response.status_code;
        let body = response.take_json::<Value>().await.unwrap();
        assert_eq!(status, Some(expected), "{method} {path}: {body}");
        body
    }

    async fn submit(&self, run: &str, input: &str) -> SessionSubmission {
        let value = self.json("POST", &format!("/api/v1/sessions/{}/queue", self.session), &self.base.owner.access_token,
            Some(json!({"delivery":"queue","run_id":run,"content":{"kind":"prompt","input":input}})), StatusCode::CREATED).await;
        serde_json::from_value(value).unwrap()
    }

    async fn compiled(&self, submission: &SubmissionId) -> ternilo_cloud::CompiledRun {
        self.base
            .state
            .cloud
            .queued_submission_run(
                &self.tenant,
                &self.base.owner.session.user.user_id,
                &self.session,
                submission,
            )
            .await
            .unwrap()
    }

    async fn close(self) {
        self.base.state.edge.shutdown().await;
    }
}

fn selection(grant: &str, model: &str) -> Value {
    serde_json::to_value(DefaultModelSelection::PlatformModel {
        grant_id: grant.to_owned(),
        model_id: model.to_owned(),
        reasoning_effort: None,
    })
    .unwrap()
}

#[tokio::test]
async fn default_model_http_scopes_use_the_owner_and_reject_shared_global_writes() {
    let f = ScopedFixture::new().await;
    let owner = &f.base.owner.access_token;
    let collaborator = &f.collaborator.access_token;
    let global = "/api/v1/default-model";
    let workspace = format!("{global}?workspace_id={}", f.workspace);
    let session = format!("{global}?session_id={}", f.session);
    assert_eq!(
        f.json("GET", global, owner, None, StatusCode::OK).await,
        serde_json::to_value(DefaultModelSelection::ProfileDefault).unwrap()
    );
    assert_eq!(
        f.json(
            "PUT",
            global,
            owner,
            Some(f.owner_model.clone()),
            StatusCode::OK
        )
        .await,
        f.owner_model
    );
    assert_eq!(
        f.json(
            "PUT",
            global,
            collaborator,
            Some(f.collaborator_model.clone()),
            StatusCode::OK
        )
        .await,
        f.collaborator_model
    );
    for path in [&workspace, &session] {
        assert_eq!(
            f.json("GET", path, owner, None, StatusCode::OK).await,
            f.owner_model
        );
        assert_eq!(
            f.json("GET", path, collaborator, None, StatusCode::OK)
                .await,
            f.owner_model
        );
        f.json(
            "PUT",
            path,
            collaborator,
            Some(f.collaborator_model.clone()),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    assert_eq!(
        f.json(
            "PUT",
            &workspace,
            owner,
            Some(f.alternate_model.clone()),
            StatusCode::OK
        )
        .await,
        f.alternate_model
    );
    assert_eq!(
        f.json("GET", &session, collaborator, None, StatusCode::OK)
            .await,
        f.alternate_model
    );
    assert_eq!(
        f.json("GET", global, collaborator, None, StatusCode::OK)
            .await,
        f.collaborator_model
    );
    assert_eq!(
        f.json(
            "PUT",
            &session,
            owner,
            Some(f.owner_model.clone()),
            StatusCode::OK
        )
        .await,
        f.owner_model
    );
    assert_eq!(
        f.json("GET", &workspace, collaborator, None, StatusCode::OK)
            .await,
        f.owner_model
    );
    assert_eq!(
        f.json("GET", global, owner, None, StatusCode::OK).await,
        f.owner_model
    );
    f.close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Compare old and newly submitted tasks with the effective catalog after configuration changes."
)]
async fn changed_session_model_and_task_limit_apply_to_new_submissions_not_old_queue_edits() {
    let mut f = ScopedFixture::new().await;
    let path = format!("/api/v1/sessions/{}", f.session);
    let configured = f
        .json(
            "PATCH",
            &path,
            &f.base.owner.access_token,
            Some(json!({"model":f.owner_model,"model_token_limit":50_000})),
            StatusCode::OK,
        )
        .await;
    assert_eq!(configured["model_token_limit"], 50_000);
    f.submit("current-head-run", "Current task").await;
    let old = f.submit("original-queued-run", "Original task").await;
    assert_eq!(old.placement, ternilo_protocol::SubmissionPlacement::Queued);
    let original = f.compiled(&old.id).await;
    assert_eq!(original.reserved_model_tokens, 50_000);
    assert_eq!(original.spec.limits.max_tool_calls, 512);
    let catalog_path = format!("/api/v1/catalog?session_id={}", f.session);
    let catalog = f
        .json(
            "GET",
            &catalog_path,
            &f.base.owner.access_token,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(catalog["host_limits"]["max_tool_calls"], 512);
    let old_model = ternilo_cloud::profile_model_snapshot(&original.spec.profile)
        .unwrap()
        .unwrap();
    assert_eq!(old_model.binding.model_id(), "public-model");
    assert_eq!(old_model.defaults.max_output_tokens, 4_096);

    let changed = f.json("PATCH", &path, &f.base.owner.access_token,
        Some(json!({"model":f.alternate_model,"model_token_limit":80_000,"permissions":"read_only","mode":"plan"})), StatusCode::OK).await;
    assert_eq!(changed["model_token_limit"], 80_000);
    assert_eq!(f.compiled(&old.id).await.spec, original.spec);
    assert_eq!(f.compiled(&old.id).await.reserved_model_tokens, 50_000);

    let mut provider = provider_input();
    provider["api_key"] = Value::Null;
    provider["profile"]["defaults"]["max_output_tokens"] = json!(8_192);
    let response = f
        .base
        .request(
            "PUT",
            "/api/v1/admin/models/providers/upstream",
            &f.base.owner.access_token,
            Some(provider),
        )
        .await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    let mut policy = (*f.base.state.worker_policy).clone();
    policy.maximum_limits.max_tool_calls = 1_024;
    f.base.state.worker_policy = Arc::new(policy);
    f.base.service = Service::new(crate::platform::web_router(f.base.state.clone()));
    let catalog = f
        .json(
            "GET",
            &catalog_path,
            &f.base.owner.access_token,
            None,
            StatusCode::OK,
        )
        .await;
    assert_eq!(catalog["host_limits"]["max_tool_calls"], 1_024);

    let edited = f
        .json(
            "PATCH",
            &format!("{path}/queue/{}", old.id),
            &f.base.owner.access_token,
            Some(
                json!({"input":"Revised original task","expected_updated_at_ms":old.updated_at_ms}),
            ),
            StatusCode::OK,
        )
        .await;
    assert_eq!(edited["content"]["input"], "Revised original task");
    let preserved = f.compiled(&old.id).await;
    let mut expected = original.spec.clone();
    expected.input = "Revised original task".to_owned();
    assert_eq!(preserved.spec, expected);
    assert_eq!(preserved.reserved_model_tokens, 50_000);
    assert_eq!(preserved.spec.permissions, PermissionPreset::WorkspaceWrite);
    assert_eq!(preserved.spec.mode, SessionMode::Execute);
    assert_eq!(
        ternilo_cloud::profile_model_snapshot(&preserved.spec.profile)
            .unwrap()
            .unwrap(),
        old_model
    );

    let new = f.submit("new-queued-run", "New task").await;
    let fresh = f.compiled(&new.id).await;
    assert_eq!(fresh.reserved_model_tokens, 80_000);
    assert_eq!(fresh.spec.limits.max_tool_calls, 1_024);
    assert_eq!(fresh.spec.permissions, PermissionPreset::ReadOnly);
    assert_eq!(fresh.spec.mode, SessionMode::Plan);
    let new_model = ternilo_cloud::profile_model_snapshot(&fresh.spec.profile)
        .unwrap()
        .unwrap();
    assert_eq!(new_model.binding.model_id(), "alternate-model");
    assert_eq!(new_model.defaults.max_output_tokens, 8_192);
    assert_eq!(f.compiled(&old.id).await.reserved_model_tokens, 50_000);
    f.close().await;
}
