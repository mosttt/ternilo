use super::*;
use ternilo_cloud::WorkspaceRecoveryTicket;

pub(super) async fn recovery_preserves_old_identity_and_cannot_release_a_new_epoch(
    fixture: &mut Fixture,
) {
    let original = fixture.session("recover-old").await;
    let run = fixture.submit(&original, "recover-old-run").await;
    let started = fixture.start(&run, "capacity-worker").await;
    let ticket = WorkspaceRecoveryTicket {
        workspace: started.claim.workspace_use.clone(),
        writer_fencing_token: started.fencing_token,
    };
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .workspace_recovery_candidates(&fixture.worker, None, now)
            .await
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(
        fixture
            .cloud
            .confirm_workspace_recovery(&fixture.worker, &ticket, now)
            .await
            .is_err(),
        "a live run cannot be recovered"
    );
    fixture
        .cloud
        .finish_run(
            &started,
            "capacity-worker",
            TerminalState::Failed,
            None,
            Some(&ternilo_protocol::HarnessError::execution(
                "fixture stopped",
            )),
            now,
        )
        .await
        .unwrap();
    let page = fixture
        .cloud
        .workspace_recovery_candidates(&fixture.worker, None, now)
        .await
        .unwrap();
    assert_eq!(page.candidates, vec![ticket.clone()]);
    assert!(page.next.is_none());
    reject_mismatched_recovery(fixture, &ticket).await;
    let now = fixture.tick();
    fixture
        .cloud
        .confirm_workspace_recovery(&fixture.worker, &ticket, now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&started).await, ExecutionPhase::Released);
    let successor = fixture
        .session_in_workspace("recover-successor", &original.workspace_id)
        .await;
    let run = fixture.submit(&successor, "recover-successor-run").await;
    let current = fixture.start(&run, "capacity-worker").await;
    assert_eq!(
        current.claim.workspace_use.occupation_epoch,
        ticket.workspace.occupation_epoch + 1
    );
    let now = fixture.tick();
    fixture
        .cloud
        .confirm_workspace_recovery(&fixture.worker, &ticket, now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&current).await, ExecutionPhase::Active);
    assert_eq!(fixture.usage("capacity-worker").await, (1, 1));
    assert_recovery_audit(fixture, &started, &ticket).await;
    assert!(
        fixture
            .cloud
            .renew_run(&started, "capacity-worker", LEASE, now)
            .await
            .is_err()
    );
    fixture.finish(&current, "capacity-worker").await;
}

async fn assert_recovery_audit(
    fixture: &Fixture,
    started: &StartedRun,
    ticket: &WorkspaceRecoveryTicket,
) {
    let audit = fixture
        .control
        .list_audit(&fixture.owner, &fixture.tenant, 1000)
        .await
        .unwrap();
    let audit: Vec<_> = audit
        .iter()
        .filter(|entry| {
            entry.action == "workspace.recovered"
                && entry.resource_id == started.claim.run_id.as_str()
        })
        .collect();
    assert_eq!(
        audit.len(),
        1,
        "repeated confirmation must not append a second audit"
    );
    assert_eq!(audit[0].actor_kind, "worker");
    assert!(audit[0].actor_user_id.is_none());
    assert_eq!(audit[0].metadata["reporter_worker_id"], "capacity-worker");
    assert_eq!(
        audit[0].metadata["occupation_epoch"],
        ticket.workspace.occupation_epoch
    );
}

async fn reject_mismatched_recovery(fixture: &mut Fixture, ticket: &WorkspaceRecoveryTicket) {
    let mut variants = Vec::new();
    let mut forged = ticket.clone();
    forged.writer_fencing_token += 1;
    variants.push(forged);
    let mut forged = ticket.clone();
    forged.workspace.worker_generation += 1;
    variants.push(forged);
    let mut forged = ticket.clone();
    forged.workspace.occupation_epoch += 1;
    variants.push(forged);
    let mut forged = ticket.clone();
    "foreign-root".clone_into(&mut forged.workspace.root_id);
    variants.push(forged);
    let mut forged = ticket.clone();
    "foreign-storage".clone_into(&mut forged.workspace.storage_id);
    variants.push(forged);
    let mut forged = ticket.clone();
    "foreign-family".clone_into(&mut forged.workspace.family_id);
    variants.push(forged);
    for forged in variants {
        let now = fixture.tick();
        assert!(
            fixture
                .cloud
                .confirm_workspace_recovery(&fixture.worker, &forged, now)
                .await
                .is_err()
        );
    }
    let mut old_identity = fixture.worker.clone();
    old_identity.generation += 1;
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .confirm_workspace_recovery(&old_identity, ticket, now)
            .await
            .is_err()
    );
    let token = fixture
        .cloud
        .create_worker_credential(&ExecutorId::new("foreign-recovery"), "foreign-storage", now)
        .await
        .unwrap()
        .token;
    let mut registration = registration("foreign-recovery", "first", WorkerCapacity::default());
    "foreign-storage".clone_into(&mut registration.storage_id);
    "foreign-root".clone_into(&mut registration.root_id);
    let foreign = fixture
        .cloud
        .register_authenticated_worker(&token, &registration, LEASE, now)
        .await
        .unwrap();
    assert!(
        fixture
            .cloud
            .workspace_recovery_candidates(&foreign, None, now)
            .await
            .unwrap()
            .candidates
            .is_empty()
    );
    assert!(
        fixture
            .cloud
            .confirm_workspace_recovery(&foreign, ticket, now)
            .await
            .is_err()
    );
}

pub(super) async fn another_worker_recovers_an_expired_resident_without_replaying_it(
    fixture: &mut Fixture,
) {
    let session = fixture.session("recover-expired").await;
    let run = fixture.submit(&session, "recover-expired-run").await;
    let started = fixture.start(&run, "capacity-worker").await;
    fixture.now += 60_001;
    let now = fixture.tick();
    let token = fixture
        .cloud
        .create_worker_credential(&ExecutorId::new("recovery-worker"), "capacity-storage", now)
        .await
        .unwrap()
        .token;
    let reporter = fixture
        .cloud
        .register_authenticated_worker(
            &token,
            &registration("recovery-worker", "first", WorkerCapacity::default()),
            LEASE,
            now,
        )
        .await
        .unwrap();
    fixture.cloud.reap_expired(now).await.unwrap();
    let candidates = fixture
        .cloud
        .workspace_recovery_candidates(&reporter, None, now)
        .await
        .unwrap()
        .candidates;
    assert_eq!(candidates.len(), 1);
    assert_eq!(fixture.phase(&started).await, ExecutionPhase::Lost);
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &run)
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate
    );
    let before = reservation(fixture, &run).await;
    fixture
        .cloud
        .confirm_workspace_recovery(&reporter, &candidates[0], now)
        .await
        .unwrap();
    assert_eq!(
        reservation(fixture, &run).await,
        before,
        "physical recovery cannot refund uncertain usage"
    );
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &run)
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate
    );
    assert_eq!(fixture.phase(&started).await, ExecutionPhase::Released);
    assert_eq!(fixture.usage("capacity-worker").await, (0, 0));
    assert!(
        fixture
            .cloud
            .claim_run("recovery-worker", LEASE, now)
            .await
            .unwrap()
            .is_none()
    );
    fixture
        .cloud
        .revoke_worker_credential(&reporter.worker_id, now)
        .await
        .unwrap();
}

async fn reservation(fixture: &Fixture, run: &RunId) -> String {
    let mut tx = fixture
        .cloud
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    ternilo_storage::set_user_scope(&mut tx, &fixture.owner.user_id)
        .await
        .unwrap();
    let state = sqlx::query_scalar("SELECT reservation.state FROM control_quota_reservations reservation JOIN cloud_runs run ON run.tenant_id=reservation.tenant_id AND run.quota_reservation_id=reservation.reservation_id WHERE run.tenant_id=$1 AND run.run_id=$2")
        .bind(fixture.tenant.as_str()).bind(run.as_str()).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    state
}

pub(super) async fn recovery_cursor_advances_past_unconfirmed_first_page(fixture: &mut Fixture) {
    let now = fixture.tick();
    fixture.worker = fixture
        .cloud
        .register_authenticated_worker(
            &fixture.worker_token,
            &registration(
                "capacity-worker",
                "recovery-pagination",
                WorkerCapacity {
                    max_active_runs: 4,
                    max_resident_runs: 64,
                },
            ),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();
    for index in 0..35 {
        let session = fixture.session(&format!("recovery-page-{index:02}")).await;
        let run = fixture
            .submit(&session, &format!("recovery-page-run-{index:02}"))
            .await;
        let run = fixture.start(&run, "capacity-worker").await;
        let now = fixture.tick();
        fixture
            .cloud
            .finish_run(
                &run,
                "capacity-worker",
                TerminalState::Failed,
                None,
                Some(&ternilo_protocol::HarnessError::execution(
                    "fixture stopped",
                )),
                now,
            )
            .await
            .unwrap();
    }
    let now = fixture.tick();
    let first = fixture
        .cloud
        .workspace_recovery_candidates(&fixture.worker, None, now)
        .await
        .unwrap();
    assert_eq!(first.candidates.len(), 32);
    let second = fixture
        .cloud
        .workspace_recovery_candidates(&fixture.worker, first.next.as_ref(), now)
        .await
        .unwrap();
    assert_eq!(second.candidates.len(), 3);
    assert!(second.next.is_none());
    assert!(
        !first
            .candidates
            .iter()
            .any(|ticket| second.candidates.contains(ticket))
    );
    for ticket in first.candidates.into_iter().chain(second.candidates) {
        fixture
            .cloud
            .confirm_workspace_recovery(&fixture.worker, &ticket, now)
            .await
            .unwrap();
    }
    assert_eq!(fixture.usage("capacity-worker").await, (0, 0));
    fixture.worker = fixture
        .cloud
        .register_authenticated_worker(
            &fixture.worker_token,
            &registration(
                "capacity-worker",
                "after-recovery",
                WorkerCapacity::default(),
            ),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();
}
