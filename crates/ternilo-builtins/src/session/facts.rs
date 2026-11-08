use std::collections::BTreeMap;
use ternilo_protocol::{
    RunId, SessionEvent, SessionEventKind, SessionInputFacts, SessionLogSummary, SessionRunFacts,
    UserMessageSource,
};

#[derive(Default)]
pub(super) struct CommittedFacts {
    pub summary: SessionLogSummary,
    pub runs: BTreeMap<RunId, SessionRunFacts>,
}

impl CommittedFacts {
    pub fn from_events(events: &[SessionEvent]) -> Self {
        let mut facts = Self::default();
        for event in events {
            facts.observe(event);
        }
        facts
    }

    pub fn observe(&mut self, event: &SessionEvent) {
        let run = self
            .runs
            .entry(event.run_id.clone())
            .or_insert_with(|| SessionRunFacts {
                first_seq: event.seq,
                last_seq: event.seq,
                terminal: false,
                input: None,
            });
        run.last_seq = event.seq;
        match &event.kind {
            SessionEventKind::UserMessage {
                provenance, source, ..
            } => {
                self.summary.latest_input_run = Some(event.run_id.clone());
                run.input.get_or_insert_with(|| SessionInputFacts {
                    seq: event.seq,
                    provenance: provenance.clone(),
                    source: source.clone(),
                });
                if let Some(UserMessageSource::Submission { submission_id, .. }) = source {
                    self.summary
                        .consumed_submissions
                        .push(submission_id.clone());
                }
            }
            SessionEventKind::TurnFinished { .. } => {
                run.terminal = true;
                self.summary.completed_turns += 1;
            }
            SessionEventKind::TurnFailed { .. } | SessionEventKind::TurnCancelled => {
                run.terminal = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    use ternilo_kernel::{
        HarnessSession, HostEnvironment, HostPolicy, MemoryEventStore, SessionEventStore,
    };
    use ternilo_protocol::{
        AgentId, HarnessError, InputAuthor, InputProvenance, RunLimits, SessionId, SessionIdentity,
        SubmissionDelivery, SubmissionId, TenantId, TurnFinishReason, UserId,
    };

    struct Store {
        memory: MemoryEventStore,
        fail: AtomicBool,
    }
    impl SessionEventStore for Store {
        fn load<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>>
        {
            self.memory.load()
        }
        fn append<'a>(
            &'a self,
            event: SessionEvent,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                if self.fail.load(Ordering::SeqCst) {
                    return Err(HarnessError::execution("fixture append failed"));
                }
                self.memory.append(event).await
            })
        }
    }
    async fn boot(store: Arc<Store>) -> HarnessSession {
        HarnessSession::boot(
            &crate::catalog().unwrap(),
            &crate::local_profile(),
            HostEnvironment::new(
                SessionIdentity {
                    tenant_id: TenantId::new("local"),
                    user_id: UserId::new("user"),
                    agent_id: AgentId::new("agent"),
                    session_id: SessionId::new("facts"),
                },
                None,
                HostPolicy::local(RunLimits::default()),
                store,
            ),
        )
        .await
        .unwrap()
    }
    fn input(id: &str, author: &str) -> SessionEventKind {
        SessionEventKind::UserMessage {
            provenance: Some(InputProvenance {
                run_id: None,
                input_id: SubmissionId::new(id),
                author: InputAuthor::Account {
                    user_id: UserId::new(author),
                    username: author.into(),
                },
            }),
            content: "payload".into(),
            display_content: None,
            source: Some(UserMessageSource::Submission {
                submission_id: SubmissionId::new(id),
                created_at_ms: 1,
                delivery: SubmissionDelivery::Queue,
                regenerate_from: None,
                skill_name: None,
            }),
            references: vec![],
            attachments: vec![],
        }
    }

    #[tokio::test]
    async fn committed_queries_preserve_interleaved_runs_primary_author_and_restart() {
        let store = Arc::new(Store {
            memory: MemoryEventStore::default(),
            fail: AtomicBool::new(false),
        });
        let harness = boot(Arc::clone(&store)).await;
        let first = RunId::new("first");
        let other = RunId::new("other");
        for (run, kind) in [
            (first.clone(), input("first-input", "alice")),
            (
                other.clone(),
                SessionEventKind::AssistantMessageDelta {
                    step: 1,
                    delta: "irrelevant body ".repeat(100_000),
                },
            ),
            (first.clone(), input("steered-input", "bob")),
            (
                other.clone(),
                SessionEventKind::TurnFailed {
                    message: "test".into(),
                },
            ),
            (
                first.clone(),
                SessionEventKind::TurnFinished {
                    answer: "done".into(),
                    finish_reason: TurnFinishReason::Completed,
                },
            ),
        ] {
            harness.append_event(run, kind).await.unwrap();
        }
        let events = store.load().await.unwrap();
        let expected = events
            .iter()
            .filter(|event| event.run_id == first)
            .cloned()
            .collect::<Vec<_>>();
        let summary = harness.summary().await;
        let facts = harness.run_facts(first.clone()).await.unwrap();
        assert_eq!(summary.completed_turns, 1);
        assert_eq!(summary.latest_input_run, Some(first.clone()));
        assert_eq!(
            summary.consumed_submissions,
            vec![
                SubmissionId::new("first-input"),
                SubmissionId::new("steered-input")
            ]
        );
        assert_eq!(
            facts
                .input
                .as_ref()
                .unwrap()
                .provenance
                .as_ref()
                .unwrap()
                .input_id,
            SubmissionId::new("first-input")
        );
        assert!(facts.terminal);
        assert_eq!(harness.run_events(first.clone()).await, expected);
        assert_eq!(harness.event_at(3).await, Some(events[3].clone()));
        assert!(harness.event_at(u64::MAX).await.is_none());
        assert!(harness.run_events(RunId::new("absent")).await.is_empty());
        harness.shutdown().await.unwrap();
        let reopened = boot(store).await;
        assert_eq!(reopened.summary().await, summary);
        assert_eq!(reopened.run_facts(first.clone()).await, Some(facts));
        assert_eq!(reopened.run_events(first).await, expected);
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_append_cannot_publish_input_authority_or_receipts() {
        let store = Arc::new(Store {
            memory: MemoryEventStore::default(),
            fail: AtomicBool::new(false),
        });
        let harness = boot(Arc::clone(&store)).await;
        store.fail.store(true, Ordering::SeqCst);
        let run = RunId::new("failed-input");
        assert!(
            harness
                .append_event(run.clone(), input("not-committed", "alice"))
                .await
                .is_err()
        );
        assert_eq!(harness.summary().await, SessionLogSummary::default());
        assert!(harness.run_facts(run.clone()).await.is_none());
        assert!(harness.run_events(run).await.is_empty());
        assert!(store.load().await.unwrap().is_empty());
        assert!(harness.shutdown().await.is_err());
    }
}
