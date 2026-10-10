use serde_json::Value;
use ternilo_cloud::{CloudSessionDraft, CloudSessionRecord, CloudStore};
use ternilo_control::{
    ControlStore, ControlUser, InstanceMode, NativeRegistration, OidcPrincipal, ResourceKind,
    ResourcePermissions, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_protocol::{
    AgentId, ModelFinishReason, ModelResponse, ModelUsage, PermissionPreset, RunId, SessionEvent,
    SessionEventKind, SessionMode, SessionStats, SubmissionDelivery, SubmissionId, TenantId,
    UserMessageSource,
};
use ternilo_storage::Json;

#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;

struct Fixture {
    url: String,
    cloud: CloudStore,
    control: ControlStore,
    owner: ControlUser,
    session: CloudSessionRecord,
    events: Vec<SessionEvent>,
}

impl Fixture {
    async fn new(url: &str, migration_url: Option<&str>) -> Self {
        let control =
            ControlStore::connect(url, migration_url, SecretCipher::from_key([41; 32]), 4)
                .await
                .unwrap();
        let cloud = CloudStore::connect(url, migration_url, 4).await.unwrap();
        let boot = control
            .initialize_owner(
                &NativeRegistration {
                    username: "stats-owner".into(),
                    email: "stats-owner@example.test".into(),
                    password: "synthetic-stats-password".into(),
                },
                1000,
            )
            .await
            .unwrap();
        let owner = boot.session.user;
        control
            .set_instance_mode(
                &owner,
                InstanceMode::MultiUser,
                boot.session.instance.revision,
                1001,
            )
            .await
            .unwrap();
        let tenant = control
            .create_tenant(
                &owner,
                "stats-team",
                "Stats team",
                TenantQuota::default(),
                1002,
            )
            .await
            .unwrap();
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
                "Stats workspace",
                1003,
            )
            .await
            .unwrap();
        let session = cloud
            .create_session(
                CloudSessionDraft {
                    project_id: project.project_id,
                    workspace_id: workspace.workspace_id,
                    session_id: None,
                    agent_id: AgentId::new("stats-agent"),
                    title: "Stats".into(),
                    permissions: PermissionPreset::WorkspaceWrite,
                    model: None,
                    reserved_model_tokens: 100,
                    agent_preset: "standard".into(),
                    profile_plugins: vec![],
                    mode: SessionMode::Execute,
                },
                &tenant.tenant_id,
                &owner.user_id,
                1004,
            )
            .await
            .unwrap();
        Self {
            url: url.into(),
            cloud,
            control,
            owner,
            session,
            events: vec![],
        }
    }

    async fn append(&mut self, run: &str, kinds: Vec<SessionEventKind>) {
        let mut tx = self
            .cloud
            .database()
            .tenant_transaction(&self.session.tenant_id)
            .await
            .unwrap();
        for kind in kinds {
            let seq = u64::try_from(self.events.len()).unwrap();
            let event = SessionEvent {
                seq,
                run_id: RunId::new(run),
                occurred_at_ms: 2000 + seq * 10,
                kind,
            };
            sqlx::query("INSERT INTO cloud_session_events VALUES($1,$2,$3,$4,$5,1,$6)")
                .bind(self.session.tenant_id.as_str())
                .bind(self.session.session_id.as_str())
                .bind(i64::try_from(seq).unwrap())
                .bind(run)
                .bind(Json(&event))
                .bind(i64::try_from(event.occurred_at_ms).unwrap())
                .execute(&mut *tx)
                .await
                .unwrap();
            self.events.push(event);
        }
        let last = self.events.last().unwrap();
        sqlx::query("UPDATE cloud_sessions SET last_seq=$3,updated_at_ms=$4 WHERE tenant_id=$1 AND session_id=$2")
            .bind(self.session.tenant_id.as_str()).bind(self.session.session_id.as_str())
            .bind(i64::try_from(last.seq).unwrap()).bind(i64::try_from(last.occurred_at_ms).unwrap())
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn stats(&self) -> SessionStats {
        let stats = self
            .cloud
            .session_stats_as(
                &self.session.tenant_id,
                &self.owner.user_id,
                &self.session.session_id,
            )
            .await
            .unwrap();
        assert_eq!(
            stats,
            ternilo_builtins::session_stats(&self.events).unwrap()
        );
        stats
    }

    async fn checkpoint(&self) -> Value {
        let mut tx = self
            .cloud
            .database()
            .tenant_transaction(&self.session.tenant_id)
            .await
            .unwrap();
        let json: String = sqlx::query_scalar(
            "SELECT checkpoint_json FROM cloud_session_stats WHERE tenant_id=$1 AND session_id=$2",
        )
        .bind(self.session.tenant_id.as_str())
        .bind(self.session.session_id.as_str())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        serde_json::from_str(&json).unwrap()
    }

    async fn replace_checkpoint(&self, json: &str) {
        let mut tx = self
            .cloud
            .database()
            .tenant_transaction(&self.session.tenant_id)
            .await
            .unwrap();
        sqlx::query("UPDATE cloud_session_stats SET checkpoint_json=$3 WHERE tenant_id=$1 AND session_id=$2")
            .bind(self.session.tenant_id.as_str()).bind(self.session.session_id.as_str()).bind(json)
            .execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
}

fn user(target: Option<u64>) -> SessionEventKind {
    SessionEventKind::UserMessage {
        content: "Synthetic stats input".into(),
        display_content: None,
        provenance: None,
        source: target.map(|target| UserMessageSource::Submission {
            submission_id: SubmissionId::new("regeneration"),
            created_at_ms: 1000,
            delivery: SubmissionDelivery::Queue,
            skill_name: None,
            regenerate_from: Some(target),
        }),
        references: vec![],
        attachments: vec![],
    }
}

fn response() -> SessionEventKind {
    SessionEventKind::AssistantMessage {
        step: 1,
        response: ModelResponse {
            provider: "fixture".into(),
            model: "fixture-model".into(),
            content: "done".into(),
            reasoning_content: None,
            provider_state: None,
            tool_calls: vec![],
            usage: Some(ModelUsage {
                input_tokens: 12,
                output_tokens: 4,
                cached_input_tokens: 3,
                cache_write_tokens: None,
                reasoning_tokens: 2,
            }),
            finish_reason: ModelFinishReason::Stop,
            provider_request_id: None,
            attempts: 1,
            request_digest: None,
            replayed: false,
        },
    }
}

async fn incremental_and_restart(fixture: &mut Fixture) {
    assert_eq!(fixture.stats().await, SessionStats::default());
    fixture
        .append(
            "original",
            vec![
                SessionEventKind::TurnStarted,
                user(None),
                SessionEventKind::StepStarted { step: 1 },
                SessionEventKind::AssistantMessageDelta {
                    step: 1,
                    delta: "first".into(),
                },
            ],
        )
        .await;
    assert_eq!(fixture.stats().await.events, 4);
    let checkpoint = fixture.checkpoint().await;
    assert_eq!(fixture.stats().await.events, 4);
    assert_eq!(
        fixture.checkpoint().await,
        checkpoint,
        "repeated reads do not rewrite the checkpoint"
    );
    fixture.cloud.database().close().await;
    fixture.cloud = CloudStore::connect(&fixture.url, None, 4).await.unwrap();
    fixture
        .append(
            "original",
            vec![
                response(),
                SessionEventKind::TurnFailed {
                    message: "synthetic ending".into(),
                },
            ],
        )
        .await;
    let stats = fixture.stats().await;
    assert_eq!(stats.exact_input_tokens, 12);
    assert_eq!(stats.exact_output_tokens, 4);
    assert_eq!(stats.first_token_duration_ms, 10);
    assert_eq!(stats.model_duration_ms, 20);
    assert_eq!(stats.failed_turns, 1);
    let (a, b, c, d) = tokio::join!(
        fixture.stats(),
        fixture.stats(),
        fixture.stats(),
        fixture.stats()
    );
    assert_eq!([a, b, c, d], [stats; 4]);
}

async fn fork_and_regeneration(fixture: &mut Fixture) {
    let child = fixture
        .cloud
        .fork_session(
            &fixture.session.tenant_id,
            &fixture.owner.user_id,
            &fixture.session.session_id,
            None,
            3000,
        )
        .await
        .unwrap();
    let child_events = fixture
        .cloud
        .session_events_as(
            &child.tenant_id,
            &fixture.owner.user_id,
            &child.session_id,
            None,
            1000,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cloud
            .session_stats_as(&child.tenant_id, &fixture.owner.user_id, &child.session_id)
            .await
            .unwrap(),
        ternilo_builtins::session_stats(&child_events).unwrap()
    );
    fixture
        .append(
            "followup",
            vec![
                SessionEventKind::TurnStarted,
                user(None),
                SessionEventKind::TurnCancelled,
            ],
        )
        .await;
    assert_eq!(fixture.stats().await.turns, 2);
    fixture
        .append(
            "replacement",
            vec![
                SessionEventKind::TurnStarted,
                user(Some(1)),
                SessionEventKind::TurnFailed {
                    message: "replacement ending".into(),
                },
            ],
        )
        .await;
    let stats = fixture.stats().await;
    assert_eq!(stats.turns, 1);
    assert_eq!(stats.events, 3);
    assert_eq!(stats.exact_input_tokens, 0);
    fixture.append("original", vec![response()]).await;
    assert_eq!(
        fixture.stats().await,
        stats,
        "late discarded run events do not reenter stats"
    );
    fixture
        .append(
            "next",
            vec![
                SessionEventKind::TurnStarted,
                user(None),
                SessionEventKind::TurnCancelled,
            ],
        )
        .await;
    assert_eq!(fixture.stats().await.turns, 2);
}

async fn invalid_checkpoints_rebuild(fixture: &Fixture) {
    let expected = fixture.stats().await;
    for (field, value) in [
        ("version", Value::from(0)),
        ("through_seq", Value::from(100_000)),
        ("state", Value::Null),
    ] {
        let mut checkpoint = fixture.checkpoint().await;
        checkpoint[field] = value;
        fixture.replace_checkpoint(&checkpoint.to_string()).await;
        assert_eq!(fixture.stats().await, expected);
    }
    fixture.replace_checkpoint("invalid json").await;
    assert_eq!(fixture.stats().await, expected);
}

async fn cached_reads_recheck_access(fixture: &Fixture) {
    let tenant = &fixture.session.tenant_id;
    let session = &fixture.session.session_id;
    let reader = fixture
        .control
        .upsert_user(
            &OidcPrincipal {
                issuer: "stats".into(),
                subject: "reader".into(),
                email: None,
                display_name: None,
            },
            "stats-reader",
            4000,
        )
        .await
        .unwrap();
    fixture
        .control
        .set_membership(
            &fixture.owner,
            tenant,
            &reader.user_id,
            TenantRole::Member,
            4000,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .session_stats_as(tenant, &reader.user_id, session)
            .await
            .is_err()
    );
    fixture
        .control
        .set_resource_share(
            &fixture.owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &reader.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: false,
                stop: false,
                configure: false,
            }),
            4001,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .cloud
            .session_stats_as(tenant, &reader.user_id, session)
            .await
            .unwrap(),
        fixture.stats().await
    );
    fixture
        .control
        .set_resource_share(
            &fixture.owner,
            tenant,
            ResourceKind::Session,
            session.as_str(),
            &reader.user_id,
            None,
            4002,
        )
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .session_stats_as(tenant, &reader.user_id, session)
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .session_stats_as(&TenantId::new("other"), &fixture.owner.user_id, session)
            .await
            .is_err()
    );
}

async fn contract(url: &str, migration_url: Option<&str>) {
    let mut fixture = Fixture::new(url, migration_url).await;
    incremental_and_restart(&mut fixture).await;
    fork_and_regeneration(&mut fixture).await;
    invalid_checkpoints_rebuild(&fixture).await;
    cached_reads_recheck_access(&fixture).await;
    fixture.cloud.database().close().await;
}

#[tokio::test]
async fn sqlite_stats_preserve_history_timing_regeneration_and_access() {
    let directory = tempfile::tempdir().unwrap();
    contract(
        &format!(
            "sqlite://{}",
            directory.path().join("stats.sqlite3").display()
        ),
        None,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires a disposable TERNILO_CLOUD_TEST_DATABASE_URL"]
async fn postgres_stats_preserve_the_same_contract_with_restricted_runtime() {
    let admin = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin.contains("ternilo_cloud_test"));
    let runtime = server_runtime::initialize(&admin, "ternilo_stats_runtime_test", [41; 32]).await;
    contract(&runtime, Some(&admin)).await;
    server_runtime::assert_scoped_without_schema_access(&runtime).await;
}
