use super::*;

pub(super) async fn workspace_occupancy_blocks_independent_families(fixture: &mut Fixture) {
    let first = fixture.session("physical-owner").await;
    let second = fixture
        .session_in_workspace("physical-peer", &first.workspace_id)
        .await;
    let first_run = fixture.submit(&first, "physical-owner-run").await;
    let second_run = fixture.submit(&second, "physical-peer-run").await;
    let running = fixture.start(&first_run, "capacity-worker").await;
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .claim_run("capacity-worker", LEASE, now)
            .await
            .unwrap()
            .is_none(),
        "an independent family must wait while the physical workspace is held"
    );
    fixture.finish(&running, "capacity-worker").await;
    let peer = fixture.start(&second_run, "capacity-worker").await;
    fixture.finish(&peer, "capacity-worker").await;
}

#[expect(
    clippy::too_many_lines,
    reason = "Preserve budgets, actors, admission and three execution levels in one shared dual-database contract."
)]
pub(super) async fn four_parents_and_nested_children(fixture: &mut Fixture) {
    let worker = "capacity-worker";
    let mut roots = vec![];
    for index in 0..5 {
        let session = fixture.session(&format!("parent-{index}")).await;
        let run = fixture
            .submit(&session, &format!("parent-run-{index}"))
            .await;
        roots.push(run);
    }
    let mut parents = vec![];
    for run in &roots[..4] {
        parents.push(fixture.start(run, worker).await);
    }
    assert_eq!(fixture.usage(worker).await, (4, 4));
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .claim_run(worker, LEASE, now)
            .await
            .unwrap()
            .is_none(),
        "queued token reservations are accepted, but a fifth foreground run cannot start"
    );
    let mut children = vec![];
    for (index, parent) in parents.iter().enumerate() {
        children.push(
            fixture
                .child(parent, &format!("child-{index}"), worker)
                .await,
        );
    }
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                std::slice::from_ref(&children[1]),
                1,
                now
            )
            .await
            .is_err(),
        "a different parent's accepted child is not a waiting ticket"
    );
    let extra = fixture
        .child(&parents[0], "detached-background", worker)
        .await;
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                &[children[0].clone(), extra.clone()],
                1,
                now
            )
            .await
            .unwrap(),
        1
    );
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                std::slice::from_ref(&children[0]),
                2,
                now
            )
            .await
            .unwrap(),
        2,
        "dropping a waiting branch can narrow dependencies while keeping the parent parked"
    );
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                &[children[0].clone(), extra.clone()],
                2,
                now
            )
            .await
            .is_err(),
        "the same activity revision cannot change its payload"
    );
    for (parent, child) in parents.iter().zip(&children).skip(1) {
        assert_eq!(fixture.park(parent, child, worker).await, 1);
    }
    assert_eq!(fixture.usage(worker).await, (0, 4));
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                std::slice::from_ref(&children[0]),
                1,
                now
            )
            .await
            .is_err(),
        "an older activity revision cannot replace the latest accepted dependency set"
    );
    assert_eq!(
        fixture
            .cloud
            .park_run(
                &parents[0],
                worker,
                std::slice::from_ref(&children[0]),
                2,
                now
            )
            .await
            .unwrap(),
        2
    );
    let mut child_runs = vec![];
    for child in &children {
        child_runs.push(fixture.start(&child.run_id, worker).await);
    }
    assert_eq!(fixture.usage(worker).await, (4, 8));
    let mut grandchildren = vec![];
    for (index, child) in child_runs.iter().enumerate() {
        grandchildren.push(
            fixture
                .child(child, &format!("grandchild-{index}"), worker)
                .await,
        );
    }
    for (child, grandchild) in child_runs.iter().zip(&grandchildren) {
        fixture.park(child, grandchild, worker).await;
    }
    let mut leaves = vec![];
    for grandchild in &grandchildren {
        leaves.push(fixture.start(&grandchild.run_id, worker).await);
    }
    assert_eq!(fixture.usage(worker).await, (4, 12));
    assert_eq!(
        fixture.resume(&parents[0], worker).await,
        RunAdmission::Pending
    );
    for leaf in &leaves {
        fixture.finish(leaf, worker).await;
    }
    for child in &child_runs {
        assert!(matches!(
            fixture.resume(child, worker).await,
            RunAdmission::Ready { .. }
        ));
    }
    assert_eq!(fixture.usage(worker).await, (4, 8));
    for child in &child_runs {
        fixture.finish(child, worker).await;
    }
    for parent in &parents {
        assert_eq!(
            fixture.resume(parent, worker).await,
            RunAdmission::Ready { admission_epoch: 2 }
        );
    }
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .resume_run(&parents[0], worker, 3, 2, now)
            .await
            .unwrap(),
        RunAdmission::Ready { admission_epoch: 2 }
    );
    assert_eq!(fixture.usage(worker).await, (4, 4));
    for parent in &parents {
        fixture.finish(parent, worker).await;
    }
    let last = fixture.start(&roots[4], worker).await;
    fixture.finish(&last, worker).await;
    let background = fixture.start(&extra.run_id, worker).await;
    fixture.finish(&background, worker).await;
    assert_eq!(fixture.usage(worker).await, (0, 0));
    let mut tx = fixture
        .control
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let reservation=sqlx::query("SELECT reservation.user_id,audit.actor_user_id FROM control_quota_reservations reservation JOIN control_audit_log audit ON audit.tenant_id=reservation.tenant_id AND audit.resource_id=reservation.reservation_id AND audit.action='quota.reserve' WHERE reservation.tenant_id=$1 AND reservation.run_id=$2")
        .bind(fixture.tenant.as_str()).bind(children[0].run_id.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        reservation.try_get::<String, _>("user_id").unwrap(),
        fixture.owner.user_id.as_str()
    );
    assert_eq!(
        reservation.try_get::<String, _>("actor_user_id").unwrap(),
        fixture.actor.user_id.as_str()
    );
    tx.commit().await.unwrap();
    // A human continuation of a child session is a new scheduling root.
    let child_session = fixture
        .cloud
        .get_session(&fixture.tenant, &children[0].session_id)
        .await
        .unwrap();
    let manual = fixture.submit(&child_session, "human-continuation").await;
    let lineage = fixture
        .cloud
        .run_lineage(&fixture.tenant, &fixture.owner.user_id, &manual)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lineage.root_run_id, manual);
    assert!(lineage.parent_run_id.is_none());
    let manual = fixture.start(&manual, worker).await;
    fixture.finish(&manual, worker).await;
}

pub(super) async fn foreground_claim_race(fixture: &mut Fixture) {
    let now = fixture.tick();
    let mut quota = fixture
        .control
        .get_quota(&fixture.owner, &fixture.tenant)
        .await
        .unwrap();
    quota.max_concurrent_runs = 1;
    fixture
        .control
        .update_quota(&fixture.owner, &fixture.tenant, quota.clone(), now)
        .await
        .unwrap();
    let first_session = fixture.session("race-first").await;
    let first = fixture.submit(&first_session, "race-first-run").await;
    let second_session = fixture.session("race-second").await;
    let second = fixture.submit(&second_session, "race-second-run").await;
    let now = fixture.tick();
    let (one, two) = tokio::join!(
        fixture.cloud.claim_run("capacity-worker", LEASE, now),
        fixture.cloud.claim_run("capacity-worker", LEASE, now)
    );
    let claims = vec![one.unwrap(), two.unwrap()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        claims.len(),
        1,
        "concurrent claim transactions cannot overbook the space foreground limit"
    );
    assert_eq!(claims[0].run_id, first);
    assert_eq!(fixture.usage("capacity-worker").await, (1, 1));
    let now = fixture.tick();
    fixture
        .cloud
        .release_claim(&claims[0], "capacity-worker", now)
        .await
        .unwrap();
    // Releasing a claim moves its availability behind the already queued peer.
    for run in [second, first] {
        let started = fixture.start(&run, "capacity-worker").await;
        fixture.finish(&started, "capacity-worker").await;
    }
    quota.max_concurrent_runs = 4;
    let now = fixture.tick();
    fixture
        .control
        .update_quota(&fixture.owner, &fixture.tenant, quota, now)
        .await
        .unwrap();
}

pub(super) async fn queued_backlog_cannot_hide_other_sessions(fixture: &mut Fixture) {
    let worker = "capacity-worker";
    let busy_session = fixture.session("busy-queue").await;
    let busy = fixture.submit(&busy_session, "busy-running").await;
    let busy = fixture.start(&busy, worker).await;
    let mut queued = Vec::new();
    for index in 0..64 {
        queued.push(
            fixture
                .submit(&busy_session, &format!("busy-queued-{index}"))
                .await,
        );
    }
    let independent_session = fixture.session("independent-ready").await;
    let independent = fixture
        .submit(&independent_session, "independent-ready-run")
        .await;
    let independent = fixture.start(&independent, worker).await;
    fixture.finish(&independent, worker).await;
    for queued_run in queued {
        let now = fixture.tick();
        assert_eq!(
            fixture
                .cloud
                .cancel_run(&fixture.tenant, &queued_run, now)
                .await
                .unwrap(),
            CloudRunState::Cancelled
        );
    }
    fixture.finish(&busy, worker).await;
    assert_eq!(fixture.usage(worker).await, (0, 0));
}
