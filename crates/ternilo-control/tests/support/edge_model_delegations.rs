use super::*;
use ternilo_protocol::{AutomatedInputSource, ModelRunOrigin, UserMessageSource};

pub(super) async fn contract(
    store: &ControlStore,
    fixture: &EdgeFixture,
    mapping: &MappingFixture,
    node: &NodePrincipal,
    original: &NodeModelRequest,
) {
    let child = edge_provenance::subagent_mapping(
        store,
        fixture,
        &format!("unique-child-{}", original.run_id.as_str()),
        &mapping.browser_session,
    )
    .await;
    let nested = edge_provenance::subagent_mapping(
        store,
        fixture,
        &format!("unique-nested-{}", original.run_id.as_str()),
        &child.browser_session,
    )
    .await;
    let mut request = original.clone();
    let mut parent_run = original.run_id.clone();
    for (index, session) in [&child.node_session, &nested.node_session]
        .into_iter()
        .enumerate()
    {
        let run = RunId::new(format!("unique-child-run-{index}"));
        let event = SessionEvent {
            seq: 0,
            occurred_at_ms: fixture.now,
            run_id: run.clone(),
            kind: SessionEventKind::UserMessage {
                content: "delegated task".into(),
                provenance: Some(InputProvenance {
                    input_id: SubmissionId::new(format!("unique-child-input-{index}")),
                    run_id: None,
                    author: InputAuthor::Automation {
                        source: AutomatedInputSource::Subagent,
                    },
                }),
                display_content: None,
                source: Some(UserMessageSource::Subagent {
                    parent_run_id: parent_run,
                }),
                references: vec![],
                attachments: vec![],
            },
        };
        request.session_id = session.clone();
        request.run_id = run.clone();
        request.request.run_id = run.clone();
        request.run_origins.insert(
            0,
            ModelRunOrigin::Subagent {
                session_id: session.clone(),
                input_seq: 0,
            },
        );
        assert!(
            !store
                .node_model_request_registered(node, &request)
                .await
                .unwrap()
        );
        store
            .edge_store()
            .merge_events(&fixture.tenant_a, &fixture.executor_a, session, &[event])
            .await
            .unwrap();
        assert!(
            store
                .node_model_request_registered(node, &request)
                .await
                .unwrap()
        );
        let mut tx = store.database().begin().await.unwrap();
        let principal = store
            .authorize_node_model_in(&mut tx, node, &request)
            .await
            .unwrap();
        assert_eq!(principal.actor_user_id, fixture.alice.user_id);
        assert_eq!(principal.run_id, run);
        tx.commit().await.unwrap();
        parent_run = run;
    }
    let mut forged = request.clone();
    forged.run_origins.reverse();
    assert_denied(store, node, &forged).await;
    forged = request.clone();
    forged.run_origins.clear();
    assert_denied(store, node, &forged).await;
    forged = request.clone();
    forged.run_id = RunId::new("another-invocation");
    forged.request.run_id = forged.run_id.clone();
    assert_denied(store, node, &forged).await;
    forged = request.clone();
    forged.provenance.as_mut().unwrap().run_id = Some(RunId::new("another-parent"));
    assert_denied(store, node, &forged).await;
}
