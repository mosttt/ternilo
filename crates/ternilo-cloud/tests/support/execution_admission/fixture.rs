use super::*;

pub(super) struct Fixture {
    pub(super) control: ControlStore,
    pub(super) cloud: CloudStore,
    pub(super) owner: ControlUser,
    pub(super) actor: ControlUser,
    pub(super) tenant: TenantId,
    pub(super) project: String,
    pub(super) worker: CloudWorkerIdentity,
    pub(super) worker_token: String,
    pub(super) now: u64,
}

impl Fixture {
    pub(super) async fn open(control: ControlStore, cloud: CloudStore) -> Self {
        let owner = control
            .initialize_owner(
                &NativeRegistration {
                    email: "capacity-owner@example.test".to_owned(),
                    username: "capacity-owner".to_owned(),
                    password: "capacity-owner-password".to_owned(),
                },
                NOW,
            )
            .await
            .unwrap()
            .session
            .user;
        control
            .set_instance_mode(&owner, InstanceMode::MultiUser, 1, NOW)
            .await
            .unwrap();
        let actor = control
            .upsert_user(
                &OidcPrincipal {
                    issuer: "https://capacity.example.test".to_owned(),
                    subject: "actor".to_owned(),
                    email: None,
                    display_name: None,
                },
                "capacity-actor",
                NOW,
            )
            .await
            .unwrap();
        let tenant = control
            .create_tenant(
                &owner,
                "capacity",
                "Capacity",
                TenantQuota {
                    max_nodes: 4,
                    max_concurrent_runs: 4,
                    monthly_model_tokens: 100_000,
                    max_secrets: 4,
                },
                NOW,
            )
            .await
            .unwrap()
            .tenant_id;
        control
            .set_membership(&owner, &tenant, &actor.user_id, TenantRole::Member, NOW)
            .await
            .unwrap();
        let project = control
            .create_project(&owner, &tenant, "Capacity", NOW)
            .await
            .unwrap()
            .project_id;
        let token = cloud
            .create_worker_credential(&ExecutorId::new("capacity-worker"), "capacity-storage", NOW)
            .await
            .unwrap()
            .token;
        let worker = cloud
            .register_authenticated_worker(
                &token,
                &registration("capacity-worker", "first", WorkerCapacity::default()),
                Duration::from_secs(300),
                NOW,
            )
            .await
            .unwrap();
        Self {
            control,
            cloud,
            owner,
            actor,
            tenant,
            project,
            worker,
            worker_token: token,
            now: NOW + 1,
        }
    }
    pub(super) fn tick(&mut self) -> u64 {
        self.now += 1;
        self.now
    }
    pub(super) async fn session(&mut self, name: &str) -> CloudSessionRecord {
        let now = self.tick();
        let workspace = self
            .control
            .create_cloud_workspace(&self.owner, &self.tenant, &self.project, name, now)
            .await
            .unwrap()
            .workspace_id;
        self.control
            .set_resource_share(
                &self.owner,
                &self.tenant,
                ResourceKind::Workspace,
                workspace.as_str(),
                &self.actor.user_id,
                Some(ResourcePermissions::OWNER),
                now,
            )
            .await
            .unwrap();
        self.cloud
            .create_session(
                CloudSessionDraft {
                    project_id: self.project.clone(),
                    workspace_id: workspace,
                    session_id: Some(SessionId::new(name)),
                    agent_id: AgentId::new("capacity-agent"),
                    title: name.to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 100,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &self.tenant,
                &self.owner.user_id,
                now,
            )
            .await
            .unwrap()
    }

    pub(super) async fn session_in_workspace(
        &mut self,
        name: &str,
        workspace: &ternilo_protocol::WorkspaceId,
    ) -> CloudSessionRecord {
        let now = self.tick();
        self.cloud
            .create_session(
                CloudSessionDraft {
                    project_id: self.project.clone(),
                    workspace_id: workspace.clone(),
                    session_id: Some(SessionId::new(name)),
                    agent_id: AgentId::new("capacity-agent"),
                    title: name.to_owned(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 100,
                    agent_preset: "standard".to_owned(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &self.tenant,
                &self.owner.user_id,
                now,
            )
            .await
            .unwrap()
    }
    pub(super) async fn submit(&mut self, session: &CloudSessionRecord, name: &str) -> RunId {
        let spec = RunSpec {
            schema_version: ternilo_protocol::RUN_SPEC_VERSION,
            catalog_revision: "capacity".to_owned(),
            policy_revision: "capacity".to_owned(),
            metadata: RunMetadata {
                tenant_id: self.tenant.clone(),
                user_id: self.owner.user_id.clone(),
                project_id: Some(self.project.clone()),
                workspace_id: session.workspace_id.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session.session_id.clone(),
                run_id: RunId::new(name),
            },
            limits: RunLimits::default(),
            permissions: PermissionPreset::WorkspaceWrite,
            mode: SessionMode::Execute,
            profile: Profile::default(),
            input: name.to_owned(),
            references: vec![],
            reference_contexts: vec![],
            attachments: vec![],
        };
        let compiled = CompiledRun {
            automated_input: None,
            actor_user_id: self.actor.user_id.clone(),
            authorization_session_id: session.session_id.clone(),
            spec,
            reserved_model_tokens: 100,
            priority: 0,
            max_attempts: 3,
        };
        let request = SessionSubmissionRequest {
            delivery: SubmissionDelivery::Queue,
            run_id: Some(compiled.spec.metadata.run_id.clone()),
            content: SubmissionContent::Prompt {
                input: name.to_owned(),
            },
            references: vec![],
            attachments: vec![],
        };
        let now = self.tick();
        self.cloud
            .enqueue_session_submission_as(&self.actor.user_id, &compiled, &request, now)
            .await
            .unwrap()
            .run
            .run_id
    }
    pub(super) async fn start(&mut self, id: &RunId, worker: &str) -> StartedRun {
        let now = self.tick();
        let claim = self
            .cloud
            .claim_run(worker, LEASE, now)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("a foreground and resident slot must be available for {id}"));
        assert_eq!(claim.run_id, *id);
        self.start_claim(claim, worker).await
    }

    pub(super) async fn start_claim(
        &mut self,
        claim: ternilo_cloud::CloudRunClaim,
        worker: &str,
    ) -> StartedRun {
        let now = self.tick();
        let run = self
            .cloud
            .start_run(claim, worker, LEASE, now)
            .await
            .unwrap()
            .unwrap();
        let now = self.tick();
        self.cloud
            .append_event(
                &run,
                worker,
                &SessionEvent {
                    seq: run.prior_events.last().map_or(0, |event| event.seq + 1),
                    occurred_at_ms: now,
                    run_id: run.claim.run_id.clone(),
                    kind: SessionEventKind::TurnStarted,
                },
                now,
            )
            .await
            .unwrap();
        run
    }
    pub(super) async fn child(
        &mut self,
        parent: &StartedRun,
        name: &str,
        worker: &str,
    ) -> AcceptedSubagentRun {
        let session = SessionId::new(format!("{name}-session"));
        let now = self.tick();
        self.cloud
            .create_subagent_for_worker(
                worker,
                parent,
                &session,
                &SubagentSessionMetadata {
                    subagent_id: SubagentId::new(name),
                    provider: "in-process".to_owned(),
                    transcript_kind: SubagentTranscriptKind::Conversation,
                },
                name,
                now,
            )
            .await
            .unwrap();
        let mut spec = parent.claim.spec.clone();
        spec.metadata.session_id = session.clone();
        spec.metadata.run_id = RunId::new(name);
        spec.input = name.to_owned();
        let now = self.tick();
        let run_id = self
            .cloud
            .enqueue_subagent_for_worker(worker, parent, &session, &spec, name, 3, now)
            .await
            .unwrap();
        AcceptedSubagentRun {
            session_id: session,
            run_id,
        }
    }
    pub(super) async fn park(
        &mut self,
        run: &StartedRun,
        dependency: &AcceptedSubagentRun,
        worker: &str,
    ) -> u64 {
        let now = self.tick();
        self.cloud
            .park_run(run, worker, std::slice::from_ref(dependency), 1, now)
            .await
            .unwrap()
    }
    pub(super) async fn resume(&mut self, run: &StartedRun, worker: &str) -> RunAdmission {
        let state = self
            .cloud
            .run_execution(&self.tenant, &self.owner.user_id, &run.claim.run_id)
            .await
            .unwrap()
            .unwrap();
        let revision = if state.phase == ExecutionPhase::Parked {
            state.activity_revision + 1
        } else {
            state.activity_revision
        };
        let now = self.tick();
        self.cloud
            .resume_run(run, worker, revision, state.parked_revision, now)
            .await
            .unwrap()
    }
    pub(super) async fn finish(&mut self, run: &StartedRun, worker: &str) {
        let seq = self
            .cloud
            .get_session(&self.tenant, &run.claim.session_id)
            .await
            .unwrap()
            .last_seq
            .map_or(0, |value| value + 1);
        let now = self.tick();
        let event = SessionEvent {
            seq,
            occurred_at_ms: now,
            run_id: run.claim.run_id.clone(),
            kind: SessionEventKind::TurnFinished {
                answer: "done".to_owned(),
                finish_reason: ternilo_protocol::TurnFinishReason::Completed,
            },
        };
        self.cloud
            .append_event(run, worker, &event, now)
            .await
            .unwrap();
        let now = self.tick();
        self.cloud
            .finish_run(
                run,
                worker,
                TerminalState::Succeeded,
                Some(&RunOutcome {
                    answer: "done".to_owned(),
                    steps: 1,
                    tool_calls: 0,
                    events: vec![event],
                    generated_title: None,
                }),
                None,
                now,
            )
            .await
            .unwrap();
        let row =
            sqlx::query("SELECT instance_nonce,generation FROM cloud_workers WHERE worker_id=$1")
                .bind(worker)
                .fetch_one(self.cloud.database().pool())
                .await
                .unwrap();
        let identity = CloudWorkerIdentity {
            worker_id: ExecutorId::new(worker),
            instance_nonce: row.try_get("instance_nonce").unwrap(),
            generation: u64::try_from(row.try_get::<i64, _>("generation").unwrap()).unwrap(),
        };
        let now = self.tick();
        self.cloud
            .release_resident(&run.into(), &identity, identity.generation, now)
            .await
            .unwrap();
    }
    pub(super) async fn phase(&self, run: &StartedRun) -> ExecutionPhase {
        self.cloud
            .run_execution(&self.tenant, &self.owner.user_id, &run.claim.run_id)
            .await
            .unwrap()
            .unwrap()
            .phase
    }
    pub(super) async fn usage(&self, worker: &str) -> (i64, i64) {
        let row = if self.cloud.database().backend() == ternilo_storage::Backend::Postgres {
            sqlx::query(
                "SELECT worker_active,worker_resident FROM ternilo_cloud_execution_usage($1,$2)",
            )
            .bind(self.tenant.as_str())
            .bind(worker)
            .fetch_one(self.cloud.database().pool())
            .await
            .unwrap()
        } else {
            sqlx::query("SELECT SUM(CASE WHEN phase IN ('active','claimed') THEN 1 ELSE 0 END) AS worker_active,COUNT(*) AS worker_resident FROM cloud_run_execution WHERE worker_id=$1 AND phase<>'released'").bind(worker).fetch_one(self.cloud.database().pool()).await.unwrap()
        };
        (
            row.try_get::<Option<i64>, _>("worker_active")
                .unwrap()
                .unwrap_or(0),
            row.try_get("worker_resident").unwrap(),
        )
    }
}

pub(super) fn registration(
    worker: &str,
    nonce: &str,
    capacity: WorkerCapacity,
) -> WorkerRegisterRequest {
    WorkerRegisterRequest {
        capacity,
        storage_id: "capacity-storage".to_owned(),
        root_id: "capacity-root".to_owned(),
        hello: ExecutorHello {
            protocol_version: ternilo_transport::EXECUTOR_PROTOCOL_VERSION,
            executor_id: ExecutorId::new(worker),
            executor_kind: ExecutorKind::CloudWorker,
            instance_nonce: nonce.to_owned(),
            catalog_revision: "capacity".to_owned(),
            capabilities: BTreeSet::from([ExecutorCapability::CloudRun]),
        },
    }
}
