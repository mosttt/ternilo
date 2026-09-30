use std::{
    collections::BTreeSet,
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest as _, Sha256};
use ternilo_cloud::{
    CloudCommandDelivery, CloudSessionCommandDraft, CloudSessionDraft, CloudSessionRecord,
    CloudStore, CompiledRun, RunLease, WorkerRegisterRequest,
};
use ternilo_control::{ControlStore, ControlUser, OidcPrincipal, SecretCipher, TenantQuota};
use ternilo_protocol::{
    AgentId, Attachment, ErrorCode, ModelUsage, PermissionPreset, ReferenceCandidateRequest, RunId,
    RunLimits, RunMetadata, RunSpec, SessionEvent, SessionEventKind, SessionId, SessionMode,
    TenantId,
};
use ternilo_transport::{
    ApplicationOperation, CommandId, CommandReply, EXECUTOR_PROTOCOL_VERSION, ExecutorCapability,
    ExecutorCommand, ExecutorCommandBody, ExecutorHello, ExecutorId, ExecutorKind, ExecutorScope,
};

#[path = "support/model_ledger.rs"]
mod model_ledger;
#[path = "support/server_runtime.rs"]
mod server_runtime;
mod support;
#[path = "support/worker_storage.rs"]
mod worker_storage;

#[tokio::test]
async fn sqlite_keeps_tenant_workspaces_and_readonly_commands_on_their_storage() {
    let temporary = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        temporary.path().join("storage.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([57; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    storage_contract(control, cloud, &url).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_enforces_the_same_storage_contract_with_a_restricted_runtime() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_cloud_test"));
    let url =
        server_runtime::initialize(&admin_url, "ternilo_storage_runtime_test", [57; 32]).await;
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([57; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    storage_contract(control, cloud, &url).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
}

#[tokio::test]
async fn sqlite_worker_access_and_model_reservations_are_durable_and_fenced() {
    let temporary = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}",
        temporary.path().join("worker.sqlite3").display()
    );
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([57; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(worker_access_contract(control, cloud, &url)).await;
}

#[tokio::test]
#[ignore = "requires TERNILO_CLOUD_TEST_DATABASE_URL pointing to a disposable PostgreSQL database"]
async fn postgres_worker_access_and_model_reservations_use_the_same_contract() {
    let admin_url = std::env::var("TERNILO_CLOUD_TEST_DATABASE_URL").unwrap();
    assert!(admin_url.contains("ternilo_cloud_test"));
    let url =
        server_runtime::initialize(&admin_url, "ternilo_worker_access_runtime_test", [57; 32])
            .await;
    let control = ControlStore::connect(&url, None, SecretCipher::from_key([57; 32]), 4)
        .await
        .unwrap();
    let cloud = CloudStore::connect(&url, None, 4).await.unwrap();
    Box::pin(worker_access_contract(control, cloud, &url)).await;
    server_runtime::assert_scoped_without_schema_access(&url).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise credential, volume, generation, request concurrency and persistent replay checks against the same live run on both databases."
)]
async fn worker_access_contract(control: ControlStore, cloud: CloudStore, url: &str) {
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let lease = Duration::from_secs(300);
    let grant = cloud
        .create_worker_credential(&ExecutorId::new("worker-a"), "storage-a", now)
        .await
        .unwrap();
    let pending = cloud.worker_credentials().await.unwrap();
    assert!(!pending[0].registered);
    assert!(!pending[0].online);
    assert_eq!(pending[0].last_seen_at_ms, None);
    assert_eq!(
        cloud
            .worker_configuration(&grant.token)
            .await
            .unwrap()
            .worker_id,
        grant.worker_id
    );
    assert!(cloud.worker_configuration("invalid-token").await.is_err());
    let mut registration = WorkerRegisterRequest {
        capacity: ternilo_cloud::WorkerCapacity::default(),
        hello: hello("worker-a"),
        storage_id: "storage-a".to_owned(),
        root_id: "persistent-root-a".to_owned(),
    };
    let identity = cloud
        .register_authenticated_worker(&grant.token, &registration, lease, now)
        .await
        .unwrap();
    let connected = cloud.worker_credentials().await.unwrap();
    assert!(connected[0].registered);
    assert!(connected[0].online);
    assert_eq!(connected[0].last_seen_at_ms, Some(now));
    assert_eq!(connected[0].lease_expires_at_ms, Some(now + 300_000));
    assert_eq!(
        cloud
            .register_authenticated_worker(&grant.token, &registration, lease, now)
            .await
            .unwrap(),
        identity
    );
    let shared = cloud
        .create_worker_credential(&ExecutorId::new("worker-shared"), "storage-a", now)
        .await
        .unwrap();
    let mut second = WorkerRegisterRequest {
        capacity: ternilo_cloud::WorkerCapacity::default(),
        hello: hello("worker-shared"),
        storage_id: "storage-a".to_owned(),
        root_id: "empty-other-disk".to_owned(),
    };
    assert_eq!(
        cloud
            .register_authenticated_worker(&shared.token, &second, lease, now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    second.root_id.clone_from(&registration.root_id);
    let shared_identity = cloud
        .register_authenticated_worker(&shared.token, &second, lease, now)
        .await
        .unwrap();
    assert!(
        cloud
            .authenticated_worker(&grant.token, &shared_identity, now)
            .await
            .is_err()
    );
    let worker = cloud
        .authenticated_worker(&grant.token, &identity, now)
        .await
        .unwrap();
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://worker.test.invalid".to_owned(),
                subject: "worker-owner".to_owned(),
                email: None,
                display_name: None,
            },
            "test-worker-owner",
            now,
        )
        .await
        .unwrap();
    let tenant = create_tenant(&control, &user, "worker-tenant", now).await;
    let unused = control
        .reserve_quota(
            &user,
            &tenant,
            Some("not-enqueued-yet"),
            100,
            Duration::from_secs(600),
            now,
        )
        .await
        .unwrap();
    cloud
        .release_unused_quota_reservation(&user, &tenant, &unused.reservation_id, now)
        .await
        .unwrap();
    assert!(
        cloud
            .release_unused_quota_reservation(&user, &tenant, &unused.reservation_id, now)
            .await
            .is_err(),
        "a released reservation cannot be released or accounted twice"
    );
    let session = create_session(&control, &cloud, &user, &tenant, "worker-session", now).await;
    enqueue_run(&control, &cloud, &user, &session, "worker-run", now).await;
    let claim = worker
        .claim_run("worker-a", lease, now)
        .await
        .unwrap()
        .unwrap();
    let canonical_claim = worker
        .authorized_claim(&RunLease::from(&claim), now)
        .await
        .unwrap();
    assert_eq!(canonical_claim.spec, claim.spec);
    let run = worker
        .start_run(canonical_claim, "worker-a", lease, now)
        .await
        .unwrap()
        .unwrap();
    let (scoped, canonical) = worker
        .authorized_run(&RunLease::from(&run), now)
        .await
        .unwrap();
    assert_eq!(canonical.claim.spec, run.claim.spec);
    let mut forged = RunLease::from(&run);
    forged.writer_fencing_token += 1;
    assert!(worker.authorized_run(&forged, now).await.is_err());
    forged = RunLease::from(&run);
    forged.tenant_id = TenantId::new("other-tenant");
    assert!(worker.authorized_run(&forged, now).await.is_err());

    let mut stale_model_run = run.clone();
    stale_model_run.fencing_token += 1;
    assert!(
        accept_authenticated_model(
            &control,
            &cloud,
            &grant.token,
            &identity,
            &stale_model_run,
            "forged-model-fence",
            1,
            now
        )
        .await
        .is_err()
    );
    assert!(
        accept_authenticated_model(
            &control,
            &cloud,
            "invalid-worker-token",
            &identity,
            &run,
            "forged-model-worker",
            1,
            now
        )
        .await
        .is_err()
    );

    // Concurrent calls draw suballocations from the same 100-token execution budget.
    let (left, right) = tokio::join!(
        accept_authenticated_model(
            &control,
            &cloud,
            &grant.token,
            &identity,
            &run,
            "1",
            60,
            now
        ),
        accept_authenticated_model(
            &control,
            &cloud,
            &grant.token,
            &identity,
            &run,
            "2",
            60,
            now
        )
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    let (accepted_key, accepted) = if let Ok(request) = left {
        ("1", request)
    } else {
        ("2", right.unwrap())
    };
    assert!(
        cloud
            .release_unused_quota_reservation(
                &user,
                &tenant,
                &accepted.workload.as_ref().unwrap().execution_reservation_id,
                now
            )
            .await
            .is_err(),
        "allocated Run budget must be cancelled through execution, even by its owner"
    );
    let unknown = control
        .finish_workload_model_request(
            &accepted.request_id,
            ternilo_control::ModelRequestState::Cancelled,
            Some("client_disconnected"),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        unknown.accounted_tokens, None,
        "a missing usage result cannot release an attempted request's budget"
    );
    let restarted = CloudStore::connect_without_migrations(url, 4)
        .await
        .unwrap();
    let reopened = restarted
        .authenticated_worker(&grant.token, &identity, now)
        .await
        .unwrap();
    let (reopened, _) = reopened
        .authorized_run(&RunLease::from(&run), now)
        .await
        .unwrap();
    assert!(
        accept_authenticated_model(
            &control,
            &restarted,
            &grant.token,
            &identity,
            &run,
            accepted_key,
            1,
            now
        )
        .await
        .is_err()
    );
    assert!(
        accept_authenticated_model(
            &control,
            &restarted,
            &grant.token,
            &identity,
            &run,
            "3",
            41,
            now
        )
        .await
        .is_err()
    );
    model_ledger::settle(
        &control,
        &accepted.request_id,
        ModelUsage {
            input_tokens: 20,
            output_tokens: 10,
            cached_input_tokens: 2,
            ..ModelUsage::default()
        },
        Some("provider-result"),
        now,
    )
    .await
    .unwrap();
    let pending = accept_authenticated_model(
        &control,
        &restarted,
        &grant.token,
        &identity,
        &run,
        "3",
        70,
        now,
    )
    .await
    .unwrap();
    assert!(
        accept_authenticated_model(
            &control,
            &restarted,
            &grant.token,
            &identity,
            &run,
            "4",
            1,
            now
        )
        .await
        .is_err()
    );
    assert!(
        accept_authenticated_model(
            &control,
            &restarted,
            &grant.token,
            &identity,
            &run,
            accepted_key,
            1,
            now
        )
        .await
        .is_err()
    );

    let inspection =
        create_session(&control, &cloud, &user, &tenant, "inspection-session", now).await;
    let bytes = b"retained output from the canonical session";
    let attachment = Attachment {
        name: "result.txt".to_owned(),
        media_type: "text/plain".to_owned(),
        content: format!(
            "{}{}",
            ternilo_protocol::ATTACHMENT_REFERENCE_PREFIX,
            Sha256::digest(bytes)
                .iter()
                .fold(String::with_capacity(64), |mut output, byte| {
                    write!(output, "{byte:02x}").unwrap();
                    output
                })
        ),
    };
    scoped
        .store_attachment_object(&run, "worker-a", &attachment, bytes, now)
        .await
        .unwrap();
    // Possession of a workspace digest, or mentioning it in text, grants no read access.
    scoped
        .append_event(
            &run,
            "worker-a",
            &SessionEvent {
                seq: 0,
                occurred_at_ms: now,
                run_id: run.claim.run_id.clone(),
                kind: SessionEventKind::AssistantMessageDelta {
                    step: 0,
                    delta: attachment.content.clone(),
                },
            },
            now,
        )
        .await
        .unwrap();
    assert!(
        scoped
            .attachment_for_worker(&run, "worker-a", &attachment, now)
            .await
            .is_err()
    );
    scoped
        .append_event(
            &run,
            "worker-a",
            &SessionEvent {
                seq: 1,
                occurred_at_ms: now,
                run_id: run.claim.run_id.clone(),
                kind: SessionEventKind::DeliverableProduced {
                    path: "result.txt".to_owned(),
                    operation: "write".to_owned(),
                    attachment: attachment.clone(),
                },
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .attachment_for_worker(&run, "worker-a", &attachment, now)
            .await
            .unwrap(),
        bytes
    );
    enqueue_reference_command(&cloud, &inspection, "reference-command", now).await;
    let commands = worker
        .claim_session_commands(&identity, &registration.hello.capabilities, lease, 1, now)
        .await
        .unwrap();
    assert_eq!(commands.len(), 1);
    let command_lease = ternilo_cloud::CommandLease::from(&commands[0]);
    let (command_store, command) = worker
        .authorized_command(&command_lease, now)
        .await
        .unwrap();
    assert_eq!(command.command, commands[0].command);
    let mut forged_command = command_lease.clone();
    forged_command.attempt_count += 1;
    assert!(
        worker
            .authorized_command(&forged_command, now)
            .await
            .is_err()
    );
    let completed = CommandReply::success(
        command_lease.command_id.clone(),
        now,
        serde_json::json!({"candidates":[]}),
    );
    worker
        .complete_authenticated_command(&command_lease, completed.outcome.clone(), now)
        .await
        .unwrap();
    worker
        .complete_authenticated_command(&command_lease, completed.outcome.clone(), now + 1)
        .await
        .unwrap();
    let changed = CommandReply::success(
        command_lease.command_id.clone(),
        now,
        serde_json::json!({"candidates":["changed"]}),
    );
    assert!(
        worker
            .complete_authenticated_command(&command_lease, changed.outcome, now + 1)
            .await
            .is_err()
    );

    cloud
        .cancel_run(&tenant, &run.claim.run_id, now)
        .await
        .unwrap();
    assert!(
        accept_authenticated_model(
            &control,
            &restarted,
            &grant.token,
            &identity,
            &run,
            "5",
            1,
            now
        )
        .await
        .is_err(),
        "cancelled executions cannot accept new model calls"
    );
    "replacement-process".clone_into(&mut registration.hello.instance_nonce);
    let replacement = cloud
        .register_authenticated_worker(&grant.token, &registration, lease, now)
        .await
        .unwrap();
    assert_eq!(replacement.generation, identity.generation + 1);
    assert!(
        accept_authenticated_model(
            &control,
            &cloud,
            &grant.token,
            &identity,
            &run,
            "stale-model-generation",
            1,
            now
        )
        .await
        .is_err()
    );
    assert!(scoped.cancel_requested(&run, "worker-a").await.is_err());
    assert!(
        scoped
            .attachment_for_worker(&run, "worker-a", &attachment, now)
            .await
            .is_err()
    );
    assert!(
        command_store
            .authorized_command(&command_lease, now)
            .await
            .is_err()
    );
    assert!(
        cloud
            .authenticated_worker(&grant.token, &identity, now)
            .await
            .is_err()
    );
    let replacement_store = cloud
        .authenticated_worker(&grant.token, &replacement, now)
        .await
        .unwrap();
    assert!(
        replacement_store
            .complete_authenticated_command(&command_lease, completed.outcome, now + 1)
            .await
            .is_err()
    );
    assert!(
        replacement_store
            .authorized_run(&RunLease::from(&run), now)
            .await
            .is_err()
    );
    "replacement-empty-root".clone_into(&mut registration.root_id);
    assert!(
        cloud
            .register_authenticated_worker(&grant.token, &registration, lease, now)
            .await
            .is_err()
    );
    cloud
        .revoke_worker_credential(&grant.worker_id, now)
        .await
        .unwrap();
    let revoked = cloud.worker_credentials().await.unwrap();
    assert!(revoked[0].registered);
    assert!(!revoked[0].online);
    assert_eq!(revoked[0].revoked_at_ms, Some(now));
    assert!(cloud.worker_configuration(&grant.token).await.is_err());
    model_ledger::settle(
        &control,
        &pending.request_id,
        ModelUsage {
            input_tokens: 4,
            output_tokens: 1,
            ..ModelUsage::default()
        },
        Some("late-after-revocation"),
        now + 1,
    )
    .await
    .unwrap();
    let mut budget_tx = control
        .database()
        .tenant_transaction(&tenant)
        .await
        .unwrap();
    let budget = ControlStore::workload_model_budget_in(
        &mut budget_tx,
        &tenant,
        &pending.workload.as_ref().unwrap().execution_reservation_id,
    )
    .await
    .unwrap();
    assert_eq!(
        (budget.used_model_tokens, budget.unknown_model_tokens),
        (35, 0),
        "late provider usage remains accountable after Worker generation replacement and credential revocation"
    );
    budget_tx.commit().await.unwrap();
    assert!(
        replacement_store
            .heartbeat_cloud_worker(&replacement, lease, now)
            .await
            .is_err()
    );
    assert!(
        cloud
            .create_worker_credential(&grant.worker_id, "storage-a", now)
            .await
            .is_err()
    );
    recovery_contract(&control, &cloud, &user, &tenant, now).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify terminal history repair and subsequent run boot for each authoritative lifecycle transition on both databases."
)]
async fn recovery_contract(
    control: &ControlStore,
    cloud: &CloudStore,
    user: &ControlUser,
    tenant: &TenantId,
    now: u64,
) {
    for action in ["reap", "drain", "replace", "finish"] {
        let id = format!("recovery-{action}");
        let grant = cloud
            .create_worker_credential(&ExecutorId::new(&id), "storage-a", now)
            .await
            .unwrap();
        let mut registration = WorkerRegisterRequest {
            capacity: ternilo_cloud::WorkerCapacity::default(),
            hello: hello(&id),
            storage_id: "storage-a".to_owned(),
            root_id: "persistent-root-a".to_owned(),
        };
        let worker = cloud
            .register_authenticated_worker(
                &grant.token,
                &registration,
                Duration::from_secs(300),
                now,
            )
            .await
            .unwrap();
        let session = create_session(control, cloud, user, tenant, &id, now).await;
        enqueue_run(control, cloud, user, &session, &format!("{id}-old"), now).await;
        let claim = cloud
            .claim_run(&id, Duration::from_secs(30), now)
            .await
            .unwrap()
            .unwrap();
        let started = cloud
            .start_run(claim, &id, Duration::from_secs(30), now)
            .await
            .unwrap()
            .unwrap();
        let kinds = [
            SessionEventKind::TurnStarted,
            SessionEventKind::CommandStarted {
                command_id: "command".to_owned(),
                command_name: "write".to_owned(),
            },
            SessionEventKind::ToolCallStarted {
                call: ternilo_protocol::ToolCall {
                    id: "tool".to_owned(),
                    name: "write_file".to_owned(),
                    arguments: serde_json::json!({}),
                    presentation: None,
                },
            },
            SessionEventKind::CodeDispatchStarted {
                parent_call_id: "tool".to_owned(),
                call: ternilo_protocol::ToolCall {
                    id: "nested".to_owned(),
                    name: "write_file".to_owned(),
                    arguments: serde_json::json!({}),
                    presentation: None,
                },
            },
        ];
        for (sequence, kind) in kinds.into_iter().enumerate() {
            cloud
                .append_event(
                    &started,
                    &id,
                    &SessionEvent {
                        seq: sequence as u64,
                        occurred_at_ms: now,
                        run_id: started.claim.run_id.clone(),
                        kind,
                    },
                    now,
                )
                .await
                .unwrap();
        }
        let finished = now + 30_001;
        match action {
            "reap" => {
                cloud.reap_expired(finished).await.unwrap();
            }
            "drain" => {
                cloud.drain_cloud_worker(&worker, now + 1).await.unwrap();
            }
            "replace" => {
                registration.hello.instance_nonce.push_str("-replacement");
                cloud
                    .register_authenticated_worker(
                        &grant.token,
                        &registration,
                        Duration::from_secs(300),
                        now + 1,
                    )
                    .await
                    .unwrap();
            }
            "finish" => {
                cloud
                    .finish_run(
                        &started,
                        &id,
                        ternilo_cloud::TerminalState::Failed,
                        None,
                        Some(&ternilo_protocol::HarnessError::execution(
                            "child exited before terminal event",
                        )),
                        now + 1,
                    )
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let repaired = cloud
            .session_events(tenant, &session.session_id, None, 1000)
            .await
            .unwrap();
        assert_eq!(
            repaired.len(),
            8,
            "{action} repairs history before another run is submitted"
        );
        assert!(
            repaired
                .iter()
                .all(|event| event.run_id == started.claim.run_id)
        );
        assert!(matches!(
            repaired[4].kind,
            SessionEventKind::CodeDispatchFinished { .. }
        ));
        assert!(matches!(
            repaired[5].kind,
            SessionEventKind::ToolCallFinished { .. }
        ));
        assert!(matches!(
            repaired[6].kind,
            SessionEventKind::CommandFinished { .. }
        ));
        assert!(matches!(
            repaired[7].kind,
            SessionEventKind::TurnFailed { .. }
        ));
        assert!(ternilo_builtins::interrupted_history_events(&repaired).is_empty());
        cloud.reap_expired(finished).await.unwrap();
        assert_eq!(
            cloud
                .session_events(tenant, &session.session_id, None, 1000)
                .await
                .unwrap(),
            repaired
        );
        enqueue_run(
            control,
            cloud,
            user,
            &session,
            &format!("{id}-next"),
            finished + 1,
        )
        .await;
        let claim = cloud
            .claim_run(&id, Duration::from_secs(30), finished + 1)
            .await
            .unwrap();
        if claim.is_none() {
            // A repaired history does not prove that the old Worker stopped writing. The
            // physical occupancy therefore keeps the next run queued until a real exit
            // confirmation arrives.
            continue;
        }
        let claim = claim.expect("claim was checked above");
        let next = cloud
            .start_run(claim, &id, Duration::from_secs(30), finished + 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.prior_events, repaired);
        let late = SessionEvent {
            seq: 8,
            occurred_at_ms: finished + 1,
            run_id: started.claim.run_id.clone(),
            kind: SessionEventKind::TurnCancelled,
        };
        assert!(
            cloud
                .append_event(&started, &id, &late, finished + 1)
                .await
                .is_err()
        );
        assert!(
            cloud
                .append_event(&next, &id, &late, finished + 1)
                .await
                .is_err()
        );
        cloud
            .finish_run(
                &next,
                &id,
                ternilo_cloud::TerminalState::Failed,
                None,
                None,
                finished + 2,
            )
            .await
            .unwrap();
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify persistent storage assignment, cross-workspace claims and fenced read-only retries in one shared database contract."
)]
async fn storage_contract(control: ControlStore, cloud: CloudStore, url: &str) {
    let now = 2_300_000_000_000;
    worker_storage::bind_workers(&cloud, &["worker-a", "worker-shared"], "storage-a").await;
    worker_storage::bind_workers(&cloud, &["worker-b"], "storage-b").await;
    let user = control
        .upsert_user(
            &OidcPrincipal {
                issuer: "https://storage.test.invalid".to_owned(),
                subject: "storage-owner".to_owned(),
                email: None,
                display_name: None,
            },
            "test-storage-owner",
            now,
        )
        .await
        .unwrap();
    let tenant = create_tenant(&control, &user, "storage-tenant", now).await;
    let other_tenant = create_tenant(&control, &user, "other-storage-tenant", now).await;
    let first = create_session(&control, &cloud, &user, &tenant, "first", now).await;
    let second = create_session(&control, &cloud, &user, &tenant, "second", now).await;
    let inspection = create_session(&control, &cloud, &user, &tenant, "inspection", now).await;
    let unrelated = create_session(&control, &cloud, &user, &other_tenant, "unrelated", now).await;
    let hello_a = hello("worker-a");
    let hello_shared = hello("worker-shared");
    let hello_b = hello("worker-b");
    let worker_a = cloud
        .register_cloud_worker(&hello_a, Duration::from_secs(300), now)
        .await
        .unwrap();
    let worker_shared = cloud
        .register_cloud_worker(&hello_shared, Duration::from_secs(300), now)
        .await
        .unwrap();
    let worker_b = cloud
        .register_cloud_worker(&hello_b, Duration::from_secs(300), now)
        .await
        .unwrap();

    enqueue_run(&control, &cloud, &user, &first, "first-run", now).await;
    let claim = cloud
        .claim_run("worker-a", Duration::from_secs(30), now + 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.spec.metadata.workspace_id, first.workspace_id);
    cloud
        .start_run(claim, "worker-a", Duration::from_secs(30), now + 2)
        .await
        .unwrap()
        .unwrap();

    // A second workspace of the same tenant inherits its existing filesystem.
    enqueue_run(&control, &cloud, &user, &second, "second-run", now + 3).await;
    assert!(
        cloud
            .claim_run("worker-b", Duration::from_secs(30), now + 4)
            .await
            .unwrap()
            .is_none()
    );
    let parallel = cloud
        .claim_run("worker-shared", Duration::from_secs(30), now + 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(parallel.spec.metadata.workspace_id, second.workspace_id);
    cloud
        .start_run(parallel, "worker-shared", Duration::from_secs(30), now + 5)
        .await
        .unwrap()
        .unwrap();

    let command = enqueue_reference_command(&cloud, &inspection, "files", now + 6).await;
    assert!(
        cloud
            .claim_session_commands(
                &worker_b,
                &hello_b.capabilities,
                Duration::from_secs(2),
                10,
                now + 7
            )
            .await
            .unwrap()
            .is_empty()
    );
    let first_delivery = cloud
        .claim_session_commands(
            &worker_a,
            &hello_a.capabilities,
            Duration::from_secs(2),
            10,
            now + 7,
        )
        .await
        .unwrap();
    assert_eq!(first_delivery.len(), 1);
    assert_eq!(first_delivery[0].command.command_id, command);
    let restarted = CloudStore::connect_without_migrations(url, 4)
        .await
        .unwrap();
    assert!(
        restarted
            .claim_session_commands(
                &worker_b,
                &hello_b.capabilities,
                Duration::from_secs(2),
                10,
                now + 2_008
            )
            .await
            .unwrap()
            .is_empty()
    );
    let retry = restarted
        .claim_session_commands(
            &worker_shared,
            &hello_shared.capabilities,
            Duration::from_secs(2),
            10,
            now + 2_008,
        )
        .await
        .unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempt_count, 2);
    let stale = CommandReply::success(
        command.clone(),
        now + 2_009,
        serde_json::json!({"directory":"","candidates":[]}),
    );
    assert_eq!(
        restarted
            .complete_session_command(&worker_a, &first_delivery[0], &stale, now + 2_009)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    restarted
        .complete_session_command(&worker_shared, &retry[0], &stale, now + 2_009)
        .await
        .unwrap();

    // A different tenant can use an independent filesystem.
    let other_command =
        enqueue_reference_command(&cloud, &unrelated, "other-files", now + 2_010).await;
    let other = cloud
        .claim_session_commands(
            &worker_b,
            &hello_b.capabilities,
            Duration::from_secs(2),
            10,
            now + 2_011,
        )
        .await
        .unwrap();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].command.command_id, other_command);
    assert_eq!(other[0].tenant_id, other_tenant);

    // Storage affinity remains when all workers for it expire; no empty-disk failover.
    cloud
        .heartbeat_cloud_worker(&worker_b, Duration::from_secs(300), now + 299_000)
        .await
        .unwrap();
    enqueue_reference_command(&cloud, &inspection, "offline-files", now + 301_000).await;
    assert!(
        cloud
            .claim_session_commands(
                &worker_b,
                &hello_b.capabilities,
                Duration::from_secs(2),
                10,
                now + 301_002
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        cloud
            .claim_run("worker-b", Duration::from_secs(30), now + 301_002)
            .await
            .unwrap()
            .is_none()
    );

    let mut scope = cloud.database().tenant_transaction(&tenant).await.unwrap();
    let stored: String =
        sqlx::query_scalar("SELECT storage_id FROM cloud_tenant_storage WHERE tenant_id=$1")
            .bind(tenant.as_str())
            .fetch_one(&mut *scope)
            .await
            .unwrap();
    assert_eq!(stored, "storage-a");
    scope.commit().await.unwrap();
    sqlx::query("UPDATE cloud_worker_credentials SET revoked_at_ms=$1 WHERE worker_id='worker-b'")
        .bind(i64::try_from(now + 301_003).unwrap())
        .execute(cloud.database().pool())
        .await
        .unwrap();
    assert_eq!(
        cloud
            .claim_run("worker-b", Duration::from_secs(30), now + 301_004)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
}

async fn create_tenant(
    control: &ControlStore,
    user: &ControlUser,
    slug: &str,
    now: u64,
) -> TenantId {
    control
        .create_tenant(
            user,
            slug,
            slug,
            TenantQuota {
                max_nodes: 2,
                max_concurrent_runs: 4,
                monthly_model_tokens: 10_000,
                max_secrets: 2,
            },
            now,
        )
        .await
        .unwrap()
        .tenant_id
}

async fn create_session(
    control: &ControlStore,
    cloud: &CloudStore,
    user: &ControlUser,
    tenant: &TenantId,
    name: &str,
    now: u64,
) -> CloudSessionRecord {
    let project = control
        .create_project(user, tenant, name, now)
        .await
        .unwrap();
    let workspace = control
        .create_cloud_workspace(user, tenant, &project.project_id, name, now)
        .await
        .unwrap();
    let model = model_ledger::configure(control, user, tenant, "model", None, now).await;
    cloud
        .create_session(
            CloudSessionDraft {
                project_id: project.project_id,
                workspace_id: workspace.workspace_id,
                session_id: Some(SessionId::new(name)),
                agent_id: AgentId::new("storage-agent"),
                title: name.to_owned(),
                permissions: PermissionPreset::WorkspaceWrite,
                model: Some(model),
                reserved_model_tokens: 100,
                agent_preset: "standard".to_owned(),
                profile_plugins: Vec::new(),
                mode: SessionMode::Execute,
            },
            tenant,
            &user.user_id,
            now,
        )
        .await
        .unwrap()
}

fn hello(worker_id: &str) -> ExecutorHello {
    ExecutorHello {
        protocol_version: EXECUTOR_PROTOCOL_VERSION,
        executor_id: ExecutorId::new(worker_id),
        executor_kind: ExecutorKind::CloudWorker,
        instance_nonce: format!("{worker_id}-process"),
        catalog_revision: "storage-test".to_owned(),
        capabilities: BTreeSet::from([
            ExecutorCapability::CloudRun,
            ExecutorCapability::AddressedSessionCommands,
            ExecutorCapability::WorkspaceFiles,
        ]),
    }
}

async fn enqueue_reference_command(
    cloud: &CloudStore,
    session: &CloudSessionRecord,
    name: &str,
    now: u64,
) -> CommandId {
    let command_id = CommandId::new(name);
    cloud
        .enqueue_session_command(
            &session.tenant_id,
            &session.user_id,
            &CloudSessionCommandDraft {
                session_id: session.session_id.clone(),
                command: ExecutorCommand {
                    input_provenance: None,
                    command_id: command_id.clone(),
                    scope: ExecutorScope {
                        tenant_id: session.tenant_id.clone(),
                        user_id: session.user_id.clone(),
                    },
                    input_authorization: None,
                    issued_at_ms: now,
                    expires_at_ms: now + 120_000,
                    body: ExecutorCommandBody::Application {
                        request: ApplicationOperation::SessionReferenceCandidates {
                            session_id: session.session_id.clone(),
                            request: ReferenceCandidateRequest::default(),
                        },
                    },
                },
                required_capability: ExecutorCapability::WorkspaceFiles,
                required_catalog_revision: Some("storage-test".to_owned()),
                delivery: CloudCommandDelivery::ReadOnly,
            },
            now,
        )
        .await
        .unwrap();
    command_id
}

async fn enqueue_run(
    control: &ControlStore,
    cloud: &CloudStore,
    user: &ControlUser,
    session: &CloudSessionRecord,
    run_id: &str,
    now: u64,
) {
    let compiled = CompiledRun {
        automated_input: None,
        actor_user_id: user.user_id.clone(),
        authorization_session_id: session.session_id.clone(),
        spec: RunSpec {
            schema_version: ternilo_protocol::RUN_SPEC_VERSION,
            catalog_revision: "storage-test".to_owned(),
            policy_revision: "storage-test".to_owned(),
            metadata: RunMetadata {
                tenant_id: session.tenant_id.clone(),
                user_id: user.user_id.clone(),
                project_id: Some(session.project_id.clone()),
                workspace_id: session.workspace_id.clone(),
                agent_id: session.agent_id.clone(),
                session_id: session.session_id.clone(),
                run_id: RunId::new(run_id),
            },
            limits: RunLimits {
                max_steps: 2,
                max_tool_calls: 2,
            },
            permissions: session.permissions,
            mode: session.mode,
            profile: model_ledger::profile(session.model.as_ref().unwrap()),
            input: "check persistent storage".to_owned(),
            references: Vec::new(),
            reference_contexts: Vec::new(),
            attachments: Vec::new(),
        },
        reserved_model_tokens: 100,
        priority: 0,
        max_attempts: 2,
    };
    let reservation = control
        .reserve_quota(
            user,
            &session.tenant_id,
            Some(run_id),
            100,
            Duration::from_secs(600),
            now,
        )
        .await
        .unwrap();
    cloud
        .submit_run(&compiled, &reservation.reservation_id, now)
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_arguments,
    reason = "Pass real Worker identity, lease and requested model into the same transaction as budget admission."
)]
async fn accept_authenticated_model(
    control: &ControlStore,
    cloud: &CloudStore,
    token: &str,
    identity: &ternilo_cloud::CloudWorkerIdentity,
    run: &ternilo_cloud::StartedRun,
    key: &str,
    reserved: u64,
    now: u64,
) -> Result<ternilo_control::ModelServiceRequest, ternilo_protocol::HarnessError> {
    let snapshot = ternilo_cloud::profile_model_snapshot(&run.claim.spec.profile)?.unwrap();
    let mut tx = control.database().begin().await?;
    let (_, principal) = cloud
        .authorize_workload_model_in(
            &mut tx,
            token,
            identity,
            &RunLease::from(run),
            &snapshot.binding,
            now,
        )
        .await?;
    let permit = control
        .reserve_workload_model_request_in(
            &mut tx,
            &principal,
            &ternilo_control::ModelRequestInput {
                request_key: key.to_owned(),
                payload_hash: "a".repeat(64),
                model_id: snapshot.binding.model_id().to_owned(),
                protocol: snapshot.protocol,
                reserved_tokens: reserved,
            },
            now,
        )
        .await
        .map_err(|error| error.error)?;
    if !permit.newly_accepted {
        return Err(ternilo_protocol::HarnessError::conflict(
            "duplicate model request cannot repeat an upstream call",
        ));
    }
    control
        .begin_workload_model_attempt_in(&mut tx, &principal, &permit.request.request_id, 1, now)
        .await
        .map_err(|error| error.error)?;
    tx.commit().await.map_err(ternilo_storage::database_error)?;
    Ok(permit.request)
}
