use std::{collections::BTreeSet, time::Duration};

use ternilo_cloud::{CloudRunDraft, CloudSessionDraft, CloudStore, TerminalState, WorkerPolicy};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, FeedbackRating, ModelResponse, ModelUsage, PermissionPreset, ReasoningEffort, RunId,
    RunLimits, RunOutcome, SessionCommandOutcomeKind, SessionEvent, SessionEventKind, SessionId,
    SessionMode, TenantId,
};
#[path = "support/model_ledger.rs"]
mod model_ledger;
#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_cloud_session_fork_feedback_and_archive_are_durable() {
    let database_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(database_url.contains("ternilo_cloud_test"));
    let runtime_url =
        server_runtime::initialize(&database_url, "ternilo_lifecycle_runtime_test", [18; 32]).await;
    let control = ControlStore::connect(
        &runtime_url,
        Some(&database_url),
        SecretCipher::from_key([18; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&database_url), 4)
        .await
        .unwrap();
    lifecycle_contract(control, cloud).await;
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
}

#[tokio::test]
async fn sqlite_cloud_lifecycle_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("lifecycle.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([18; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    lifecycle_contract(control, cloud).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise feedback, canonical events, fork history and archive visibility in one shared lifecycle."
)]
async fn lifecycle_contract(control: ControlStore, cloud: CloudStore) {
    worker_storage::bind_workers(&cloud, &["lifecycle-worker"], "shared-contract-storage").await;
    let now = 2_100_000_000_000_u64;
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://issuer.example".to_owned(),
                subject: "cloud-lifecycle-user".to_owned(),
                email: None,
                display_name: Some("Lifecycle user".to_owned()),
            },
            "test-cloud-lifecycle-user",
            now,
        )
        .await
        .unwrap();
    let tenant = control
        .create_tenant(
            &user,
            "cloud-lifecycle",
            "Cloud lifecycle",
            TenantQuota {
                max_nodes: 1,
                max_concurrent_runs: 2,
                monthly_model_tokens: 10_000,
                max_secrets: 4,
            },
            now + 1,
        )
        .await
        .unwrap();
    let project = control
        .create_project(&user, &tenant.tenant_id, "Lifecycle", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &user,
            &tenant.tenant_id,
            &project.project_id,
            "Lifecycle workspace",
            now + 3,
        )
        .await
        .unwrap();
    let model = model_ledger::configure(
        &control,
        &user,
        &tenant.tenant_id,
        "model",
        Some(ReasoningEffort::Medium),
        now + 3,
    )
    .await;
    let parent_id = SessionId::new("lifecycle-parent");
    let parent = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(parent_id.clone()),
                agent_id: AgentId::new("agent"),
                title: "New session".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: Some(model.clone()),
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &user.user_id,
            now + 4,
        )
        .await
        .unwrap();
    assert_eq!(parent.parent_session_id, None);
    assert_eq!(parent.archived_at_ms, None);

    let command_session_id = SessionId::new("lifecycle-feedback-command");
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(command_session_id.clone()),
                agent_id: AgentId::new("agent"),
                title: "Command feedback".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: Some(model.clone()),
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &user.user_id,
            now + 4,
        )
        .await
        .unwrap();
    let accepted_command = cloud
        .record_command_feedback(
            &tenant.tenant_id,
            &user.user_id,
            &command_session_id,
            "  cloud feedback  ".to_owned(),
            now + 5,
        )
        .await
        .unwrap();
    assert_eq!(accepted_command.events.len(), 3);
    assert!(matches!(
        &accepted_command.events[1].kind,
        SessionEventKind::FeedbackSubmitted { text, .. } if text == "cloud feedback"
    ));
    let rejected_command = cloud
        .record_command_feedback(
            &tenant.tenant_id,
            &user.user_id,
            &command_session_id,
            "  ".to_owned(),
            now + 6,
        )
        .await
        .unwrap();
    assert!(matches!(
        &rejected_command.events[1].kind,
        SessionEventKind::CommandFinished { outcome, .. }
            if outcome.kind == SessionCommandOutcomeKind::Error
                && outcome.code == "feedback_text_required"
    ));
    assert_eq!(
        cloud
            .session_events(&tenant.tenant_id, &command_session_id, None, 100)
            .await
            .unwrap()
            .len(),
        5
    );
    assert!(
        cloud
            .active_session_runs(&tenant.tenant_id, &user.user_id, &command_session_id)
            .await
            .unwrap()
            .is_empty()
    );

    let policy = policy();
    let catalog = model_ledger::catalog("lifecycle-catalog");
    let run_id = RunId::new("lifecycle-run");
    let compiled = policy
        .compile_run(
            CloudRunDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                agent_id: AgentId::new("agent"),
                session_id: parent_id.clone(),
                run_id: Some(run_id.clone()),
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                },
                permissions: PermissionPreset::WorkspaceWrite,
                mode: ternilo_protocol::SessionMode::Execute,
                profile: model_ledger::profile(&model),
                input: "hello".to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 100,
            },
            tenant.tenant_id.clone(),
            user.user_id.clone(),
            user.user_id.clone(),
            &catalog,
        )
        .unwrap();
    let reservation = control
        .reserve_quota(
            &user,
            &tenant.tenant_id,
            Some(run_id.as_str()),
            100,
            Duration::from_hours(1),
            now + 5,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&compiled, &reservation.reservation_id, now + 6)
        .await
        .unwrap();
    let claim = cloud
        .claim_run("lifecycle-worker", Duration::from_secs(30), now + 7)
        .await
        .unwrap()
        .unwrap();
    let started = cloud
        .start_run(claim, "lifecycle-worker", Duration::from_secs(30), now + 8)
        .await
        .unwrap()
        .unwrap();
    let mut events = completed_turn(&run_id, now + 9);
    if let SessionEventKind::UserMessage { provenance, .. } = &mut events[1].kind {
        provenance.clone_from(&started.claim.provenance);
    }
    for event in &events {
        cloud
            .append_event(&started, "lifecycle-worker", event, event.occurred_at_ms)
            .await
            .unwrap();
    }
    let accepted = model_ledger::accept(
        &control,
        &cloud,
        &started,
        "lifecycle-worker",
        "lifecycle-call",
        100,
        now + 12,
    )
    .await
    .unwrap();
    model_ledger::settle(
        &control,
        &accepted.request.request_id,
        ModelUsage {
            input_tokens: 4,
            output_tokens: 2,
            cached_input_tokens: 1,
            cache_write_tokens: Some(1),
            reasoning_tokens: 0,
        },
        None,
        now + 13,
    )
    .await
    .unwrap();
    cloud
        .finish_run(
            &started,
            "lifecycle-worker",
            TerminalState::Succeeded,
            Some(&RunOutcome {
                answer: "answer".to_owned(),
                steps: 1,
                tool_calls: 0,
                events: events.clone(),
                generated_title: Some("Cloud lifecycle review".to_owned()),
            }),
            None,
            now + 14,
        )
        .await
        .unwrap();

    let feedback = cloud
        .record_session_feedback(
            &tenant.tenant_id,
            &user.user_id,
            &parent_id,
            2,
            0,
            Some(FeedbackRating::Positive),
            Some(" useful ".to_owned()),
            now + 15,
        )
        .await
        .unwrap();
    assert_eq!(feedback.seq, 5);
    assert!(matches!(
        feedback.kind,
        SessionEventKind::FeedbackRecorded {
            target_seq: 2,
            revision: 1,
            note: Some(ref note),
            ..
        } if note == "useful"
    ));
    let conflict = cloud
        .record_session_feedback(
            &tenant.tenant_id,
            &user.user_id,
            &parent_id,
            2,
            0,
            Some(FeedbackRating::Negative),
            Some("concurrent".to_owned()),
            now + 16,
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code, ternilo_protocol::ErrorCode::Conflict);

    let child = cloud
        .fork_session(
            &tenant.tenant_id,
            &user.user_id,
            &parent_id,
            Some(2),
            now + 17,
        )
        .await
        .unwrap();
    assert_eq!(child.parent_session_id.as_ref(), Some(&parent_id));
    assert_eq!(child.title, "Cloud lifecycle review (1)");
    assert_eq!(child.last_seq, Some(3));
    let child_events = cloud
        .session_events(&tenant.tenant_id, &child.session_id, None, 100)
        .await
        .unwrap();
    assert_eq!(child_events, events[..4]);

    assert_shared_fork(
        &control,
        &cloud,
        &user,
        &tenant.tenant_id,
        &parent_id,
        now + 17,
    )
    .await;

    let archived = cloud
        .archive_session(&tenant.tenant_id, &user.user_id, &parent_id, now + 18)
        .await
        .unwrap();
    assert_eq!(archived.archived_at_ms, Some(now + 18));
    assert!(
        cloud
            .find_owned_session(&tenant.tenant_id, &user.user_id, &parent_id)
            .await
            .unwrap()
            .is_none()
    );
    let visible = cloud.list_sessions(&tenant.tenant_id, 100).await.unwrap();
    assert_eq!(visible.len(), 2);
    assert!(
        visible
            .iter()
            .any(|session| session.session_id == child.session_id)
    );
    assert!(
        visible
            .iter()
            .any(|session| session.session_id == command_session_id)
    );
    assert_archive_restore_contract(&cloud, archived, now + 19).await;
}

async fn assert_archive_restore_contract(
    cloud: &CloudStore,
    mut archived: ternilo_cloud::CloudSessionRecord,
    now: u64,
) {
    let tenant = &archived.tenant_id;
    let actor = &archived.user_id;
    let session_id = &archived.session_id;
    let events = cloud
        .session_events_as(tenant, actor, session_id, None, 1000)
        .await
        .unwrap();
    let page = cloud
        .session_history_as(
            tenant,
            actor,
            session_id,
            ternilo_protocol::SessionHistoryQuery {
                before_seq: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.events, events[events.len().saturating_sub(2)..]);
    if let Some(before) = page.next_before_seq {
        let earlier = cloud
            .session_history_as(
                tenant,
                actor,
                session_id,
                ternilo_protocol::SessionHistoryQuery {
                    before_seq: Some(before),
                    limit: 1000,
                },
            )
            .await
            .unwrap();
        assert_eq!(earlier.events, events[..events.len() - 2]);
        assert_eq!(earlier.next_before_seq, None);
    }
    assert!(
        cloud
            .session_history_as(
                tenant,
                &ternilo_protocol::UserId::new("unknown-history-reader"),
                session_id,
                ternilo_protocol::SessionHistoryQuery::default()
            )
            .await
            .is_err()
    );
    let restored = cloud
        .restore_session(tenant, actor, session_id, now)
        .await
        .unwrap();
    assert_eq!(
        cloud
            .session_events_as(tenant, actor, session_id, None, 1000)
            .await
            .unwrap(),
        events
    );
    archived.archived_at_ms = None;
    assert_eq!(restored, archived);
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify a user-created fork copies only the initiating grantee's permissions and remains independently revocable."
)]
async fn assert_shared_fork(
    control: &ControlStore,
    cloud: &CloudStore,
    owner: &ControlUser,
    tenant_id: &TenantId,
    parent_id: &SessionId,
    now: u64,
) {
    let bootstrap = control
        .initialize_owner(
            &NativeRegistration {
                email: "fork-instance-owner@example.test".to_owned(),
                username: "fork-instance-owner".to_owned(),
                password: "fork-instance-password".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    control
        .set_instance_mode(
            &bootstrap.session.user,
            InstanceMode::MultiUser,
            bootstrap.session.instance.revision,
            now,
        )
        .await
        .unwrap();
    let mut grantees = Vec::new();
    for subject in ["fork-author", "parent-viewer"] {
        let member = control
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://issuer.example".to_owned(),
                    subject: subject.to_owned(),
                    email: None,
                    display_name: None,
                },
                &format!("test-{subject}"),
                now,
            )
            .await
            .unwrap();
        control
            .set_membership(owner, tenant_id, &member.user_id, TenantRole::Member, now)
            .await
            .unwrap();
        grantees.push(member);
    }
    let author = &grantees[0];
    let viewer = &grantees[1];
    let permissions = ResourcePermissions {
        view: true,
        submit: true,
        stop: true,
        configure: false,
    };
    control
        .set_resource_share(
            owner,
            tenant_id,
            ResourceKind::Session,
            parent_id.as_str(),
            &author.user_id,
            Some(permissions),
            now,
        )
        .await
        .unwrap();
    control
        .set_resource_share(
            owner,
            tenant_id,
            ResourceKind::Session,
            parent_id.as_str(),
            &viewer.user_id,
            Some(ResourcePermissions {
                view: true,
                ..ResourcePermissions::default()
            }),
            now,
        )
        .await
        .unwrap();
    let child = cloud
        .fork_session(tenant_id, &author.user_id, parent_id, None, now)
        .await
        .unwrap();
    assert_eq!(child.user_id, owner.user_id);
    assert_eq!(child.parent_session_id.as_ref(), Some(parent_id));
    let access = control
        .resource_access(
            author,
            tenant_id,
            ResourceKind::Session,
            child.session_id.as_str(),
        )
        .await
        .unwrap();
    assert!(!access.is_owner);
    assert_eq!(access.permissions, permissions);
    assert!(
        cloud
            .find_accessible_session(tenant_id, &author.user_id, &child.session_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        cloud
            .find_accessible_session(tenant_id, &viewer.user_id, &child.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        cloud
            .session_events_as(tenant_id, &author.user_id, &child.session_id, None, 100,)
            .await
            .unwrap()
            .len(),
        4
    );
    let shares = control
        .list_resource_shares(
            owner,
            tenant_id,
            ResourceKind::Session,
            child.session_id.as_str(),
            &ternilo_control::PageQuery::default(),
        )
        .await
        .unwrap();
    assert_eq!(shares.shares.len(), 1);
    assert!(matches!(&shares.shares[0].subject,
        ternilo_control::ShareSubject::User { user } if user.user_id == author.user_id));
    let audit = control.list_audit(owner, tenant_id, 100).await.unwrap();
    assert!(
        audit
            .iter()
            .any(|entry| entry.action == "resource.fork_access"
                && entry.resource_id == child.session_id.as_str()
                && entry.actor_user_id.as_ref() == Some(&author.user_id)
                && entry.metadata["owner_user_id"] == owner.user_id.as_str())
    );
    control
        .set_resource_share(
            owner,
            tenant_id,
            ResourceKind::Session,
            child.session_id.as_str(),
            &author.user_id,
            None,
            now,
        )
        .await
        .unwrap();
    assert!(
        cloud
            .find_accessible_session(tenant_id, &author.user_id, &child.session_id)
            .await
            .unwrap()
            .is_none()
    );
    cloud
        .delete_session(tenant_id, &child.session_id, &owner.user_id, now)
        .await
        .unwrap();
}

fn completed_turn(run_id: &RunId, now: u64) -> Vec<SessionEvent> {
    vec![
        SessionEvent {
            seq: 0,
            occurred_at_ms: now,
            run_id: run_id.clone(),
            kind: SessionEventKind::TurnStarted,
        },
        SessionEvent {
            seq: 1,
            occurred_at_ms: now + 1,
            run_id: run_id.clone(),
            kind: SessionEventKind::UserMessage {
                provenance: None,
                content: "hello".to_owned(),
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: Vec::new(),
            },
        },
        SessionEvent {
            seq: 2,
            occurred_at_ms: now + 2,
            run_id: run_id.clone(),
            kind: SessionEventKind::AssistantMessage {
                step: 1,
                response: ModelResponse {
                    provider: "contract-provider".to_owned(),
                    model: "model".to_owned(),
                    content: "answer".to_owned(),
                    reasoning_content: None,
                    provider_state: None,
                    tool_calls: Vec::new(),
                    usage: Some(ModelUsage {
                        input_tokens: 4,
                        output_tokens: 2,
                        cached_input_tokens: 1,
                        cache_write_tokens: Some(1),
                        reasoning_tokens: 0,
                    }),
                    finish_reason: ternilo_protocol::ModelFinishReason::Stop,
                    provider_request_id: None,
                    attempts: 1,
                    request_digest: None,
                    replayed: false,
                },
            },
        },
        SessionEvent {
            seq: 3,
            occurred_at_ms: now + 3,
            run_id: run_id.clone(),
            kind: SessionEventKind::TurnFinished {
                answer: "answer".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        },
        SessionEvent {
            seq: 4,
            occurred_at_ms: now + 4,
            run_id: run_id.clone(),
            kind: SessionEventKind::SessionTitleGenerated {
                title: "Cloud lifecycle review".to_owned(),
            },
        },
    ]
}

fn policy() -> WorkerPolicy {
    WorkerPolicy {
        catalog_revision: "lifecycle-catalog".to_owned(),
        policy_revision: "lifecycle-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 4,
        },
        max_run_attempts: 1,
        max_tenant_workspace_bytes: 1024 * 1024,
        max_tenant_workspace_entries: 100,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::from([ternilo_cloud::BROKERED_MODEL_KIND.to_owned()]),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::new(),
    }
}
