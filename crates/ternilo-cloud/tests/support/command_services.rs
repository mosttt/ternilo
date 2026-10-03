use std::{collections::BTreeSet, time::Duration};

use ternilo_cloud::{
    ClaimedCloudSessionCommand, CloudCommandDelivery, CloudSessionCommandDraft,
    CloudSessionCommandState, CloudSessionRecord, CloudStore, CloudWorkerIdentity, CompiledRun,
    StartedRun, TerminalState,
};
use ternilo_control::{ControlStore, ControlUser, ResourceAction};
use ternilo_protocol::{
    ErrorCode, Profile, RunId, RunLimits, RunMetadata, RunSpec, SessionServiceKind,
    SessionServiceSnapshot, SessionServiceStatus, TenantId,
};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandOutcome, CommandReply, ExecutorCapabilities,
    ExecutorCapability, ExecutorCommand, ExecutorCommandBody, ExecutorScope,
};

const DISPATCH_LEASE_MS: u64 = 5_000;

pub struct Fixture<'a> {
    pub control: &'a ControlStore,
    pub cloud: &'a CloudStore,
    pub worker: &'a CloudStore,
    pub session: &'a CloudSessionRecord,
    pub owner: &'a ControlUser,
    pub other_user: &'a ControlUser,
    pub other_tenant: &'a TenantId,
    pub worker_a: &'a CloudWorkerIdentity,
    pub worker_b: &'a CloudWorkerIdentity,
    pub now: u64,
}

impl Fixture<'_> {
    fn tick(&mut self) -> u64 {
        self.now += 1;
        self.now
    }

    fn draft(
        &mut self,
        id: &str,
        request: ApplicationOperation,
        delivery: CloudCommandDelivery,
    ) -> CloudSessionCommandDraft {
        let now = self.tick();
        CloudSessionCommandDraft {
            session_id: self.session.session_id.clone(),
            command: ExecutorCommand {
                input_provenance: None,
                command_id: CommandId::new(id),
                scope: ExecutorScope {
                    tenant_id: self.session.tenant_id.clone(),
                    user_id: self.owner.user_id.clone(),
                },
                input_authorization: None,
                issued_at_ms: now,
                expires_at_ms: now + 1_000,
                body: ExecutorCommandBody::Application { request },
            },
            required_capability: ExecutorCapability::AddressedSessionCommands,
            required_catalog_revision: None,
            delivery,
        }
    }

    async fn enqueue(
        &mut self,
        draft: &CloudSessionCommandDraft,
    ) -> ternilo_cloud::CloudSessionCommandRecord {
        self.cloud
            .enqueue_session_command(
                &self.session.tenant_id,
                &self.owner.user_id,
                draft,
                self.tick(),
            )
            .await
            .unwrap()
    }

    async fn claim(&mut self, identity: &CloudWorkerIdentity) -> Vec<ClaimedCloudSessionCommand> {
        self.worker
            .claim_session_commands(
                identity,
                &capabilities(),
                Duration::from_millis(DISPATCH_LEASE_MS),
                16,
                self.tick(),
            )
            .await
            .unwrap()
    }

    async fn complete(
        &mut self,
        identity: &CloudWorkerIdentity,
        command: &ClaimedCloudSessionCommand,
        value: serde_json::Value,
    ) -> CommandReply {
        let now = self.tick();
        let reply = CommandReply::success(command.command.command_id.clone(), now, value);
        self.worker
            .complete_session_command(identity, command, &reply, now)
            .await
            .unwrap();
        reply
    }

    async fn start_run(&mut self, name: &str) -> StartedRun {
        let now = self.tick();
        let compiled = CompiledRun {
            automated_input: None,
            actor_user_id: self.owner.user_id.clone(),
            authorization_session_id: self.session.session_id.clone(),
            spec: RunSpec {
                schema_version: ternilo_protocol::RUN_SPEC_VERSION,
                catalog_revision: "command-test-catalog".to_owned(),
                policy_revision: "command-services-policy".to_owned(),
                metadata: RunMetadata {
                    tenant_id: self.session.tenant_id.clone(),
                    user_id: self.owner.user_id.clone(),
                    project_id: Some(self.session.project_id.clone()),
                    workspace_id: self.session.workspace_id.clone(),
                    agent_id: self.session.agent_id.clone(),
                    session_id: self.session.session_id.clone(),
                    run_id: RunId::new(name),
                },
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 2,
                },
                permissions: self.session.permissions,
                mode: self.session.mode,
                profile: Profile::default(),
                input: "service command writer fixture".to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
            },
            reserved_model_tokens: 100,
            priority: 0,
            max_attempts: 2,
        };
        let reservation = self
            .control
            .reserve_quota(
                self.owner,
                &self.session.tenant_id,
                Some(name),
                100,
                Duration::from_secs(600),
                now,
            )
            .await
            .unwrap();
        self.cloud
            .submit_run(&compiled, &reservation.reservation_id, now)
            .await
            .unwrap();
        let claim = self
            .worker
            .claim_run(
                self.worker_a.worker_id.as_str(),
                Duration::from_secs(60),
                self.tick(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claim.run_id.as_str(), name);
        self.worker
            .start_run(
                claim,
                self.worker_a.worker_id.as_str(),
                Duration::from_secs(60),
                self.tick(),
            )
            .await
            .unwrap()
            .unwrap()
    }
}

fn capabilities() -> ExecutorCapabilities {
    BTreeSet::from([
        ExecutorCapability::AddressedSessionCommands,
        ExecutorCapability::Skills,
    ])
}

fn service(status: SessionServiceStatus) -> SessionServiceSnapshot {
    SessionServiceSnapshot {
        id: "mcp:project-tools".to_owned(),
        name: "Project tools".to_owned(),
        kind: SessionServiceKind::Mcp,
        status,
        active_calls: 0,
        error: None,
    }
}

fn value(reply: &CommandReply) -> serde_json::Value {
    match &reply.outcome {
        CommandOutcome::Ok { value } => value.clone(),
        CommandOutcome::Error { error } => panic!("service command unexpectedly failed: {error}"),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise durable service reads, writer routing, mutation replay, and stale fencing on both databases."
)]
pub async fn verify(mut fixture: Fixture<'_>) {
    let tenant = fixture.session.tenant_id.clone();
    let session = fixture.session.session_id.clone();
    let owner = fixture.owner.user_id.clone();
    let worker_a = fixture.worker_a.clone();
    let worker_b = fixture.worker_b.clone();
    for action in [
        ResourceAction::View,
        ResourceAction::Submit,
        ResourceAction::Stop,
    ] {
        assert!(
            fixture
                .cloud
                .session_runtime_target(&tenant, &owner, &session, action, fixture.tick())
                .await
                .unwrap()
                .is_none()
        );
    }
    let read = fixture.draft(
        "service-catalog",
        ApplicationOperation::SessionServices {
            session_id: session.clone(),
        },
        CloudCommandDelivery::ReadOnly,
    );
    let initial = fixture.enqueue(&read).await;
    assert_eq!(
        initial.command_seq,
        fixture.enqueue(&read).await.command_seq
    );
    let first_read = fixture.claim(&worker_b).await.pop().unwrap();
    assert_eq!(first_read.command.command_id, read.command.command_id);
    assert_eq!(
        fixture
            .worker
            .release_session_commands(&worker_b, fixture.tick())
            .await
            .unwrap(),
        1
    );
    let replayed_read = fixture.claim(&worker_a).await.pop().unwrap();
    assert_eq!(
        replayed_read.attempt_count, 2,
        "read-only inspection may be reclaimed after release"
    );
    let listed = vec![service(SessionServiceStatus::Idle)];
    let read_reply = fixture
        .complete(
            &worker_a,
            &replayed_read,
            serde_json::to_value(&listed).unwrap(),
        )
        .await;
    let stored_read = fixture
        .cloud
        .wait_for_session_command_reply_as(
            &tenant,
            &owner,
            &session,
            &read.command.command_id,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_read, read_reply);
    assert_eq!(
        serde_json::from_value::<Vec<SessionServiceSnapshot>>(value(&stored_read)).unwrap(),
        listed
    );

    revoked_reader_cannot_receive_a_late_reply(&mut fixture).await;

    let inactive_run = fixture.start_run("service-command-inactive-run").await;
    fixture
        .worker
        .finish_run(
            &inactive_run,
            worker_a.worker_id.as_str(),
            TerminalState::Cancelled,
            None,
            None,
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .worker
        .release_resident(
            &(&inactive_run).into(),
            &worker_a,
            worker_a.generation,
            fixture.tick(),
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .session_runtime_target(
                &tenant,
                &owner,
                &session,
                ResourceAction::Submit,
                fixture.tick(),
            )
            .await
            .unwrap()
            .is_none()
    );
    let inactive = fixture.draft(
        "service-without-live-run",
        ApplicationOperation::SessionServiceStart {
            session_id: session.clone(),
            service_id: "mcp:project-tools".to_owned(),
        },
        CloudCommandDelivery::TargetRun {
            run_id: inactive_run.claim.run_id,
            writer_fencing_token: inactive_run.fencing_token,
        },
    );
    fixture.enqueue(&inactive).await;
    assert!(
        fixture.claim(&worker_a).await.is_empty(),
        "no service mutation can execute without a live writer target"
    );
    let run = fixture.start_run("service-command-run").await;
    let delivery = fixture
        .cloud
        .session_runtime_target(
            &tenant,
            &owner,
            &session,
            ResourceAction::Submit,
            fixture.tick(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        delivery,
        CloudCommandDelivery::TargetRun {
            run_id: run.claim.run_id.clone(),
            writer_fencing_token: run.fencing_token
        }
    );
    assert!(
        fixture
            .cloud
            .session_runtime_target(
                &tenant,
                &fixture.other_user.user_id,
                &session,
                ResourceAction::View,
                fixture.now
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .session_runtime_target(
                fixture.other_tenant,
                &owner,
                &session,
                ResourceAction::View,
                fixture.now
            )
            .await
            .is_err()
    );

    let start = fixture.draft(
        "service-start",
        ApplicationOperation::SessionServiceStart {
            session_id: session.clone(),
            service_id: "mcp:project-tools".to_owned(),
        },
        delivery.clone(),
    );
    let start_record = fixture.enqueue(&start).await;
    assert_eq!(
        fixture.enqueue(&start).await.command_seq,
        start_record.command_seq
    );
    assert!(
        fixture.claim(&worker_b).await.is_empty(),
        "a different live Worker cannot control this writer's runtime"
    );
    let start_claim = fixture.claim(&worker_a).await.pop().unwrap();
    assert_eq!(start_claim.command.command_id, start.command.command_id);
    assert_eq!(start_claim.delivery, delivery);
    let running = service(SessionServiceStatus::Running);
    let start_reply = fixture
        .complete(
            &worker_a,
            &start_claim,
            serde_json::to_value(&running).unwrap(),
        )
        .await;
    fixture
        .worker
        .complete_session_command(
            &worker_a,
            &start_claim,
            &start_reply,
            start_reply.completed_at_ms,
        )
        .await
        .unwrap();
    let repeated_start = fixture.enqueue(&start).await;
    assert_eq!(repeated_start.state, CloudSessionCommandState::Completed);
    assert_eq!(repeated_start.reply, Some(start_reply.clone()));
    assert_eq!(
        serde_json::from_value::<SessionServiceSnapshot>(value(
            repeated_start.reply.as_ref().unwrap()
        ))
        .unwrap(),
        running
    );
    let changed_reply = CommandReply::success(
        start_claim.command.command_id.clone(),
        start_reply.completed_at_ms,
        serde_json::to_value(service(SessionServiceStatus::Stopped)).unwrap(),
    );
    assert!(
        fixture
            .worker
            .complete_session_command(
                &worker_a,
                &start_claim,
                &changed_reply,
                changed_reply.completed_at_ms
            )
            .await
            .is_err(),
        "completed mutations cannot be rewritten with a conflicting reply"
    );

    let wrong_fence = fixture.draft(
        "service-stale-fence",
        ApplicationOperation::SessionServiceStart {
            session_id: session.clone(),
            service_id: "mcp:project-tools".to_owned(),
        },
        CloudCommandDelivery::TargetRun {
            run_id: run.claim.run_id.clone(),
            writer_fencing_token: run.fencing_token + 1,
        },
    );
    fixture.enqueue(&wrong_fence).await;
    assert!(
        fixture.claim(&worker_a).await.is_empty(),
        "a stale fence is never delivered"
    );
    let stop = fixture.draft(
        "service-stop",
        ApplicationOperation::SessionServiceStop {
            session_id: session.clone(),
            service_id: "mcp:project-tools".to_owned(),
        },
        delivery.clone(),
    );
    fixture.enqueue(&stop).await;
    let stop_claim = fixture.claim(&worker_a).await.pop().unwrap();
    assert_eq!(stop_claim.command.command_id, stop.command.command_id);
    let stopped = service(SessionServiceStatus::Stopped);
    let stop_reply = fixture
        .complete(
            &worker_a,
            &stop_claim,
            serde_json::to_value(&stopped).unwrap(),
        )
        .await;
    assert_eq!(
        serde_json::from_value::<SessionServiceSnapshot>(value(&stop_reply)).unwrap(),
        stopped
    );
    assert!(
        fixture
            .cloud
            .session_command(
                &tenant,
                &fixture.other_user.user_id,
                &stop.command.command_id
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .cloud
            .session_command(fixture.other_tenant, &owner, &stop.command.command_id)
            .await
            .unwrap()
            .is_none()
    );

    let mut interrupted = fixture.draft(
        "service-interrupted-stop",
        ApplicationOperation::SessionServiceStop {
            session_id: session.clone(),
            service_id: "mcp:project-tools".to_owned(),
        },
        delivery,
    );
    interrupted.command.expires_at_ms = interrupted.command.issued_at_ms + 2 * DISPATCH_LEASE_MS;
    fixture.enqueue(&interrupted).await;
    let interrupted_claim = fixture.claim(&worker_a).await.pop().unwrap();
    let dispatch_expires_at_ms = fixture.now + DISPATCH_LEASE_MS;
    assert_eq!(
        interrupted_claim.command.command_id,
        interrupted.command.command_id
    );
    fixture
        .worker
        .finish_run(
            &run,
            worker_a.worker_id.as_str(),
            TerminalState::Cancelled,
            None,
            None,
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .worker
        .release_resident(
            &(&run).into(),
            &worker_a,
            worker_a.generation,
            fixture.tick(),
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .session_runtime_target(
                &tenant,
                &owner,
                &session,
                ResourceAction::Stop,
                fixture.tick()
            )
            .await
            .unwrap()
            .is_none()
    );
    let next_run = fixture.start_run("service-command-next-run").await;
    let stale_reply = CommandReply::success(
        interrupted_claim.command.command_id.clone(),
        fixture.tick(),
        serde_json::to_value(&stopped).unwrap(),
    );
    let error = fixture
        .worker
        .complete_session_command(
            &worker_a,
            &interrupted_claim,
            &stale_reply,
            stale_reply.completed_at_ms,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    assert!(
        fixture.claim(&worker_a).await.is_empty(),
        "old target mutations cannot move to the replacement run"
    );
    fixture
        .worker
        .finish_run(
            &next_run,
            worker_a.worker_id.as_str(),
            TerminalState::Cancelled,
            None,
            None,
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .worker
        .release_resident(
            &(&next_run).into(),
            &worker_a,
            worker_a.generation,
            fixture.tick(),
        )
        .await
        .unwrap();
    // Lose the dispatch owner while the mutation itself is still within its deadline.
    fixture.now = dispatch_expires_at_ms + 1;
    assert!(fixture.now < interrupted.command.expires_at_ms);
    fixture
        .worker
        .reap_session_commands(fixture.now)
        .await
        .unwrap();
    let indeterminate = fixture
        .cloud
        .session_command(&tenant, &owner, &interrupted.command.command_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(indeterminate.state, CloudSessionCommandState::Indeterminate);
    assert_eq!(indeterminate.attempt_count, 1);
    assert!(
        fixture.claim(&worker_a).await.is_empty(),
        "an uncertain mutation is not automatically replayed"
    );
    assert_eq!(
        fixture
            .cloud
            .session_command(&tenant, &owner, &start.command.command_id)
            .await
            .unwrap()
            .unwrap()
            .reply,
        Some(start_reply),
        "the committed mutation reply remains readable after its command deadline",
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise authenticated sharing, an in-flight read, revocation and owner-only late delivery as one lifecycle."
)]
async fn revoked_reader_cannot_receive_a_late_reply(fixture: &mut Fixture<'_>) {
    use ternilo_control::{ResourceKind, ResourcePermissions, TenantRole};
    let bootstrap = fixture
        .control
        .initialize_owner(
            &ternilo_control::NativeRegistration {
                email: "command-owner@example.test".into(),
                username: "command-owner".into(),
                password: "command-owner-fixture-password".into(),
            },
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .control
        .set_instance_mode(
            &bootstrap.session.user,
            ternilo_control::InstanceMode::MultiUser,
            bootstrap.session.instance.revision,
            fixture.tick(),
        )
        .await
        .unwrap();
    let tenant = fixture.session.tenant_id.clone();
    let session = fixture.session.session_id.clone();
    let viewer = fixture.other_user.user_id.clone();
    fixture
        .control
        .set_membership(
            fixture.owner,
            &tenant,
            &viewer,
            TenantRole::Member,
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .control
        .set_resource_share(
            fixture.owner,
            &tenant,
            ResourceKind::Session,
            session.as_str(),
            &viewer,
            Some(ResourcePermissions {
                view: true,
                ..ResourcePermissions::default()
            }),
            fixture.tick(),
        )
        .await
        .unwrap();
    let draft = fixture.draft(
        "late-private-inspection",
        ApplicationOperation::SessionCommands {
            session_id: session.clone(),
        },
        CloudCommandDelivery::ReadOnly,
    );
    fixture.enqueue(&draft).await;
    let worker = fixture.worker_a.clone();
    let claim = fixture.claim(&worker).await.pop().unwrap();
    let cloud = fixture.cloud.clone();
    let command = draft.command.command_id.clone();
    let read_tenant = tenant.clone();
    let read_session = session.clone();
    let read_viewer = viewer.clone();
    let waiting = tokio::spawn(async move {
        cloud
            .wait_for_session_command_reply_as(
                &read_tenant,
                &read_viewer,
                &read_session,
                &command,
                Duration::from_secs(5),
                Duration::from_millis(10),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !waiting.is_finished(),
        "reader waits for the original pending command"
    );
    fixture
        .control
        .set_resource_share(
            fixture.owner,
            &tenant,
            ResourceKind::Session,
            session.as_str(),
            &viewer,
            None,
            fixture.tick(),
        )
        .await
        .unwrap();
    fixture
        .complete(
            &worker,
            &claim,
            serde_json::json!({"private":"late-result"}),
        )
        .await;
    assert_eq!(
        waiting.await.unwrap().unwrap_err().code,
        ErrorCode::PolicyDenied
    );
    let owner_reply = fixture
        .cloud
        .session_command_as(
            &tenant,
            &fixture.owner.user_id,
            &session,
            &draft.command.command_id,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        owner_reply.reply.is_some(),
        "the owner's result remains available after revoking the reader"
    );
}
