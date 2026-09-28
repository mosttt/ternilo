use std::{collections::BTreeSet, fmt::Write as _, time::Duration};

use sha2::{Digest, Sha256};
use ternilo_cloud::{
    CloudRunDraft, CloudRunState, CloudSessionDraft, CloudSessionUpdate, CloudStore, CompiledRun,
    StartedRun, TerminalState, WorkerPolicy,
};
use ternilo_control::{
    ControlStore, ControlUser, GroupInput, InstanceMode, NativeRegistration, OidcPrincipal,
    ResourceKind, ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    AgentId, AgentPresetCopyRequest, Attachment, DefaultModelSelection, ErrorCode,
    PermissionPreset, Profile, ProviderModel, ProviderModelDefaults, ProviderModelSettings,
    ProviderProfile, ProviderProtocol, QueueEditRequest, RunId, RunLimits, SessionEvent,
    SessionEventKind, SessionId, SessionMode, SessionSearchFilters, SessionSearchRequest,
    SessionSubmissionRequest, SubmissionContent, SubmissionDelivery, ToolApprovalContext,
    UserAnswer, UserQuestion, UserQuestionPresentation,
};

#[path = "support/model_ledger.rs"]
mod model_ledger;
#[path = "support/queue_edit.rs"]
mod queue_edit;
#[path = "support/server_runtime.rs"]
mod server_runtime;
#[path = "support/steering_edit.rs"]
mod steering_edit;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;
#[path = "support/workspace_browser.rs"]
mod workspace_browser;

#[tokio::test]
async fn sqlite_sharing_preserves_owner_quota_actor_audit_and_independent_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("sharing.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([47; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    sharing_contract(control, cloud).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_sharing_enforces_the_same_contract_with_a_restricted_runtime() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("set a disposable PostgreSQL test database");
    assert!(admin_url.contains("ternilo_cloud_test"));
    let url =
        server_runtime::initialize(&admin_url, "ternilo_sharing_runtime_test", [47; 32]).await;
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([47; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    sharing_contract(control, cloud).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise grants, atomic submission, actor audit and revocation against the same canonical resource."
)]
async fn sharing_contract(control: ControlStore, cloud: CloudStore) {
    worker_storage::bind_workers(&cloud, &["approval-worker"], "shared-contract-storage").await;
    let now = 2_200_000_000_000;
    let bootstrap = control
        .initialize_owner(
            &NativeRegistration {
                email: "owner@example.test".to_owned(),
                username: "owner".to_owned(),
                password: "owner-password-123".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    let owner = bootstrap.session.user;
    let instance = bootstrap.session.instance;
    let team = control
        .create_tenant(
            &owner,
            "sharing-team",
            "Sharing team",
            TenantQuota::default(),
            now,
        )
        .await
        .unwrap();
    let tenant = &team.tenant_id;
    let project_id = control.list_projects(&owner, tenant).await.unwrap()[0]
        .project_id
        .clone();
    control
        .set_instance_mode(&owner, InstanceMode::MultiUser, instance.revision, now + 1)
        .await
        .unwrap();
    let member = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://test.example".to_owned(),
                subject: "member".to_owned(),
                email: None,
                display_name: None,
            },
            "test-member",
            now + 2,
        )
        .await
        .unwrap();
    control
        .set_membership(&owner, tenant, &member.user_id, TenantRole::Member, now + 3)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(&owner, tenant, &project_id, "Owned workspace", now + 4)
        .await
        .unwrap();
    let session_id = SessionId::new("shared-session");
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(session_id.clone()),
                agent_id: AgentId::new("agent"),
                title: "Owned session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: vec![],
                mode: SessionMode::Execute,
            },
            tenant,
            &owner.user_id,
            now + 5,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &session_id)
            .await
            .unwrap()
            .is_none()
    );
    let read = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            Some(read),
            now + 6,
        )
        .await
        .unwrap();
    workspace_browser::assert_workspace_read_boundary(
        &control,
        &cloud,
        &owner,
        &member,
        &session,
        now + 6,
    )
    .await;
    // A persisted permissionless grant must neither reveal the row nor turn
    // the whole visible list into an authorization error.
    let mut transaction = control.database().tenant_transaction(tenant).await.unwrap();
    sqlx::query("UPDATE control_resource_shares SET permissions_json=$1 WHERE tenant_id=$2 AND resource_kind='session' AND resource_id=$3 AND grantee_user_id=$4")
        .bind(ternilo_storage::Json(ResourcePermissions::default())).bind(tenant.as_str())
        .bind(session_id.as_str()).bind(member.user_id.as_str())
        .execute(&mut *transaction).await.unwrap();
    transaction.commit().await.unwrap();
    assert!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    let title_search = |query: &str| SessionSearchRequest {
        query: query.to_owned(),
        session_id: None,
        workspace_id: None,
        filters: SessionSearchFilters::default(),
        limit: 100,
    };
    assert!(
        cloud
            .search_sessions(tenant, &member.user_id, title_search("Owned session"))
            .await
            .unwrap()
            .is_empty()
    );
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            Some(read),
            now + 6,
        )
        .await
        .unwrap();
    let hits = cloud
        .search_sessions(tenant, &member.user_id, title_search("Owned session"))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session_id);
    let owner_provider = provider(
        "Owner model",
        "https://private.example/v1?token=private-token",
    );
    let member_provider = provider("Member model", "https://member.example/v1");
    control
        .upsert_user_provider_profile(&owner, tenant, owner_provider.clone(), now + 6)
        .await
        .unwrap();
    control
        .upsert_user_provider_profile(&member, tenant, member_provider, now + 6)
        .await
        .unwrap();
    control
        .put_user_credential(&owner, tenant, "OWNER_KEY", "owner-secret-value", now + 6)
        .await
        .unwrap();
    let default_model = DefaultModelSelection::NamedProvider {
        provider_id: "route".to_owned(),
        model: "model".to_owned(),
        reasoning_effort: None,
    };
    control
        .set_user_default_model(&owner, tenant, default_model.clone(), now + 6)
        .await
        .unwrap();
    control
        .set_user_default_model(
            &member,
            tenant,
            DefaultModelSelection::ProfileDefault,
            now + 6,
        )
        .await
        .unwrap();
    let owner_preset = control
        .copy_user_agent_preset(
            &owner,
            tenant,
            AgentPresetCopyRequest {
                from: "standard".to_owned(),
                id: "custom".to_owned(),
                display_name: Some("Owner preset".to_owned()),
            },
            now + 6,
        )
        .await
        .unwrap();
    control
        .copy_user_agent_preset(
            &member,
            tenant,
            AgentPresetCopyRequest {
                from: "standard".to_owned(),
                id: "custom".to_owned(),
                display_name: Some("Member preset".to_owned()),
            },
            now + 6,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .resource_default_model(
                tenant,
                &member.user_id,
                ResourceKind::Session,
                session_id.as_str()
            )
            .await
            .unwrap(),
        default_model
    );
    assert_eq!(
        cloud
            .resource_agent_preset(
                tenant,
                &member.user_id,
                ResourceKind::Session,
                session_id.as_str(),
                "custom"
            )
            .await
            .unwrap(),
        owner_preset
    );
    let roster = cloud
        .resource_agent_preset_roster(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert!(!roster.authorable);
    assert!(
        roster
            .presets
            .iter()
            .any(|preset| preset.id == "custom" && preset.display_name == "Owner preset")
    );
    assert!(
        !roster
            .presets
            .iter()
            .any(|preset| preset.display_name == "Member preset")
    );
    let resolved_owner = cloud
        .resource_provider_profile(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
            "route",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved_owner, owner_provider);
    let public_providers = cloud
        .resource_model_providers(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert_eq!(public_providers[0].display_name, "Owner model");
    assert!(public_providers[0].base_url.is_empty());
    let personal = control
        .account_provider_space(&owner.user_id)
        .await
        .unwrap();
    let mut account_provider = owner_provider.clone();
    "Account source".clone_into(&mut account_provider.display_name);
    control
        .upsert_user_provider_profile(&owner, &personal, account_provider, now + 6)
        .await
        .unwrap();
    control
        .put_user_credential(&owner, &personal, "OWNER_KEY", "account-secret", now + 6)
        .await
        .unwrap();
    control
        .put_user_credential(
            &owner,
            &personal,
            "UNRELATED_PRIVATE_KEY",
            "unrelated-secret",
            now + 6,
        )
        .await
        .unwrap();
    let (hidden, _) = cloud
        .resource_account_models(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert!(
        hidden.is_empty(),
        "view access does not expose an owner's unselected private Providers"
    );
    let account_snapshot = control
        .resolve_workload_model_snapshot(
            &owner.user_id,
            &owner.user_id,
            tenant,
            &ternilo_protocol::RunModelBinding::UserProvider {
                tenant_id: personal.clone(),
                owner_user_id: owner.user_id.clone(),
                provider_id: "route".to_owned(),
                model: "model".to_owned(),
            },
            None,
            now + 6,
        )
        .await
        .unwrap();
    cloud
        .update_session(
            tenant,
            &session_id,
            &owner.user_id,
            CloudSessionUpdate {
                model: Some(Some(account_snapshot)),
                ..CloudSessionUpdate::default()
            },
            now + 6,
        )
        .await
        .unwrap();
    let (catalog, credentials) = cloud
        .resource_account_models(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].display_name, "Account source");
    assert!(catalog[0].base_url.is_empty());
    assert!(credentials.records.is_empty());
    assert_eq!(credentials.references.len(), 1);
    assert_eq!(credentials.references[0].reference, "OWNER_KEY");
    assert!(credentials.references[0].configured);
    assert!(!credentials.references[0].writable);
    cloud
        .update_session(
            tenant,
            &session_id,
            &owner.user_id,
            CloudSessionUpdate {
                model: Some(None),
                ..CloudSessionUpdate::default()
            },
            now + 6,
        )
        .await
        .unwrap();
    let readiness = cloud
        .resource_credential_inventory(
            tenant,
            &member.user_id,
            ResourceKind::Session,
            session_id.as_str(),
        )
        .await
        .unwrap();
    assert_eq!(readiness.references[0].reference, "OWNER_KEY");
    assert!(readiness.references[0].configured);
    assert!(!readiness.references[0].writable);
    let public_json = serde_json::to_string(&(public_providers, readiness)).unwrap();
    assert!(!public_json.contains("private-token"));
    assert!(!public_json.contains("owner-secret-value"));
    assert_eq!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &session_id)
            .await
            .unwrap()
            .unwrap()
            .user_id,
        owner.user_id
    );
    assert_eq!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        cloud
            .session_inbox(tenant, &member.user_id, &session_id)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let update = CloudSessionUpdate {
        title: Some("Shared edit".to_owned()),
        ..CloudSessionUpdate::default()
    };
    assert_eq!(
        cloud
            .update_session(
                tenant,
                &session_id,
                &member.user_id,
                update.clone(),
                now + 7
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let request = SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(RunId::new("shared-run")),
        content: SubmissionContent::Prompt {
            input: "shared work".to_owned(),
        },
        references: vec![],
        attachments: vec![],
    };
    let mut compiled = policy()
        .compile_run(
            CloudRunDraft {
                project_id: project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session_id.clone(),
                run_id: request.run_id.clone(),
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                },
                permissions: PermissionPreset::WorkspaceWrite,
                mode: SessionMode::Execute,
                profile: Profile::default(),
                input: "shared work".to_owned(),
                references: vec![],
                reference_contexts: vec![],
                attachments: vec![],
                reserved_model_tokens: 100,
            },
            tenant.clone(),
            owner.user_id.clone(),
            member.user_id.clone(),
            &Catalog::new("sharing-catalog"),
        )
        .unwrap();
    assert_eq!(
        cloud
            .enqueue_session_submission_as(&member.user_id, &compiled, &request, now + 8)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let submit = ResourcePermissions {
        submit: true,
        ..read
    };
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            Some(submit),
            now + 9,
        )
        .await
        .unwrap();
    compiled.spec.metadata.user_id = member.user_id.clone();
    assert_eq!(
        cloud
            .enqueue_session_submission_as(&member.user_id, &compiled, &request, now + 10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    compiled.spec.metadata.user_id = owner.user_id.clone();
    let receipt = cloud
        .enqueue_session_submission_as(&member.user_id, &compiled, &request, now + 11)
        .await
        .unwrap();
    assert_eq!(receipt.run.user_id, owner.user_id);
    let mut transaction = cloud.database().tenant_transaction(tenant).await.unwrap();
    let reserved_owner: String = sqlx::query_scalar(
        "SELECT user_id FROM control_quota_reservations WHERE tenant_id=$1 AND run_id=$2",
    )
    .bind(tenant.as_str())
    .bind(receipt.run.run_id.as_str())
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(reserved_owner, owner.user_id.as_str());
    let reservations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_quota_reservations WHERE tenant_id=$1")
            .bind(tenant.as_str())
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
    assert_eq!(reservations, 1);
    transaction.commit().await.unwrap();
    assert_eq!(
        cloud
            .cancel_run_as(
                tenant,
                &member.user_id,
                &session_id,
                &receipt.run.run_id,
                now + 12
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let configure = ResourcePermissions {
        configure: true,
        ..read
    };
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            Some(configure),
            now + 13,
        )
        .await
        .unwrap();
    let updated = cloud
        .update_session(tenant, &session_id, &member.user_id, update, now + 14)
        .await
        .unwrap();
    assert_eq!(updated.user_id, owner.user_id);
    assert_eq!(updated.title, "Shared edit");
    assert_eq!(
        cloud
            .delete_session(tenant, &session_id, &member.user_id, now + 15)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let stop = ResourcePermissions { stop: true, ..read };
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            Some(stop),
            now + 16,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .cancel_run_as(
                tenant,
                &member.user_id,
                &session_id,
                &receipt.run.run_id,
                now + 17
            )
            .await
            .unwrap(),
        CloudRunState::Cancelled
    );
    assert_shared_question_permissions(&control, &cloud, &owner, &member, &compiled, now + 18)
        .await;
    control
        .set_resource_share(
            &owner,
            tenant,
            ResourceKind::Session,
            session_id.as_str(),
            &member.user_id,
            None,
            now + 18,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .resource_account_models(
                tenant,
                &member.user_id,
                ResourceKind::Session,
                session_id.as_str()
            )
            .await
            .is_err()
    );
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        cloud
            .session_inbox(tenant, &member.user_id, &session_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        cloud
            .get_session(tenant, &session_id)
            .await
            .unwrap()
            .user_id,
        owner.user_id
    );
    assert_eq!(
        cloud
            .resource_provider_profile(
                tenant,
                &member.user_id,
                ResourceKind::Session,
                session_id.as_str(),
                "route"
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        cloud
            .search_sessions(tenant, &member.user_id, title_search("Shared edit"))
            .await
            .unwrap()
            .is_empty()
    );
    let audit = control.list_audit(&owner, tenant, 100).await.unwrap();
    for action in [
        "quota.reserve",
        "resource.submit",
        "resource.configure",
        "resource.stop",
    ] {
        assert!(
            audit.iter().any(|entry| entry.action == action
                && entry.actor_user_id.as_ref() == Some(&member.user_id)
                && entry.metadata["owner_user_id"] == owner.user_id.as_str()),
            "missing actor audit for {action}"
        );
    }
    assert_group_access(
        &control,
        &cloud,
        &owner,
        &member,
        tenant,
        &session_id,
        now + 100,
    )
    .await;
    Box::pin(steering_edit::assert_edit_freeze(
        &control,
        &cloud,
        &owner,
        &member,
        &compiled,
        now + 200,
    ))
    .await;
    Box::pin(queue_edit::assert_shared_edit_conflicts(
        &control,
        &cloud,
        &owner,
        &member,
        &compiled,
        now + 20_000,
    ))
    .await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise actual persisted questions and independent reply, configure, stop and owner permissions in one running session."
)]
async fn assert_shared_question_permissions(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    template: &CompiledRun,
    now: u64,
) {
    let mut compiled = template.clone();
    compiled.spec.metadata.run_id = RunId::new("shared-question-run");
    compiled.actor_user_id = owner.user_id.clone();
    let scope = &compiled.spec.metadata;
    let tenant = &scope.tenant_id;
    let session = &scope.session_id;
    let request = SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(scope.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: vec![],
        attachments: vec![],
    };
    cloud
        .enqueue_session_submission_as(&owner.user_id, &compiled, &request, now)
        .await
        .unwrap();
    let claim = cloud
        .claim_run("approval-worker", Duration::from_secs(30), now)
        .await
        .unwrap()
        .unwrap();
    let started = cloud
        .start_run(claim, "approval-worker", Duration::from_secs(30), now)
        .await
        .unwrap()
        .unwrap();
    let question = |id: &str, presentation, tool_name: Option<&str>| UserQuestion {
        id: id.to_owned(),
        question: id.to_owned(),
        detail: None,
        header: None,
        options: vec![],
        multi_select: false,
        presentation,
        tool_approval: tool_name.map(|name| ToolApprovalContext {
            tool_name: name.to_owned(),
            call_id: id.to_owned(),
            reason: "Contract approval".to_owned(),
            arguments: serde_json::json!({}),
            presentation: None,
        }),
    };
    let questions = [
        question("normal", None, None),
        question(
            "plan",
            Some(UserQuestionPresentation::PlanReview {
                title: "Plan".to_owned(),
                plan: "# Plan".to_owned(),
                approve_label: "Approve".to_owned(),
            }),
            None,
        ),
        question("global-plugin", None, Some("extension_set_enabled")),
        question("stop-tool", None, Some("job_kill")),
    ];
    cloud
        .append_event(
            &started,
            "approval-worker",
            &SessionEvent {
                seq: 0,
                occurred_at_ms: now,
                run_id: scope.run_id.clone(),
                kind: SessionEventKind::TurnStarted,
            },
            now,
        )
        .await
        .unwrap();
    for (offset, question) in questions.iter().enumerate() {
        cloud
            .record_question(&started, "approval-worker", question, now)
            .await
            .unwrap();
        cloud
            .append_event(
                &started,
                "approval-worker",
                &SessionEvent {
                    seq: u64::try_from(offset).unwrap() + 1,
                    occurred_at_ms: now,
                    run_id: scope.run_id.clone(),
                    kind: SessionEventKind::UserQuestionAsked {
                        question: question.clone(),
                    },
                },
                now,
            )
            .await
            .unwrap();
    }
    let answer = |id: &str| UserAnswer {
        question_id: id.to_owned(),
        selected: vec!["Approve".to_owned()],
        custom: None,
    };
    let view = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &member.user_id,
            Some(ResourcePermissions {
                submit: true,
                ..view
            }),
            now,
        )
        .await
        .unwrap();
    cloud
        .answer_question(tenant, &member.user_id, session, &answer("normal"), now)
        .await
        .unwrap();
    for id in ["plan", "global-plugin", "stop-tool"] {
        assert_eq!(
            cloud
                .answer_question(tenant, &member.user_id, session, &answer(id), now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
    assert_eq!(
        cloud
            .pending_questions(tenant, &member.user_id, session)
            .await
            .unwrap()
            .len(),
        3
    );
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &member.user_id,
            Some(ResourcePermissions {
                configure: true,
                ..view
            }),
            now,
        )
        .await
        .unwrap();
    cloud
        .answer_question(tenant, &member.user_id, session, &answer("plan"), now)
        .await
        .unwrap();
    for id in ["global-plugin", "stop-tool"] {
        assert_eq!(
            cloud
                .answer_question(tenant, &member.user_id, session, &answer(id), now)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied
        );
    }
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &member.user_id,
            Some(ResourcePermissions { stop: true, ..view }),
            now,
        )
        .await
        .unwrap();
    cloud
        .answer_question(tenant, &member.user_id, session, &answer("stop-tool"), now)
        .await
        .unwrap();
    cloud
        .answer_question(
            tenant,
            &owner.user_id,
            session,
            &answer("global-plugin"),
            now,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .pending_questions(tenant, &owner.user_id, session)
            .await
            .unwrap()
            .is_empty()
    );
    assert_shared_attachment_permissions(control, cloud, owner, member, &started, &compiled, now)
        .await;
    cloud
        .append_event(
            &started,
            "approval-worker",
            &SessionEvent {
                seq: 7,
                occurred_at_ms: now,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::TurnCancelled,
            },
            now,
        )
        .await
        .unwrap();
    cloud
        .finish_run(
            &started,
            "approval-worker",
            TerminalState::Cancelled,
            None,
            None,
            now,
        )
        .await
        .unwrap();
    let worker = ternilo_cloud::CloudWorkerIdentity {
        worker_id: ternilo_transport::ExecutorId::new("approval-worker"),
        instance_nonce: "storage-contract-approval-worker".to_owned(),
        generation: 1,
    };
    cloud
        .release_resident(&(&started).into(), &worker, worker.generation, now)
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify shared attachment history, queue ownership, opaque references and upload preservation against one running session."
)]
async fn assert_shared_attachment_permissions(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    started: &StartedRun,
    template: &CompiledRun,
    now: u64,
) {
    let tenant = &started.claim.tenant_id;
    let session = &started.claim.session_id;
    let private_bytes = b"private unreferenced object";
    let public_bytes = b"shared deliverable";
    let private = attachment_reference("private.txt", private_bytes);
    let public = attachment_reference("deliverable.txt", public_bytes);
    for (attachment, bytes) in [
        (&private, private_bytes.as_slice()),
        (&public, public_bytes.as_slice()),
    ] {
        cloud
            .store_attachment_object(started, "approval-worker", attachment, bytes, now)
            .await
            .unwrap();
    }
    // Mentioning the opaque digest in text is not an attachment reference.
    cloud
        .append_event(
            started,
            "approval-worker",
            &SessionEvent {
                seq: 5,
                occurred_at_ms: now,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::AssistantMessageDelta {
                    step: 0,
                    delta: private.content.clone(),
                },
            },
            now,
        )
        .await
        .unwrap();
    cloud
        .append_event(
            started,
            "approval-worker",
            &SessionEvent {
                seq: 6,
                occurred_at_ms: now,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::DeliverableProduced {
                    path: "deliverable.txt".to_owned(),
                    operation: "write".to_owned(),
                    attachment: public.clone(),
                },
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        cloud
            .resolve_session_attachment_as(tenant, &member.user_id, session, private.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        cloud
            .resolve_session_attachment_as(tenant, &owner.user_id, session, private.clone())
            .await
            .unwrap()
            .content,
        String::from_utf8(private_bytes.to_vec()).unwrap()
    );
    assert_eq!(
        cloud
            .resolve_session_attachment_as(tenant, &member.user_id, session, public.clone())
            .await
            .unwrap()
            .content,
        "shared deliverable"
    );
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &member.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                ..ResourcePermissions::default()
            }),
            now,
        )
        .await
        .unwrap();
    let submission = |actor: &ControlUser, id: &str, attachments: Vec<Attachment>| {
        let mut compiled = template.clone();
        compiled.actor_user_id = actor.user_id.clone();
        compiled.spec.metadata.run_id = RunId::new(id);
        compiled.spec.attachments.clone_from(&attachments);
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(compiled.spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: compiled.spec.input.clone(),
            },
            references: vec![],
            attachments,
        };
        (compiled, request)
    };
    let (forged, forged_request) =
        submission(member, "unrelated-attachment-run", vec![private.clone()]);
    assert_eq!(
        cloud
            .enqueue_session_submission_as(&member.user_id, &forged, &forged_request, now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let mut transaction = control.database().tenant_transaction(tenant).await.unwrap();
    let reservations: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_quota_reservations WHERE tenant_id=$1 AND run_id=$2",
    )
    .bind(tenant.as_str())
    .bind(forged.spec.metadata.run_id.as_str())
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(reservations, 0);
    transaction.commit().await.unwrap();
    let inline = Attachment {
        name: "new.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content: "member upload".to_owned(),
    };
    let (queued, queued_request) =
        submission(member, "member-inline-attachment", vec![inline.clone()]);
    let receipt = cloud
        .enqueue_session_submission_as(&member.user_id, &queued, &queued_request, now)
        .await
        .unwrap();
    let mut replacement = queued.clone();
    "edited".clone_into(&mut replacement.spec.input);
    replacement.spec.attachments = vec![private.clone()];
    assert_eq!(
        cloud
            .edit_queued_session_submission(
                tenant,
                &member.user_id,
                session,
                &receipt.submission.id,
                QueueEditRequest {
                    input: "edited".to_owned(),
                    expected_updated_at_ms: receipt.submission.updated_at_ms,
                },
                &replacement,
                now,
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let preserved = cloud
        .session_inbox(tenant, &member.user_id, session)
        .await
        .unwrap();
    assert_eq!(
        preserved
            .items
            .iter()
            .find(|item| item.id == receipt.submission.id)
            .unwrap()
            .attachments,
        vec![inline]
    );
    // Owner-approved queue attachments are part of this session before the
    // shared reader resolves them, even while the active turn is still running.
    let (owner_queue, owner_request) =
        submission(owner, "owner-reference-attachment", vec![private.clone()]);
    let owner_receipt = cloud
        .enqueue_session_submission_as(&owner.user_id, &owner_queue, &owner_request, now)
        .await
        .unwrap();
    assert_eq!(
        cloud
            .resolve_session_attachment_as(tenant, &member.user_id, session, private)
            .await
            .unwrap()
            .content,
        String::from_utf8(private_bytes.to_vec()).unwrap()
    );
    for run_id in [&receipt.run.run_id, &owner_receipt.run.run_id] {
        cloud
            .cancel_run_as(tenant, &owner.user_id, session, run_id, now)
            .await
            .unwrap();
    }
}

fn attachment_reference(name: &str, content: &[u8]) -> Attachment {
    let mut digest = String::new();
    for byte in Sha256::digest(content) {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    Attachment {
        name: name.to_owned(),
        media_type: "text/plain".to_owned(),
        content: format!("{}{digest}", ternilo_protocol::ATTACHMENT_REFERENCE_PREFIX),
    }
}

fn policy() -> WorkerPolicy {
    WorkerPolicy {
        catalog_revision: "sharing-catalog".to_owned(),
        policy_revision: "sharing-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 4,
        },
        max_run_attempts: 1,
        max_tenant_workspace_bytes: 1024 * 1024,
        max_tenant_workspace_entries: 100,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::new(),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::new(),
    }
}

fn provider(display_name: &str, base_url: &str) -> ProviderProfile {
    ProviderProfile {
        id: "route".to_owned(),
        display_name: display_name.to_owned(),
        base_url: base_url.to_owned(),
        protocol: ProviderProtocol::OpenAiResponses,
        api_key_ref: Some("OWNER_KEY".to_owned()),
        defaults: ProviderModelDefaults {
            context_window: 4096,
            max_output_tokens: 1024,
            reasoning: None,
        },
        models: vec![ProviderModel {
            id: "model".to_owned(),
            display_name: None,
            settings: ProviderModelSettings::Inherit,
        }],
        timeout_ms: 30_000,
        max_attempts: 2,
        retry_base_delay_ms: 100,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise real Cloud discovery, inherited grants and fork revocation with two different group members."
)]
async fn assert_group_access(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    member: &ControlUser,
    tenant: &ternilo_protocol::TenantId,
    session: &SessionId,
    now: u64,
) {
    let group = control
        .create_permission_group(
            owner,
            tenant,
            &GroupInput {
                name: "Reviewers".to_owned(),
                description: Some("Shared review access".to_owned()),
            },
            now,
        )
        .await
        .unwrap();
    let peer = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://test.example".to_owned(),
                subject: "review-peer".to_owned(),
                email: None,
                display_name: None,
            },
            "test-review-peer",
            now,
        )
        .await
        .unwrap();
    control
        .set_membership(owner, tenant, &peer.user_id, TenantRole::Member, now)
        .await
        .unwrap();
    for user in [&member.user_id, &peer.user_id] {
        control
            .set_permission_group_member(owner, tenant, &group.group_id, user, true, now)
            .await
            .unwrap();
    }
    let read = ResourcePermissions {
        view: true,
        ..ResourcePermissions::default()
    };
    let submit = ResourcePermissions {
        submit: true,
        ..read
    };
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &group.group_id,
            Some(submit),
            now,
        )
        .await
        .unwrap();
    let matches = cloud
        .list_accessible_sessions(tenant, &member.user_id, 100)
        .await
        .unwrap();
    assert!(matches.iter().any(|row| row.session_id == *session));
    let search = || SessionSearchRequest {
        query: "Shared edit".to_owned(),
        session_id: None,
        workspace_id: None,
        filters: SessionSearchFilters::default(),
        limit: 100,
    };
    assert!(
        !cloud
            .search_sessions(tenant, &member.user_id, search())
            .await
            .unwrap()
            .is_empty()
    );
    control
        .set_membership(owner, tenant, &member.user_id, TenantRole::Viewer, now + 1)
        .await
        .unwrap();
    let limited = control
        .resource_access(member, tenant, ResourceKind::Session, session.as_str())
        .await
        .unwrap();
    assert!(limited.role_limited && limited.permissions.view && !limited.permissions.submit);
    control
        .set_membership(owner, tenant, &member.user_id, TenantRole::Member, now + 2)
        .await
        .unwrap();

    let fork = cloud
        .fork_session(tenant, &member.user_id, session, None, now + 3)
        .await
        .unwrap();
    let nested = cloud
        .fork_session(tenant, &member.user_id, &fork.session_id, None, now + 4)
        .await
        .unwrap();
    assert_eq!(fork.user_id, owner.user_id);
    assert!(
        cloud
            .find_accessible_session(tenant, &peer.user_id, &fork.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .iter()
            .any(|row| row.session_id == nested.session_id)
    );
    assert!(
        !cloud
            .search_sessions(tenant, &member.user_id, search())
            .await
            .unwrap()
            .is_empty()
    );
    let stronger = ResourcePermissions {
        stop: true,
        ..submit
    };
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &group.group_id,
            Some(stronger),
            now + 5,
        )
        .await
        .unwrap();
    assert!(
        control
            .resource_access(member, tenant, ResourceKind::Session, session.as_str())
            .await
            .unwrap()
            .permissions
            .stop
    );
    assert!(
        !control
            .resource_access(
                member,
                tenant,
                ResourceKind::Session,
                fork.session_id.as_str()
            )
            .await
            .unwrap()
            .permissions
            .stop
    );
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &group.group_id,
            Some(read),
            now + 6,
        )
        .await
        .unwrap();
    let reduced = control
        .resource_access(
            member,
            tenant,
            ResourceKind::Session,
            nested.session_id.as_str(),
        )
        .await
        .unwrap();
    assert!(reduced.permissions.view && !reduced.permissions.submit);

    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            fork.session_id.as_str(),
            &member.user_id,
            Some(read),
            now + 7,
        )
        .await
        .unwrap();
    control
        .set_permission_group_member(
            owner,
            tenant,
            &group.group_id,
            &member.user_id,
            false,
            now + 8,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, session)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &nested.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &fork.session_id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        cloud
            .find_accessible_session(tenant, &peer.user_id, session)
            .await
            .unwrap()
            .is_some()
    );
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            fork.session_id.as_str(),
            &member.user_id,
            None,
            now + 9,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .search_sessions(tenant, &member.user_id, search())
            .await
            .unwrap()
            .is_empty()
    );

    control
        .set_permission_group_member(
            owner,
            tenant,
            &group.group_id,
            &member.user_id,
            true,
            now + 10,
        )
        .await
        .unwrap();
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &group.group_id,
            Some(submit),
            now + 11,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &nested.session_id)
            .await
            .unwrap()
            .is_none()
    );
    let last_fork = cloud
        .fork_session(tenant, &member.user_id, session, None, now + 12)
        .await
        .unwrap();
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &group.group_id,
            None,
            now + 13,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant, &member.user_id, &last_fork.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        cloud
            .search_sessions(tenant, &peer.user_id, search())
            .await
            .unwrap()
            .is_empty()
    );

    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Workspace,
            fork.workspace_id.as_str(),
            &group.group_id,
            Some(read),
            now + 14,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant, &peer.user_id, &fork.session_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .len()
            >= 4
    );
    control
        .delete_permission_group(owner, tenant, &group.group_id, now + 15)
        .await
        .unwrap();
    assert!(
        cloud
            .list_accessible_sessions(tenant, &member.user_id, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        cloud
            .search_sessions(tenant, &peer.user_id, search())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        cloud.get_session(tenant, session).await.unwrap().user_id,
        owner.user_id
    );
    let deletion_group = control
        .create_permission_group(
            owner,
            tenant,
            &GroupInput {
                name: "Deletion check".to_owned(),
                description: None,
            },
            now + 16,
        )
        .await
        .unwrap();
    control
        .set_permission_group_member(
            owner,
            tenant,
            &deletion_group.group_id,
            &member.user_id,
            true,
            now + 16,
        )
        .await
        .unwrap();
    control
        .set_resource_group_share(
            owner,
            tenant,
            ResourceKind::Session,
            last_fork.session_id.as_str(),
            &deletion_group.group_id,
            Some(read),
            now + 17,
        )
        .await
        .unwrap();
    control
        .set_resource_share(
            owner,
            tenant,
            ResourceKind::Session,
            last_fork.session_id.as_str(),
            &peer.user_id,
            Some(read),
            now + 17,
        )
        .await
        .unwrap();
    cloud
        .delete_session(tenant, &last_fork.session_id, &owner.user_id, now + 18)
        .await
        .unwrap();
    let mut tx = control.database().tenant_transaction(tenant).await.unwrap();
    let retained: i64 = sqlx::query_scalar("SELECT
        (SELECT COUNT(*) FROM control_resource_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2)
        + (SELECT COUNT(*) FROM control_resource_group_shares WHERE tenant_id=$1 AND resource_kind='session' AND resource_id=$2)
        + (SELECT COUNT(*) FROM control_resource_fork_group_sources WHERE tenant_id=$1 AND session_id=$2)")
        .bind(tenant.as_str()).bind(last_fork.session_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        retained, 0,
        "deleted session IDs must not retain grants for a future resource"
    );
    tx.commit().await.unwrap();
}
