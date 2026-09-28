use std::time::Duration;

use ternilo_cloud::{
    CloudRunLineage, CloudSessionDraft, CloudSessionRecord, CloudStore, CompiledRun, StartedRun,
    TerminalState,
};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, ErrorCode, PermissionPreset, Profile, RunId, RunLimits, RunMetadata, RunSpec,
    SessionEvent, SessionEventKind, SessionId, SessionMode, SessionSubmissionRequest, SubagentId,
    SubagentSessionMetadata, SubagentTranscriptKind, SubmissionContent, SubmissionDelivery,
    TenantId,
};

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_run_lineage_keeps_accepted_dependencies_scoped_and_durable() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(admin_url.contains("ternilo_cloud_test"));
    let runtime_url =
        server_runtime::initialize(&admin_url, "ternilo_lineage_runtime_test", [49; 32]).await;
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([49; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&admin_url), 4)
        .await
        .unwrap();
    Box::pin(lineage_contract(control, cloud)).await;
    server_runtime::assert_scoped_without_schema_access(&runtime_url).await;
    let pool = sqlx::PgPool::connect(&runtime_url).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_run_lineage")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "unscoped runtime connections cannot inspect scheduling ancestry"
    );
    assert!(
        sqlx::query("UPDATE cloud_run_lineage SET depth=0")
            .execute(&pool)
            .await
            .is_err(),
        "runtime grants do not allow rewriting immutable ancestry"
    );
    pool.close().await;
}

#[tokio::test]
async fn sqlite_run_lineage_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("lineage.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([49; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(lineage_contract(control, cloud)).await;
}

struct Fixture {
    cloud: CloudStore,
    owner: ControlUser,
    actor: ControlUser,
    session: CloudSessionRecord,
    now: u64,
}

impl Fixture {
    fn tick(&mut self) -> u64 {
        self.now += 1;
        self.now
    }

    fn compiled(
        &self,
        session: &CloudSessionRecord,
        name: &str,
        actor: &ControlUser,
    ) -> CompiledRun {
        CompiledRun {
            automated_input: None,
            actor_user_id: actor.user_id.clone(),
            authorization_session_id: session.session_id.clone(),
            spec: RunSpec {
                schema_version: ternilo_protocol::RUN_SPEC_VERSION,
                catalog_revision: "lineage-fixture".to_owned(),
                policy_revision: "lineage-fixture".to_owned(),
                metadata: RunMetadata {
                    tenant_id: session.tenant_id.clone(),
                    user_id: self.owner.user_id.clone(),
                    project_id: Some(session.project_id.clone()),
                    workspace_id: session.workspace_id.clone(),
                    agent_id: session.agent_id.clone(),
                    session_id: session.session_id.clone(),
                    run_id: RunId::new(name),
                },
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                },
                permissions: session.permissions,
                mode: session.mode,
                profile: Profile::default(),
                input: name.to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
            },
            reserved_model_tokens: 100,
            priority: 0,
            max_attempts: 2,
        }
    }

    async fn submit(
        &mut self,
        session: &CloudSessionRecord,
        name: &str,
        actor: &ControlUser,
    ) -> RunId {
        let compiled = self.compiled(session, name, actor);
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(compiled.spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: name.to_owned(),
            },
            references: Vec::new(),
            attachments: Vec::new(),
        };
        let now = self.tick();
        self.cloud
            .enqueue_session_submission_as(&actor.user_id, &compiled, &request, now)
            .await
            .unwrap()
            .run
            .run_id
    }

    async fn start(&mut self, expected: &RunId) -> StartedRun {
        let now = self.tick();
        let claim = self
            .cloud
            .claim_run("lineage-worker", Duration::from_secs(60), now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&claim.run_id, expected);
        let now = self.tick();
        let started = self
            .cloud
            .start_run(claim, "lineage-worker", Duration::from_secs(60), now)
            .await
            .unwrap()
            .unwrap();
        let now = self.tick();
        self.cloud
            .append_event(
                &started,
                "lineage-worker",
                &SessionEvent {
                    seq: started.prior_events.last().map_or(0, |event| event.seq + 1),
                    occurred_at_ms: now,
                    run_id: started.claim.run_id.clone(),
                    kind: SessionEventKind::TurnStarted,
                },
                now,
            )
            .await
            .unwrap();
        started
    }

    async fn child(&mut self, parent: &StartedRun, name: &str) -> (SessionId, RunId) {
        let session = SessionId::new(format!("{name}-session"));
        let metadata = SubagentSessionMetadata {
            subagent_id: SubagentId::new(name),
            provider: "in-process".to_owned(),
            transcript_kind: SubagentTranscriptKind::Conversation,
        };
        let now = self.tick();
        self.cloud
            .create_subagent_for_worker("lineage-worker", parent, &session, &metadata, name, now)
            .await
            .unwrap();
        let mut spec = parent.claim.spec.clone();
        spec.metadata.session_id = session.clone();
        spec.metadata.run_id = RunId::new(name);
        spec.input = name.to_owned();
        let now = self.tick();
        let run = self
            .cloud
            .enqueue_subagent_for_worker("lineage-worker", parent, &session, &spec, name, 2, now)
            .await
            .unwrap();
        (session, run)
    }

    async fn lineage(&self, run: &RunId) -> CloudRunLineage {
        self.cloud
            .run_lineage(&self.session.tenant_id, &self.owner.user_id, run)
            .await
            .unwrap()
            .unwrap()
    }

    async fn finish(&mut self, run: &StartedRun) {
        let now = self.tick();
        self.cloud
            .append_event(
                run,
                "lineage-worker",
                &SessionEvent {
                    seq: run.prior_events.last().map_or(1, |event| event.seq + 2),
                    occurred_at_ms: now,
                    run_id: run.claim.run_id.clone(),
                    kind: SessionEventKind::TurnCancelled,
                },
                now,
            )
            .await
            .unwrap();
        let now = self.tick();
        self.cloud
            .finish_run(
                run,
                "lineage-worker",
                TerminalState::Cancelled,
                None,
                None,
                now,
            )
            .await
            .unwrap();
        self.confirm_exit(run).await;
    }

    async fn finish_successfully(&mut self, run: &StartedRun, answer: &str) {
        let now = self.tick();
        let event = SessionEvent {
            seq: run.prior_events.last().map_or(1, |event| event.seq + 2),
            occurred_at_ms: now,
            run_id: run.claim.run_id.clone(),
            kind: SessionEventKind::TurnFinished {
                answer: answer.to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        };
        self.cloud
            .append_event(run, "lineage-worker", &event, now)
            .await
            .unwrap();
        let outcome = ternilo_protocol::RunOutcome {
            answer: answer.to_owned(),
            steps: 1,
            tool_calls: 0,
            events: vec![event],
            generated_title: None,
        };
        let now = self.tick();
        self.cloud
            .finish_run(
                run,
                "lineage-worker",
                TerminalState::Succeeded,
                Some(&outcome),
                None,
                now,
            )
            .await
            .unwrap();
        self.confirm_exit(run).await;
    }

    async fn confirm_exit(&mut self, run: &StartedRun) {
        let now = self.tick();
        let worker = ternilo_cloud::CloudWorkerIdentity {
            worker_id: ternilo_transport::ExecutorId::new("lineage-worker"),
            instance_nonce: "storage-contract-lineage-worker".to_owned(),
            generation: 1,
        };
        self.cloud
            .release_resident(&run.into(), &worker, worker.generation, now)
            .await
            .unwrap();
    }

    async fn reservation(&self, run: &RunId) -> (String, String) {
        let mut tx = self
            .cloud
            .database()
            .tenant_transaction(&self.session.tenant_id)
            .await
            .unwrap();
        let result = sqlx::query_as::<_, (String, String)>(
            "SELECT reservation.reservation_id,reservation.state
             FROM cloud_runs AS run JOIN control_quota_reservations AS reservation
               ON reservation.tenant_id=run.tenant_id AND reservation.reservation_id=run.quota_reservation_id
             WHERE run.tenant_id=$1 AND run.run_id=$2",
        ).bind(self.session.tenant_id.as_str()).bind(run.as_str()).fetch_one(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        result
    }
}

async fn user(control: &ControlStore, name: &str, now: u64) -> ControlUser {
    control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://lineage.example.test".to_owned(),
                subject: name.to_owned(),
                email: None,
                display_name: None,
            },
            name,
            now,
        )
        .await
        .unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "Use the same accepted ancestry and stale dependency contract on SQLite and restricted PostgreSQL."
)]
async fn lineage_contract(control: ControlStore, cloud: CloudStore) {
    worker_storage::bind_workers(&cloud, &["lineage-worker"], "lineage-storage").await;
    let now = 2_200_000_000_000;
    let bootstrap = control
        .initialize_owner(
            &NativeRegistration {
                username: "lineage-owner".to_owned(),
                email: "lineage-owner@example.test".to_owned(),
                password: "lineage-owner-password".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    let owner = bootstrap.session.user;
    control
        .set_instance_mode(
            &owner,
            InstanceMode::MultiUser,
            bootstrap.session.instance.revision,
            now,
        )
        .await
        .unwrap();
    let actor = user(&control, "lineage-actor", now).await;
    let tenant = control
        .create_tenant(
            &owner,
            "lineage",
            "Lineage",
            TenantQuota {
                max_nodes: 1,
                max_concurrent_runs: 16,
                monthly_model_tokens: 100_000,
                max_secrets: 2,
            },
            now + 1,
        )
        .await
        .unwrap();
    control
        .set_membership(
            &owner,
            &tenant.tenant_id,
            &actor.user_id,
            TenantRole::Member,
            now + 2,
        )
        .await
        .unwrap();
    let project = control
        .create_project(&owner, &tenant.tenant_id, "Lineage", now + 3)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &owner,
            &tenant.tenant_id,
            &project.project_id,
            "Lineage",
            now + 4,
        )
        .await
        .unwrap();
    let session = cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id,
                workspace_id: workspace.workspace_id,
                session_id: Some(SessionId::new("lineage-parent")),
                agent_id: AgentId::new("agent"),
                title: "Lineage".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            &tenant.tenant_id,
            &owner.user_id,
            now + 5,
        )
        .await
        .unwrap();
    control
        .set_resource_share(
            &owner,
            &tenant.tenant_id,
            ResourceKind::Session,
            session.session_id.as_str(),
            &actor.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                stop: true,
                configure: false,
            }),
            now + 6,
        )
        .await
        .unwrap();
    let mut fixture = Fixture {
        cloud,
        owner,
        actor,
        session,
        now: now + 6,
    };
    let session = fixture.session.clone();
    let actor = fixture.actor.clone();
    let owner = fixture.owner.clone();
    let parent_id = fixture.submit(&session, "accepted-parent", &actor).await;
    let root = fixture.lineage(&parent_id).await;
    assert_eq!(root.root_run_id, parent_id);
    assert_eq!(root.parent_run_id, None);
    assert_eq!(root.depth, 0);
    assert_eq!(root.owner_user_id, owner.user_id);
    assert_eq!(root.actor_user_id, actor.user_id);
    let parent = fixture.start(&parent_id).await;
    let (child_session, child_id) = fixture.child(&parent, "accepted-child").await;
    let child_lineage = fixture.lineage(&child_id).await;
    assert_eq!(child_lineage.root_run_id, parent_id);
    assert_eq!(child_lineage.parent_run_id.as_ref(), Some(&parent_id));
    assert_eq!(
        child_lineage.parent_lease_token,
        Some(parent.claim.lease_token)
    );
    assert_eq!(
        child_lineage.parent_writer_fencing_token,
        Some(parent.fencing_token)
    );
    assert_eq!(child_lineage.actor_user_id, actor.user_id);
    assert_eq!(child_lineage.depth, 1);
    let accepted = fixture
        .cloud
        .accepted_subagent_dependency_for_worker(
            "lineage-worker",
            &parent,
            &child_session,
            &child_id,
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(accepted.session_id, child_session);
    assert_eq!(accepted.run_id, child_id);
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Queued
    );
    for (candidate_session, candidate_run) in [
        (&session.session_id, &child_id),
        (&child_session, &parent_id),
    ] {
        let error = fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &parent,
                candidate_session,
                candidate_run,
                fixture.now,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PolicyDenied);
    }
    let mut stale = parent.clone();
    stale.claim.lease_token += 1;
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &stale,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    stale = parent.clone();
    stale.fencing_token += 1;
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &stale,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now + 60_001
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        fixture
            .cloud
            .run_lineage(&tenant.tenant_id, &actor.user_id, &child_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .cloud
            .run_lineage(
                &TenantId::new("different-tenant"),
                &owner.user_id,
                &child_id
            )
            .await
            .unwrap()
            .is_none()
    );

    let child = fixture.start(&child_id).await;
    let (_, grandchild_id) = fixture.child(&child, "accepted-grandchild").await;
    let grandchild_lineage = fixture.lineage(&grandchild_id).await;
    assert_eq!(grandchild_lineage.root_run_id, parent_id);
    assert_eq!(grandchild_lineage.parent_run_id.as_ref(), Some(&child_id));
    assert_eq!(grandchild_lineage.depth, 2);
    let grandchild = fixture.start(&grandchild_id).await;
    fixture.finish(&grandchild).await;
    assert_eq!(
        fixture
            .cloud
            .cancel_subagent_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap(),
        ternilo_cloud::CloudRunState::CancelRequested
    );
    fixture.finish(&child).await;

    let child_record = fixture
        .cloud
        .cloud_subagent(
            &tenant.tenant_id,
            &owner.user_id,
            &session.session_id,
            &SubagentId::new("accepted-child"),
        )
        .await
        .unwrap()
        .child;
    let manual_id = fixture
        .submit(&child_record, "manual-child-followup", &owner)
        .await;
    let manual = fixture.lineage(&manual_id).await;
    assert_eq!(manual.root_run_id, manual_id);
    assert_eq!(manual.parent_run_id, None);
    assert_eq!(manual.depth, 0);
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &manual_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .cloud
            .cancel_subagent_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &manual_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &manual_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    let manual = fixture.start(&manual_id).await;
    fixture.finish(&manual).await;

    let (cleanup_session, cleanup_run) = fixture.child(&parent, "accepted-before-stop").await;
    let deferred_session = SessionId::new("created-before-stop");
    let deferred_metadata = SubagentSessionMetadata {
        subagent_id: SubagentId::new("created-before-stop"),
        provider: "in-process".to_owned(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    let now = fixture.tick();
    fixture
        .cloud
        .create_subagent_for_worker(
            "lineage-worker",
            &parent,
            &deferred_session,
            &deferred_metadata,
            "Created before stop",
            now,
        )
        .await
        .unwrap();
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .cancel_run_as(
                &tenant.tenant_id,
                &owner.user_id,
                &session.session_id,
                &parent_id,
                now
            )
            .await
            .unwrap(),
        ternilo_cloud::CloudRunState::CancelRequested
    );
    let error = fixture
        .cloud
        .create_subagent_for_worker(
            "lineage-worker",
            &parent,
            &SessionId::new("created-after-stop"),
            &SubagentSessionMetadata {
                subagent_id: SubagentId::new("created-after-stop"),
                ..deferred_metadata
            },
            "Must not be created",
            fixture.now,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    let mut rejected_spec = parent.claim.spec.clone();
    rejected_spec.metadata.session_id = deferred_session.clone();
    rejected_spec.metadata.run_id = RunId::new("enqueued-after-stop");
    let error = fixture
        .cloud
        .enqueue_subagent_for_worker(
            "lineage-worker",
            &parent,
            &deferred_session,
            &rejected_spec,
            &rejected_spec.input,
            2,
            fixture.now,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    assert!(
        fixture
            .cloud
            .run_lineage(
                &tenant.tenant_id,
                &owner.user_id,
                &rejected_spec.metadata.run_id
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &cleanup_session,
                &cleanup_run,
                fixture.now
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Queued
    );
    assert_eq!(
        fixture
            .cloud
            .cancel_subagent_for_worker(
                "lineage-worker",
                &parent,
                &cleanup_session,
                &cleanup_run,
                fixture.now
            )
            .await
            .unwrap(),
        ternilo_cloud::CloudRunState::Cancelled
    );
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &cleanup_session,
                &cleanup_run,
                fixture.now
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Cancelled
    );
    fixture.finish(&parent).await;
    let now = fixture.tick();
    let fork = fixture
        .cloud
        .fork_session(
            &tenant.tenant_id,
            &owner.user_id,
            &session.session_id,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(fork.parent_session_id.as_ref(), Some(&session.session_id));
    assert!(fork.subagent.is_none());
    let fork_id = fixture.submit(&fork, "ordinary-fork", &owner).await;
    assert_eq!(fixture.lineage(&fork_id).await.root_run_id, fork_id);
    assert_eq!(fixture.lineage(&fork_id).await.parent_run_id, None);
    let fork_run = fixture.start(&fork_id).await;
    fixture.finish(&fork_run).await;

    let replacement_id = fixture.submit(&session, "next-parent-turn", &actor).await;
    let replacement = fixture.start(&replacement_id).await;
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &replacement,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .cloud
            .cancel_subagent_for_worker(
                "lineage-worker",
                &replacement,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &replacement,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        fixture
            .cloud
            .accepted_subagent_dependency_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    fixture.finish(&replacement).await;

    // History removal must not erase the immutable ancestor used by surviving descendants.
    let mut tx = fixture
        .cloud
        .database()
        .tenant_transaction(&tenant.tenant_id)
        .await
        .unwrap();
    ternilo_storage::set_user_scope(&mut tx, &owner.user_id)
        .await
        .unwrap();
    // Session deletion removes its command journal before deleting run history.
    sqlx::query("DELETE FROM cloud_session_commands WHERE tenant_id=$1 AND target_run_id=$2")
        .bind(tenant.tenant_id.as_str())
        .bind(parent_id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM cloud_runs WHERE tenant_id=$1 AND run_id=$2")
        .bind(tenant.tenant_id.as_str())
        .bind(parent_id.as_str())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(fixture.lineage(&parent_id).await, root);
    assert_eq!(fixture.lineage(&child_id).await, child_lineage);
    assert_eq!(fixture.lineage(&grandchild_id).await, grandchild_lineage);
    let before = fixture.lineage(&parent_id).await;
    let compiled = fixture.compiled(&session, parent_id.as_str(), &actor);
    let request = SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(parent_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
    };
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .enqueue_session_submission_as(&actor.user_id, &compiled, &request, now)
            .await
            .is_err(),
        "removing history cannot allow an accepted run ID to acquire different ancestry"
    );
    assert_eq!(fixture.lineage(&parent_id).await, before);
    background_child_continues_after_successful_parent(&mut fixture).await;
}

async fn background_child_continues_after_successful_parent(fixture: &mut Fixture) {
    let session = fixture.session.clone();
    let actor = fixture.actor.clone();
    let parent_id = fixture.submit(&session, "background-parent", &actor).await;
    let parent = fixture.start(&parent_id).await;
    let (child_session, child_id) = fixture.child(&parent, "independent-background-child").await;
    let parent_reservation = fixture.reservation(&parent_id).await;
    let child_reservation = fixture.reservation(&child_id).await;
    assert_ne!(parent_reservation.0, child_reservation.0);
    assert_eq!(parent_reservation.1, "active");
    assert_eq!(child_reservation.1, "active");
    fixture
        .finish_successfully(&parent, "The background child can continue independently")
        .await;
    assert_eq!(
        fixture
            .cloud
            .get_run(&session.tenant_id, &parent_id)
            .await
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Succeeded
    );
    assert_eq!(
        fixture
            .cloud
            .get_run(&session.tenant_id, &child_id)
            .await
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Queued
    );
    assert_eq!(
        fixture.reservation(&parent_id).await,
        (parent_reservation.0.clone(), "released".to_owned())
    );
    assert_eq!(
        fixture.reservation(&child_id).await,
        child_reservation,
        "finishing the parent must not settle the child's separate budget"
    );
    assert!(
        fixture
            .cloud
            .subagent_run_for_worker(
                "lineage-worker",
                &parent,
                &child_session,
                &child_id,
                fixture.now
            )
            .await
            .unwrap()
            .is_none(),
        "the retired parent lease cannot serve as the child's execution authority"
    );

    let child = fixture.start(&child_id).await;
    assert_eq!(child.claim.actor_user_id, actor.user_id);
    assert_eq!(
        child.claim.authorization_session_id,
        parent.claim.authorization_session_id
    );
    let now = fixture.tick();
    fixture
        .cloud
        .renew_run(&child, "lineage-worker", Duration::from_secs(60), now)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cloud
            .model_budget(&child, "lineage-worker")
            .await
            .unwrap(),
        (100, 0)
    );
    fixture
        .finish_successfully(&child, "Background work completed after its parent")
        .await;
    let finished = fixture
        .cloud
        .get_run(&session.tenant_id, &child_id)
        .await
        .unwrap();
    assert_eq!(finished.state, ternilo_cloud::CloudRunState::Succeeded);
    assert_eq!(
        finished.outcome.unwrap().answer,
        "Background work completed after its parent"
    );
    assert_eq!(
        fixture.reservation(&child_id).await,
        (child_reservation.0, "released".to_owned())
    );
    assert_eq!(
        fixture.reservation(&parent_id).await,
        (parent_reservation.0, "released".to_owned())
    );
}
