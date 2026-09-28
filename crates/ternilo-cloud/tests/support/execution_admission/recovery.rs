use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "Prove stale run and Worker generations cannot release other residents or recover execution privileges."
)]
pub(super) async fn expired_residents_and_generation_fences(fixture: &mut Fixture) {
    let session = fixture.session("expiring-resident").await;
    let run = fixture.submit(&session, "expiring-resident-run").await;
    let started = fixture.start(&run, "capacity-worker").await;
    let child = fixture
        .child(&started, "expiring-child", "capacity-worker")
        .await;
    fixture.park(&started, &child, "capacity-worker").await;
    fixture.now += 60_001;
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .renew_run(&started, "capacity-worker", LEASE, now)
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .resume_run(&started, "capacity-worker", 2, 1, now)
            .await
            .is_err()
    );
    fixture
        .cloud
        .resolve_execution_pressure("capacity-worker", now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&started).await, ExecutionPhase::Lost);
    assert!(
        fixture
            .cloud
            .renew_run(&started, "capacity-worker", LEASE, now - 60_001)
            .await
            .is_err(),
        "a reconciled lost resident cannot be revived by a request timestamp captured before expiry"
    );

    assert_eq!(fixture.usage("capacity-worker").await, (0, 1));
    let lease = RunLease::from(&started);
    let mut forged = lease.clone();
    forged.writer_fencing_token += 1;
    assert!(
        fixture
            .cloud
            .release_resident(&forged, &fixture.worker, fixture.worker.generation, now)
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .release_resident(&lease, &fixture.worker, fixture.worker.generation + 1, now)
            .await
            .is_err()
    );
    fixture
        .cloud
        .release_resident(&lease, &fixture.worker, fixture.worker.generation, now)
        .await
        .unwrap();
    fixture
        .cloud
        .release_resident(&lease, &fixture.worker, fixture.worker.generation, now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&started).await, ExecutionPhase::Released);
    assert!(
        fixture
            .cloud
            .renew_run(&started, "capacity-worker", LEASE, now)
            .await
            .is_err()
    );
    fixture.cloud.reap_expired(now).await.unwrap();
    let child = fixture.start(&child.run_id, "capacity-worker").await;
    fixture.finish(&child, "capacity-worker").await;
    let session = fixture.session("old-daemon").await;
    let run = fixture.submit(&session, "old-daemon-run").await;
    let old = fixture.start(&run, "capacity-worker").await;
    let old_identity = fixture.worker.clone();
    let now = fixture.tick();
    fixture.worker = fixture
        .cloud
        .register_authenticated_worker(
            &fixture.worker_token,
            &registration(
                "capacity-worker",
                "replacement",
                WorkerCapacity {
                    max_active_runs: 1,
                    max_resident_runs: 1,
                },
            ),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();
    assert!(fixture.worker.generation > old_identity.generation);
    fixture
        .cloud
        .resolve_execution_pressure("capacity-worker", now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&old).await, ExecutionPhase::Lost);
    let session = fixture
        .session_in_workspace("new-daemon", &old.claim.spec.metadata.workspace_id)
        .await;
    let next = fixture.submit(&session, "new-daemon-run").await;
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .claim_run("capacity-worker", LEASE, now)
            .await
            .unwrap()
            .is_none(),
        "changing the daemon generation cannot pretend the old physical resident disappeared"
    );
    assert_eq!(fixture.usage("capacity-worker").await, (0, 1));
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &next)
            .await
            .unwrap()
            .state,
        CloudRunState::Queued
    );
    assert!(
        fixture
            .cloud
            .release_resident(
                &RunLease::from(&old),
                &old_identity,
                old_identity.generation,
                now
            )
            .await
            .is_err()
    );
    assert!(
        fixture
            .cloud
            .release_resident(
                &RunLease::from(&old),
                &fixture.worker,
                old_identity.generation,
                now
            )
            .await
            .is_err()
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Hold a real PostgreSQL session lock while recovery commits another run, then prove deferred budget and writer cleanup."
)]
pub(super) async fn postgres_reaper_skips_sessions_waiting_for_the_quota(fixture: &mut Fixture) {
    if fixture.cloud.database().backend() != ternilo_storage::Backend::Postgres {
        return;
    }
    let worker = "capacity-worker";
    let first_session = fixture.session("reaper-first").await;
    let first = fixture.submit(&first_session, "reaper-first-run").await;
    let first = fixture.start(&first, worker).await;
    let second_session = fixture.session("reaper-second").await;
    let second = fixture.submit(&second_session, "reaper-second-run").await;
    let second = fixture.start(&second, worker).await;
    fixture.now += 60_001;
    let now = fixture.tick();
    let mut submitter = fixture
        .cloud
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    ternilo_storage::set_user_scope(&mut submitter, &fixture.owner.user_id)
        .await
        .unwrap();
    sqlx::query(
        "SELECT session_id FROM cloud_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE",
    )
    .bind(fixture.tenant.as_str())
    .bind(second_session.session_id.as_str())
    .fetch_one(&mut *submitter)
    .await
    .unwrap();
    let reaped = tokio::time::timeout(Duration::from_secs(10), fixture.cloud.reap_expired(now))
        .await
        .expect(
            "recovery must not wait for the session held by a submitter that can need its quota",
        )
        .unwrap();
    assert_eq!(reaped, 1);
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &first.claim.run_id)
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate
    );
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &second.claim.run_id)
            .await
            .unwrap()
            .state,
        CloudRunState::Running
    );
    let reservation:String = sqlx::query_scalar("SELECT reservation.state FROM control_quota_reservations reservation JOIN cloud_runs run ON run.tenant_id=reservation.tenant_id AND run.quota_reservation_id=reservation.reservation_id WHERE run.tenant_id=$1 AND run.run_id=$2")
        .bind(fixture.tenant.as_str()).bind(second.claim.run_id.as_str()).fetch_one(&mut *submitter).await.unwrap();
    assert_eq!(
        reservation, "active",
        "skipping a locked session must not settle its budget"
    );
    let writers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM cloud_session_writer_leases WHERE tenant_id=$1 AND session_id=$2",
    )
    .bind(fixture.tenant.as_str())
    .bind(second_session.session_id.as_str())
    .fetch_one(&mut *submitter)
    .await
    .unwrap();
    assert_eq!(
        writers, 1,
        "the final expired-writer sweep must also defer the locked session"
    );
    // Match the submit path's session-then-quota lock order on the same connection.
    tokio::time::timeout(
        Duration::from_secs(10),
        sqlx::query("SELECT tenant_id FROM control_quotas WHERE tenant_id=$1 FOR UPDATE")
            .bind(fixture.tenant.as_str())
            .fetch_one(&mut *submitter),
    )
    .await
    .unwrap()
    .unwrap();
    submitter.commit().await.unwrap();
    assert_eq!(fixture.cloud.reap_expired(now).await.unwrap(), 1);
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &second.claim.run_id)
            .await
            .unwrap()
            .state,
        CloudRunState::Indeterminate
    );
    for run in [&first, &second] {
        fixture
            .cloud
            .release_resident(
                &RunLease::from(run),
                &fixture.worker,
                fixture.worker.generation,
                now,
            )
            .await
            .unwrap();
    }
    assert_eq!(fixture.usage(worker).await, (0, 0));
}
