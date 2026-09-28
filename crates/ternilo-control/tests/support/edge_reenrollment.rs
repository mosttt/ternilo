use ternilo_control::{ControlStore, EdgeStore, NodeSessionResource, NodeWorkspaceResource};
use ternilo_protocol::{
    ErrorCode, InputProvenance, RunId, SessionEvent, SessionId, SessionSubmission,
    SubmissionContent, SubmissionId, SubmissionPlacement, WorkspaceId,
};
use ternilo_transport::ExecutorId;

use super::{EdgeFixture, accepted, message};

fn provenance(fixture: &EdgeFixture, id: &str) -> InputProvenance {
    InputProvenance {
        input_id: SubmissionId::new(id),
        ..accepted(fixture)
    }
}

fn pending(fixture: &EdgeFixture) -> SessionSubmission {
    let provenance = provenance(fixture, "old-route-pending-input");
    SessionSubmission {
        id: provenance.input_id.clone(),
        provenance: Some(provenance),
        run_id: RunId::new("pending-run"),
        content: SubmissionContent::Prompt {
            input: "A pending task retained on the computer".to_owned(),
        },
        references: Vec::new(),
        attachments: Vec::new(),
        placement: SubmissionPlacement::Queued,
        created_at_ms: fixture.now + 2_000,
        updated_at_ms: fixture.now + 2_000,
    }
}

fn history(fixture: &EdgeFixture) -> Vec<SessionEvent> {
    vec![message(
        0,
        Some(provenance(fixture, "old-route-consumed-input")),
        fixture,
    )]
}

async fn register(store: &ControlStore, fixture: &EdgeFixture, id: &str) -> ExecutorId {
    let project = store
        .list_projects(&fixture.alice, &fixture.tenant_a)
        .await
        .unwrap()
        .remove(0);
    let executor = super::super::enroll(
        store,
        &fixture.alice,
        &fixture.tenant_a,
        &project.project_id,
        id,
        fixture.now + 2_000,
    )
    .await;
    store
        .edge_store()
        .register_executor(
            &fixture.tenant_a,
            &super::super::hello(executor.clone()),
            fixture.now + 2_001,
        )
        .await
        .unwrap();
    store
        .discover_owned_node_resources(
            &fixture.alice,
            &fixture.tenant_a,
            &executor,
            &[NodeWorkspaceResource {
                workspace_id: WorkspaceId::new("retained-local-workspace"),
                title: "Retained local project".to_owned(),
                registered: true,
            }],
            &[NodeSessionResource {
                session_id: SessionId::new("retained-local-session"),
                workspace_id: WorkspaceId::new("retained-local-workspace"),
                metadata: super::super::metadata(
                    "Retained conversation",
                    None,
                    fixture.now + 2_000,
                ),
            }],
            fixture.now + 2_002,
        )
        .await
        .unwrap();
    executor
}

#[expect(
    clippy::too_many_lines,
    reason = "Re-enroll retained Local data under a new Node identity while checking raw replay and public author projection."
)]
pub(super) async fn contract(store: &ControlStore, fixture: &EdgeFixture) {
    let old = register(store, fixture, "provenance-reenroll-old").await;
    let session = SessionId::new("retained-local-session");
    let edge = store.edge_store();
    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    for id in ["old-route-consumed-input", "old-route-pending-input"] {
        EdgeStore::record_input_provenance_in_transaction(
            &mut transaction,
            &fixture.tenant_a,
            &old,
            &session,
            None,
            &provenance(fixture, id),
            fixture.now + 2_003,
        )
        .await
        .unwrap();
    }
    transaction.commit().await.unwrap();
    let raw = history(fixture);
    edge.merge_events(&fixture.tenant_a, &old, &session, &raw)
        .await
        .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &old, &session)
            .await
            .unwrap(),
        raw
    );
    let mut queued = pending(fixture);
    edge.project_submission_provenance(&fixture.tenant_a, &old, &session, &mut queued)
        .await
        .unwrap();
    assert_eq!(queued, pending(fixture));
    store
        .revoke_owned_executor(&fixture.alice, &fixture.tenant_a, &old, fixture.now + 2_004)
        .await
        .unwrap();
    let replacement = register(store, fixture, "provenance-reenroll-new").await;
    for _ in 0..2 {
        edge.merge_events(&fixture.tenant_a, &replacement, &session, &raw)
            .await
            .unwrap();
    }
    let mut public = raw.clone();
    edge.project_event_provenance(&fixture.tenant_a, &replacement, &session, &mut public)
        .await
        .unwrap();
    let ternilo_protocol::SessionEventKind::UserMessage { provenance, .. } = &public[0].kind else {
        unreachable!()
    };
    assert!(provenance.is_none());
    assert_eq!(
        edge.events(&fixture.tenant_a, &replacement, &session)
            .await
            .unwrap(),
        public
    );
    let mut queued = pending(fixture);
    edge.project_submission_provenance(&fixture.tenant_a, &replacement, &session, &mut queued)
        .await
        .unwrap();
    let mut expected_queue = pending(fixture);
    expected_queue.provenance = None;
    assert_eq!(
        queued, expected_queue,
        "queue identity, content, attachments, and placement remain intact"
    );
    assert!(
        pending(fixture).provenance.is_some(),
        "projecting a copy cannot rewrite the computer's durable input"
    );

    let mut changed = raw.clone();
    let ternilo_protocol::SessionEventKind::UserMessage {
        provenance: Some(author),
        ..
    } = &mut changed[0].kind
    else {
        unreachable!()
    };
    author.author = ternilo_protocol::InputAuthor::Account {
        user_id: fixture.alice.user_id.clone(),
        username: fixture.alice.username.clone(),
    };
    assert_eq!(
        edge.merge_events(&fixture.tenant_a, &replacement, &session, &changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied,
        "unverified raw author metadata is still immutable for replay"
    );
    let mut changed = raw.clone();
    let ternilo_protocol::SessionEventKind::UserMessage { content, .. } = &mut changed[0].kind
    else {
        unreachable!()
    };
    "Rewritten historical content".clone_into(content);
    assert_eq!(
        edge.merge_events(&fixture.tenant_a, &replacement, &session, &changed)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PolicyDenied
    );
    edge.merge_events(&fixture.tenant_a, &replacement, &session, &raw)
        .await
        .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &old, &session)
            .await
            .unwrap(),
        raw,
        "the original route retains its verified labels even after revocation"
    );
}

pub(super) async fn assert_reopened(store: &ControlStore, fixture: &EdgeFixture) {
    let edge = store.edge_store();
    let session = SessionId::new("retained-local-session");
    let replacement = ExecutorId::new("provenance-reenroll-new");
    let raw = history(fixture);
    edge.merge_events(&fixture.tenant_a, &replacement, &session, &raw)
        .await
        .unwrap();
    let mut public = raw.clone();
    edge.project_event_provenance(&fixture.tenant_a, &replacement, &session, &mut public)
        .await
        .unwrap();
    assert_ne!(public, raw);
    assert_eq!(
        edge.events(&fixture.tenant_a, &replacement, &session)
            .await
            .unwrap(),
        public
    );
    let mut queued = pending(fixture);
    edge.project_submission_provenance(&fixture.tenant_a, &replacement, &session, &mut queued)
        .await
        .unwrap();
    assert!(queued.provenance.is_none());
    assert_eq!(
        edge.events(
            &fixture.tenant_a,
            &ExecutorId::new("provenance-reenroll-old"),
            &session
        )
        .await
        .unwrap(),
        raw
    );
}
