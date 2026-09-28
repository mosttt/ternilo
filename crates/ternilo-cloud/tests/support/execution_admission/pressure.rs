use super::*;

#[derive(Clone, Copy)]
pub(super) enum OtherWorker {
    Absent,
    Idle,
    Active,
}

impl OtherWorker {
    fn prefix(self) -> &'static str {
        match self {
            Self::Absent => "full",
            Self::Idle => "spare",
            Self::Active => "busy-peer",
        }
    }
}

pub(super) async fn deep_resident_pressure(fixture: &mut Fixture, other: OtherWorker) {
    let prefix = other.prefix();
    let other_id = format!("{prefix}-worker");
    if !matches!(other, OtherWorker::Absent) {
        register_other_worker(fixture, &other_id).await;
    }
    let session = fixture.session(&format!("{prefix}-root")).await;
    let root = fixture.submit(&session, &format!("{prefix}-run-0")).await;
    let mut current = fixture.start(&root, "capacity-worker").await;
    let mut parents = vec![];
    for index in 1..16 {
        let child = fixture
            .child(
                &current,
                &format!("{prefix}-run-{index}"),
                "capacity-worker",
            )
            .await;
        fixture.park(&current, &child, "capacity-worker").await;
        parents.push(current);
        current = fixture.start(&child.run_id, "capacity-worker").await;
    }
    assert_eq!(fixture.usage("capacity-worker").await, (1, 16));
    let child = fixture
        .child(&current, &format!("{prefix}-overflow"), "capacity-worker")
        .await;
    no_claim(fixture, "capacity-worker").await;
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &child.run_id)
            .await
            .unwrap()
            .state,
        CloudRunState::Queued,
        "a genuinely active parent can still make progress"
    );
    if !matches!(other, OtherWorker::Absent) {
        no_claim(fixture, &other_id).await;
        assert_eq!(
            fixture.usage(&other_id).await,
            (0, 0),
            "a spare Worker cannot claim the child pinned to the directory holder"
        );
    }

    let ordinary_session = fixture.session(&format!("{prefix}-ordinary")).await;
    let ordinary = fixture
        .submit(&ordinary_session, &format!("{prefix}-ordinary-run"))
        .await;
    let other_run = if matches!(other, OtherWorker::Active) {
        Some(fixture.start(&ordinary, &other_id).await)
    } else {
        None
    };
    fixture.park(&current, &child, "capacity-worker").await;
    parents.push(current);
    no_claim(fixture, "capacity-worker").await;
    assert_exhausted_without_model_charge(fixture, &child.run_id).await;
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, &ordinary)
            .await
            .unwrap()
            .state,
        if other_run.is_some() {
            CloudRunState::Running
        } else {
            CloudRunState::Queued
        },
        "ordinary roots do not borrow an ancestor's dependency handling"
    );

    if let Some(other_run) = other_run {
        fixture.finish(&other_run, &other_id).await;
    } else if matches!(other, OtherWorker::Idle) {
        let ordinary_run = fixture.start(&ordinary, &other_id).await;
        fixture.finish(&ordinary_run, &other_id).await;
    }
    for parent in parents.iter().rev() {
        assert!(matches!(
            fixture.resume(parent, "capacity-worker").await,
            RunAdmission::Ready { .. }
        ));
        fixture.finish(parent, "capacity-worker").await;
    }
    if matches!(other, OtherWorker::Absent) {
        let ordinary = fixture.start(&ordinary, "capacity-worker").await;
        fixture.finish(&ordinary, "capacity-worker").await;
    } else {
        let now = fixture.tick();
        fixture
            .cloud
            .revoke_worker_credential(&ExecutorId::new(&other_id), now)
            .await
            .unwrap();
    }
    assert_eq!(fixture.usage("capacity-worker").await, (0, 0));
}

async fn register_other_worker(fixture: &mut Fixture, worker: &str) {
    let now = fixture.tick();
    let token = fixture
        .cloud
        .create_worker_credential(&ExecutorId::new(worker), "capacity-storage", now)
        .await
        .unwrap()
        .token;
    fixture
        .cloud
        .register_authenticated_worker(
            &token,
            &registration(worker, "first", WorkerCapacity::default()),
            Duration::from_secs(300),
            now,
        )
        .await
        .unwrap();
}

async fn no_claim(fixture: &mut Fixture, worker: &str) {
    let now = fixture.tick();
    assert!(
        fixture
            .cloud
            .claim_run(worker, LEASE, now)
            .await
            .unwrap()
            .is_none()
    );
}

async fn assert_exhausted_without_model_charge(fixture: &Fixture, child: &RunId) {
    let failed = fixture.cloud.get_run(&fixture.tenant, child).await.unwrap();
    assert_eq!(
        failed.state,
        CloudRunState::Failed,
        "unusable capacity on another Worker must not hide a pinned dependency deadlock"
    );
    assert!(
        failed
            .error
            .unwrap()
            .message
            .starts_with("capacity_exhausted:")
    );
    let mut tx = fixture
        .control
        .database()
        .tenant_transaction(&fixture.tenant)
        .await
        .unwrap();
    let reservation = sqlx::query(
        "SELECT state,COALESCE(committed_model_tokens,0) AS tokens FROM control_quota_reservations WHERE tenant_id=$1 AND run_id=$2",
    ).bind(fixture.tenant.as_str()).bind(child.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        reservation.try_get::<String, _>("state").unwrap(),
        "released"
    );
    assert_eq!(reservation.try_get::<i64, _>("tokens").unwrap(), 0);
    tx.commit().await.unwrap();
}
