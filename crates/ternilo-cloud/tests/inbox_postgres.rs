use std::{collections::BTreeSet, time::Duration};

use sqlx::Executor;
use ternilo_cloud::{
    CloudCommandDelivery, CloudRunDraft, CloudRunState, CloudSessionCommandDraft,
    CloudSessionDraft, CloudSessionUpdate, CloudStore, CloudSubmissionReceipt, CloudWorkerIdentity,
    RunLease, StartedRun, TerminalState, WorkerPolicy,
};
use ternilo_control::{
    ControlStore, ControlUser, OidcPrincipal, SecretCipher, TenantQuota, TenantRole,
};
use ternilo_kernel::Catalog;
use ternilo_protocol::{
    AgentId, PermissionPreset, Profile, QueueEditRequest, RunId, RunLimits, SessionEvent,
    SessionEventKind, SessionId, SessionSubmissionRequest, SubagentId, SubagentSessionMetadata,
    SubagentTranscriptKind, SubmissionContent, SubmissionDelivery, SubmissionPlacement, TenantId,
    UserId, UserMessageSource, WorkspaceId,
};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability,
    ExecutorCommand, ExecutorCommandBody, ExecutorHello, ExecutorId, ExecutorKind, ExecutorScope,
};

mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
#[allow(clippy::too_many_lines)]
async fn postgres_session_inbox_is_owner_scoped_fifo_and_worker_authoritative() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL")
        .expect("TERNILO_CLOUD_TEST_DATABASE_URL must be set for the ignored PostgreSQL test");
    assert!(
        admin_url.contains("ternilo_cloud_test"),
        "integration test refuses a database URL without ternilo_cloud_test",
    );
    reset_database(&admin_url).await;

    let runtime_url = support::database_url_for_role(
        &admin_url,
        "ternilo_inbox_runtime_test",
        "inbox-runtime-password",
    );
    let control = ControlStore::connect(
        &runtime_url,
        Some(&admin_url),
        SecretCipher::from_key([7; 32]),
        4,
    )
    .await
    .unwrap();
    let cloud = CloudStore::connect(&runtime_url, Some(&admin_url), 4)
        .await
        .unwrap();
    let worker = CloudStore::connect_without_migrations(&runtime_url, 2)
        .await
        .unwrap();
    let audit_database = ternilo_storage::Database::connect(&admin_url, 2)
        .await
        .unwrap();
    let audit = audit_database.pool().clone();

    Box::pin(inbox_contract(control, cloud, worker, audit, &runtime_url)).await;
}

#[tokio::test]
async fn sqlite_session_inbox_enforces_the_same_contract() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("cloud.sqlite").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([7; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let worker = cloud.clone();
    let audit = cloud.database().pool().clone();
    Box::pin(inbox_contract(control, cloud, worker, audit, &url)).await;
}

#[tokio::test]
async fn sqlite_batches_preserve_individual_inputs_and_settle_each_accepted_submission() {
    let directory = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        directory.path().join("batch.sqlite").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([7; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    worker_storage::bind_workers(&cloud, &["worker-batch"], "batch-storage").await;
    let now = 2_000_000_000_000;
    let owner = user(&control, "batch-owner", "Batch owner", now).await;
    let tenant = control
        .create_tenant(
            &owner,
            "batch",
            "Batch",
            TenantQuota {
                max_nodes: 2,
                max_concurrent_runs: 8,
                monthly_model_tokens: 100_000,
                max_secrets: 4,
            },
            now + 1,
        )
        .await
        .unwrap()
        .tenant_id;
    let project = control
        .create_project(&owner, &tenant, "Batch", now + 2)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(&owner, &tenant, &project.project_id, "Batch", now + 3)
        .await
        .unwrap();
    let session = SessionId::new("batch-session");
    let catalog = Catalog::new("inbox-test-catalog");
    let policy = policy();
    let mut receipts = Vec::new();
    for (offset, name) in ["A", "B", "C"].iter().enumerate() {
        let run_id = format!("batch-{name}");
        let compiled = compile(
            &policy,
            &catalog,
            &tenant,
            &owner.user_id,
            &project.project_id,
            &workspace.workspace_id,
            &session,
            &run_id,
            name,
        );
        let reservation =
            reserve(&control, &owner, &tenant, &run_id, now + 10 + offset as u64).await;
        receipts.push(
            cloud
                .enqueue_session_submission(
                    &compiled,
                    &reservation,
                    &request(&run_id, name, SubmissionDelivery::Queue),
                    now + 14 + offset as u64,
                )
                .await
                .unwrap(),
        );
    }
    let reopened =
        assert_batch_migration_and_resume(&control, &tenant, &owner, &session, &receipts, now)
            .await;
    let started = assert_batch_input_acceptance(&reopened, &receipts, now).await;
    assert_batch_settlement(
        &reopened, &tenant, &owner, &session, &receipts, &started, now,
    )
    .await;
}

async fn assert_batch_migration_and_resume(
    control: &ControlStore,
    tenant: &TenantId,
    owner: &ControlUser,
    session: &SessionId,
    receipts: &[CloudSubmissionReceipt],
    now: u64,
) -> CloudStore {
    sqlx::raw_sql(
        "DROP INDEX cloud_session_submissions_batch;
        ALTER TABLE cloud_session_submissions DROP COLUMN batch_run_id;
        DELETE FROM ternilo_schema WHERE component='cloud_queue_batches';",
    )
    .execute(control.database().pool())
    .await
    .unwrap();
    let cloud = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    assert_eq!(
        cloud
            .session_inbox(tenant, &owner.user_id, session)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    cloud
        .pause_session_inbox(tenant, &owner.user_id, session, None, now + 20)
        .await
        .unwrap();
    cloud
        .cancel_run_as(
            tenant,
            &owner.user_id,
            session,
            &receipts[0].submission.run_id,
            now + 21,
        )
        .await
        .unwrap();
    cloud
        .resume_session_inbox(tenant, &owner.user_id, session, now + 22)
        .await
        .unwrap();
    let reopened = CloudStore::from_database(control.database().clone())
        .await
        .unwrap();
    let inbox = reopened
        .session_inbox(tenant, &owner.user_id, session)
        .await
        .unwrap();
    assert_eq!(
        inbox.active_run_id,
        Some(receipts[1].submission.run_id.clone())
    );
    assert_eq!(
        inbox
            .items
            .iter()
            .map(|item| item.content.input())
            .collect::<Vec<_>>(),
        ["B", "C"]
    );
    assert!(
        inbox
            .items
            .iter()
            .all(|item| item.placement == SubmissionPlacement::Running)
    );
    reopened
}

async fn assert_batch_input_acceptance(
    reopened: &CloudStore,
    receipts: &[CloudSubmissionReceipt],
    now: u64,
) -> StartedRun {
    let claim = reopened
        .claim_run("worker-batch", Duration::from_secs(30), now + 23)
        .await
        .unwrap()
        .unwrap();
    let started = reopened
        .start_run(claim, "worker-batch", Duration::from_secs(30), now + 24)
        .await
        .unwrap()
        .unwrap();
    let inputs = reopened
        .run_batch_for_worker("worker-batch", &started, now + 25)
        .await
        .unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].input, "C");
    assert_eq!(inputs[0].provenance, receipts[2].submission.provenance);
    let first_seq = started.prior_events.last().map_or(0, |event| event.seq + 1);
    for (offset, receipt) in receipts[1..].iter().enumerate() {
        let item = &receipt.submission;
        let event = SessionEvent {
            seq: first_seq + offset as u64,
            occurred_at_ms: now + 26,
            run_id: started.claim.run_id.clone(),
            kind: SessionEventKind::UserMessage {
                content: item.content.input().to_owned(),
                display_content: None,
                provenance: item.provenance.clone(),
                references: Vec::new(),
                attachments: Vec::new(),
                source: Some(UserMessageSource::Submission {
                    submission_id: item.id.clone(),
                    created_at_ms: item.created_at_ms,
                    delivery: SubmissionDelivery::Queue,
                    skill_name: None,
                    regenerate_from: None,
                }),
            },
        };
        if offset == 1 {
            let mut forged = event.clone();
            if let SessionEventKind::UserMessage {
                provenance: Some(provenance),
                ..
            } = &mut forged.kind
            {
                provenance.author = ternilo_protocol::InputAuthor::Local;
            }
            assert!(
                reopened
                    .append_event(&started, "worker-batch", &forged, now + 26)
                    .await
                    .is_err()
            );
            assert_eq!(
                reopened
                    .run_batch_for_worker("worker-batch", &started, now + 26)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        reopened
            .append_event(&started, "worker-batch", &event, now + 26)
            .await
            .unwrap();
    }
    assert!(
        reopened
            .run_batch_for_worker("worker-batch", &started, now + 27)
            .await
            .unwrap()
            .is_empty()
    );
    started
}

async fn assert_batch_settlement(
    reopened: &CloudStore,
    tenant: &TenantId,
    owner: &ControlUser,
    session: &SessionId,
    receipts: &[CloudSubmissionReceipt],
    started: &StartedRun,
    now: u64,
) {
    assert!(
        reopened
            .get_run(tenant, &receipts[2].submission.run_id)
            .await
            .is_err()
    );
    let events = reopened
        .session_events(tenant, session, None, 100)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match &event.kind {
                SessionEventKind::UserMessage { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["B", "C"]
    );
    reopened
        .finish_run(
            started,
            "worker-batch",
            TerminalState::Failed,
            None,
            Some(&ternilo_protocol::HarnessError::execution(
                "fixture complete",
            )),
            now + 28,
        )
        .await
        .unwrap();
    assert!(
        reopened
            .session_inbox(tenant, &owner.user_id, session)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

#[allow(clippy::too_many_lines)]
async fn inbox_contract(
    control: ControlStore,
    cloud: CloudStore,
    worker: CloudStore,
    audit: sqlx::AnyPool,
    runtime_url: &str,
) {
    worker_storage::bind_workers(&cloud, &["worker-a", "worker-b"], "shared-contract-storage")
        .await;
    let now = 2_000_000_000_000_u64;
    let alice = user(&control, "inbox-alice", "Alice", now).await;
    let bob = user(&control, "inbox-bob", "Bob", now).await;
    let quota = TenantQuota {
        max_nodes: 2,
        max_concurrent_runs: 8,
        monthly_model_tokens: 100_000,
        max_secrets: 4,
    };
    let tenant = control
        .create_tenant(&alice, "inbox-a", "Inbox A", quota.clone(), now + 1)
        .await
        .unwrap();
    let other_tenant = control
        .create_tenant(&bob, "inbox-b", "Inbox B", quota, now + 2)
        .await
        .unwrap();
    control
        .set_membership(
            &alice,
            &tenant.tenant_id,
            &bob.user_id,
            TenantRole::Member,
            now + 3,
        )
        .await
        .unwrap();
    let project = control
        .create_project(&alice, &tenant.tenant_id, "Inbox project", now + 4)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(
            &alice,
            &tenant.tenant_id,
            &project.project_id,
            "Inbox workspace",
            now + 5,
        )
        .await
        .unwrap();
    let policy = policy();
    let catalog = Catalog::new("inbox-test-catalog");
    let session_id = SessionId::new("fifo-session");
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                session_id: Some(session_id.clone()),
                agent_id: AgentId::new("agent"),
                title: "Preset locking".to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: None,
                reserved_model_tokens: 100,
                agent_preset: "minimal".to_owned(),
                profile_plugins: Vec::new(),
                mode: ternilo_protocol::SessionMode::Execute,
            },
            &tenant.tenant_id,
            &alice.user_id,
            now + 8,
        )
        .await
        .unwrap();
    let change_preset = || CloudSessionUpdate {
        agent_preset: Some("standard".to_owned()),
        ..CloudSessionUpdate::default()
    };
    assert_eq!(
        cloud
            .update_session(
                &tenant.tenant_id,
                &session_id,
                &alice.user_id,
                change_preset(),
                now + 9
            )
            .await
            .unwrap()
            .agent_preset,
        "standard"
    );

    let first = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-1",
        "same input",
    );
    let first_reservation = reserve(&control, &alice, &tenant.tenant_id, "run-1", now + 10).await;
    let first_request = request("run-1", "same input", SubmissionDelivery::Queue);
    let first_receipt = cloud
        .enqueue_session_submission(&first, &first_reservation, &first_request, now + 11)
        .await
        .unwrap();
    assert_eq!(
        first_receipt.submission.placement,
        SubmissionPlacement::Running
    );
    assert!(
        cloud
            .update_session(
                &tenant.tenant_id,
                &session_id,
                &alice.user_id,
                change_preset(),
                now + 12
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("preset is locked")
    );

    let reservations_before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_quota_reservations WHERE tenant_id=$1")
            .bind(tenant.tenant_id.as_str())
            .fetch_one(&audit)
            .await
            .unwrap();
    let audits_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_audit_log WHERE tenant_id=$1 AND action='quota.reserve'",
    )
    .bind(tenant.tenant_id.as_str())
    .fetch_one(&audit)
    .await
    .unwrap();
    let mut rollback = cloud.database().begin().await.unwrap();
    let reservation = ControlStore::reserve_quota_in(
        &mut rollback,
        &alice.user_id,
        &tenant.tenant_id,
        Some("run-1"),
        first.reserved_model_tokens,
        Duration::from_secs(60),
        now + 11,
    )
    .await
    .unwrap();
    assert!(
        CloudStore::enqueue_session_submission_in(
            &mut rollback,
            &first,
            &reservation.reservation_id,
            &first_request,
            now + 11
        )
        .await
        .is_err(),
        "a duplicate execution must roll back its new reservation"
    );
    rollback.rollback().await.unwrap();
    let reservations_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM control_quota_reservations WHERE tenant_id=$1")
            .bind(tenant.tenant_id.as_str())
            .fetch_one(&audit)
            .await
            .unwrap();
    let audits_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control_audit_log WHERE tenant_id=$1 AND action='quota.reserve'",
    )
    .bind(tenant.tenant_id.as_str())
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(
        (reservations_before, audits_before),
        (reservations_after, audits_after),
        "Control quota/audit and Cloud execution share one rollback boundary"
    );

    let second = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-2",
        "same input",
    );
    let second_reservation = reserve(&control, &alice, &tenant.tenant_id, "run-2", now + 12).await;
    let second_receipt = cloud
        .enqueue_session_submission(
            &second,
            &second_reservation,
            &request("run-2", "same input", SubmissionDelivery::Steer),
            now + 13,
        )
        .await
        .unwrap();
    let third = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-3",
        "same input",
    );
    let third_reservation = reserve(&control, &alice, &tenant.tenant_id, "run-3", now + 14).await;
    let third_receipt = cloud
        .enqueue_session_submission(
            &third,
            &third_reservation,
            &request("run-3", "same input", SubmissionDelivery::Queue),
            now + 15,
        )
        .await
        .unwrap();
    assert_ne!(second_receipt.submission.id, third_receipt.submission.id);
    let snapshot = cloud
        .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .items
            .iter()
            .map(|item| (&item.run_id, item.placement))
            .collect::<Vec<_>>(),
        [
            (&RunId::new("run-1"), SubmissionPlacement::Running),
            (&RunId::new("run-2"), SubmissionPlacement::Queued),
            (&RunId::new("run-3"), SubmissionPlacement::Queued),
        ]
    );
    let reopened = CloudStore::connect_without_migrations(runtime_url, 1)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
            .await
            .unwrap(),
        snapshot,
        "The database remains authoritative across Control process reconnection",
    );
    drop(reopened);
    assert!(
        cloud
            .session_inbox(&tenant.tenant_id, &bob.user_id, &session_id)
            .await
            .is_err(),
        "same-tenant non-owner must not observe the inbox",
    );
    assert!(
        cloud
            .session_inbox(&other_tenant.tenant_id, &bob.user_id, &session_id)
            .await
            .is_err(),
        "cross-tenant lookup must not observe the inbox",
    );
    if cloud.database().backend() == ternilo_storage::Backend::Postgres {
        assert_owner_rls(runtime_url, &tenant.tenant_id, &alice.user_id, &bob.user_id).await;
    }

    assert!(
        cloud
            .remove_queued_session_submission(
                &tenant.tenant_id,
                &alice.user_id,
                &session_id,
                &first_receipt.submission.id,
                now + 16,
            )
            .await
            .is_err(),
        "running occurrence cannot be removed",
    );
    let replacement_first = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-1",
        "cannot edit running",
    );
    assert!(
        cloud
            .edit_queued_session_submission(
                &tenant.tenant_id,
                &alice.user_id,
                &session_id,
                &first_receipt.submission.id,
                QueueEditRequest {
                    input: "cannot edit running".to_owned(),
                    expected_updated_at_ms: first_receipt.submission.updated_at_ms,
                },
                &replacement_first,
                now + 16,
            )
            .await
            .is_err(),
        "running occurrence cannot be edited",
    );
    assert_eq!(
        cloud
            .strict_steering_candidate(
                &tenant.tenant_id,
                &alice.user_id,
                &session_id,
                &second_receipt.submission.id,
            )
            .await
            .unwrap()
            .placement,
        SubmissionPlacement::Queued,
    );
    assert_eq!(
        cloud
            .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
            .await
            .unwrap()
            .items[1]
            .placement,
        SubmissionPlacement::Queued,
        "strict steering without a Worker ACK is read-only",
    );

    let edited_second = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-2",
        "edited second",
    );
    let edited = cloud
        .edit_queued_session_submission(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &second_receipt.submission.id,
            QueueEditRequest {
                input: "edited second".to_owned(),
                expected_updated_at_ms: second_receipt.submission.updated_at_ms,
            },
            &edited_second,
            now + 17,
        )
        .await
        .unwrap();
    assert_eq!(edited.content.input(), "edited second");
    let removed = cloud
        .remove_queued_session_submission(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &third_receipt.submission.id,
            now + 18,
        )
        .await
        .unwrap();
    assert_eq!(removed.run_id, RunId::new("run-3"));
    assert!(
        cloud
            .get_run(&tenant.tenant_id, &RunId::new("run-3"))
            .await
            .is_err(),
    );
    let released_state: String = sqlx::query_scalar(
        "SELECT state FROM control_quota_reservations
         WHERE tenant_id = $1 AND reservation_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(&third_reservation)
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(released_state, "released");

    if cloud.database().backend() == ternilo_storage::Backend::Postgres {
        let mut pausing = cloud
            .database()
            .owner_transaction(&tenant.tenant_id, &alice.user_id)
            .await
            .unwrap();
        sqlx::query("UPDATE cloud_session_inboxes SET paused=1 WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3")
            .bind(tenant.tenant_id.as_str()).bind(alice.user_id.as_str()).bind(session_id.as_str()).execute(&mut *pausing).await.unwrap();
        let claim = tokio::time::timeout(
            Duration::from_secs(2),
            worker.claim_run("worker-a", Duration::from_secs(10), now + 19),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            claim.is_none(),
            "claim skips the inbox row while a pause transaction owns its lock"
        );
        pausing.rollback().await.unwrap();
    }

    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 1 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();
    assert!(
        worker
            .claim_run("worker-a", Duration::from_secs(10), now + 19)
            .await
            .unwrap()
            .is_none(),
        "the backup claim barrier must stop new worker leases while preserving the queue",
    );
    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 0 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();

    let first_claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 19)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.run_id, RunId::new("run-1"));
    sqlx::query(
        "UPDATE cloud_runs SET lease_expires_at_ms = $3
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(first_claim.run_id.as_str())
    .bind(i64::try_from(now + 20).unwrap())
    .execute(&audit)
    .await
    .unwrap();
    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 1 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();
    assert!(
        worker
            .claim_run("worker-b", Duration::from_secs(10), now + 21)
            .await
            .unwrap()
            .is_none(),
        "the backup claim barrier must also stop reclaiming an expired lease",
    );
    sqlx::query("UPDATE cloud_runtime_control SET claims_paused = 0 WHERE singleton = 1")
        .execute(&audit)
        .await
        .unwrap();
    worker
        .release_claim(&first_claim, "worker-a", now + 20)
        .await
        .unwrap();
    let reclaimed = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 21)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.run_id, RunId::new("run-1"));
    assert!(reclaimed.lease_token > first_claim.lease_token);
    let started = worker
        .start_run(reclaimed, "worker-a", Duration::from_secs(10), now + 22)
        .await
        .unwrap()
        .unwrap();
    let worker_submission = worker
        .run_submission_for_worker("worker-a", &started, now + 22)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(worker_submission.id, first_receipt.submission.id);
    assert_eq!(worker_submission.content.input(), "same input");
    assert_eq!(
        worker_submission.provenance,
        first_receipt.submission.provenance
    );
    assert_eq!(
        started.claim.provenance,
        first_receipt.submission.provenance
    );
    let provenance = started.claim.provenance.clone().unwrap();
    assert_eq!(provenance.input_id, first_receipt.submission.id);
    assert_eq!(
        provenance.author,
        ternilo_protocol::InputAuthor::Account {
            user_id: alice.user_id.clone(),
            username: alice.username.clone()
        }
    );
    let mut forged = provenance.clone();
    forged.author = ternilo_protocol::InputAuthor::Account {
        user_id: bob.user_id.clone(),
        username: bob.username.clone(),
    };
    let mut wrong_input = provenance.clone();
    wrong_input.input_id = ternilo_protocol::SubmissionId::new("unaccepted-input");
    for reported in [None, Some(forged), Some(wrong_input)] {
        let error = worker
            .append_event(
                &started,
                "worker-a",
                &SessionEvent {
                    seq: 0,
                    occurred_at_ms: now + 22,
                    run_id: started.claim.run_id.clone(),
                    kind: SessionEventKind::UserMessage {
                        provenance: reported,
                        content: "same input".to_owned(),
                        display_content: None,
                        source: None,
                        references: Vec::new(),
                        attachments: Vec::new(),
                    },
                },
                now + 22,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
        assert!(error.message.contains("accepted cloud input"));
    }
    assert!(
        worker
            .session_events(&tenant.tenant_id, &session_id, None, 100)
            .await
            .unwrap()
            .is_empty(),
        "rejected author claims cannot advance the event cursor"
    );

    assert!(
        worker
            .run_submission_for_worker("worker-b", &started, now + 22)
            .await
            .unwrap()
            .is_none(),
        "a Worker that does not own the active run cannot read its submission",
    );
    let child_session_id = SessionId::new("child-worker-host");
    let child_metadata = SubagentSessionMetadata {
        subagent_id: SubagentId::new("researcher"),
        provider: "in-process".to_owned(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    assert!(
        worker
            .create_subagent_for_worker(
                "worker-b",
                &started,
                &child_session_id,
                &child_metadata,
                "Researcher",
                now + 22,
            )
            .await
            .is_err(),
        "another Worker cannot create a child from a run it does not own",
    );
    assert_eq!(
        worker
            .create_subagent_for_worker(
                "worker-a",
                &started,
                &child_session_id,
                &child_metadata,
                "Researcher",
                now + 22,
            )
            .await
            .unwrap(),
        child_session_id,
    );
    let mapped = cloud
        .cloud_subagent(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &child_metadata.subagent_id,
        )
        .await
        .unwrap();
    assert_eq!(mapped.child.session_id, child_session_id);
    let mut child_spec = started.claim.spec.clone();
    child_spec.metadata.session_id = child_session_id.clone();
    child_spec.metadata.run_id = RunId::new("child-worker-run");
    "delegated task".clone_into(&mut child_spec.input);
    child_spec.references.clear();
    child_spec.reference_contexts.clear();
    child_spec.attachments.clear();
    let child_run_id = worker
        .enqueue_subagent_for_worker(
            "worker-a",
            &started,
            &child_session_id,
            &child_spec,
            "delegated task",
            1,
            now + 23,
        )
        .await
        .unwrap();
    assert_eq!(child_run_id, RunId::new("child-worker-run"));
    let child_claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 23)
        .await
        .unwrap()
        .expect("a durable canonical child must be immediately claimable");
    assert_eq!(child_claim.run_id, child_run_id);
    assert!(matches!(
        child_claim.provenance.as_ref().map(|value| &value.author),
        Some(ternilo_protocol::InputAuthor::Automation {
            source: ternilo_protocol::AutomatedInputSource::Subagent
        })
    ));
    assert_eq!(
        child_claim.actor_user_id, started.claim.actor_user_id,
        "automated child attribution does not replace parent authorization"
    );

    worker
        .release_claim(&child_claim, "worker-a", now + 23)
        .await
        .unwrap();
    let team_before_worker_restart = cloud
        .agent_team_snapshot(&tenant.tenant_id, &alice.user_id, &session_id)
        .await
        .unwrap();
    assert_eq!(team_before_worker_restart.members.len(), 2);
    assert_eq!(
        cloud
            .active_subagent_run(&tenant.tenant_id, &alice.user_id, &child_session_id,)
            .await
            .unwrap()
            .unwrap()
            .run_id,
        child_run_id,
    );
    drop(worker);
    let worker = CloudStore::connect_without_migrations(runtime_url, 2)
        .await
        .unwrap();
    let mapped_after_worker_restart = cloud
        .cloud_subagent(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &child_metadata.subagent_id,
        )
        .await
        .unwrap();
    assert_eq!(
        mapped_after_worker_restart.child.session_id,
        child_session_id
    );
    assert_eq!(
        cloud
            .agent_team_snapshot(&tenant.tenant_id, &alice.user_id, &child_session_id)
            .await
            .unwrap()
            .team_id,
        team_before_worker_restart.team_id,
        "canonical child and Team identity must survive a Worker reconnection",
    );
    assert_eq!(
        worker
            .subagent_run_for_worker(
                "worker-a",
                &started,
                &child_session_id,
                &child_run_id,
                now + 23,
            )
            .await
            .unwrap()
            .unwrap()
            .state,
        ternilo_cloud::CloudRunState::Queued,
    );
    assert_eq!(
        worker
            .cancel_subagent_for_worker(
                "worker-a",
                &started,
                &child_session_id,
                &child_run_id,
                now + 24,
            )
            .await
            .unwrap(),
        ternilo_cloud::CloudRunState::Cancelled,
    );

    let followup = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &child_session_id,
        "child-web-followup",
        "continue from the Web Team panel",
    );
    let followup_reservation = reserve(
        &control,
        &alice,
        &tenant.tenant_id,
        "child-web-followup",
        now + 25,
    )
    .await;
    cloud
        .enqueue_session_submission(
            &followup,
            &followup_reservation,
            &request(
                "child-web-followup",
                "continue from the Web Team panel",
                SubmissionDelivery::Queue,
            ),
            now + 25,
        )
        .await
        .unwrap();
    let active_followup = cloud
        .active_subagent_run(&tenant.tenant_id, &alice.user_id, &child_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active_followup.run_id, RunId::new("child-web-followup"));
    assert_eq!(
        cloud
            .cancel_run(&tenant.tenant_id, &active_followup.run_id, now + 26)
            .await
            .unwrap(),
        ternilo_cloud::CloudRunState::Cancelled,
        "the addressed Web Stop path must cancel the canonical child run",
    );
    assert!(
        cloud
            .active_subagent_run(&tenant.tenant_id, &alice.user_id, &child_session_id,)
            .await
            .unwrap()
            .is_none(),
        "the addressed Web follow-up/Stop state must be durable rather than Worker memory",
    );
    let live_steer = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-steer",
        "live steer",
    );
    let live_steer_reservation =
        reserve(&control, &alice, &tenant.tenant_id, "run-steer", now + 22).await;
    let live_steer_receipt = cloud
        .enqueue_session_submission(
            &live_steer,
            &live_steer_reservation,
            &request("run-steer", "live steer", SubmissionDelivery::Steer),
            now + 22,
        )
        .await
        .unwrap();
    let fifo_before: i64 = sqlx::query_scalar(
        "SELECT fifo_position FROM cloud_session_submissions
         WHERE tenant_id = $1 AND session_id = $2 AND submission_id = $3",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(session_id.as_str())
    .bind(live_steer_receipt.submission.id.as_str())
    .fetch_one(&audit)
    .await
    .unwrap();
    let steering_capabilities = BTreeSet::from([
        ExecutorCapability::CloudRun,
        ExecutorCapability::AddressedSessionCommands,
        ExecutorCapability::SessionSteering,
    ]);
    let worker_b_hello = ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new("worker-b"),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: "storage-contract-worker-b".to_owned(),
        catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
        capabilities: steering_capabilities.clone(),
    };
    let worker_b = worker
        .register_cloud_worker(&worker_b_hello, Duration::from_secs(60), now + 22)
        .await
        .unwrap();
    let primary_worker_hello = ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new("worker-a"),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: "storage-contract-worker-a".to_owned(),
        catalog_revision: ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned(),
        capabilities: steering_capabilities.clone(),
    };
    let worker_a = worker
        .register_cloud_worker(&primary_worker_hello, Duration::from_secs(60), now + 22)
        .await
        .unwrap();
    let restart_probe = CloudSessionCommandDraft {
        session_id: session_id.clone(),
        command: ExecutorCommand {
            input_provenance: None,
            command_id: CommandId::new("restart-inspection"),
            scope: ExecutorScope {
                tenant_id: tenant.tenant_id.clone(),
                user_id: alice.user_id.clone(),
            },
            issued_at_ms: now + 22,
            expires_at_ms: now + 120_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionCommands {
                    session_id: session_id.clone(),
                },
            },
        },
        required_capability: ExecutorCapability::AddressedSessionCommands,
        required_catalog_revision: Some(ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned()),
        delivery: CloudCommandDelivery::ReadOnly,
    };
    cloud
        .enqueue_session_command(&tenant.tenant_id, &alice.user_id, &restart_probe, now + 22)
        .await
        .unwrap();
    assert!(
        worker
            .claim_session_commands(
                &worker_b,
                &steering_capabilities,
                Duration::from_secs(5),
                1,
                now + 22,
            )
            .await
            .unwrap()
            .is_empty(),
        "a replacement Worker must not inspect while another Worker still owns a live run",
    );
    sqlx::query(
        "UPDATE cloud_runs SET lease_expires_at_ms = $3
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(started.claim.run_id.as_str())
    .bind(i64::try_from(now + 21).unwrap())
    .execute(&audit)
    .await
    .unwrap();
    let restart_claim = worker
        .claim_session_commands(
            &worker_b,
            &steering_capabilities,
            Duration::from_secs(5),
            1,
            now + 22,
        )
        .await
        .unwrap()
        .pop()
        .expect("replacement Worker claims inspection immediately after the old run lease expires");
    worker
        .complete_session_command(
            &worker_b,
            &restart_claim,
            &CommandReply::success(
                restart_claim.command.command_id.clone(),
                now + 22,
                serde_json::json!({ "commands": [] }),
            ),
            now + 22,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE cloud_runs SET lease_expires_at_ms = $3
         WHERE tenant_id = $1 AND run_id = $2",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(started.claim.run_id.as_str())
    .bind(i64::try_from(now + 32).unwrap())
    .execute(&audit)
    .await
    .unwrap();
    let false_ticket = cloud
        .begin_session_steering(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &live_steer_receipt.submission.id,
            now + 22,
        )
        .await
        .unwrap();
    assert!(false_ticket.command_id.is_some());
    assert!(
        worker
            .claim_session_commands(
                &worker_b,
                &steering_capabilities,
                Duration::from_secs(5),
                1,
                now + 22,
            )
            .await
            .unwrap()
            .is_empty(),
        "a Worker that does not own the active run cannot claim steering",
    );
    let false_claim = worker
        .claim_session_commands(
            &worker_a,
            &steering_capabilities,
            Duration::from_secs(5),
            1,
            now + 22,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        worker
            .complete_steering_command(&worker_b, &false_claim, false, now + 22)
            .await
            .is_err(),
        "the wrong Worker generation cannot ACK steering",
    );
    assert_eq!(
        worker
            .steering_submission_for_worker(&worker_a, &false_claim, now + 22)
            .await
            .unwrap()
            .unwrap()
            .content
            .input(),
        "live steer",
    );
    worker
        .complete_steering_command(&worker_a, &false_claim, false, now + 22)
        .await
        .unwrap();
    let fifo_after_false: i64 = sqlx::query_scalar(
        "SELECT fifo_position FROM cloud_session_submissions
         WHERE tenant_id = $1 AND session_id = $2 AND submission_id = $3",
    )
    .bind(tenant.tenant_id.as_str())
    .bind(session_id.as_str())
    .bind(live_steer_receipt.submission.id.as_str())
    .fetch_one(&audit)
    .await
    .unwrap();
    assert_eq!(fifo_after_false, fifo_before);
    let accepted_ticket = cloud
        .begin_session_steering(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            &live_steer_receipt.submission.id,
            now + 22,
        )
        .await
        .unwrap();
    let accepted_claim = worker
        .claim_session_commands(
            &worker_a,
            &steering_capabilities,
            Duration::from_secs(5),
            1,
            now + 22,
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        Some(accepted_claim.command.command_id.clone()),
        accepted_ticket.command_id,
    );
    worker
        .complete_steering_command(&worker_a, &accepted_claim, true, now + 22)
        .await
        .unwrap();
    assert!(
        cloud
            .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
            .await
            .unwrap()
            .items
            .iter()
            .any(|item| item.id == live_steer_receipt.submission.id
                && item.placement == SubmissionPlacement::Steering),
    );
    worker
        .append_event(
            &started,
            "worker-a",
            &SessionEvent {
                seq: 0,
                occurred_at_ms: now + 22,
                run_id: started.claim.run_id.clone(),
                kind: SessionEventKind::UserMessage {
                    provenance: live_steer_receipt.submission.provenance.clone(),
                    content: "live steer".to_owned(),
                    display_content: None,
                    source: Some(UserMessageSource::Submission {
                        regenerate_from: None,
                        submission_id: live_steer_receipt.submission.id.clone(),
                        created_at_ms: live_steer_receipt.submission.created_at_ms,
                        delivery: SubmissionDelivery::Steer,
                        skill_name: None,
                    }),
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            },
            now + 22,
        )
        .await
        .unwrap();
    assert_eq!(
        worker.model_budget(&started, "worker-a").await.unwrap(),
        (200, 0)
    );
    assert!(
        cloud
            .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
            .await
            .unwrap()
            .items
            .iter()
            .all(|item| item.id != live_steer_receipt.submission.id),
        "the canonical steer event consumes the candidate run occurrence",
    );
    let paused = cloud
        .pause_session_inbox(
            &tenant.tenant_id,
            &alice.user_id,
            &session_id,
            Some("cancelled by owner"),
            now + 23,
        )
        .await
        .unwrap();
    assert!(paused.paused);
    cloud
        .cancel_run(&tenant.tenant_id, &RunId::new("run-1"), now + 24)
        .await
        .unwrap();
    worker
        .finish_run(
            &started,
            "worker-a",
            TerminalState::Cancelled,
            None,
            None,
            now + 25,
        )
        .await
        .unwrap();
    worker
        .release_resident(
            &RunLease::from(&started),
            &CloudWorkerIdentity {
                worker_id: ExecutorId::new("worker-a"),
                instance_nonce: "storage-contract-worker-a".to_owned(),
                generation: 1,
            },
            1,
            now + 25,
        )
        .await
        .unwrap();
    let paused_tail = cloud
        .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
        .await
        .unwrap();
    assert!(paused_tail.paused);
    assert_eq!(paused_tail.active_run_id, None);
    assert_eq!(paused_tail.items.len(), 1);
    assert_eq!(paused_tail.items[0].run_id, RunId::new("run-2"));
    assert_eq!(paused_tail.items[0].placement, SubmissionPlacement::Queued);
    assert!(
        worker
            .claim_run("worker-b", Duration::from_secs(10), now + 26)
            .await
            .unwrap()
            .is_none(),
        "paused inbox is not dispatchable",
    );

    let wake = compile(
        &policy,
        &catalog,
        &tenant.tenant_id,
        &alice.user_id,
        &project.project_id,
        &workspace.workspace_id,
        &session_id,
        "run-4",
        "wake queue",
    );
    let wake_reservation = reserve(&control, &alice, &tenant.tenant_id, "run-4", now + 27).await;
    let wake_receipt = cloud
        .enqueue_session_submission(
            &wake,
            &wake_reservation,
            &request("run-4", "wake queue", SubmissionDelivery::Queue),
            now + 28,
        )
        .await
        .unwrap();
    assert_eq!(
        wake_receipt.submission.placement,
        SubmissionPlacement::Queued
    );
    let awakened = cloud
        .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
        .await
        .unwrap();
    assert!(!awakened.paused);
    assert_eq!(awakened.error, None);
    assert_eq!(
        awakened
            .items
            .iter()
            .map(|item| (&item.run_id, item.placement))
            .collect::<Vec<_>>(),
        [
            (&RunId::new("run-2"), SubmissionPlacement::Running),
            (&RunId::new("run-4"), SubmissionPlacement::Running),
        ],
        "wake submission joins the preserved batch without overtaking its head",
    );
    assert!(
        cloud
            .strict_steering_candidate(
                &tenant.tenant_id,
                &alice.user_id,
                &session_id,
                &wake_receipt.submission.id
            )
            .await
            .is_err()
    );

    let second_claim = worker
        .claim_run("worker-a", Duration::from_secs(10), now + 29)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second_claim.run_id, RunId::new("run-2"));
    assert_eq!(second_claim.spec.input, "edited second");
    let second_started = worker
        .start_run(second_claim, "worker-a", Duration::from_secs(10), now + 30)
        .await
        .unwrap()
        .unwrap();
    worker
        .finish_run(
            &second_started,
            "worker-a",
            TerminalState::Cancelled,
            None,
            None,
            now + 31,
        )
        .await
        .unwrap();
    worker
        .release_resident(
            &RunLease::from(&second_started),
            &CloudWorkerIdentity {
                worker_id: ExecutorId::new("worker-a"),
                instance_nonce: "storage-contract-worker-a".to_owned(),
                generation: 1,
            },
            1,
            now + 31,
        )
        .await
        .unwrap();
    let promoted = cloud
        .session_inbox(&tenant.tenant_id, &alice.user_id, &session_id)
        .await
        .unwrap();
    assert_eq!(promoted.items.len(), 1);
    assert_eq!(promoted.items[0].run_id, RunId::new("run-4"));
    assert_eq!(promoted.items[0].placement, SubmissionPlacement::Running);
    let final_claim = worker
        .claim_run("worker-b", Duration::from_secs(10), now + 32)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(final_claim.run_id, RunId::new("run-4"));
    let final_started = worker
        .start_run(final_claim, "worker-b", Duration::from_secs(10), now + 32)
        .await
        .unwrap()
        .unwrap();
    let shutdown_probe = CloudSessionCommandDraft {
        session_id: session_id.clone(),
        command: ExecutorCommand {
            input_provenance: None,
            command_id: CommandId::new("graceful-shutdown-inspection"),
            scope: ExecutorScope {
                tenant_id: tenant.tenant_id.clone(),
                user_id: alice.user_id.clone(),
            },
            issued_at_ms: now + 33,
            expires_at_ms: now + 120_000,
            body: ExecutorCommandBody::Application {
                request: ApplicationOperation::SessionCommands {
                    session_id: session_id.clone(),
                },
            },
        },
        required_capability: ExecutorCapability::AddressedSessionCommands,
        required_catalog_revision: Some(ternilo_cloud::CLOUD_CATALOG_REVISION.to_owned()),
        delivery: CloudCommandDelivery::ReadOnly,
    };
    cloud
        .enqueue_session_command(&tenant.tenant_id, &alice.user_id, &shutdown_probe, now + 33)
        .await
        .unwrap();
    assert!(
        worker
            .claim_session_commands(
                &worker_a,
                &steering_capabilities,
                Duration::from_secs(5),
                1,
                now + 33,
            )
            .await
            .unwrap()
            .is_empty(),
        "another Worker cannot inspect while the run owner is live",
    );
    assert_eq!(
        worker
            .drain_cloud_worker(&worker_b, now + 33)
            .await
            .unwrap(),
        1,
    );
    assert_eq!(
        cloud
            .get_run(&tenant.tenant_id, &final_started.claim.run_id)
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate,
    );
    let shutdown_claim = worker
        .claim_session_commands(
            &worker_a,
            &steering_capabilities,
            Duration::from_secs(5),
            1,
            now + 33,
        )
        .await
        .unwrap()
        .pop()
        .expect("graceful drain makes inspection immediately claimable by another Worker");
    worker
        .complete_session_command(
            &worker_a,
            &shutdown_claim,
            &CommandReply::success(
                shutdown_claim.command.command_id.clone(),
                now + 33,
                serde_json::json!({ "commands": [] }),
            ),
            now + 33,
        )
        .await
        .unwrap();
}

async fn reset_database(admin_url: &str) {
    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute("DROP SCHEMA IF EXISTS public CASCADE")
        .await
        .unwrap();
    admin.execute("CREATE SCHEMA public").await.unwrap();
    admin
        .execute("DROP ROLE IF EXISTS ternilo_inbox_runtime_test")
        .await
        .unwrap();
    drop(admin);

    let control = ControlStore::connect(admin_url, None, SecretCipher::from_key([7; 32]), 2)
        .await
        .unwrap();
    drop(control);
    let cloud = CloudStore::connect(admin_url, None, 2).await.unwrap();
    drop(cloud);

    let admin = sqlx::PgPool::connect(admin_url).await.unwrap();
    admin
        .execute("CREATE ROLE ternilo_inbox_runtime_test LOGIN PASSWORD 'inbox-runtime-password'")
        .await
        .unwrap();
    admin
        .execute("GRANT USAGE ON SCHEMA public TO ternilo_inbox_runtime_test")
        .await
        .unwrap();
    admin
        .execute(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public
             TO ternilo_inbox_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute(
            "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public
             TO ternilo_inbox_runtime_test",
        )
        .await
        .unwrap();
    admin
        .execute("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO ternilo_inbox_runtime_test")
        .await
        .unwrap();
    admin
        .execute("REVOKE ALL ON cloud_runtime_control FROM ternilo_inbox_runtime_test")
        .await
        .unwrap();
    admin.execute("REVOKE INSERT,UPDATE,DELETE ON ternilo_schema,cloud_live_changes FROM ternilo_inbox_runtime_test").await.unwrap();
}

async fn user(store: &ControlStore, subject: &str, display: &str, now: u64) -> ControlUser {
    store
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://id.example.com".to_owned(),
                subject: subject.to_owned(),
                email: None,
                display_name: Some(display.to_owned()),
            },
            &format!("test-{subject}"),
            now,
        )
        .await
        .unwrap()
}

fn policy() -> WorkerPolicy {
    WorkerPolicy {
        catalog_revision: "inbox-test-catalog".to_owned(),
        policy_revision: "inbox-test-policy".to_owned(),
        maximum_limits: RunLimits {
            max_steps: 4,
            max_tool_calls: 8,
        },
        max_run_attempts: 2,
        max_tenant_workspace_bytes: 1024 * 1024 * 1024,
        max_tenant_workspace_entries: 100_000,
        minimum_workspace_free_bytes: 0,
        allowed_plugin_kinds: BTreeSet::default(),
        max_extension_packages_per_run: 0,
        extension_host_policy: ternilo_extension::ExtensionHostPolicy::default(),
        denied_tools: BTreeSet::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn compile(
    policy: &WorkerPolicy,
    catalog: &Catalog,
    tenant_id: &TenantId,
    user_id: &UserId,
    project_id: &str,
    workspace_id: &WorkspaceId,
    session_id: &SessionId,
    run_id: &str,
    input: &str,
) -> ternilo_cloud::CompiledRun {
    policy
        .compile_run(
            CloudRunDraft {
                project_id: project_id.to_owned(),
                workspace_id: workspace_id.clone(),
                agent_id: AgentId::new("agent"),
                session_id: session_id.clone(),
                run_id: Some(RunId::new(run_id)),
                limits: RunLimits {
                    max_steps: 2,
                    max_tool_calls: 4,
                },
                permissions: PermissionPreset::WorkspaceWrite,
                mode: ternilo_protocol::SessionMode::Execute,
                profile: Profile::default(),
                input: input.to_owned(),
                references: Vec::new(),
                reference_contexts: Vec::new(),
                attachments: Vec::new(),
                reserved_model_tokens: 100,
            },
            tenant_id.clone(),
            user_id.clone(),
            user_id.clone(),
            catalog,
        )
        .unwrap()
}

async fn reserve(
    control: &ControlStore,
    user: &ControlUser,
    tenant_id: &TenantId,
    run_id: &str,
    now: u64,
) -> String {
    control
        .reserve_quota(
            user,
            tenant_id,
            Some(run_id),
            100,
            Duration::from_hours(1),
            now,
        )
        .await
        .unwrap()
        .reservation_id
}

fn request(run_id: &str, input: &str, delivery: SubmissionDelivery) -> SessionSubmissionRequest {
    SessionSubmissionRequest {
        delivery,
        run_id: Some(RunId::new(run_id)),
        content: SubmissionContent::Prompt {
            input: input.to_owned(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
    }
}

async fn assert_owner_rls(
    runtime_url: &str,
    tenant_id: &TenantId,
    alice_id: &UserId,
    bob_id: &UserId,
) {
    let pool = sqlx::PgPool::connect(runtime_url).await.unwrap();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT set_config('ternilo.tenant_id', $1, true),
                set_config('ternilo.user_id', $2, true)",
    )
    .bind(tenant_id.as_str())
    .bind(bob_id.as_str())
    .execute(&mut *transaction)
    .await
    .unwrap();
    let bob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_session_submissions")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(bob_count, 0);
    transaction.rollback().await.unwrap();

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT set_config('ternilo.tenant_id', $1, true),
                set_config('ternilo.user_id', $2, true)",
    )
    .bind(tenant_id.as_str())
    .bind(alice_id.as_str())
    .execute(&mut *transaction)
    .await
    .unwrap();
    let alice_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cloud_session_submissions")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(alice_count, 3);
    transaction.rollback().await.unwrap();
}
