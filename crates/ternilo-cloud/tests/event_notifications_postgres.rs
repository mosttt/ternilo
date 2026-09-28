use std::time::Duration;

use sqlx::Executor;
use ternilo_cloud::{CloudLiveNotification, CloudSessionDraft, CloudSessionEventFeed, CloudStore};
use ternilo_control::{ControlStore, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{
    AgentId, AgentTeamTaskCreate, AgentTeamTaskStatus, PermissionPreset, RunId, SessionId,
    SessionMode, UserAnswer, UserQuestion,
};

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
#[expect(
    clippy::too_many_lines,
    reason = "The complete transactional notification, scope and rollback contract stays in one scenario."
)]
async fn postgres_live_feed_maps_committed_invalidations_and_isolates_scopes() {
    let database_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set");
    assert!(database_url.contains("ternilo_cloud_test"));
    let database = sqlx::PgPool::connect(&database_url).await.unwrap();
    database
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    database.execute("CREATE SCHEMA public").await.unwrap();
    drop(database);

    let control = ControlStore::connect(&database_url, None, SecretCipher::from_key([31; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&database_url, None, 4).await.unwrap();
    let feed = CloudSessionEventFeed::connect(&database_url).await.unwrap();
    let now = 2_200_000_000_000_u64;

    let alice = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://issuer.example".to_owned(),
                subject: "event-feed-alice".to_owned(),
                email: None,
                display_name: Some("Alice".to_owned()),
            },
            "test-event-feed-alice",
            now,
        )
        .await
        .unwrap();
    let bob = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://issuer.example".to_owned(),
                subject: "event-feed-bob".to_owned(),
                email: None,
                display_name: Some("Bob".to_owned()),
            },
            "test-event-feed-bob",
            now + 1,
        )
        .await
        .unwrap();
    let quota = TenantQuota {
        max_nodes: 1,
        max_concurrent_runs: 2,
        monthly_model_tokens: 10_000,
        max_secrets: 2,
    };
    let tenant_a = control
        .create_tenant(
            &alice,
            "event-feed-a",
            "Event feed A",
            quota.clone(),
            now + 2,
        )
        .await
        .unwrap();
    let tenant_b = control
        .create_tenant(&bob, "event-feed-b", "Event feed B", quota, now + 3)
        .await
        .unwrap();
    let project_a = control
        .create_project(&alice, &tenant_a.tenant_id, "Project A", now + 4)
        .await
        .unwrap();
    let project_b = control
        .create_project(&bob, &tenant_b.tenant_id, "Project B", now + 5)
        .await
        .unwrap();
    let workspace_a = control
        .create_cloud_workspace(
            &alice,
            &tenant_a.tenant_id,
            &project_a.project_id,
            "Workspace A",
            now + 6,
        )
        .await
        .unwrap();
    let workspace_b = control
        .create_cloud_workspace(
            &bob,
            &tenant_b.tenant_id,
            &project_b.project_id,
            "Workspace B",
            now + 7,
        )
        .await
        .unwrap();

    let commit_session = SessionId::new("event-feed-commit");
    let other_tenant_session = SessionId::new("event-feed-other-tenant");
    create_session(
        &cloud,
        &tenant_a.tenant_id,
        &alice.user_id,
        &project_a.project_id,
        &workspace_a.workspace_id,
        &commit_session,
        now + 10,
    )
    .await;
    create_session(
        &cloud,
        &tenant_b.tenant_id,
        &bob.user_id,
        &project_b.project_id,
        &workspace_b.workspace_id,
        &other_tenant_session,
        now + 11,
    )
    .await;

    let audit = sqlx::PgPool::connect(&database_url).await.unwrap();
    // Two logical connections share one feed; the canonical Queue change below
    // must reach both without a per-connection PostgreSQL listener.
    let mut live_a = feed.subscribe();
    let mut live_b = feed.subscribe();
    let event_trigger_definition = sqlx::query_scalar::<_, String>(
        "SELECT pg_get_functiondef('ternilo_notify_cloud_session_event()'::regprocedure)",
    )
    .fetch_one(&audit)
    .await
    .unwrap();
    assert!(event_trigger_definition.contains("event_type"));
    assert!(event_trigger_definition.contains("NEW.event"));
    sqlx::query("SELECT pg_notify('ternilo_cloud_session_events', $1)")
        .bind(
            serde_json::json!({
                "tenant_id": tenant_a.tenant_id,
                "session_id": commit_session,
                "event_type": "assistant_message_delta",
            })
            .to_string(),
        )
        .execute(&audit)
        .await
        .unwrap();
    let streaming = receive_live(&mut live_a, Duration::from_secs(1), |notification| {
        matches!(
            notification,
            CloudLiveNotification::Session {
                tenant_id,
                session_id: Some(session_id),
                dirty,
                activity: false,
                ..
            } if tenant_id == &tenant_a.tenant_id
                && session_id == &commit_session
                && dirty.events
                && dirty.metadata().is_empty()
        )
    })
    .await;
    assert!(streaming.is_some());

    // The canonical question row is committed independently from the event
    // journal, so both its pending insert and answered update must wake live
    // metadata readers on their own.
    let question_run_id = RunId::new("event-feed-question-run");
    let reservation = control
        .reserve_quota(
            &alice,
            &tenant_a.tenant_id,
            Some(question_run_id.as_str()),
            100,
            Duration::from_secs(3_600),
            now + 25,
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO cloud_runs
            (tenant_id, run_id, user_id, actor_user_id, authorization_session_id,
             project_id, workspace_id, agent_id, session_id,
             spec, spec_digest, quota_reservation_id, state, priority, attempt, max_attempts,
             available_at_ms, created_at_ms, updated_at_ms)
         VALUES ($1, $2, $3, $3, $6, $4, $5, 'agent', $6,
                 '{}', $7, $8, 'running', 0, 0, 1, $9, $9, $9)",
    )
    .bind(tenant_a.tenant_id.as_str())
    .bind(question_run_id.as_str())
    .bind(alice.user_id.as_str())
    .bind(&project_a.project_id)
    .bind(workspace_a.workspace_id.as_str())
    .bind(commit_session.as_str())
    .bind(vec![0_u8; 32])
    .bind(&reservation.reservation_id)
    .bind(i64::try_from(now + 26).unwrap())
    .execute(&audit)
    .await
    .unwrap();
    let question = UserQuestion {
        id: "event-feed-question".to_owned(),
        question: "Does the pending row wake live readers?".to_owned(),
        detail: None,
        header: None,
        options: Vec::new(),
        multi_select: false,
        presentation: None,
        tool_approval: None,
    };
    sqlx::query(
        "INSERT INTO cloud_session_questions
            (tenant_id, user_id, session_id, run_id, question_id, question, created_at_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant_a.tenant_id.as_str())
    .bind(alice.user_id.as_str())
    .bind(commit_session.as_str())
    .bind(question_run_id.as_str())
    .bind(&question.id)
    .bind(serde_json::to_string(&question).unwrap())
    .bind(i64::try_from(now + 27).unwrap())
    .execute(&audit)
    .await
    .unwrap();
    for phase in ["pending", "answered"] {
        assert!(
            receive_live(&mut live_a, Duration::from_secs(1), |notification| {
                matches!(
                    notification,
                    CloudLiveNotification::Session {
                        tenant_id,
                        user_id: Some(user_id),
                        session_id: Some(session_id),
                        dirty,
                        ..
                    } if tenant_id == &tenant_a.tenant_id
                        && user_id == &alice.user_id
                        && session_id == &commit_session
                        && dirty.questions
                        && !dirty.events
                )
            })
            .await
            .is_some(),
            "{phase} question change did not wake live metadata",
        );
        if phase == "pending" {
            cloud
                .answer_question(
                    &tenant_a.tenant_id,
                    &alice.user_id,
                    &commit_session,
                    &UserAnswer {
                        question_id: question.id.clone(),
                        selected: Vec::new(),
                        custom: Some("yes".to_owned()),
                    },
                    now + 28,
                )
                .await
                .unwrap();
        }
    }

    cloud
        .pause_session_inbox(
            &tenant_a.tenant_id,
            &alice.user_id,
            &commit_session,
            None,
            now + 30,
        )
        .await
        .unwrap();
    for receiver in [&mut live_a, &mut live_b] {
        let notification = receive_live(receiver, Duration::from_secs(1), |notification| {
            matches!(
                notification,
                CloudLiveNotification::Session {
                    tenant_id,
                    user_id: Some(user_id),
                    session_id: Some(session_id),
                    dirty,
                    ..
                } if tenant_id == &tenant_a.tenant_id
                    && user_id == &alice.user_id
                    && session_id == &commit_session
                    && dirty.inbox
            )
        })
        .await;
        assert!(notification.is_some());
    }

    cloud
        .create_agent_team_task(
            &tenant_a.tenant_id,
            &alice.user_id,
            &commit_session,
            AgentTeamTaskCreate {
                subject: "Cross-connection task".to_owned(),
                description: String::new(),
                status: AgentTeamTaskStatus::Pending,
                dependencies: Vec::new(),
                owner: None,
            },
            now + 31,
        )
        .await
        .unwrap();
    assert!(
        receive_live(&mut live_a, Duration::from_secs(1), |notification| {
            matches!(
                notification,
                CloudLiveNotification::Session {
                    tenant_id,
                    user_id: Some(user_id),
                    dirty,
                    ..
                } if tenant_id == &tenant_a.tenant_id
                    && user_id == &alice.user_id
                    && dirty.agent_team
            )
        })
        .await
        .is_some()
    );

    // Generic notifications retain tenant/user scope, so another tenant can
    // wake the shared feed without matching Alice's live subscription.
    cloud
        .pause_session_inbox(
            &tenant_b.tenant_id,
            &bob.user_id,
            &other_tenant_session,
            None,
            now + 32,
        )
        .await
        .unwrap();
    assert!(
        receive_live(&mut live_a, Duration::from_secs(1), |notification| {
            matches!(
                notification,
                CloudLiveNotification::Session { tenant_id, user_id: Some(user_id), .. }
                    if tenant_id == &tenant_b.tenant_id && user_id == &bob.user_id
            )
        })
        .await
        .is_some()
    );

    // PostgreSQL NOTIFY is transactional: a rolled-back inbox update emits no
    // durable invalidation.
    while live_a.try_recv().is_ok() {}
    let mut transaction = audit.begin().await.unwrap();
    sqlx::query(
        "UPDATE cloud_session_inboxes SET updated_at_ms = updated_at_ms + 1
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3",
    )
    .bind(tenant_a.tenant_id.as_str())
    .bind(alice.user_id.as_str())
    .bind(commit_session.as_str())
    .execute(&mut *transaction)
    .await
    .unwrap();
    transaction.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), live_a.recv())
            .await
            .is_err()
    );
}

async fn receive_live(
    receiver: &mut tokio::sync::broadcast::Receiver<CloudLiveNotification>,
    wait: Duration,
    predicate: impl Fn(&CloudLiveNotification) -> bool,
) -> Option<CloudLiveNotification> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notification = tokio::time::timeout_at(deadline, receiver.recv())
            .await
            .ok()?
            .ok()?;
        if predicate(&notification) {
            return Some(notification);
        }
    }
}

async fn create_session(
    cloud: &CloudStore,
    tenant_id: &ternilo_protocol::TenantId,
    user_id: &ternilo_protocol::UserId,
    project_id: &str,
    workspace_id: &ternilo_protocol::WorkspaceId,
    session_id: &SessionId,
    now_ms: u64,
) {
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project_id.to_owned(),
                workspace_id: workspace_id.clone(),
                session_id: Some(session_id.clone()),
                agent_id: AgentId::new("agent"),
                title: "Event feed test".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            tenant_id,
            user_id,
            now_ms,
        )
        .await
        .unwrap();
}
