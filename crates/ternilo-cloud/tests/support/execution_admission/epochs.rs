use super::*;
use ternilo_cloud::CloudRunClaim;

pub(super) async fn workspace_epochs_are_durable_and_tickets_are_bound_to_the_run(
    fixture: &mut Fixture,
) {
    let parent = fixture.session("epoch-parent").await;
    let run = fixture.submit(&parent, "epoch-parent-run").await;
    let parent_run = fixture.start(&run, "capacity-worker").await;
    let first = parent_run.claim.workspace_use.occupation_epoch;
    assert_eq!(first, 1);
    let child = fixture
        .child(&parent_run, "epoch-child", "capacity-worker")
        .await;
    let child_run = fixture.start(&child.run_id, "capacity-worker").await;
    assert_eq!(child_run.claim.workspace_use.occupation_epoch, first);
    fixture.finish(&parent_run, "capacity-worker").await;
    let followup = fixture.submit(&parent, "epoch-followup").await;
    let followup = fixture.start(&followup, "capacity-worker").await;
    assert_eq!(
        followup.claim.workspace_use.occupation_epoch, first,
        "a resident sibling keeps the same physical occupation across human turns"
    );
    fixture.finish(&followup, "capacity-worker").await;
    fixture.finish(&child_run, "capacity-worker").await;

    let independent = fixture
        .session_in_workspace("epoch-independent", &parent.workspace_id)
        .await;
    let run = fixture.submit(&independent, "epoch-independent-run").await;
    let claim = claim_next(fixture, &run).await;
    assert_eq!(claim.workspace_use.occupation_epoch, first + 1);
    rejected_tickets_do_not_start_or_consume_the_valid_claim(fixture, &claim).await;
    let running = fixture.start_claim(claim, "capacity-worker").await;
    fixture.finish(&running, "capacity-worker").await;
    let now = fixture.tick();
    fixture
        .cloud
        .delete_session(
            &fixture.tenant,
            &independent.session_id,
            &fixture.owner.user_id,
            now,
        )
        .await
        .unwrap();

    let next = fixture
        .submit(&parent, "epoch-after-history-deletion")
        .await;
    let running = fixture.start(&next, "capacity-worker").await;
    assert_eq!(
        running.claim.workspace_use.occupation_epoch,
        first + 2,
        "deleting visible history cannot reset the workspace high-water mark"
    );
    fixture.finish(&running, "capacity-worker").await;
    let other = fixture.session("epoch-other-workspace").await;
    let run = fixture.submit(&other, "epoch-other-run").await;
    let running = fixture.start(&run, "capacity-worker").await;
    assert_eq!(running.claim.workspace_use.occupation_epoch, 1);
    fixture.finish(&running, "capacity-worker").await;
}

async fn claim_next(fixture: &mut Fixture, run: &RunId) -> CloudRunClaim {
    let now = fixture.tick();
    let claim = fixture
        .cloud
        .claim_run("capacity-worker", LEASE, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.run_id, *run);
    claim
}

async fn rejected_tickets_do_not_start_or_consume_the_valid_claim(
    fixture: &mut Fixture,
    claim: &CloudRunClaim,
) {
    let mut invalid = Vec::new();
    let mut stale_epoch = claim.clone();
    stale_epoch.workspace_use.occupation_epoch -= 1;
    invalid.push(stale_epoch);
    let mut wrong_workspace = claim.clone();
    wrong_workspace.workspace_use.workspace_id =
        ternilo_protocol::WorkspaceId::new("unlocked-workspace");
    invalid.push(wrong_workspace);
    let mut wrong_family = claim.clone();
    "unrelated-family".clone_into(&mut wrong_family.workspace_use.family_id);
    invalid.push(wrong_family);
    let mut wrong_root = claim.clone();
    "another-storage-root".clone_into(&mut wrong_root.workspace_use.root_id);
    invalid.push(wrong_root);
    let mut wrong_run = claim.clone();
    wrong_run.workspace_use.run_id = RunId::new("another-run");
    invalid.push(wrong_run);
    for forged in invalid {
        let now = fixture.tick();
        assert_eq!(
            fixture
                .cloud
                .start_run(forged, "capacity-worker", LEASE, now)
                .await
                .unwrap_err()
                .code,
            ternilo_protocol::ErrorCode::PolicyDenied
        );
        assert_eq!(
            fixture
                .cloud
                .get_run(&fixture.tenant, &claim.run_id)
                .await
                .unwrap()
                .state,
            CloudRunState::Leased
        );
        assert_eq!(fixture.usage("capacity-worker").await, (1, 1));
    }
}
