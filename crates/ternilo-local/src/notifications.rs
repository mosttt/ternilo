use ternilo_protocol::{SessionEventKind, SessionLiveDirty};
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalInvalidationCategory {
    Workbench,
    Activity,
    Events,
    Inbox,
    Stats,
    Projection,
    Questions,
    Profile,
    AgentTeam,
}

impl LocalInvalidationCategory {
    #[must_use]
    pub fn session_dirty(self) -> Option<SessionLiveDirty> {
        match self {
            Self::Workbench | Self::Activity => None,
            Self::Events => Some(SessionLiveDirty {
                events: true,
                ..SessionLiveDirty::default()
            }),
            Self::Inbox => Some(SessionLiveDirty {
                inbox: true,
                ..SessionLiveDirty::default()
            }),
            Self::Stats => Some(SessionLiveDirty {
                stats: true,
                ..SessionLiveDirty::default()
            }),
            Self::Projection => Some(SessionLiveDirty {
                projection: true,
                ..SessionLiveDirty::default()
            }),
            Self::Questions => Some(SessionLiveDirty {
                questions: true,
                ..SessionLiveDirty::default()
            }),
            Self::Profile => Some(SessionLiveDirty {
                profile: true,
                ..SessionLiveDirty::default()
            }),
            Self::AgentTeam => Some(SessionLiveDirty {
                agent_team: true,
                ..SessionLiveDirty::default()
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalInvalidationNotification {
    pub session_id: Option<String>,
    pub category: LocalInvalidationCategory,
    pub revision: Option<u64>,
}

pub(crate) fn publish_invalidation(
    sender: &broadcast::Sender<LocalInvalidationNotification>,
    session_id: Option<&str>,
    category: LocalInvalidationCategory,
    revision: Option<u64>,
) {
    let _ = sender.send(LocalInvalidationNotification {
        session_id: session_id.map(str::to_owned),
        category,
        revision,
    });
}

pub(crate) fn event_session_dirty(kind: &SessionEventKind) -> SessionLiveDirty {
    SessionLiveDirty::for_event(kind)
}

pub(crate) fn event_updates_workbench(kind: &SessionEventKind) -> bool {
    matches!(
        kind,
        SessionEventKind::TurnStarted
            | SessionEventKind::WorkspaceExecutionWaiting
            | SessionEventKind::WorkspaceExecutionAcquired
            | SessionEventKind::ExecutionActivityChanged { .. }
            | SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled
            | SessionEventKind::SessionTitleGenerated { .. }
            | SessionEventKind::SubagentUpdated { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_map_exactly_to_live_dirty_bits() {
        let cases = [
            (
                LocalInvalidationCategory::Events,
                SessionLiveDirty {
                    events: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::Inbox,
                SessionLiveDirty {
                    inbox: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::Stats,
                SessionLiveDirty {
                    stats: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::Projection,
                SessionLiveDirty {
                    projection: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::Questions,
                SessionLiveDirty {
                    questions: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::Profile,
                SessionLiveDirty {
                    profile: true,
                    ..SessionLiveDirty::default()
                },
            ),
            (
                LocalInvalidationCategory::AgentTeam,
                SessionLiveDirty {
                    agent_team: true,
                    ..SessionLiveDirty::default()
                },
            ),
        ];
        for (category, expected) in cases {
            assert_eq!(category.session_dirty(), Some(expected));
        }
        assert_eq!(LocalInvalidationCategory::Workbench.session_dirty(), None);
        assert_eq!(LocalInvalidationCategory::Activity.session_dirty(), None);
    }

    #[test]
    fn durable_events_map_to_dirty_reads_without_streaming_amplification() {
        let streaming = event_session_dirty(&SessionEventKind::AssistantMessageDelta {
            step: 1,
            delta: "chunk".to_owned(),
        });
        assert!(streaming.events);
        assert!(!streaming.stats);
        assert!(!streaming.projection);

        let question = event_session_dirty(&SessionEventKind::UserQuestionAsked {
            question: ternilo_protocol::UserQuestion {
                id: "question".to_owned(),
                question: "Continue?".to_owned(),
                detail: None,
                header: None,
                options: Vec::new(),
                multi_select: false,
                presentation: None,
                tool_approval: None,
            },
        });
        assert!(question.events);
        assert!(question.stats);
        assert!(question.projection);
        assert!(question.questions);
        assert!(question.inbox);

        let turn_started = event_session_dirty(&SessionEventKind::TurnStarted);
        assert!(turn_started.events);
        assert!(turn_started.inbox);

        let subagent = SessionEventKind::SubagentUpdated {
            subagent: ternilo_protocol::SubagentSnapshot {
                subagent_id: ternilo_protocol::SubagentId::new("child"),
                provider: "local".to_owned(),
                label: "Reviewer".to_owned(),
                task: "Review".to_owned(),
                supports_followup: false,
                session_id: None,
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::ProcessLifecycle,
                status: ternilo_protocol::SubagentStatus::Running,
                output: None,
                error: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        };
        assert!(event_session_dirty(&subagent).agent_team);
        assert!(event_updates_workbench(&subagent));
    }
}
