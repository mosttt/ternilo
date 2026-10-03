use super::*;
use ternilo_control::PageQuery;
use ternilo_protocol::{ProviderProtocol, ProviderUsageRoute, ReportedModelUsage};

#[expect(
    clippy::too_many_lines,
    reason = "Verify copied histories, offline replay, actor provenance, stable pages and ownership on both storage backends."
)]
pub(super) async fn verify(store: &ControlStore, fixture: &EdgeFixture) {
    let source = SessionId::new("device-usage-source");
    let fork = SessionId::new("device-usage-copy");
    let original = mapping(store, fixture, &source, "Original usage").await;
    let copied = mapping(store, fixture, &fork, "Copied usage").await;
    let mut events = vec![serde_json::from_value(json!({
        "seq":0,"occurred_at_ms":fixture.now,"run_id":"device-call","type":"user_message","content":"private prompt must not appear in usage",
        "provenance":{"input_id":"unaccepted-claim","author":{"kind":"account","user_id":fixture.bob.user_id,"username":"forged-account"}}
    })).unwrap()];
    events.push(start(&source, 1, 1, fixture.now + 1));
    events.push(finish(2, 1, Some(70), None, fixture.now + 2));
    events.push(start(&source, 3, 2, fixture.now + 3));
    events.push(finish(4, 3, Some(5), Some(7), fixture.now + 4));
    events.push(start(&source, 5, 3, fixture.now + 5));
    let ledger = ledger_counts(store).await;
    for node in [&source, &fork] {
        store
            .edge_store()
            .merge_events(&fixture.tenant_a, &fixture.executor_a, node, &events)
            .await
            .unwrap();
    }
    let first = store
        .computer_provider_usage(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.executor_a,
            None,
            &PageQuery {
                limit: 2,
                ..PageQuery::default()
            },
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(first.source, "device_reported");
    assert_eq!(
        first
            .observations
            .iter()
            .map(|value| value.started_seq)
            .collect::<Vec<_>>(),
        [5, 3]
    );
    assert!(
        first
            .observations
            .iter()
            .all(|value| value.session_id == original.session_id
                && value.session_title == "Original usage"
                && value.input_author.is_none())
    );
    assert_eq!(first.observations[0].finished_at_ms, None);
    assert_eq!(first.observations[0].usage, None);
    assert_eq!(
        first.observations[1].usage.as_ref().unwrap().input_tokens,
        Some(5)
    );
    assert_eq!(
        first.observations[1].usage.as_ref().unwrap().output_tokens,
        Some(7)
    );
    let summary = store
        .computer_provider_usage_summary(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.executor_a,
            None,
            None,
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(
        summary.totals.attempts, 3,
        "monthly totals include calls outside the detail page"
    );
    assert_eq!(summary.totals.completed, 2);
    assert_eq!(summary.totals.failed, 0);
    assert_eq!(summary.totals.input.tokens, Some(75));
    assert_eq!(summary.totals.input.reported_attempts, 2);
    assert_eq!(summary.totals.output.tokens, Some(7));
    assert_eq!(summary.totals.output.reported_attempts, 1);
    assert_eq!(
        summary.totals.reasoning.tokens, None,
        "missing counters remain unknown"
    );
    assert_eq!(summary.groups.len(), 1);
    assert_eq!(summary.groups[0].totals.attempts, 3);
    assert!(
        store
            .computer_provider_usage_summary(
                &fixture.bob,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                None,
                fixture.now
            )
            .await
            .is_err()
    );
    assert!(
        store
            .computer_provider_usage_summary(
                &fixture.alice,
                &fixture.tenant_b,
                &fixture.executor_a,
                None,
                None,
                fixture.now
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .computer_provider_usage_summary(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                Some("unrelated"),
                fixture.now
            )
            .await
            .unwrap()
            .totals
            .attempts,
        0
    );
    assert_eq!(
        store
            .computer_provider_usage_summary(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                Some("2000-01"),
                None,
                fixture.now
            )
            .await
            .unwrap()
            .totals
            .attempts,
        0
    );
    let next = store
        .computer_provider_usage(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.executor_a,
            None,
            &PageQuery {
                cursor: first.next_cursor.clone(),
                limit: 2,
                ..PageQuery::default()
            },
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(next.observations.len(), 1);
    assert_eq!(next.next_cursor, None);
    assert_eq!(next.observations[0].started_seq, 1);
    assert_eq!(
        next.observations[0].usage.as_ref().unwrap().input_tokens,
        Some(70)
    );
    assert_eq!(
        next.observations[0].usage.as_ref().unwrap().output_tokens,
        None
    );
    let serialized = serde_json::to_string(&first).unwrap();
    assert!(
        !serialized.contains("private prompt")
            && !serialized.contains("forged-account")
            && !serialized.contains(source.as_str())
    );
    store
        .edge_store()
        .merge_events(
            &fixture.tenant_a,
            &fixture.executor_a,
            &source,
            &[finish(6, 5, Some(1), Some(2), fixture.now + 6)],
        )
        .await
        .unwrap();
    store
        .edge_store()
        .merge_events(&fixture.tenant_a, &fixture.executor_a, &source, &events)
        .await
        .unwrap();
    let complete = store
        .computer_provider_usage(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.executor_a,
            None,
            &PageQuery::default(),
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(
        complete.observations.len(),
        3,
        "replayed and forked journals do not duplicate attempts"
    );
    assert_eq!(
        complete.observations[0]
            .usage
            .as_ref()
            .unwrap()
            .output_tokens,
        Some(2)
    );
    let summary = store
        .computer_provider_usage_summary(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.executor_a,
            None,
            None,
            fixture.now,
        )
        .await
        .unwrap();
    assert_eq!(
        summary.totals.attempts, 3,
        "replay and fork copies do not inflate summaries"
    );
    assert_eq!(
        summary.totals.completed, 3,
        "late finishes update the next snapshot"
    );
    assert_eq!(summary.totals.input.tokens, Some(76));
    assert_eq!(summary.totals.output.tokens, Some(9));
    assert_eq!(
        ledger_counts(store).await,
        ledger,
        "device reports never insert authoritative model requests or attempts"
    );
    assert!(
        store
            .computer_provider_usage(
                &fixture.bob,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                &PageQuery::default(),
                fixture.now
            )
            .await
            .is_err(),
        "team membership does not disclose computer-wide usage"
    );
    assert!(
        store
            .computer_provider_usage(
                &fixture.alice,
                &fixture.tenant_b,
                &fixture.executor_b,
                None,
                &PageQuery::default(),
                fixture.now
            )
            .await
            .is_err()
    );
    assert!(
        store
            .computer_provider_usage(
                &fixture.bob,
                &fixture.tenant_b,
                &fixture.executor_b,
                None,
                &PageQuery::default(),
                fixture.now
            )
            .await
            .unwrap()
            .observations
            .is_empty()
    );
    assert!(
        store
            .computer_provider_usage(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                &PageQuery {
                    query: Some("unrelated-model".to_owned()),
                    ..PageQuery::default()
                },
                fixture.now
            )
            .await
            .unwrap()
            .observations
            .is_empty()
    );
    assert!(
        store
            .computer_provider_usage(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                Some("2000-01"),
                &PageQuery::default(),
                fixture.now
            )
            .await
            .unwrap()
            .observations
            .is_empty()
    );
    for mapping in [original, copied] {
        store
            .delete_edge_session_mapping(
                &fixture.alice,
                &fixture.tenant_a,
                &mapping.session_id,
                &mapping.node_session_id,
                fixture.now + 20,
            )
            .await
            .unwrap();
    }
    assert!(
        store
            .computer_provider_usage(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                &PageQuery::default(),
                fixture.now
            )
            .await
            .unwrap()
            .observations
            .is_empty()
    );
    assert_eq!(
        store
            .computer_provider_usage_summary(
                &fixture.alice,
                &fixture.tenant_a,
                &fixture.executor_a,
                None,
                None,
                fixture.now
            )
            .await
            .unwrap()
            .totals
            .attempts,
        0,
        "deleted mappings are excluded from summaries"
    );
    let mut tx = store
        .database()
        .tenant_read_transaction(&fixture.tenant_a)
        .await
        .unwrap();
    let orphaned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM control_edge_usage_events u LEFT JOIN control_edge_events e ON e.tenant_id=u.tenant_id AND e.executor_id=u.executor_id AND e.session_id=u.session_id AND e.seq=u.seq WHERE e.seq IS NULL").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(
        orphaned, 0,
        "deleting canonical events must cascade to their usage projection"
    );
    tx.commit().await.unwrap();
}

async fn mapping(
    store: &ControlStore,
    fixture: &EdgeFixture,
    node: &SessionId,
    title: &str,
) -> ternilo_control::EdgeSessionRecord {
    store
        .create_edge_session_mapping(
            &fixture.alice,
            &fixture.tenant_a,
            &fixture.workspace_a,
            &fixture.executor_a,
            node,
            None,
            metadata(title, None, fixture.now),
            fixture.now,
        )
        .await
        .unwrap()
}

fn start(source: &SessionId, seq: u64, attempt: u32, now: u64) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: now,
        run_id: RunId::new("device-call"),
        kind: SessionEventKind::ProviderUsageStarted {
            source_session_id: Some(source.clone()),
            step: 1,
            attempt,
            route: ProviderUsageRoute {
                provider: "same-provider".to_owned(),
                model: "same-model".to_owned(),
                protocol: ProviderProtocol::OpenAiResponses,
            },
        },
    }
}

fn finish(
    seq: u64,
    started_seq: u64,
    input: Option<u64>,
    output: Option<u64>,
    now: u64,
) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: now,
        run_id: RunId::new("device-call"),
        kind: SessionEventKind::ProviderUsageFinished {
            started_seq,
            usage: Some(ReportedModelUsage {
                input_tokens: input,
                output_tokens: output,
                ..ReportedModelUsage::default()
            }),
            upstream_request_id: Some("upstream-id".to_owned()),
            error_code: None,
        },
    }
}

async fn ledger_counts(store: &ControlStore) -> (i64, i64) {
    let mut tx = store.database().begin().await.unwrap();
    if ternilo_storage::backend(&tx) == ternilo_storage::Backend::Postgres {
        sqlx::query("SELECT set_config('ternilo.model_service', 'on', true)")
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    let counts = sqlx::query_as("SELECT (SELECT COUNT(*) FROM control_model_requests), (SELECT COUNT(*) FROM control_model_attempts)").fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    counts
}
