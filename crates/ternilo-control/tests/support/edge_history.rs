use super::*;

pub(super) async fn verify(store: &ControlStore, fixture: &EdgeFixture, mapping: &MappingFixture) {
    let edge = store.edge_store();
    let events: Vec<_> = (4..1205)
        .map(|seq| SessionEvent {
            seq,
            occurred_at_ms: fixture.now + 21 + seq,
            run_id: RunId::new("long-history"),
            kind: SessionEventKind::AssistantReasoningDelta {
                step: 1,
                delta: format!("reason-{seq}"),
            },
        })
        .collect();
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &events,
    )
    .await
    .unwrap();
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &events[1000..],
    )
    .await
    .unwrap();
    assert_eq!(
        edge.last_event_seq(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session
        )
        .await
        .unwrap(),
        Some(1204)
    );
    let tail = edge
        .events_after(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(1200),
        )
        .await
        .unwrap();
    assert_eq!(tail, events[1197..]);
    assert!(
        edge.events_after(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(1204)
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert_conflicting_event_rejected(store, fixture, mapping, &events[1000]).await;
    assert_batch_atomicity_and_deduplication(store, fixture, mapping, &events[0]).await;
}

async fn assert_conflicting_event_rejected(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    event: &SessionEvent,
) {
    let edge = store.edge_store();
    let mut changed = event.clone();
    changed.kind = SessionEventKind::AssistantReasoningDelta {
        step: 1,
        delta: "changed".to_owned(),
    };
    assert!(
        edge.merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            &[changed]
        )
        .await
        .is_err()
    );
}

async fn assert_batch_atomicity_and_deduplication(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    event: &SessionEvent,
) {
    let edge = store.edge_store();
    let next = SessionEvent {
        seq: 1205,
        ..event.clone()
    };
    let skipped = SessionEvent {
        seq: 1207,
        ..event.clone()
    };
    assert!(
        edge.merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            &[next.clone(), skipped]
        )
        .await
        .is_err()
    );
    assert_eq!(
        edge.last_event_seq(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session
        )
        .await
        .unwrap(),
        Some(1204),
        "failed batches remain atomic"
    );
    edge.merge_events(
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        &[next.clone(), next],
    )
    .await
    .unwrap();
    assert_eq!(
        edge.events_after(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            Some(1204)
        )
        .await
        .unwrap()
        .len(),
        1
    );
}
