#[path = "edge_reenrollment.rs"]
mod reenrollment;

use ternilo_control::{ControlStore, EdgeStore};
use ternilo_protocol::{
    AutomatedInputSource, ErrorCode, InputAuthor, InputProvenance, RunId, SessionEvent,
    SessionEventKind, SessionId, SubmissionDelivery, SubmissionId, TurnFinishReason,
    UserMessageSource,
};

use super::{EdgeFixture, MappingFixture};

fn accepted(fixture: &EdgeFixture) -> InputProvenance {
    InputProvenance {
        run_id: None,
        input_id: SubmissionId::new("accepted-shared-input"),
        author: InputAuthor::Account {
            user_id: fixture.bob.user_id.clone(),
            username: fixture.bob.username.clone(),
        },
    }
}

fn message(seq: u64, provenance: Option<InputProvenance>, fixture: &EdgeFixture) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: fixture.now + 1_000 + seq,
        run_id: RunId::new("provenance-run"),
        kind: SessionEventKind::UserMessage {
            source: provenance
                .as_ref()
                .map(|value| UserMessageSource::Submission {
                    regenerate_from: None,
                    submission_id: value.input_id.clone(),
                    created_at_ms: fixture.now + 1_000,
                    delivery: SubmissionDelivery::Queue,
                    skill_name: None,
                }),
            provenance,
            content: "An accepted shared task".to_owned(),
            display_content: None,
            references: Vec::new(),
            attachments: Vec::new(),
        },
    }
}

fn events(fixture: &EdgeFixture) -> Vec<SessionEvent> {
    let mut events = vec![SessionEvent {
        seq: 0,
        occurred_at_ms: fixture.now + 1_000,
        run_id: RunId::new("provenance-run"),
        kind: SessionEventKind::TurnStarted,
    }];
    for (offset, provenance) in [
        Some(accepted(fixture)),
        Some(InputProvenance {
            run_id: None,
            input_id: SubmissionId::new("local-input"),
            author: InputAuthor::Local,
        }),
        Some(InputProvenance {
            run_id: None,
            input_id: SubmissionId::new("automated-input"),
            author: InputAuthor::Automation {
                source: AutomatedInputSource::Subagent,
            },
        }),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        events.push(message(
            u64::try_from(offset).unwrap() + 1,
            provenance,
            fixture,
        ));
    }
    events.push(SessionEvent {
        seq: 5,
        occurred_at_ms: fixture.now + 1_005,
        run_id: RunId::new("provenance-run"),
        kind: SessionEventKind::TurnFinished {
            answer: "Shared task completed".to_owned(),
            finish_reason: TurnFinishReason::Completed,
        },
    });
    events
}

async fn mapping(
    store: &ControlStore,
    fixture: &EdgeFixture,
    name: &str,
    parent: &SessionId,
) -> MappingFixture {
    let mapping = MappingFixture {
        browser_session: SessionId::new(format!("provenance-browser-{name}")),
        node_session: SessionId::new(format!("provenance-node-{name}")),
    };
    store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(&mapping.browser_session),
            super::metadata(
                "Provenance contract",
                Some(parent.clone()),
                fixture.now + 1_000,
            ),
            fixture.now + 1_000,
        )
        .await
        .unwrap();
    mapping
}

#[expect(
    clippy::too_many_lines,
    reason = "Verify author acceptance, rollback, immutable facts, tenant isolation, and copied history as one contract."
)]
pub(super) async fn contract(store: &ControlStore, fixture: &EdgeFixture, parent: &MappingFixture) {
    let origin = mapping(store, fixture, "origin", &parent.browser_session).await;
    let edge = store.edge_store();
    let provenance = accepted(fixture);
    let mut rolled_back = provenance.clone();
    rolled_back.input_id = SubmissionId::new("rolled-back-input");
    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut transaction,
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        None,
        &rolled_back,
        fixture.now + 1_000,
    )
    .await
    .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &SessionId::new("provenance-node-origin"),
            &rolled_back
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );

    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut transaction,
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        None,
        &provenance,
        fixture.now + 1_001,
    )
    .await
    .unwrap();
    let pending = InputProvenance {
        input_id: SubmissionId::new("accepted-before-restart"),
        ..provenance.clone()
    };
    EdgeStore::record_input_provenance_in_transaction(
        &mut transaction,
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        None,
        &pending,
        fixture.now + 1_001,
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    edge.verify_input_provenance(
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        &provenance,
    )
    .await
    .unwrap();
    assert!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &origin.node_session)
            .await
            .unwrap()
            .is_empty(),
        "author proof is committed before any Node reply or event receipt"
    );

    queue_contract(store, fixture, &origin.node_session).await;
    let mut changed = provenance.clone();
    changed.author = InputAuthor::Account {
        user_id: fixture.alice.user_id.clone(),
        username: fixture.alice.username.clone(),
    };
    let mut missing = provenance.clone();
    missing.input_id = SubmissionId::new("never-accepted-input");
    for forged in [&changed, &missing, &rolled_back] {
        assert_eq!(
            edge.verify_input_provenance(
                &fixture.tenant_a,
                &fixture.executor_a,
                &origin.node_session,
                forged
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PolicyDenied
        );
        if forged.input_id != provenance.input_id {
            continue;
        }
        let batch = [
            events(fixture).remove(0),
            message(1, Some(forged.clone()), fixture),
        ];
        assert_eq!(
            edge.merge_events(
                &fixture.tenant_a,
                &fixture.executor_a,
                &origin.node_session,
                &batch
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PolicyDenied
        );
        assert!(
            edge.events(&fixture.tenant_a, &fixture.executor_a, &origin.node_session)
                .await
                .unwrap()
                .is_empty(),
            "the event preceding a forged author rolls back with the entire batch"
        );
    }

    for (session, replacement) in [
        (&origin.node_session, &changed),
        (&parent.node_session, &provenance),
    ] {
        let mut transaction = store
            .database()
            .tenant_transaction(&fixture.tenant_a)
            .await
            .unwrap();
        assert_eq!(
            EdgeStore::record_input_provenance_in_transaction(
                &mut transaction,
                &fixture.tenant_a,
                &fixture.executor_a,
                session,
                None,
                replacement,
                fixture.now + 1_002,
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        transaction.rollback().await.unwrap();
    }
    for (tenant, executor) in [
        (&fixture.tenant_a, &fixture.executor_b),
        (&fixture.tenant_b, &fixture.executor_a),
        (&fixture.tenant_b, &fixture.executor_b),
    ] {
        assert_eq!(
            edge.verify_input_provenance(tenant, executor, &origin.node_session, &provenance)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PolicyDenied,
            "a different tenant or Node cannot borrow accepted author proof"
        );
    }
    let mut omitted = message(1, Some(provenance.clone()), fixture);
    let SessionEventKind::UserMessage {
        provenance: author, ..
    } = &mut omitted.kind
    else {
        unreachable!()
    };
    *author = None;
    let downgraded = InputProvenance {
        author: InputAuthor::Local,
        ..provenance.clone()
    };
    for forged in [omitted, message(1, Some(downgraded), fixture)] {
        let batch = [events(fixture).remove(0), forged];
        assert_eq!(
            edge.merge_events(
                &fixture.tenant_a,
                &fixture.executor_a,
                &origin.node_session,
                &batch
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PolicyDenied
        );
        assert!(
            edge.events(&fixture.tenant_a, &fixture.executor_a, &origin.node_session)
                .await
                .unwrap()
                .is_empty()
        );
    }
    let unrelated = mapping(store, fixture, "unrelated", &parent.browser_session).await;
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &unrelated.node_session,
            &provenance
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        edge.merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &unrelated.node_session,
            &events(fixture)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );
    assert!(
        edge.events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &unrelated.node_session
        )
        .await
        .unwrap()
        .is_empty()
    );
    let expected = events(fixture);
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        &expected,
    )
    .await
    .unwrap();
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin.node_session,
        &expected,
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &origin.node_session)
            .await
            .unwrap(),
        expected,
        "Local, automation, and historical unknown authors must remain distinct from platform accounts"
    );
    let child = mapping(store, fixture, "fork", &origin.browser_session).await;
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &child.node_session,
        &expected,
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &child.node_session)
            .await
            .unwrap(),
        expected,
        "copying a legitimate history preserves original author identities"
    );
    followup_contract(store, fixture, parent).await;
    reenrollment::contract(store, fixture).await;
}

pub(super) async fn assert_reopened(store: &ControlStore, fixture: &EdgeFixture) {
    reenrollment::assert_reopened(store, fixture).await;
    let edge = store.edge_store();
    let copy = SessionId::new("provenance-node-target-copy");
    let provenance = followup(fixture);
    edge.verify_input_provenance(&fixture.tenant_a, &fixture.executor_a, &copy, &provenance)
        .await
        .unwrap();
    let delivered = message(0, Some(provenance), fixture);
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &copy,
        std::slice::from_ref(&delivered),
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &copy)
            .await
            .unwrap(),
        vec![delivered]
    );

    edge.verify_input_provenance(
        &fixture.tenant_a,
        &fixture.executor_a,
        &SessionId::new("provenance-node-origin"),
        &accepted(fixture),
    )
    .await
    .unwrap();
    for name in ["origin", "fork"] {
        assert_eq!(
            edge.events(
                &fixture.tenant_a,
                &fixture.executor_a,
                &SessionId::new(format!("provenance-node-{name}"))
            )
            .await
            .unwrap(),
            events(fixture)
        );
    }
    let pending = InputProvenance {
        input_id: SubmissionId::new("accepted-before-restart"),
        ..accepted(fixture)
    };
    edge.verify_input_provenance(
        &fixture.tenant_a,
        &fixture.executor_a,
        &SessionId::new("provenance-node-origin"),
        &pending,
    )
    .await
    .unwrap();
    let delayed_message = message(6, Some(pending), fixture);
    let origin = SessionId::new("provenance-node-origin");
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &origin,
        std::slice::from_ref(&delayed_message),
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &origin)
            .await
            .unwrap()
            .last(),
        Some(&delayed_message),
        "a Node can deliver accepted input for the first time after a Control restart"
    );
    let rolled_back = InputProvenance {
        input_id: SubmissionId::new("rolled-back-input"),
        ..accepted(fixture)
    };
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &SessionId::new("provenance-node-origin"),
            &rolled_back
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );
}

pub(super) async fn subagent_mapping(
    store: &ControlStore,
    fixture: &EdgeFixture,
    name: &str,
    parent: &SessionId,
) -> MappingFixture {
    let mapping = MappingFixture {
        browser_session: SessionId::new(format!("provenance-browser-{name}")),
        node_session: SessionId::new(format!("provenance-node-{name}")),
    };
    let mut metadata = super::metadata(
        "Human child followup",
        Some(parent.clone()),
        fixture.now + 1_000,
    );
    metadata.subagent = Some(ternilo_protocol::SubagentSessionMetadata {
        subagent_id: ternilo_protocol::SubagentId::new(name),
        provider: "in-process".to_owned(),
        transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
    });
    store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(&mapping.browser_session),
            metadata,
            fixture.now + 1_000,
        )
        .await
        .unwrap();
    mapping
}

fn followup(fixture: &EdgeFixture) -> InputProvenance {
    InputProvenance {
        input_id: SubmissionId::new("accepted-human-child-followup"),
        ..accepted(fixture)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise a targeted child, sibling isolation, copied history, and durable proof after parent deletion."
)]
async fn followup_contract(store: &ControlStore, fixture: &EdgeFixture, parent: &MappingFixture) {
    let owner = mapping(store, fixture, "followup-owner", &parent.browser_session).await;
    let edge = store.edge_store();
    let provenance = followup(fixture);
    let target = ternilo_protocol::SubagentId::new("target-child");
    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut transaction,
        &fixture.tenant_a,
        &fixture.executor_a,
        &owner.node_session,
        Some(&target),
        &provenance,
        fixture.now + 1_001,
    )
    .await
    .unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &owner.node_session,
            &provenance
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied,
        "a child followup cannot become an owner conversation message"
    );
    let child = subagent_mapping(store, fixture, "target-child", &owner.browser_session).await;
    let sibling = subagent_mapping(store, fixture, "other-child", &owner.browser_session).await;
    edge.verify_input_provenance(
        &fixture.tenant_a,
        &fixture.executor_a,
        &child.node_session,
        &provenance,
    )
    .await
    .unwrap();
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &sibling.node_session,
            &provenance
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );
    assert_eq!(
        edge.verify_input_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            &child.node_session,
            &accepted(fixture)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied,
        "subagent contexts do not inherit another conversation's human input proof"
    );
    let delivered = message(0, Some(provenance.clone()), fixture);
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &child.node_session,
        std::slice::from_ref(&delivered),
    )
    .await
    .unwrap();
    let copy = mapping(store, fixture, "target-copy", &child.browser_session).await;
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &copy.node_session,
        std::slice::from_ref(&delivered),
    )
    .await
    .unwrap();
    let mut transaction = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    assert_eq!(
        EdgeStore::record_session_provenance_context_in_transaction(
            &mut transaction,
            &fixture.tenant_a,
            &fixture.executor_a,
            &copy.node_session,
            Some(&sibling.node_session),
            None
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    transaction.rollback().await.unwrap();
    for deleted in [&owner, &child] {
        store
            .delete_edge_session_mapping(
                &fixture.alice,
                &fixture.tenant_a,
                &deleted.browser_session,
                &deleted.node_session,
                fixture.now + 1_100,
            )
            .await
            .unwrap();
    }
    edge.verify_input_provenance(
        &fixture.tenant_a,
        &fixture.executor_a,
        &copy.node_session,
        &provenance,
    )
    .await
    .unwrap();
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &copy.node_session,
        std::slice::from_ref(&delivered),
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events(&fixture.tenant_a, &fixture.executor_a, &copy.node_session)
            .await
            .unwrap(),
        vec![delivered]
    );
}

async fn queue_contract(store: &ControlStore, fixture: &EdgeFixture, session: &SessionId) {
    let edge = store.edge_store();
    let accepted = accepted(fixture);
    let mut submission = ternilo_protocol::SessionSubmission {
        id: accepted.input_id.clone(),
        run_id: RunId::new("accepted-queued-run"),
        content: ternilo_protocol::SubmissionContent::Prompt {
            input: "Shared task".to_owned(),
        },
        provenance: Some(accepted),
        references: Vec::new(),
        attachments: Vec::new(),
        placement: ternilo_protocol::SubmissionPlacement::Queued,
        created_at_ms: fixture.now + 1_000,
        updated_at_ms: fixture.now + 1_000,
    };
    edge.verify_submission_provenance(&fixture.tenant_a, &fixture.executor_a, session, &submission)
        .await
        .unwrap();
    submission.provenance = None;
    assert_eq!(
        edge.verify_submission_provenance(
            &fixture.tenant_a,
            &fixture.executor_a,
            session,
            &submission
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::PolicyDenied
    );
    submission.id = SubmissionId::new("historical-unknown-submission");
    edge.verify_submission_provenance(&fixture.tenant_a, &fixture.executor_a, session, &submission)
        .await
        .unwrap();
    assert!(submission.provenance.is_none());
}
