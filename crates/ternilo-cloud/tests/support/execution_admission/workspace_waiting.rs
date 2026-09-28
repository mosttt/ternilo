use super::*;
use ternilo_protocol::SessionExecutionPhase;

pub(super) async fn blocked_directories_do_not_hide_ready_workspaces(fixture: &mut Fixture) {
    let workspace = fixture.session("blocked-directory-owner").await;
    let run = fixture
        .submit(&workspace, "blocked-directory-owner-run")
        .await;
    let holder = fixture.start(&run, "capacity-worker").await;
    let mut blocked = Vec::new();
    for index in 0..70 {
        let session = fixture
            .session_in_workspace(
                &format!("directory-waiter-{index:02}"),
                &workspace.workspace_id,
            )
            .await;
        let run = fixture
            .submit(&session, &format!("directory-waiter-run-{index:02}"))
            .await;
        blocked.push((session, run));
    }
    let independent = fixture.session("directory-independent").await;
    let run = fixture
        .submit(&independent, "directory-independent-run")
        .await;
    let ready = fixture.start(&run, "capacity-worker").await;
    assert_eq!(
        fixture.usage("capacity-worker").await,
        (2, 2),
        "seventy blocked directories must not consume foreground slots or hide the next ready candidate"
    );
    for _ in 0..3 {
        let now = fixture.tick();
        assert!(
            fixture
                .cloud
                .claim_run("capacity-worker", LEASE, now)
                .await
                .unwrap()
                .is_none()
        );
    }
    for (session, run) in &blocked {
        assert_waiting_without_turn(fixture, session, run).await;
    }
    fixture.finish(&ready, "capacity-worker").await;
    for (_, run) in &blocked {
        let now = fixture.tick();
        assert_eq!(
            fixture
                .cloud
                .cancel_run(&fixture.tenant, run, now)
                .await
                .unwrap(),
            CloudRunState::Cancelled
        );
    }
    fixture.finish(&holder, "capacity-worker").await;
    assert_eq!(fixture.usage("capacity-worker").await, (0, 0));
}

pub(super) async fn expired_members_block_their_own_family_until_physical_confirmation(
    fixture: &mut Fixture,
) {
    let session = fixture.session("family-waits-for-exit").await;
    let run = fixture.submit(&session, "family-waits-for-exit-run").await;
    let holder = fixture.start(&run, "capacity-worker").await;
    let child = fixture
        .child(&holder, "family-waiting-child", "capacity-worker")
        .await;
    fixture.now += 60_001;
    let now = fixture.tick();
    fixture.cloud.reap_expired(now).await.unwrap();
    assert!(
        fixture
            .cloud
            .claim_run("capacity-worker", LEASE, now)
            .await
            .unwrap()
            .is_none(),
        "the same family and Worker generation are insufficient after its resident loses authority"
    );
    let queued = fixture
        .cloud
        .get_session(&fixture.tenant, &child.session_id)
        .await
        .unwrap();
    assert_waiting_without_turn(fixture, &queued, &child.run_id).await;
    assert_eq!(fixture.phase(&holder).await, ExecutionPhase::Lost);
    let page = fixture
        .cloud
        .workspace_recovery_candidates(&fixture.worker, None, now)
        .await
        .unwrap();
    assert_eq!(page.candidates.len(), 1);
    fixture
        .cloud
        .confirm_workspace_recovery(&fixture.worker, &page.candidates[0], now)
        .await
        .unwrap();
    let running = fixture.start(&child.run_id, "capacity-worker").await;
    assert_eq!(
        running.claim.workspace_use.occupation_epoch,
        holder.claim.workspace_use.occupation_epoch + 1
    );
    let current = fixture
        .cloud
        .get_session(&fixture.tenant, &child.session_id)
        .await
        .unwrap();
    assert_eq!(
        current.execution.unwrap().phase,
        SessionExecutionPhase::Running,
        "the real TurnStarted event replaces its queued waiting projection"
    );
    fixture.finish(&running, "capacity-worker").await;
}

async fn assert_waiting_without_turn(fixture: &Fixture, session: &CloudSessionRecord, run: &RunId) {
    let current = fixture
        .cloud
        .get_session(&fixture.tenant, &session.session_id)
        .await
        .unwrap();
    let execution = current.execution.unwrap();
    assert_eq!(execution.run_id, *run);
    assert_eq!(execution.phase, SessionExecutionPhase::WaitingForWorkspace);
    assert!(
        current.last_seq.is_none(),
        "waiting metadata must not manufacture model events"
    );
    assert_eq!(
        fixture
            .cloud
            .get_run(&fixture.tenant, run)
            .await
            .unwrap()
            .state,
        CloudRunState::Queued
    );
}
