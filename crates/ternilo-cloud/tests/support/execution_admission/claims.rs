use super::*;
use ternilo_cloud::CloudRunClaim;

pub(super) async fn unstarted_claims_release_ownership_but_started_runs_require_confirmation(
    fixture: &mut Fixture,
) {
    let now = fixture.tick();
    let token = fixture
        .cloud
        .create_worker_credential(&ExecutorId::new("handoff-worker"), "capacity-storage", now)
        .await
        .unwrap()
        .token;
    fixture
        .cloud
        .register_authenticated_worker(
            &token,
            &registration("handoff-worker", "first", WorkerCapacity::default()),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();

    expired_claim_can_retry_on_another_worker(fixture).await;
    replaced_generation_cannot_keep_unstarted_ownership(fixture).await;
    started_execution_keeps_ownership_after_expiry(fixture).await;

    let now = fixture.tick();
    fixture
        .cloud
        .revoke_worker_credential(&ExecutorId::new("handoff-worker"), now)
        .await
        .unwrap();
}

async fn claim(fixture: &mut Fixture, worker: &str) -> Option<CloudRunClaim> {
    let now = fixture.tick();
    fixture.cloud.claim_run(worker, LEASE, now).await.unwrap()
}

async fn occupancy_state(fixture: &Fixture, claim: &CloudRunClaim) -> String {
    let mut tx = fixture
        .cloud
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let state = sqlx::query_scalar(
        "SELECT state FROM cloud_workspace_occupancy WHERE tenant_id=$1 AND run_id=$2 AND lease_token=$3",
    ).bind(fixture.tenant.as_str()).bind(claim.run_id.as_str())
        .bind(i64::try_from(claim.lease_token).unwrap()).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    state
}

async fn expired_claim_can_retry_on_another_worker(fixture: &mut Fixture) {
    let session = fixture.session("expired-claim").await;
    let run = fixture.submit(&session, "expired-claim-run").await;
    let old = claim(fixture, "capacity-worker").await.unwrap();
    assert_eq!(old.run_id, run);
    assert!(claim(fixture, "handoff-worker").await.is_none());
    fixture.now += 60_001;
    let retry = claim(fixture, "handoff-worker").await.expect(
        "a claim that never started must not leave directory ownership on its previous Worker",
    );
    assert_eq!(retry.run_id, run);
    assert!(retry.lease_token > old.lease_token);
    assert_eq!(occupancy_state(fixture, &old).await, "released");
    assert_eq!(fixture.usage("capacity-worker").await, (0, 0));
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .start_run(old.clone(), "capacity-worker", LEASE, now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .cloud
            .release_claim(&old, "capacity-worker", now)
            .await
            .is_err()
    );
    assert_eq!(occupancy_state(fixture, &retry).await, "held");
    let running = fixture.start_claim(retry, "handoff-worker").await;
    fixture.finish(&running, "handoff-worker").await;
}

async fn replaced_generation_cannot_keep_unstarted_ownership(fixture: &mut Fixture) {
    let first = fixture.session("replaced-claim").await;
    let run = fixture.submit(&first, "replaced-claim-run").await;
    let old = claim(fixture, "capacity-worker").await.unwrap();
    assert_eq!(old.run_id, run);
    let peer = fixture
        .session_in_workspace("replaced-peer", &first.workspace_id)
        .await;
    let peer_run = fixture.submit(&peer, "replaced-peer-run").await;
    assert!(claim(fixture, "handoff-worker").await.is_none());
    let now = fixture.tick();
    fixture.worker = fixture
        .cloud
        .register_authenticated_worker(
            &fixture.worker_token,
            &registration(
                "capacity-worker",
                "replacement-before-start",
                WorkerCapacity::default(),
            ),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();
    let next = claim(fixture, "handoff-worker")
        .await
        .expect("replacing a generation releases only its unstarted claims");
    assert_eq!(
        next.run_id, run,
        "registration requeues the unstarted claim"
    );
    assert!(next.lease_token > old.lease_token);
    assert_eq!(occupancy_state(fixture, &old).await, "released");
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .start_run(old, "capacity-worker", LEASE, now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        claim(fixture, "capacity-worker").await.is_none(),
        "the independent peer waits while the replacement owns this workspace"
    );
    let running = fixture.start_claim(next, "handoff-worker").await;
    fixture.finish(&running, "handoff-worker").await;
    let peer = fixture.start(&peer_run, "handoff-worker").await;
    fixture.finish(&peer, "handoff-worker").await;
}

async fn started_execution_keeps_ownership_after_expiry(fixture: &mut Fixture) {
    let first = fixture.session("started-handoff").await;
    let run = fixture.submit(&first, "started-handoff-run").await;
    let running = fixture.start(&run, "capacity-worker").await;
    let peer = fixture
        .session_in_workspace("started-peer", &first.workspace_id)
        .await;
    let peer_run = fixture.submit(&peer, "started-peer-run").await;
    assert!(claim(fixture, "handoff-worker").await.is_none());
    fixture.now += 60_001;
    assert!(claim(fixture, "handoff-worker").await.is_none());
    assert_eq!(fixture.phase(&running).await, ExecutionPhase::Lost);
    let now = fixture.tick();
    fixture.cloud.reap_expired(now).await.unwrap();
    assert!(
        claim(fixture, "handoff-worker").await.is_none(),
        "repairing expired run history does not prove its physical writers exited"
    );
    assert_eq!(occupancy_state(fixture, &running.claim).await, "cleanup");
    let now = fixture.tick();
    fixture
        .cloud
        .release_resident(
            &RunLease::from(&running),
            &fixture.worker,
            fixture.worker.generation,
            now,
        )
        .await
        .unwrap();
    assert_eq!(occupancy_state(fixture, &running.claim).await, "released");
    let peer = fixture.start(&peer_run, "handoff-worker").await;
    fixture.finish(&peer, "handoff-worker").await;
}
