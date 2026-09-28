use super::*;
use ternilo_protocol::{
    ScheduleChange, ScheduleId, ScheduleModelOrigin, ScheduleRecord, ScheduleRule, SessionEvent,
    SessionEventKind,
};

pub(super) async fn contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    original: &NodeModelRequest,
) {
    let events = store.edge_store();
    let first = events
        .last_event_seq(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
        )
        .await
        .unwrap()
        .map_or(0, |seq| seq + 1);
    let scheduled_run = RunId::new("scheduled-model-run");
    let nested_run = RunId::new("nested-scheduled-model-run");
    let later = InputProvenance {
        input_id: SubmissionId::new("later-parent-model-input"),
        run_id: Some(RunId::new("later-parent-run")),
        author: InputAuthor::Account {
            user_id: fixture.bob.user_id.clone(),
            username: fixture.bob.username.clone(),
        },
    };
    let mut tx = store
        .database()
        .tenant_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    EdgeStore::record_input_provenance_in_transaction(
        &mut tx,
        &fixture.tenant_a,
        &fixture.executor_a,
        &mapping.node_session,
        None,
        &later,
        fixture.now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let history = schedule_history(
        fixture,
        first,
        original,
        &later,
        &scheduled_run,
        &nested_run,
    );
    let mut scheduled = original.clone();
    scheduled.run_id = scheduled_run.clone();
    scheduled.request.run_id = scheduled_run;
    scheduled.schedule_origins.push(ScheduleModelOrigin {
        session_id: mapping.node_session.clone(),
        created_seq: first,
        dispatched_seq: first + 2,
    });
    assert!(
        !store
            .node_model_request_registered(node, &scheduled)
            .await
            .unwrap()
    );
    events
        .merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &mapping.node_session,
            &history,
        )
        .await
        .unwrap();
    assert!(
        store
            .node_model_request_registered(node, &scheduled)
            .await
            .unwrap()
    );
    let mut tx = store.database().begin().await.unwrap();
    let principal = store
        .authorize_node_model_in(&mut tx, node, &scheduled)
        .await
        .unwrap();
    assert_eq!(principal.run_id, scheduled.run_id);
    assert_eq!(principal.actor_user_id, fixture.alice.user_id);
    assert_eq!(
        principal.snapshot.binding.beneficiary_user_id(),
        &fixture.bob.user_id
    );
    tx.commit().await.unwrap();
    assert_tampered_origins_denied(store, node, &scheduled, later, first).await;
    assert_nested_schedule_authorization(
        store, fixture, mapping, node, &scheduled, nested_run, first,
    )
    .await;
    assert_revoked_share_denies_schedule(store, fixture, mapping, node, &scheduled).await;
}

fn schedule_history(
    fixture: &EdgeFixture,
    first: u64,
    original: &NodeModelRequest,
    later: &InputProvenance,
    scheduled_run: &RunId,
    nested_run: &RunId,
) -> Vec<SessionEvent> {
    let schedule_id = ScheduleId::new("scheduled-model");
    let make_event = |offset, run_id, kind| SessionEvent {
        seq: first + offset,
        run_id,
        occurred_at_ms: fixture.now + offset,
        kind,
    };
    let creation = |id| SessionEventKind::ScheduleChanged {
        change: ScheduleChange::Create {
            schedule: ScheduleRecord {
                id,
                prompt: "scheduled model task".to_owned(),
                rule: ScheduleRule::After { after_seconds: 1 },
                scheduled_at_ms: fixture.now + 1_000,
                created_at_ms: fixture.now,
            },
        },
    };
    let dispatch = |id, run_id| SessionEventKind::ScheduleChanged {
        change: ScheduleChange::Dispatch {
            id,
            run_id: Some(run_id),
            accepted_at_ms: fixture.now + 1_000,
            next_scheduled_at_ms: None,
        },
    };
    vec![
        make_event(0, original.run_id.clone(), creation(schedule_id.clone())),
        make_event(
            1,
            later.run_id.clone().unwrap(),
            SessionEventKind::UserMessage {
                content: "later input from another account".to_owned(),
                provenance: Some(later.clone()),
                display_content: None,
                source: None,
                references: Vec::new(),
                attachments: Vec::new(),
            },
        ),
        make_event(
            2,
            RunId::new("schedule-event"),
            dispatch(schedule_id, scheduled_run.clone()),
        ),
        make_event(
            3,
            scheduled_run.clone(),
            creation(ScheduleId::new("nested-scheduled-model")),
        ),
        make_event(
            4,
            RunId::new("nested-schedule-event"),
            dispatch(
                ScheduleId::new("nested-scheduled-model"),
                nested_run.clone(),
            ),
        ),
    ]
}

async fn assert_tampered_origins_denied(
    store: &ControlStore,
    node: &NodePrincipal,
    scheduled: &NodeModelRequest,
    later: InputProvenance,
    first: u64,
) {
    let mut changed = scheduled.clone();
    changed.provenance = Some(later);
    assert_denied(store, node, &changed).await;
    changed = scheduled.clone();
    changed.schedule_origins.clear();
    assert_denied(store, node, &changed).await;
    changed = scheduled.clone();
    changed.schedule_origins[0].created_seq = first + 1;
    assert_denied(store, node, &changed).await;
    changed = scheduled.clone();
    changed.run_id = RunId::new("unregistered-occurrence");
    changed.request.run_id = changed.run_id.clone();
    assert_denied(store, node, &changed).await;
}

async fn assert_nested_schedule_authorization(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    scheduled: &NodeModelRequest,
    nested_run: RunId,
    first: u64,
) {
    let mut nested = scheduled.clone();
    nested.run_id = nested_run.clone();
    nested.request.run_id = nested_run;
    nested.schedule_origins.insert(
        0,
        ScheduleModelOrigin {
            session_id: mapping.node_session.clone(),
            created_seq: first + 3,
            dispatched_seq: first + 4,
        },
    );
    let mut tx = store.database().begin().await.unwrap();
    let principal = store
        .authorize_node_model_in(&mut tx, node, &nested)
        .await
        .unwrap();
    assert_eq!(principal.actor_user_id, fixture.alice.user_id);
    assert_eq!(principal.run_id, nested.run_id);
    tx.commit().await.unwrap();
    nested.schedule_origins.reverse();
    assert_denied(store, node, &nested).await;
}

async fn assert_revoked_share_denies_schedule(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    scheduled: &NodeModelRequest,
) {
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Session,
            mapping.browser_session.as_str(),
            &fixture.bob.user_id,
            None,
            fixture.now + 500_001,
        )
        .await
        .unwrap();
    assert_denied(store, node, scheduled).await;
    store
        .set_resource_share(
            &fixture.alice,
            &fixture.tenant_a,
            ResourceKind::Session,
            mapping.browser_session.as_str(),
            &fixture.bob.user_id,
            Some(ResourcePermissions {
                view: true,
                submit: true,
                configure: true,
                ..Default::default()
            }),
            fixture.now + 500_002,
        )
        .await
        .unwrap();
}
