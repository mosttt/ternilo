use std::{collections::BTreeSet, time::Duration};

use sqlx::{AnyPool, Row};
use ternilo_cloud::{
    CloudRunDraft, CloudRunState, CloudSessionDraft, CloudSessionRecord, CloudStore, CompiledRun,
    TerminalState, WorkerPolicy,
};
use ternilo_control::{
    AccountStatus, AccountStatusAction, ControlStore, ControlUser, InstanceMode,
    NativeRegistration, OidcPrincipal, ResourceKind, ResourcePermissions, SecretCipher,
    TenantQuota, TenantRole,
};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    AgentId, ErrorCode, PermissionPreset, Profile, QueueEditRequest, RunId, RunLimits, SessionId,
    SessionMode, SessionSubmissionRequest, SubagentId, SubagentSessionMetadata,
    SubagentTranscriptKind, SubmissionContent, SubmissionDelivery, SubmissionPlacement, TenantId,
};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

const NOW: u64 = 2_000_000_000_000;
const WORKER: &str = "account-cleanup-worker";

struct Fixture {
    control: ControlStore,
    cloud: CloudStore,
    audit: AnyPool,
    admin: ControlUser,
    alice: ControlUser,
    alice_token: String,
    bob: ControlUser,
    sessions: Vec<CloudSessionRecord>,
}

impl Fixture {
    #[expect(
        clippy::too_many_lines,
        reason = "Provision the same real accounts and shared resources for both database contracts."
    )]
    async fn new(control: ControlStore, cloud: CloudStore, audit: AnyPool) -> Self {
        let admin = control
            .initialize_owner(
                &NativeRegistration {
                    email: "cleanup-owner@example.test".to_owned(),
                    username: "cleanup-owner".to_owned(),
                    password: "account-cleanup-password".to_owned(),
                },
                NOW,
            )
            .await
            .unwrap()
            .session
            .user;
        control
            .set_instance_mode(&admin, InstanceMode::MultiUser, 1, NOW + 1)
            .await
            .unwrap();
        let alice = user(&control, "cleanup-alice").await;
        let bob = user(&control, "cleanup-bob").await;
        let alice_token = control
            .create_browser_session(alice.clone(), NOW + 2)
            .await
            .unwrap()
            .access_token;
        let mut sessions = Vec::new();
        for (index, name) in ["parent", "leased", "other-tenant"].iter().enumerate() {
            let tenant = control
                .create_tenant(
                    &bob,
                    &format!("cleanup-{name}"),
                    "Cleanup resource owner",
                    TenantQuota {
                        max_concurrent_runs: 10,
                        monthly_model_tokens: 100_000,
                        ..TenantQuota::default()
                    },
                    NOW + 2,
                )
                .await
                .unwrap();
            control
                .set_membership(
                    &bob,
                    &tenant.tenant_id,
                    &alice.user_id,
                    TenantRole::Member,
                    NOW + 3,
                )
                .await
                .unwrap();
            let project = control
                .create_project(&bob, &tenant.tenant_id, "Cleanup project", NOW + 4)
                .await
                .unwrap();
            let workspace = control
                .create_cloud_workspace(
                    &bob,
                    &tenant.tenant_id,
                    &project.project_id,
                    "Cleanup",
                    NOW + 5,
                )
                .await
                .unwrap();
            let session = cloud
                .create_session(
                    CloudSessionDraft {
                        project_id: project.project_id,
                        workspace_id: workspace.workspace_id,
                        session_id: Some(SessionId::new(format!("cleanup-session-{index}"))),
                        agent_id: AgentId::new("agent"),
                        title: "Cleanup shared session".to_owned(),
                        permissions: PermissionPreset::WorkspaceWrite,
                        model: None,
                        reserved_model_tokens: 100,
                        agent_preset: "standard".to_owned(),
                        profile_plugins: Vec::new(),
                        mode: SessionMode::Execute,
                    },
                    &tenant.tenant_id,
                    &bob.user_id,
                    NOW + 6,
                )
                .await
                .unwrap();
            control
                .set_resource_share(
                    &bob,
                    &tenant.tenant_id,
                    ResourceKind::Session,
                    session.session_id.as_str(),
                    &alice.user_id,
                    Some(ResourcePermissions {
                        view: true,
                        submit: true,
                        stop: true,
                        configure: false,
                    }),
                    NOW + 7,
                )
                .await
                .unwrap();
            sessions.push(session);
        }
        worker_storage::bind_workers(&cloud, &[WORKER], "cleanup-storage").await;
        Self {
            control,
            cloud,
            audit,
            admin,
            alice,
            alice_token,
            bob,
            sessions,
        }
    }

    fn compiled(&self, index: usize, actor: &ControlUser, run_id: &str) -> CompiledRun {
        let session = &self.sessions[index];
        WorkerPolicy {
            catalog_revision: "account-cleanup-catalog".to_owned(),
            policy_revision: "account-cleanup-policy".to_owned(),
            maximum_limits: RunLimits {
                max_steps: 2,
                max_tool_calls: 2,
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
        .compile_run(
            CloudRunDraft {
                project_id: session.project_id.clone(),
                workspace_id: session.workspace_id.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session.session_id.clone(),
                run_id: Some(RunId::new(run_id)),
                limits: RunLimits {
                    max_steps: 1,
                    max_tool_calls: 1,
                },
                permissions: session.permissions,
                mode: session.mode,
                profile: Profile::default(),
                input: run_id.to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 100,
            },
            session.tenant_id.clone(),
            session.user_id.clone(),
            actor.user_id.clone(),
            &Catalog::new("account-cleanup-catalog"),
        )
        .unwrap()
    }

    async fn enqueue(&self, index: usize, actor: &ControlUser, run_id: &str, now: u64) {
        let compiled = self.compiled(index, actor, run_id);
        self.cloud
            .enqueue_session_submission_as(&actor.user_id, &compiled, &request(&compiled), now)
            .await
            .unwrap();
    }

    async fn state(&self, tenant: &TenantId, run_id: &str) -> CloudRunState {
        self.cloud
            .get_run(tenant, &RunId::new(run_id))
            .await
            .unwrap()
            .state
    }

    async fn reservation_state(&self, run_id: &str) -> String {
        sqlx::query_scalar("SELECT state FROM control_quota_reservations WHERE run_id=$1")
            .bind(run_id)
            .fetch_one(&self.audit)
            .await
            .unwrap()
    }
}

async fn sqlite_fixture() -> (tempfile::TempDir, Fixture) {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("cleanup.sqlite").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([37; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let audit = cloud.database().pool().clone();
    (directory, Fixture::new(control, cloud, audit).await)
}

#[tokio::test]
async fn sqlite_account_cleanup_preserves_other_authors_and_requires_execution_receipts() {
    let (_directory, fixture) = sqlite_fixture().await;
    queued_authorization_contract(&fixture).await;
    cleanup_contract(&fixture).await;
}

#[tokio::test]
async fn sqlite_failed_cleanup_rolls_back_account_and_preceding_cancellations() {
    let (_directory, fixture) = sqlite_fixture().await;
    atomic_failure_contract(&fixture).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_account_cleanup_is_atomic_scoped_and_serializes_admission() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_cloud_test"));
    let runtime_url =
        server_runtime::initialize(&admin_url, "ternilo_cleanup_runtime_test", [37; 32]).await;
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
    let control = ControlStore::connect(&runtime_url, None, SecretCipher::from_key([37; 32]), 6)
        .await
        .unwrap();
    let cloud = CloudStore::connect_without_migrations(&runtime_url, 6)
        .await
        .unwrap();
    let audit_database = ternilo_storage::Database::connect(&admin_url, 4)
        .await
        .unwrap();
    let fixture = Fixture::new(control, cloud, audit_database.pool().clone()).await;
    multi_session_lock_contract(&fixture).await;
    admission_lock_contract(&fixture, &admin_url).await;
    expiration_lock_contract(&fixture).await;
    resume_cleanup_lock_contract(&fixture, &admin_url).await;
    queued_authorization_contract(&fixture).await;
    cleanup_contract(&fixture).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise one accepted parent, child, leased run and shared queue through ban, unban, removal and a real finish receipt."
)]
async fn cleanup_contract(fixture: &Fixture) {
    fixture
        .enqueue(0, &fixture.alice, "alice-parent", NOW + 20)
        .await;
    let claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 21)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, RunId::new("alice-parent"));
    let parent = fixture
        .cloud
        .start_run(claim, WORKER, Duration::from_secs(60), NOW + 22)
        .await
        .unwrap()
        .unwrap();
    let child_session_id = SessionId::new("cleanup-child");
    fixture
        .cloud
        .create_subagent_for_worker(
            WORKER,
            &parent,
            &child_session_id,
            &SubagentSessionMetadata {
                subagent_id: SubagentId::new("cleanup-helper"),
                provider: "in-process".to_owned(),
                transcript_kind: SubagentTranscriptKind::Conversation,
            },
            "Cleanup helper",
            NOW + 23,
        )
        .await
        .unwrap();
    let mut child_spec = parent.claim.spec.clone();
    child_spec.metadata.session_id = child_session_id.clone();
    child_spec.metadata.run_id = RunId::new("alice-child");
    "child input".clone_into(&mut child_spec.input);
    fixture
        .cloud
        .enqueue_subagent_for_worker(
            WORKER,
            &parent,
            &child_session_id,
            &child_spec,
            "child input",
            1,
            NOW + 24,
        )
        .await
        .unwrap();
    fixture
        .enqueue(1, &fixture.alice, "alice-leased", NOW + 25)
        .await;
    let child_claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 26)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_claim.actor_user_id, fixture.alice.user_id);
    let leased_claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 27)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(leased_claim.run_id, RunId::new("alice-leased"));
    fixture
        .enqueue(0, &fixture.alice, "alice-queued", NOW + 28)
        .await;
    fixture
        .enqueue(0, &fixture.bob, "bob-same-session", NOW + 29)
        .await;
    fixture
        .enqueue(2, &fixture.alice, "alice-other-tenant", NOW + 30)
        .await;
    fixture
        .enqueue(2, &fixture.bob, "bob-other-tenant", NOW + 31)
        .await;
    let account = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            account.status_revision,
            NOW + 32,
        )
        .await
        .unwrap();
    assert_eq!(banned.status, AccountStatus::Banned);
    assert!(
        fixture
            .control
            .authenticate_native_session(&fixture.alice_token, NOW + 33)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.state(&parent.claim.tenant_id, "alice-parent").await,
        CloudRunState::CancelRequested
    );
    assert_eq!(fixture.reservation_state("alice-parent").await, "active");
    for (tenant, run) in [
        (&fixture.sessions[0].tenant_id, "alice-child"),
        (&fixture.sessions[0].tenant_id, "alice-queued"),
        (&fixture.sessions[1].tenant_id, "alice-leased"),
        (&fixture.sessions[2].tenant_id, "alice-other-tenant"),
    ] {
        assert_eq!(
            fixture.state(tenant, run).await,
            CloudRunState::Cancelled,
            "{run}"
        );
        assert_eq!(fixture.reservation_state(run).await, "released", "{run}");
    }
    for (index, run) in [(0, "bob-same-session"), (2, "bob-other-tenant")] {
        assert_eq!(
            fixture.state(&fixture.sessions[index].tenant_id, run).await,
            CloudRunState::Queued
        );
        assert_eq!(fixture.reservation_state(run).await, "active");
    }
    assert!(
        fixture
            .cloud
            .start_run(leased_claim, WORKER, Duration::from_secs(60), NOW + 33)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .cloud
            .start_run(child_claim, WORKER, Duration::from_secs(60), NOW + 33)
            .await
            .unwrap()
            .is_none()
    );
    let reopened = CloudStore::connect_without_migrations(
        fixture
            .cloud
            .database()
            .pool()
            .connect_options()
            .database_url
            .as_str(),
        2,
    )
    .await
    .unwrap();
    let mut tx = reopened
        .database()
        .owner_transaction(&parent.claim.tenant_id, &fixture.bob.user_id)
        .await
        .unwrap();
    let commands = sqlx::query("SELECT command_json, actor_user_id, state FROM cloud_session_commands WHERE target_run_id=$1")
        .bind(parent.claim.run_id.as_str()).fetch_all(&mut *tx).await.unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].try_get::<String, _>("state").unwrap(),
        "pending"
    );
    assert_eq!(
        commands[0].try_get::<String, _>("actor_user_id").unwrap(),
        fixture.admin.user_id.as_str()
    );
    let command = commands[0]
        .try_get::<ternilo_storage::Json<ternilo_transport::ExecutorCommand>, _>("command_json")
        .unwrap()
        .0;
    assert!(
        matches!(command.body, ternilo_transport::ExecutorCommandBody::CancelRun { ref run_id, .. } if run_id == &parent.claim.run_id)
    );
    tx.commit().await.unwrap();
    let active = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 34,
        )
        .await
        .unwrap();
    assert_eq!(active.status, AccountStatus::Active);
    assert!(
        fixture
            .control
            .authenticate_native_session(&fixture.alice_token, NOW + 34)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.state(&parent.claim.tenant_id, "alice-parent").await,
        CloudRunState::CancelRequested
    );
    assert_eq!(
        fixture
            .state(&fixture.sessions[1].tenant_id, "alice-leased")
            .await,
        CloudRunState::Cancelled
    );
    let removed = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Remove,
            active.status_revision,
            NOW + 35,
        )
        .await
        .unwrap();
    assert_eq!(removed.status, AccountStatus::Removed);
    let command_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cloud_session_commands WHERE target_run_id=$1")
            .bind(parent.claim.run_id.as_str())
            .fetch_one(&fixture.audit)
            .await
            .unwrap();
    assert_eq!(
        command_count, 1,
        "removal reuses the durable cancellation for the same lease"
    );
    let mut child_again = child_spec;
    child_again.metadata.run_id = RunId::new("alice-child-after-remove");
    "late child".clone_into(&mut child_again.input);
    assert!(
        fixture
            .cloud
            .enqueue_subagent_for_worker(
                WORKER,
                &parent,
                &child_session_id,
                &child_again,
                "late child",
                1,
                NOW + 36
            )
            .await
            .is_err()
    );
    let bob_claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 36)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bob_claim.run_id, RunId::new("bob-other-tenant"));
    assert_eq!(bob_claim.actor_user_id, fixture.bob.user_id);
    assert!(
        fixture
            .cloud
            .start_run(bob_claim, WORKER, Duration::from_secs(60), NOW + 37)
            .await
            .unwrap()
            .is_some()
    );
    fixture
        .cloud
        .finish_run(
            &parent,
            WORKER,
            TerminalState::Cancelled,
            None,
            None,
            NOW + 38,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.state(&parent.claim.tenant_id, "alice-parent").await,
        CloudRunState::Cancelled
    );
    assert_ne!(fixture.reservation_state("alice-parent").await, "active");
}

async fn atomic_failure_contract(fixture: &Fixture) {
    let running_index = (0..fixture.sessions.len())
        .max_by_key(|index| fixture.sessions[*index].tenant_id.as_str())
        .unwrap();
    let queued_index = (0..fixture.sessions.len())
        .min_by_key(|index| fixture.sessions[*index].tenant_id.as_str())
        .unwrap();
    fixture
        .enqueue(running_index, &fixture.alice, "rollback-run", NOW + 20)
        .await;
    let claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 21)
        .await
        .unwrap()
        .unwrap();
    let started = fixture
        .cloud
        .start_run(claim, WORKER, Duration::from_secs(60), NOW + 22)
        .await
        .unwrap()
        .unwrap();
    fixture
        .enqueue(queued_index, &fixture.alice, "rollback-queued", NOW + 23)
        .await;
    sqlx::query("UPDATE cloud_runs SET session_fencing_token=NULL WHERE run_id=$1")
        .bind(started.claim.run_id.as_str())
        .execute(&fixture.audit)
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .set_account_status(
                &fixture.admin,
                &fixture.alice.user_id,
                AccountStatusAction::Ban,
                1,
                NOW + 24
            )
            .await
            .is_err()
    );
    let account = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    assert_eq!(
        (account.status, account.status_revision),
        (AccountStatus::Active, 1)
    );
    assert!(
        fixture
            .control
            .authenticate_native_session(&fixture.alice_token, NOW + 25)
            .await
            .is_ok()
    );
    assert_eq!(
        fixture
            .state(&started.claim.tenant_id, "rollback-run")
            .await,
        CloudRunState::Running
    );
    assert_eq!(
        fixture
            .state(&fixture.sessions[queued_index].tenant_id, "rollback-queued")
            .await,
        CloudRunState::Queued
    );
    assert_eq!(fixture.reservation_state("rollback-queued").await, "active");
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify source attribution, execution reauthorization and batch separation together without running a worker."
)]
async fn queued_authorization_contract(fixture: &Fixture) {
    let session = &fixture.sessions[2];
    fixture
        .enqueue(2, &fixture.bob, "authorization-blocker", NOW + 8)
        .await;
    fixture
        .enqueue(2, &fixture.alice, "source-alice-execution-alice", NOW + 9)
        .await;
    fixture
        .enqueue(2, &fixture.alice, "source-alice-execution-bob", NOW + 10)
        .await;
    let queued = fixture
        .cloud
        .session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
        )
        .await
        .unwrap();
    let second = queued
        .items
        .iter()
        .find(|item| item.run_id == RunId::new("source-alice-execution-bob"))
        .unwrap();
    let mut replacement = fixture.compiled(2, &fixture.bob, "source-alice-execution-bob");
    "edited by Bob".clone_into(&mut replacement.spec.input);
    let edited = fixture
        .cloud
        .edit_queued_session_submission(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
            &second.id,
            QueueEditRequest {
                input: replacement.spec.input.clone(),
                expected_updated_at_ms: second.updated_at_ms,
            },
            &replacement,
            NOW + 11,
        )
        .await
        .unwrap();
    assert!(
        matches!(edited.provenance.as_ref().map(|input| &input.author), Some(ternilo_protocol::InputAuthor::Account { user_id, .. }) if user_id == &fixture.alice.user_id)
    );
    let reauthorized = fixture
        .cloud
        .get_run(&session.tenant_id, &edited.run_id)
        .await
        .unwrap();
    assert_eq!(reauthorized.actor_user_id, fixture.bob.user_id);
    fixture
        .cloud
        .pause_session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
            None,
            NOW + 11,
        )
        .await
        .unwrap();
    fixture
        .cloud
        .cancel_run(
            &session.tenant_id,
            &RunId::new("authorization-blocker"),
            NOW + 11,
        )
        .await
        .unwrap();
    let paused = fixture
        .cloud
        .session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
        )
        .await
        .unwrap();
    assert!(paused.paused);
    assert_eq!(paused.active_run_id, None);
    assert_eq!(paused.items.len(), 2);
    assert!(
        paused
            .items
            .iter()
            .all(|item| item.placement == SubmissionPlacement::Queued)
    );
    fixture
        .cloud
        .resume_session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
            NOW + 12,
        )
        .await
        .unwrap();
    let resumed = fixture
        .cloud
        .session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
        )
        .await
        .unwrap();
    assert_eq!(
        resumed
            .items
            .iter()
            .find(|item| item.run_id == RunId::new("source-alice-execution-alice"))
            .unwrap()
            .placement,
        SubmissionPlacement::Running
    );
    assert_eq!(
        resumed
            .items
            .iter()
            .find(|item| item.run_id == edited.run_id)
            .unwrap()
            .placement,
        SubmissionPlacement::Queued,
        "same source authors with different execution actors cannot share a batch"
    );
    let revision = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap()
        .status_revision;
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            revision,
            NOW + 13,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state(&session.tenant_id, "source-alice-execution-alice")
            .await,
        CloudRunState::Cancelled
    );
    assert_eq!(
        fixture
            .state(&session.tenant_id, "source-alice-execution-bob")
            .await,
        CloudRunState::Queued
    );
    assert_eq!(
        fixture
            .reservation_state("source-alice-execution-bob")
            .await,
        "active"
    );
    fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 14,
        )
        .await
        .unwrap();
    let revision = fixture
        .control
        .get_account(&fixture.admin, &fixture.bob.user_id)
        .await
        .unwrap()
        .status_revision;
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.bob.user_id,
            AccountStatusAction::Ban,
            revision,
            NOW + 15,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state(&session.tenant_id, "source-alice-execution-bob")
            .await,
        CloudRunState::Cancelled
    );
    assert_eq!(
        fixture
            .reservation_state("source-alice-execution-bob")
            .await,
        "released"
    );
    fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.bob.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 16,
        )
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Hold real session, budget and reservation locks to deterministically exercise atomic cleanup's lock-cycle boundary."
)]
async fn multi_session_lock_contract(fixture: &Fixture) {
    let first = &fixture.sessions[0];
    let sibling = fixture
        .cloud
        .create_session(
            CloudSessionDraft {
                project_id: first.project_id.clone(),
                workspace_id: first.workspace_id.clone(),
                session_id: Some(SessionId::new("cleanup-session-0-z")),
                agent_id: first.agent_id.clone(),
                title: "Cleanup sibling".to_owned(),
                permissions: first.permissions,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: first.mode,
            },
            &first.tenant_id,
            &fixture.bob.user_id,
            NOW + 8,
        )
        .await
        .unwrap();
    fixture
        .control
        .set_resource_share(
            &fixture.bob,
            &first.tenant_id,
            ResourceKind::Session,
            sibling.session_id.as_str(),
            &fixture.alice.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                stop: true,
                configure: false,
            }),
            NOW + 8,
        )
        .await
        .unwrap();
    fixture
        .enqueue(0, &fixture.alice, "multi-session-a", NOW + 8)
        .await;
    let mut compiled = fixture.compiled(0, &fixture.alice, "multi-session-b");
    compiled.spec.metadata.session_id = sibling.session_id.clone();
    compiled.authorization_session_id = sibling.session_id.clone();
    fixture
        .cloud
        .enqueue_session_submission_as(
            &fixture.alice.user_id,
            &compiled,
            &request(&compiled),
            NOW + 8,
        )
        .await
        .unwrap();
    let account = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    let mut blocker = fixture
        .cloud
        .database()
        .owner_transaction(&first.tenant_id, &fixture.bob.user_id)
        .await
        .unwrap();
    sqlx::query(
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    )
    .bind(first.tenant_id.as_str())
    .bind(sibling.session_id.as_str())
    .fetch_one(&mut *blocker)
    .await
    .unwrap();
    ControlStore::reserve_quota_for_owner_in(
        &mut blocker,
        &fixture.bob.user_id,
        &fixture.bob.user_id,
        &first.tenant_id,
        Some("multi-session-blocker-unused"),
        100,
        Duration::from_secs(60),
        NOW + 8,
    )
    .await
    .unwrap();
    sqlx::query("SELECT reservation_id FROM control_quota_reservations WHERE tenant_id=$1 AND run_id='multi-session-a' FOR UPDATE")
        .bind(first.tenant_id.as_str()).fetch_one(&mut *blocker).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.cloud.set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            account.status_revision,
            NOW + 9,
        ),
    )
    .await
    .expect("cleanup must not retain reservation locks while waiting for another session");
    assert_eq!(
        result.unwrap_err().code,
        ErrorCode::Conflict,
        "busy sessions exhaust a bounded retry without partial account closure"
    );
    let unchanged = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    assert_eq!(
        (unchanged.status, unchanged.status_revision),
        (AccountStatus::Active, account.status_revision)
    );
    assert!(
        fixture
            .control
            .authenticate_native_session(&fixture.alice_token, NOW + 9)
            .await
            .is_ok()
    );
    assert_eq!(
        fixture.state(&first.tenant_id, "multi-session-a").await,
        CloudRunState::Queued
    );
    assert_eq!(
        fixture.state(&first.tenant_id, "multi-session-b").await,
        CloudRunState::Queued
    );
    sqlx::query("SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE NOWAIT")
        .bind(first.tenant_id.as_str()).bind(first.session_id.as_str()).fetch_one(&mut *blocker).await.unwrap();
    blocker.rollback().await.unwrap();
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            account.status_revision,
            NOW + 10,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.state(&first.tenant_id, "multi-session-a").await,
        CloudRunState::Cancelled
    );
    assert_eq!(
        fixture.state(&first.tenant_id, "multi-session-b").await,
        CloudRunState::Cancelled
    );
    fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 11,
        )
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Use the real cross-account expiration sweep and same-session admission to exercise reservation/session ordering."
)]
async fn expiration_lock_contract(fixture: &Fixture) {
    let session = &fixture.sessions[2];
    fixture
        .enqueue(2, &fixture.alice, "expiration-alice", NOW + 40)
        .await;
    sqlx::query("UPDATE control_quota_reservations SET expires_at_ms=$2 WHERE run_id=$1")
        .bind("expiration-alice")
        .bind(i64::try_from(NOW + 45).unwrap())
        .execute(&fixture.audit)
        .await
        .unwrap();
    let token = fixture
        .control
        .create_browser_session(fixture.alice.clone(), NOW + 49)
        .await
        .unwrap()
        .access_token;
    let account = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    let compiled = fixture.compiled(2, &fixture.bob, "expiration-bob");
    let mut admission = fixture.cloud.database().begin().await.unwrap();
    let reservation = ControlStore::reserve_quota_for_owner_in(
        &mut admission,
        &fixture.bob.user_id,
        &fixture.bob.user_id,
        &session.tenant_id,
        Some(compiled.spec.metadata.run_id.as_str()),
        100,
        Duration::from_secs(60),
        NOW + 50,
    )
    .await
    .unwrap();
    let expired: String =
        sqlx::query_scalar("SELECT state FROM control_quota_reservations WHERE run_id=$1")
            .bind("expiration-alice")
            .fetch_one(&mut *admission)
            .await
            .unwrap();
    assert_eq!(
        expired, "expired",
        "Bob's real admission sweep owns Alice's expired reservation row"
    );
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.cloud.set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            account.status_revision,
            NOW + 51,
        ),
    )
    .await
    .expect("cleanup must never wait for Bob's reservation row while holding their shared session");
    assert_eq!(result.unwrap_err().code, ErrorCode::Conflict);
    let unchanged = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    assert_eq!(
        (unchanged.status, unchanged.status_revision),
        (AccountStatus::Active, account.status_revision)
    );
    assert!(
        fixture
            .control
            .authenticate_native_session(&token, NOW + 51)
            .await
            .is_ok()
    );
    assert_eq!(
        fixture.state(&session.tenant_id, "expiration-alice").await,
        CloudRunState::Queued
    );
    let receipt = tokio::time::timeout(
        Duration::from_secs(2),
        CloudStore::enqueue_session_submission_in(
            &mut admission,
            &compiled,
            &reservation.reservation_id,
            &request(&compiled),
            NOW + 52,
        ),
    )
    .await
    .expect("all failed cleanup attempts must release the shared session for Bob's admission")
    .unwrap();
    assert_eq!(receipt.run.actor_user_id, fixture.bob.user_id);
    admission.commit().await.unwrap();
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            account.status_revision,
            NOW + 53,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.state(&session.tenant_id, "expiration-alice").await,
        CloudRunState::Cancelled
    );
    assert_eq!(
        fixture.reservation_state("expiration-alice").await,
        "expired"
    );
    assert_eq!(
        fixture.state(&session.tenant_id, "expiration-bob").await,
        CloudRunState::Queued
    );
    assert_eq!(fixture.reservation_state("expiration-bob").await, "active");
    fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 54,
        )
        .await
        .unwrap();
    fixture
        .cloud
        .cancel_run(&session.tenant_id, &compiled.spec.metadata.run_id, NOW + 55)
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "Serialize the real paused-queue resume against atomic account cleanup and verify the surviving author becomes the head."
)]
async fn resume_cleanup_lock_contract(fixture: &Fixture, admin_url: &str) {
    let session = &fixture.sessions[2];
    fixture
        .enqueue(2, &fixture.alice, "resume-alice", NOW + 61)
        .await;
    fixture
        .enqueue(2, &fixture.bob, "resume-bob", NOW + 62)
        .await;
    fixture
        .cloud
        .pause_session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
            None,
            NOW + 62,
        )
        .await
        .unwrap();
    let account = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap();
    let mut cleanup = fixture
        .cloud
        .database()
        .owner_transaction(&session.tenant_id, &fixture.bob.user_id)
        .await
        .unwrap();
    sqlx::query(
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    )
    .bind(session.tenant_id.as_str())
    .bind(session.session_id.as_str())
    .fetch_one(&mut *cleanup)
    .await
    .unwrap();
    sqlx::query("SELECT submission_id FROM cloud_session_submissions WHERE tenant_id=$1 AND session_id=$2 AND run_id=$3 FOR UPDATE")
        .bind(session.tenant_id.as_str()).bind(session.session_id.as_str()).bind("resume-alice").fetch_one(&mut *cleanup).await.unwrap();
    let pid = transaction_pid(&mut cleanup).await;
    let admin_pool = sqlx::PgPool::connect(admin_url).await.unwrap();
    let pending = {
        let cloud = fixture.cloud.clone();
        let tenant = session.tenant_id.clone();
        let bob = fixture.bob.user_id.clone();
        let session_id = session.session_id.clone();
        tokio::spawn(async move {
            cloud
                .resume_session_inbox(&tenant, &bob, &session_id, NOW + 63)
                .await
        })
    };
    wait_until_blocked(&admin_pool, pid, &pending).await;
    let paused: i64 = sqlx::query_scalar("SELECT paused FROM cloud_session_inboxes WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE NOWAIT")
        .bind(session.tenant_id.as_str()).bind(session.session_id.as_str()).fetch_one(&mut *cleanup).await.unwrap();
    assert_eq!(
        paused, 1,
        "resume must wait on the session before holding or changing the inbox"
    );
    let banned = ControlStore::set_account_status_in(
        &mut cleanup,
        &fixture.admin,
        &fixture.alice.user_id,
        AccountStatusAction::Ban,
        account.status_revision,
        NOW + 64,
    )
    .await
    .unwrap();
    assert_eq!(
        CloudStore::cancel_run_in(
            &mut cleanup,
            &session.tenant_id,
            &RunId::new("resume-alice"),
            Some(&fixture.admin.user_id),
            NOW + 64
        )
        .await
        .unwrap(),
        CloudRunState::Cancelled
    );
    cleanup.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resumed = fixture
        .cloud
        .session_inbox(
            &session.tenant_id,
            &fixture.bob.user_id,
            &session.session_id,
        )
        .await
        .unwrap();
    assert!(!resumed.paused);
    assert_eq!(resumed.active_run_id, Some(RunId::new("resume-bob")));
    assert_eq!(resumed.items.len(), 1);
    assert_eq!(resumed.items[0].run_id, RunId::new("resume-bob"));
    assert_eq!(
        resumed.items[0].placement,
        SubmissionPlacement::Running,
        "Bob's surviving input must become the head after cleanup commits"
    );
    assert_eq!(
        fixture.state(&session.tenant_id, "resume-alice").await,
        CloudRunState::Cancelled
    );
    assert_eq!(fixture.reservation_state("resume-bob").await, "active");
    fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 65,
        )
        .await
        .unwrap();
    fixture
        .cloud
        .cancel_run(&session.tenant_id, &RunId::new("resume-bob"), NOW + 66)
        .await
        .unwrap();
    admin_pool.close().await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Observe both real admission/ban lock interleavings and a worker start against an uncommitted account closure."
)]
async fn admission_lock_contract(fixture: &Fixture, admin_url: &str) {
    let admin_pool = sqlx::PgPool::connect(admin_url).await.unwrap();
    let initial_revision = fixture
        .control
        .get_account(&fixture.admin, &fixture.alice.user_id)
        .await
        .unwrap()
        .status_revision;
    let compiled = fixture.compiled(2, &fixture.alice, "race-accepted-before-ban");
    let mut tx = fixture.cloud.database().begin().await.unwrap();
    let reservation = ControlStore::reserve_quota_for_owner_in(
        &mut tx,
        &fixture.alice.user_id,
        &fixture.bob.user_id,
        &compiled.spec.metadata.tenant_id,
        Some(compiled.spec.metadata.run_id.as_str()),
        100,
        Duration::from_secs(60),
        NOW + 8,
    )
    .await
    .unwrap();
    CloudStore::enqueue_session_submission_in(
        &mut tx,
        &compiled,
        &reservation.reservation_id,
        &request(&compiled),
        NOW + 9,
    )
    .await
    .unwrap();
    let pid = transaction_pid(&mut tx).await;
    let pending = {
        let cloud = fixture.cloud.clone();
        let admin = fixture.admin.clone();
        let alice = fixture.alice.user_id.clone();
        tokio::spawn(async move {
            cloud
                .set_account_status(
                    &admin,
                    &alice,
                    AccountStatusAction::Ban,
                    initial_revision,
                    NOW + 10,
                )
                .await
        })
    };
    wait_until_blocked(&admin_pool, pid, &pending).await;
    tx.commit().await.unwrap();
    let banned = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture
            .state(&fixture.sessions[2].tenant_id, "race-accepted-before-ban")
            .await,
        CloudRunState::Cancelled
    );
    let active = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 11,
        )
        .await
        .unwrap();

    fixture
        .enqueue(2, &fixture.alice, "race-session-held", NOW + 12)
        .await;
    let mut tx = fixture
        .cloud
        .database()
        .owner_transaction(&fixture.sessions[2].tenant_id, &fixture.bob.user_id)
        .await
        .unwrap();
    sqlx::query(
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    )
    .bind(fixture.sessions[2].tenant_id.as_str())
    .bind(fixture.sessions[2].session_id.as_str())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let mut status_tx = fixture.cloud.database().begin().await.unwrap();
    ControlStore::set_account_status_in(
        &mut status_tx,
        &fixture.admin,
        &fixture.alice.user_id,
        AccountStatusAction::Ban,
        active.status_revision,
        NOW + 13,
    )
    .await
    .unwrap();
    let late = fixture.compiled(2, &fixture.alice, "race-late-admission");
    let reservation = ControlStore::reserve_quota_for_owner_in(
        &mut tx,
        &fixture.alice.user_id,
        &fixture.bob.user_id,
        &late.spec.metadata.tenant_id,
        Some(late.spec.metadata.run_id.as_str()),
        100,
        Duration::from_secs(60),
        NOW + 14,
    )
    .await
    .unwrap();
    let rejected = tokio::time::timeout(
        Duration::from_secs(2),
        CloudStore::enqueue_session_submission_in(
            &mut tx,
            &late,
            &reservation.reservation_id,
            &request(&late),
            NOW + 14,
        ),
    )
    .await
    .expect("admission must not wait for the account lock while retaining session/budget locks")
    .unwrap_err();
    assert_eq!(rejected.code, ErrorCode::Conflict);
    tx.rollback().await.unwrap();
    status_tx.rollback().await.unwrap();
    let banned = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Ban,
            active.status_revision,
            NOW + 14,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .state(&fixture.sessions[2].tenant_id, "race-session-held")
            .await,
        CloudRunState::Cancelled
    );
    assert!(
        fixture
            .cloud
            .get_run(&fixture.sessions[2].tenant_id, &late.spec.metadata.run_id)
            .await
            .is_err()
    );
    let reservations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_quota_reservations WHERE run_id=$1")
            .bind(late.spec.metadata.run_id.as_str())
            .fetch_one(&fixture.audit)
            .await
            .unwrap();
    assert_eq!(reservations, 0);
    let active = fixture
        .cloud
        .set_account_status(
            &fixture.admin,
            &fixture.alice.user_id,
            AccountStatusAction::Unban,
            banned.status_revision,
            NOW + 15,
        )
        .await
        .unwrap();

    fixture
        .enqueue(1, &fixture.alice, "race-worker-start", NOW + 16)
        .await;
    let claim = fixture
        .cloud
        .claim_run(WORKER, Duration::from_secs(60), NOW + 17)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, RunId::new("race-worker-start"));
    let mut status_tx = fixture.cloud.database().begin().await.unwrap();
    ControlStore::set_account_status_in(
        &mut status_tx,
        &fixture.admin,
        &fixture.alice.user_id,
        AccountStatusAction::Ban,
        active.status_revision,
        NOW + 18,
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_secs(2),
            fixture
                .cloud
                .start_run(claim.clone(), WORKER, Duration::from_secs(60), NOW + 18)
        )
        .await
        .unwrap()
        .unwrap()
        .is_none()
    );
    status_tx.rollback().await.unwrap();
    fixture
        .cloud
        .cancel_run(&claim.tenant_id, &claim.run_id, NOW + 19)
        .await
        .unwrap();
    admin_pool.close().await;
}

async fn transaction_pid(tx: &mut ternilo_storage::Transaction) -> i64 {
    sqlx::query_scalar("SELECT CAST(pg_backend_pid() AS BIGINT)")
        .fetch_one(&mut **tx)
        .await
        .unwrap()
}

async fn wait_until_blocked<T>(
    admin: &sqlx::PgPool,
    blocker_pid: i64,
    task: &tokio::task::JoinHandle<T>,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(!task.is_finished(), "the operation must wait at the account/session boundary");
            let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pg_stat_activity WHERE datname=current_database() AND CAST($1 AS INTEGER)=ANY(pg_blocking_pids(pid))")
                .bind(blocker_pid).fetch_one(admin).await.unwrap();
            if waiting != 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("the real ban must enter an observable PostgreSQL lock wait");
}

async fn user(control: &ControlStore, name: &str) -> ControlUser {
    control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://cleanup.example.test".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            NOW + 1,
        )
        .await
        .unwrap()
}

fn request(compiled: &CompiledRun) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
    }
}
