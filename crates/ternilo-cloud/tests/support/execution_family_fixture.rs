use std::time::Duration;

use ternilo_cloud::{
    CloudSessionDraft, CloudSessionRecord, CloudStore, CloudWorkerIdentity, CompiledRun,
    StartedRun, TerminalState,
};
use ternilo_control::{ControlStore, ControlUser, ResourceKind, ResourcePermissions};
use ternilo_protocol::{
    AgentId, Profile, RunId, RunLimits, RunMetadata, RunOutcome, RunSpec, SessionEvent,
    SessionEventKind, SessionId, SessionSubmissionRequest, SubagentId, SubagentSessionMetadata,
    SubagentTranscriptKind, SubmissionContent, SubmissionDelivery, TenantId, UserId, WorkspaceId,
};

#[path = "execution_family_fixture/provision.rs"]
mod provision;

const WORKER: &str = "family-worker";
const NOW: u64 = 2_200_000_000_000;
const LEASE: Duration = Duration::from_secs(60);

pub struct Fixture {
    pub control: ControlStore,
    pub cloud: CloudStore,
    pub owner: ControlUser,
    pub actor: ControlUser,
    pub tenant: TenantId,
    pub project: String,
    pub workspace: WorkspaceId,
    worker: CloudWorkerIdentity,
    now: u64,
}

impl Fixture {
    pub fn tick(&mut self) -> u64 {
        self.now += 1;
        self.now
    }

    pub fn draft(&self, session: &SessionId) -> CloudSessionDraft {
        CloudSessionDraft {
            project_id: self.project.clone(),
            workspace_id: self.workspace.clone(),
            session_id: Some(session.clone()),
            agent_id: AgentId::new("family-agent"),
            title: session.to_string(),
            permissions: ternilo_protocol::PermissionPreset::WorkspaceWrite,
            model: None,
            reserved_model_tokens: 100,
            agent_preset: "standard".to_owned(),
            profile_plugins: vec![],
            mode: ternilo_protocol::SessionMode::Execute,
        }
    }

    pub async fn session(&mut self, name: &str) -> CloudSessionRecord {
        let now = self.tick();
        self.cloud
            .create_session(
                self.draft(&SessionId::new(name)),
                &self.tenant,
                &self.owner.user_id,
                now,
            )
            .await
            .unwrap()
    }

    pub async fn share(&mut self, session: &SessionId) {
        let now = self.tick();
        self.control
            .set_resource_share(
                &self.owner,
                &self.tenant,
                ResourceKind::Session,
                session.as_str(),
                &self.actor.user_id,
                Some(ResourcePermissions {
                    view: true,
                    submit: true,
                    stop: true,
                    configure: false,
                }),
                now,
            )
            .await
            .unwrap();
    }

    pub fn compiled(
        &self,
        session: &CloudSessionRecord,
        name: &str,
        actor: &UserId,
    ) -> CompiledRun {
        CompiledRun {
            automated_input: None,
            actor_user_id: actor.clone(),
            authorization_session_id: session.session_id.clone(),
            spec: RunSpec {
                schema_version: ternilo_protocol::RUN_SPEC_VERSION,
                catalog_revision: "family-contract".to_owned(),
                policy_revision: "family-contract".to_owned(),
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
                permissions: session.permissions,
                mode: session.mode,
                profile: Profile::default(),
                input: name.to_owned(),
                references: vec![],
                reference_contexts: vec![],
                attachments: vec![],
            },
            reserved_model_tokens: 100,
            priority: 0,
            max_attempts: 2,
        }
    }

    pub async fn submit(
        &mut self,
        session: &CloudSessionRecord,
        name: &str,
        actor: &UserId,
    ) -> RunId {
        let compiled = self.compiled(session, name, actor);
        let now = self.tick();
        self.cloud
            .enqueue_session_submission_as(actor, &compiled, &request(&compiled), now)
            .await
            .unwrap()
            .run
            .run_id
    }

    pub async fn reserve(&mut self, compiled: &CompiledRun) -> String {
        let now = self.tick();
        self.control
            .reserve_quota(
                &self.owner,
                &self.tenant,
                Some(compiled.spec.metadata.run_id.as_str()),
                compiled.reserved_model_tokens,
                Duration::from_secs(300),
                now,
            )
            .await
            .unwrap()
            .reservation_id
    }

    pub async fn start(&mut self, expected: &RunId) -> StartedRun {
        let now = self.tick();
        let claim = self
            .cloud
            .claim_run(WORKER, LEASE, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&claim.run_id, expected);
        let now = self.tick();
        let started = self
            .cloud
            .start_run(claim, WORKER, LEASE, now)
            .await
            .unwrap()
            .unwrap();
        let now = self.tick();
        self.cloud
            .append_event(
                &started,
                WORKER,
                &SessionEvent {
                    seq: started.prior_events.last().map_or(0, |event| event.seq + 1),
                    occurred_at_ms: now,
                    run_id: expected.clone(),
                    kind: SessionEventKind::TurnStarted,
                },
                now,
            )
            .await
            .unwrap();
        started
    }

    pub async fn child(&mut self, parent: &StartedRun, name: &str) -> (CloudSessionRecord, RunId) {
        let session = SessionId::new(format!("{name}-session"));
        let now = self.tick();
        self.cloud
            .create_subagent_for_worker(
                WORKER,
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
        let run = self
            .cloud
            .enqueue_subagent_for_worker(WORKER, parent, &session, &spec, name, 2, now)
            .await
            .unwrap();
        (
            self.cloud
                .get_session(&self.tenant, &session)
                .await
                .unwrap(),
            run,
        )
    }

    pub async fn finish(&mut self, run: &StartedRun) {
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
            .append_event(run, WORKER, &event, now)
            .await
            .unwrap();
        let now = self.tick();
        self.cloud
            .finish_run(
                run,
                WORKER,
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
        let now = self.tick();
        self.cloud
            .release_resident(&run.into(), &self.worker, self.worker.generation, now)
            .await
            .unwrap();
    }

    pub async fn family(&self, session: &SessionId) -> (String, String, String, i64) {
        let mut tx = self
            .cloud
            .database()
            .tenant_transaction(&self.tenant)
            .await
            .unwrap();
        ternilo_storage::set_user_scope(&mut tx, &self.owner.user_id)
            .await
            .unwrap();
        let family = sqlx::query_as(
            "SELECT family_id,owner_user_id,workspace_id,created_at_ms FROM cloud_execution_families
             WHERE tenant_id=$1 AND session_id=$2 AND owner_user_id=$3",
        ).bind(self.tenant.as_str()).bind(session.as_str()).bind(self.owner.user_id.as_str()).fetch_one(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        family
    }
}

pub fn request(compiled: &CompiledRun) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery: SubmissionDelivery::Queue,
        run_id: Some(compiled.spec.metadata.run_id.clone()),
        content: SubmissionContent::Prompt {
            input: compiled.spec.input.clone(),
        },
        references: vec![],
        attachments: vec![],
    }
}
