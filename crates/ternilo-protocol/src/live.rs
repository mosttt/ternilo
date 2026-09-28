use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentTeamSnapshot, ErrorCode, ExecutionActivityPhase, Profile, RunId, SessionEvent,
    SessionEventKind, SessionId, SessionInboxSnapshot, SessionProjectionSnapshot, SessionStats,
    TenantId, UserQuestion,
};

pub const LIVE_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "protocol fields independently select session read groups"
)]
pub struct SessionLiveReadMask {
    pub inbox: bool,
    pub stats: bool,
    pub projection: bool,
    pub questions: bool,
    pub profile: bool,
    pub agent_team: bool,
}

impl SessionLiveReadMask {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            inbox: true,
            stats: true,
            projection: true,
            questions: true,
            profile: true,
            agent_team: true,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.inbox
            && !self.stats
            && !self.projection
            && !self.questions
            && !self.profile
            && !self.agent_team
    }

    pub fn merge(&mut self, other: Self) {
        self.inbox |= other.inbox;
        self.stats |= other.stats;
        self.projection |= other.projection;
        self.questions |= other.questions;
        self.profile |= other.profile;
        self.agent_team |= other.agent_team;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "protocol fields independently mark dirty session groups"
)]
pub struct SessionLiveDirty {
    pub events: bool,
    pub inbox: bool,
    pub stats: bool,
    pub projection: bool,
    pub questions: bool,
    pub profile: bool,
    pub agent_team: bool,
}

impl SessionLiveDirty {
    #[must_use]
    pub fn for_event(kind: &crate::SessionEventKind) -> Self {
        let refresh_derived = !matches!(
            kind,
            crate::SessionEventKind::AssistantMessageDelta { .. }
                | crate::SessionEventKind::AssistantReasoningDelta { .. }
        );
        Self {
            events: true,
            inbox: matches!(
                kind,
                crate::SessionEventKind::TurnStarted
                    | crate::SessionEventKind::WorkspaceExecutionWaiting
                    | crate::SessionEventKind::WorkspaceExecutionAcquired
                    | crate::SessionEventKind::ExecutionActivityChanged { .. }
                    | crate::SessionEventKind::UserMessage { .. }
                    | crate::SessionEventKind::UserQuestionAsked { .. }
                    | crate::SessionEventKind::UserQuestionAnswered { .. }
                    | crate::SessionEventKind::TurnFinished { .. }
                    | crate::SessionEventKind::TurnFailed { .. }
                    | crate::SessionEventKind::TurnCancelled
            ),
            stats: refresh_derived,
            projection: refresh_derived,
            questions: matches!(
                kind,
                crate::SessionEventKind::UserQuestionAsked { .. }
                    | crate::SessionEventKind::UserQuestionAnswered { .. }
            ),
            profile: matches!(
                kind,
                crate::SessionEventKind::RuntimeExtensionChanged { .. }
                    | crate::SessionEventKind::PlanReviewCompleted { approved: true, .. }
            ),
            agent_team: matches!(
                kind,
                crate::SessionEventKind::SubagentUpdated { .. }
                    | crate::SessionEventKind::SessionTitleGenerated { .. }
            ),
        }
    }

    #[must_use]
    pub const fn metadata(self) -> SessionLiveReadMask {
        SessionLiveReadMask {
            inbox: self.inbox,
            stats: self.stats,
            projection: self.projection,
            questions: self.questions,
            profile: self.profile,
            agent_team: self.agent_team,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        !self.events && self.metadata().is_empty()
    }

    pub fn merge(&mut self, other: Self) {
        self.events |= other.events;
        let mut metadata = self.metadata();
        metadata.merge(other.metadata());
        self.inbox = metadata.inbox;
        self.stats = metadata.stats;
        self.projection = metadata.projection;
        self.questions = metadata.questions;
        self.profile = metadata.profile;
        self.agent_team = metadata.agent_team;
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LivePendingQuestion {
    pub session_id: SessionId,
    pub question: UserQuestion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLiveActivity {
    pub session_id: SessionId,
    pub running: bool,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<SessionExecutionActivity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExecutionPhase {
    Running,
    WaitingForSubagents,
    WaitingForCapacity,
    WaitingForWorkspace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionExecutionActivity {
    pub run_id: RunId,
    pub phase: SessionExecutionPhase,
}

/// Apply canonical events in sequence; late activity or completion belongs only to its own run.
pub fn update_execution_activity(
    current: &mut Option<SessionExecutionActivity>,
    event: &SessionEvent,
) -> bool {
    if matches!(event.kind, SessionEventKind::TurnStarted) {
        let next = SessionExecutionActivity {
            run_id: event.run_id.clone(),
            phase: SessionExecutionPhase::Running,
        };
        if current.as_ref() == Some(&next) {
            return false;
        }
        *current = Some(next);
        return true;
    }
    let Some(activity) = current
        .as_mut()
        .filter(|activity| activity.run_id == event.run_id)
    else {
        return false;
    };
    let phase = match &event.kind {
        SessionEventKind::ExecutionActivityChanged { phase } => match phase {
            ExecutionActivityPhase::Running => SessionExecutionPhase::Running,
            ExecutionActivityPhase::WaitingForSubagents => {
                SessionExecutionPhase::WaitingForSubagents
            }
            ExecutionActivityPhase::WaitingForCapacity => SessionExecutionPhase::WaitingForCapacity,
        },
        SessionEventKind::WorkspaceExecutionWaiting => SessionExecutionPhase::WaitingForWorkspace,
        SessionEventKind::WorkspaceExecutionAcquired => SessionExecutionPhase::Running,
        SessionEventKind::TurnFinished { .. }
        | SessionEventKind::TurnFailed { .. }
        | SessionEventKind::TurnCancelled => {
            *current = None;
            return true;
        }
        _ => return false,
    };
    if activity.phase == phase {
        return false;
    }
    activity.phase = phase;
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLiveMetadata {
    pub read: SessionLiveReadMask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<SessionInboxSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<SessionStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<SessionProjectionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<LivePendingQuestion>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_team: Option<AgentTeamSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LiveClientFrame {
    Hello {
        protocol_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tenant_id: Option<TenantId>,
    },
    Subscribe {
        subscription_id: u64,
        session_id: SessionId,
        /// Last accepted event sequence; the server starts at the following sequence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_seq: Option<u64>,
        metadata: SessionLiveReadMask,
    },
    Unsubscribe {
        subscription_id: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "short-lived typed wire frames keep directly serializable payloads"
)]
pub enum LiveServerFrame {
    Ready {
        protocol_version: u32,
    },
    Workbench {
        revision: u64,
        state: Value,
        activity: Vec<SessionLiveActivity>,
    },
    Activity {
        activity: SessionLiveActivity,
    },
    EventBatch {
        subscription_id: u64,
        session_id: SessionId,
        reset: bool,
        complete: bool,
        events: Vec<SessionEvent>,
        /// First event sequence not included in this batch or any preceding batch.
        next_seq: u64,
    },
    SessionMetadata {
        subscription_id: u64,
        session_id: SessionId,
        metadata: SessionLiveMetadata,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subscription_id: Option<u64>,
        code: ErrorCode,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        AgentTeamId, AgentTeamMember, AgentTeamMemberId, AgentTeamMemberRole, RunId,
        SessionEventKind,
    };

    #[test]
    fn client_frames_use_tagged_wire_shape() {
        let frame = LiveClientFrame::Subscribe {
            subscription_id: 7,
            session_id: SessionId::new("session-a"),
            after_seq: Some(42),
            metadata: SessionLiveReadMask::all(),
        };

        let value = serde_json::to_value(&frame).expect("serialize live client frame");
        assert_eq!(value["type"], "subscribe");
        assert_eq!(value["subscription_id"], 7);
        assert_eq!(value["session_id"], "session-a");
        assert_eq!(value["after_seq"], 42);
        assert_eq!(
            serde_json::from_value::<LiveClientFrame>(value)
                .expect("deserialize live client frame"),
            frame
        );
    }

    #[test]
    fn optional_hello_credentials_are_omitted() {
        let frame = LiveClientFrame::Hello {
            protocol_version: LIVE_PROTOCOL_VERSION,
            bearer_token: None,
            tenant_id: None,
        };

        assert_eq!(
            serde_json::to_value(frame).expect("serialize hello"),
            json!({
                "type": "hello",
                "protocol_version": LIVE_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn server_frames_round_trip_metadata_and_workbench() {
        let frames = [
            LiveServerFrame::Workbench {
                revision: 9,
                state: json!({ "sessions": [] }),
                activity: Vec::new(),
            },
            LiveServerFrame::EventBatch {
                subscription_id: 3,
                session_id: SessionId::new("session-b"),
                reset: true,
                complete: false,
                events: vec![SessionEvent {
                    seq: 0,
                    occurred_at_ms: 100,
                    run_id: RunId::new("run-b"),
                    kind: SessionEventKind::TurnCancelled,
                }],
                next_seq: 1,
            },
            LiveServerFrame::SessionMetadata {
                subscription_id: 3,
                session_id: SessionId::new("session-b"),
                metadata: SessionLiveMetadata {
                    read: SessionLiveReadMask {
                        stats: true,
                        ..SessionLiveReadMask::default()
                    },
                    stats: Some(SessionStats {
                        events: 12,
                        ..SessionStats::default()
                    }),
                    ..SessionLiveMetadata::default()
                },
            },
            LiveServerFrame::Error {
                subscription_id: Some(3),
                code: ErrorCode::Unavailable,
                message: "temporary failure".to_owned(),
            },
        ];

        for frame in frames {
            let encoded = serde_json::to_string(&frame).expect("serialize live server frame");
            assert_eq!(
                serde_json::from_str::<LiveServerFrame>(&encoded)
                    .expect("deserialize live server frame"),
                frame
            );
        }
    }

    #[test]
    fn dirty_flags_merge_into_one_metadata_read() {
        let mut dirty = SessionLiveDirty {
            events: true,
            stats: true,
            ..SessionLiveDirty::default()
        };
        dirty.merge(SessionLiveDirty {
            inbox: true,
            questions: true,
            agent_team: true,
            ..SessionLiveDirty::default()
        });

        assert!(dirty.events);
        assert_eq!(
            dirty.metadata(),
            SessionLiveReadMask {
                inbox: true,
                stats: true,
                questions: true,
                agent_team: true,
                ..SessionLiveReadMask::default()
            }
        );
        assert!(!dirty.is_empty());
    }

    #[test]
    fn metadata_mask_and_agent_team_share_the_wire_contract() {
        let lead_id = AgentTeamMemberId::new("lead");
        let metadata = SessionLiveMetadata {
            read: SessionLiveReadMask {
                agent_team: true,
                ..SessionLiveReadMask::default()
            },
            agent_team: Some(AgentTeamSnapshot {
                team_id: AgentTeamId::new("team-a"),
                current_member_id: lead_id.clone(),
                members: vec![AgentTeamMember {
                    id: lead_id,
                    parent_id: None,
                    subagent_id: None,
                    label: "Lead".to_owned(),
                    provider: None,
                    role: AgentTeamMemberRole::Lead,
                }],
                tasks: Vec::new(),
                messages: Vec::new(),
            }),
            ..SessionLiveMetadata::default()
        };

        let value = serde_json::to_value(metadata).expect("serialize Agent Team metadata");
        assert_eq!(value["read"]["agent_team"], true);
        assert_eq!(value["agent_team"]["team_id"], "team-a");
        assert!(value.get("stats").is_none());
    }

    fn execution_event(run: &str, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: RunId::new(run),
            kind,
        }
    }

    #[test]
    fn execution_activity_tracks_only_the_current_run_and_clears_each_terminal_kind() {
        let mut current = None;
        assert!(!update_execution_activity(
            &mut current,
            &execution_event("old", SessionEventKind::WorkspaceExecutionWaiting)
        ));
        assert!(update_execution_activity(
            &mut current,
            &execution_event("old", SessionEventKind::TurnStarted)
        ));
        assert!(update_execution_activity(
            &mut current,
            &execution_event(
                "old",
                SessionEventKind::ExecutionActivityChanged {
                    phase: ExecutionActivityPhase::WaitingForSubagents
                }
            )
        ));
        assert!(update_execution_activity(
            &mut current,
            &execution_event("current", SessionEventKind::TurnStarted)
        ));
        let expected = current.clone();
        for kind in [
            SessionEventKind::ExecutionActivityChanged {
                phase: ExecutionActivityPhase::WaitingForCapacity,
            },
            SessionEventKind::WorkspaceExecutionWaiting,
            SessionEventKind::WorkspaceExecutionAcquired,
            SessionEventKind::TurnCancelled,
        ] {
            assert!(!update_execution_activity(
                &mut current,
                &execution_event("old", kind)
            ));
            assert_eq!(current, expected);
        }
        for terminal in [
            SessionEventKind::TurnFinished {
                answer: "done".to_owned(),
                finish_reason: crate::TurnFinishReason::Completed,
            },
            SessionEventKind::TurnFailed {
                message: "failed".to_owned(),
            },
            SessionEventKind::TurnCancelled,
        ] {
            update_execution_activity(
                &mut current,
                &execution_event("current", SessionEventKind::TurnStarted),
            );
            assert!(update_execution_activity(
                &mut current,
                &execution_event("current", terminal)
            ));
            assert_eq!(current, None);
            assert!(!update_execution_activity(
                &mut current,
                &execution_event(
                    "current",
                    SessionEventKind::ExecutionActivityChanged {
                        phase: ExecutionActivityPhase::Running
                    }
                )
            ));
        }
    }

    #[test]
    fn workspace_waiting_and_admission_phases_remain_independent_between_sessions() {
        let start = execution_event("same-run-id", SessionEventKind::TurnStarted);
        let mut first = None;
        let mut second = None;
        update_execution_activity(&mut first, &start);
        update_execution_activity(&mut second, &start);
        for (kind, expected) in [
            (
                SessionEventKind::WorkspaceExecutionWaiting,
                SessionExecutionPhase::WaitingForWorkspace,
            ),
            (
                SessionEventKind::WorkspaceExecutionAcquired,
                SessionExecutionPhase::Running,
            ),
            (
                SessionEventKind::ExecutionActivityChanged {
                    phase: ExecutionActivityPhase::WaitingForSubagents,
                },
                SessionExecutionPhase::WaitingForSubagents,
            ),
            (
                SessionEventKind::ExecutionActivityChanged {
                    phase: ExecutionActivityPhase::WaitingForCapacity,
                },
                SessionExecutionPhase::WaitingForCapacity,
            ),
            (
                SessionEventKind::ExecutionActivityChanged {
                    phase: ExecutionActivityPhase::Running,
                },
                SessionExecutionPhase::Running,
            ),
        ] {
            let event = execution_event("same-run-id", kind);
            assert!(update_execution_activity(&mut first, &event));
            assert_eq!(first.as_ref().unwrap().phase, expected);
            assert!(!update_execution_activity(&mut first, &event));
            assert_eq!(
                second.as_ref().unwrap().phase,
                SessionExecutionPhase::Running
            );
        }
    }

    #[test]
    fn live_execution_metadata_is_optional_and_activity_events_invalidate_readers() {
        let old = json!({"session_id":"session", "running":false, "updated_at_ms":1});
        let decoded: SessionLiveActivity = serde_json::from_value(old.clone()).unwrap();
        assert!(decoded.execution.is_none());
        assert_eq!(serde_json::to_value(decoded).unwrap(), old);
        let live = SessionLiveActivity {
            session_id: SessionId::new("session"),
            running: true,
            updated_at_ms: 2,
            execution: Some(SessionExecutionActivity {
                run_id: RunId::new("run"),
                phase: SessionExecutionPhase::WaitingForWorkspace,
            }),
        };
        assert_eq!(
            serde_json::to_value(live).unwrap()["execution"],
            json!({"run_id":"run", "phase":"waiting_for_workspace"})
        );
        let dirty = SessionLiveDirty::for_event(&SessionEventKind::ExecutionActivityChanged {
            phase: ExecutionActivityPhase::WaitingForCapacity,
        });
        assert!(dirty.events && dirty.inbox);
    }
}
