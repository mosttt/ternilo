use ternilo_cloud::{CloudWorkerIdentity, RunAdmission, TerminalState};
use ternilo_protocol::AcceptedSubagentRun;

use super::*;

async fn reply(
    service: &Service,
    bearer: &str,
    identity: &CloudWorkerIdentity,
    request: WorkerRequest,
) -> WorkerReply {
    let mut response = worker_request(service, bearer, identity, request).await;
    assert_eq!(response.status_code, Some(StatusCode::OK));
    response.take_json().await.unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the real HTTP park, child execution, pending resume and successful readmission sequence together."
)]
pub(super) async fn verify_capacity_handoff(
    service: &Service,
    bearer: &str,
    identity: &CloudWorkerIdentity,
    parent: &StartedRun,
    child: &AcceptedSubagentRun,
) {
    assert!(matches!(
        reply(service, bearer, identity, WorkerRequest::ClaimRun).await,
        WorkerReply::Claim { claim: None }
    ));
    let mut forged = child.clone();
    forged.run_id = parent.claim.run_id.clone();
    let denied = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::ParkRun {
            run: parent.into(),
            activity_revision: 1,
            dependencies: vec![forged],
        },
    )
    .await;
    assert_eq!(denied.status_code, Some(StatusCode::FORBIDDEN));
    let parked = reply(
        service,
        bearer,
        identity,
        WorkerRequest::ParkRun {
            run: parent.into(),
            activity_revision: 1,
            dependencies: vec![child.clone()],
        },
    )
    .await;
    let WorkerReply::Parked { revision } = parked else {
        panic!("parent did not park")
    };
    assert_eq!(revision, 1);
    assert!(matches!(
        reply(
            service,
            bearer,
            identity,
            WorkerRequest::ParkRun {
                run: parent.into(),
                activity_revision: 1,
                dependencies: vec![child.clone()],
            }
        )
        .await,
        WorkerReply::Parked { revision: 1 }
    ));
    let stale = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::ResumeRun {
            run: parent.into(),
            activity_revision: 2,
            parked_revision: revision + 1,
        },
    )
    .await;
    assert_eq!(stale.status_code, Some(StatusCode::CONFLICT));
    let claimed = reply(service, bearer, identity, WorkerRequest::ClaimRun).await;
    let WorkerReply::Claim { claim: Some(claim) } = claimed else {
        panic!("child did not acquire released foreground capacity")
    };
    assert_eq!(claim.run_id, child.run_id);
    let started = reply(
        service,
        bearer,
        identity,
        WorkerRequest::StartRun {
            run: (&claim).into(),
        },
    )
    .await;
    let WorkerReply::Started { run: Some(started) } = started else {
        panic!("child did not start")
    };
    for _ in 0..2 {
        assert!(matches!(
            reply(
                service,
                bearer,
                identity,
                WorkerRequest::ResumeRun {
                    run: parent.into(),
                    activity_revision: 2,
                    parked_revision: revision,
                }
            )
            .await,
            WorkerReply::Resumed {
                admission: RunAdmission::Pending
            }
        ));
    }
    let live_cleanup = worker_request(
        service,
        bearer,
        identity,
        WorkerRequest::ReleaseResident {
            run: (&started).into(),
            worker_generation: identity.generation,
        },
    )
    .await;
    assert_eq!(live_cleanup.status_code, Some(StatusCode::FORBIDDEN));
    assert!(matches!(
        reply(
            service,
            bearer,
            identity,
            WorkerRequest::FinishRun {
                run: (&started).into(),
                terminal: TerminalState::Cancelled,
                outcome: None,
                error: None,
            }
        )
        .await,
        WorkerReply::Unit
    ));
    verify_retired_cleanup(service, bearer, identity, &started).await;
    let resumed = reply(
        service,
        bearer,
        identity,
        WorkerRequest::ResumeRun {
            run: parent.into(),
            activity_revision: 2,
            parked_revision: revision,
        },
    )
    .await;
    let WorkerReply::Resumed {
        admission: RunAdmission::Ready { admission_epoch },
    } = resumed
    else {
        panic!("parent did not regain foreground capacity")
    };
    assert!(admission_epoch > 1);
    let repeated = reply(
        service,
        bearer,
        identity,
        WorkerRequest::ResumeRun {
            run: parent.into(),
            activity_revision: 2,
            parked_revision: revision,
        },
    )
    .await;
    assert!(
        matches!(repeated, WorkerReply::Resumed { admission: RunAdmission::Ready { admission_epoch: epoch } } if epoch == admission_epoch)
    );
}

async fn verify_retired_cleanup(
    service: &Service,
    bearer: &str,
    identity: &CloudWorkerIdentity,
    finished: &StartedRun,
) {
    let mut wrong_fence = RunLease::from(finished);
    wrong_fence.writer_fencing_token += 1;
    for request in [
        WorkerRequest::ReleaseResident {
            run: finished.into(),
            worker_generation: identity.generation + 1,
        },
        WorkerRequest::ReleaseResident {
            run: wrong_fence,
            worker_generation: identity.generation,
        },
    ] {
        assert_eq!(
            worker_request(service, bearer, identity, request)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
    for _ in 0..2 {
        assert!(matches!(
            reply(
                service,
                bearer,
                identity,
                WorkerRequest::ReleaseResident {
                    run: finished.into(),
                    worker_generation: identity.generation,
                }
            )
            .await,
            WorkerReply::Unit
        ));
    }
    for request in [
        WorkerRequest::RenewRun {
            run: finished.into(),
        },
        WorkerRequest::ResumeRun {
            run: finished.into(),
            activity_revision: 3,
            parked_revision: 1,
        },
    ] {
        assert_eq!(
            worker_request(service, bearer, identity, request)
                .await
                .status_code,
            Some(StatusCode::FORBIDDEN)
        );
    }
}
