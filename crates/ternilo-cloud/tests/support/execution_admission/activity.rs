use super::*;

#[expect(
    clippy::too_many_lines,
    reason = "Verify event projection, parked admission denial and dependency cleanup together on both databases."
)]
pub(super) async fn execution_activity_projection(fixture: &mut Fixture) {
    use ternilo_protocol::{ExecutionActivityPhase, SessionExecutionPhase};
    let worker = "capacity-worker";
    let session = fixture.session("projected-activity").await;
    let run_id = fixture.submit(&session, "projected-activity-run").await;
    let run = fixture.start(&run_id, worker).await;
    let current = fixture
        .cloud
        .get_session(&fixture.tenant, &session.session_id)
        .await
        .unwrap();
    assert_eq!(
        current.execution.unwrap().phase,
        SessionExecutionPhase::Running
    );
    let child = fixture.child(&run, "projected-child", worker).await;
    let empty_child = SessionId::new("projected-empty-child");
    let child_metadata = SubagentSessionMetadata {
        subagent_id: SubagentId::new("projected-empty-child"),
        provider: "in-process".to_owned(),
        transcript_kind: SubagentTranscriptKind::Conversation,
    };
    let now = fixture.tick();
    fixture
        .cloud
        .create_subagent_for_worker(
            worker,
            &run,
            &empty_child,
            &child_metadata,
            "Empty child",
            now,
        )
        .await
        .unwrap();
    fixture.park(&run, &child, worker).await;
    let now = fixture.tick();
    let error = fixture
        .cloud
        .create_subagent_for_worker(
            worker,
            &run,
            &SessionId::new("parked-child"),
            &child_metadata,
            "Parked child",
            now,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
    let mut child_spec = run.claim.spec.clone();
    child_spec.metadata.session_id = empty_child.clone();
    child_spec.metadata.run_id = RunId::new("parked-child-run");
    let error = fixture
        .cloud
        .enqueue_subagent_for_worker(
            worker,
            &run,
            &empty_child,
            &child_spec,
            "Blocked input",
            3,
            now,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
    assert_eq!(
        fixture
            .cloud
            .subagent_run_for_worker(worker, &run, &child.session_id, &child.run_id, now)
            .await
            .unwrap()
            .unwrap()
            .state,
        CloudRunState::Queued,
        "a parked parent can still read its already accepted dependency"
    );
    let now = fixture.tick();
    let event = SessionEvent {
        seq: current.last_seq.unwrap() + 1,
        occurred_at_ms: now,
        run_id: run_id.clone(),
        kind: SessionEventKind::ExecutionActivityChanged {
            phase: ExecutionActivityPhase::WaitingForCapacity,
        },
    };
    fixture
        .cloud
        .append_event(&run, worker, &event, now)
        .await
        .unwrap();
    fixture
        .cloud
        .append_event(&run, worker, &event, now)
        .await
        .unwrap();
    assert_eq!(fixture.phase(&run).await, ExecutionPhase::Parked);
    let visible = fixture
        .cloud
        .list_accessible_sessions(&fixture.tenant, &fixture.actor.user_id, 500)
        .await
        .unwrap();
    let projected = visible
        .iter()
        .find(|value| value.session_id == session.session_id)
        .unwrap()
        .execution
        .as_ref()
        .unwrap();
    assert_eq!(projected.run_id, run_id);
    assert_eq!(
        projected.phase,
        SessionExecutionPhase::WaitingForCapacity,
        "the workbench projection follows the acknowledged event even before the resume RPC changes admission phase"
    );
    let now = fixture.tick();
    assert_eq!(
        fixture
            .cloud
            .cancel_subagent_for_worker(worker, &run, &child.session_id, &child.run_id, now)
            .await
            .unwrap(),
        CloudRunState::Cancelled,
        "a parked parent retains cleanup authority for its accepted child"
    );
    assert!(matches!(
        fixture.resume(&run, worker).await,
        RunAdmission::Ready { .. }
    ));
    fixture.finish(&run, worker).await;
    assert!(
        fixture
            .cloud
            .get_session(&fixture.tenant, &session.session_id)
            .await
            .unwrap()
            .execution
            .is_none()
    );
}
