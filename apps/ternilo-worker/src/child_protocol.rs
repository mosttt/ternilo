use serde::{Deserialize, Serialize};
use ternilo_protocol::{
    AgentTeamMessageId, AgentTeamMessageSend, AgentTeamTaskCreate, AgentTeamTaskId,
    AgentTeamTaskReplace, HarnessError, ModelRequest, ModelResponse, ModelRetryFailure,
    PreparedSkillInvocation, RunId, RunOutcome, SessionCommandCatalog, SessionEvent, SessionId,
    SkillCatalogSnapshot, SteeringInput, SubagentId, SubagentSnapshot, SubagentTranscriptKind,
    UserAnswer, UserQuestion,
};
use ternilo_transport::CommandId;
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::ChildStdin,
    sync::Mutex,
};

const STARTUP_AUTHORIZATION: &[u8] = b"ternilo-execute-v1\n";

/// EOF is never authorization: a dead daemon must not boot workspace plugins.
pub(crate) async fn await_startup_authorization() -> Result<(), HarnessError> {
    let mut received = vec![0; STARTUP_AUTHORIZATION.len()];
    tokio::io::stdin()
        .read_exact(&mut received)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("read cloud child startup authorization: {error}"))
        })?;
    if received != STARTUP_AUTHORIZATION {
        return Err(HarnessError::policy(
            "cloud child startup was not authorized",
        ));
    }
    Ok(())
}

pub(crate) async fn authorize_startup(input: &mut ChildStdin) -> Result<(), HarnessError> {
    input
        .write_all(STARTUP_AUTHORIZATION)
        .await
        .map_err(|error| {
            HarnessError::execution(format!("authorize cloud child startup: {error}"))
        })?;
    input.flush().await.map_err(|error| {
        HarnessError::execution(format!("flush cloud child startup authorization: {error}"))
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChildToParentFrame {
    Event {
        event: Box<SessionEvent>,
    },
    ModelRequest {
        request_id: u64,
        binding: ternilo_protocol::RunModelBinding,
        request: Box<ModelRequest>,
    },
    Question {
        question: Box<UserQuestion>,
    },
    SessionCommandReply {
        command_id: CommandId,
        outcome: ChildSessionCommandOutcome,
    },
    HostRequest {
        request_id: u64,
        request: CloudHostRequest,
    },
    Outcome {
        outcome: RunOutcome,
    },
    Error {
        error: HarnessError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChildSessionCommand {
    Steer { input: Box<SteeringInput> },
    Cancel { run_id: RunId },
    Commands,
    Skills,
    Services,
    StartService { service_id: String },
    StopService { service_id: String },
    ResolveSkill { name: String, input: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChildSessionCommandOutcome {
    Services {
        services: Vec<ternilo_protocol::SessionServiceSnapshot>,
    },
    Service {
        service: ternilo_protocol::SessionServiceSnapshot,
    },
    Steer {
        accepted: bool,
    },
    Cancelled,
    Commands {
        catalog: SessionCommandCatalog,
    },
    Skills {
        catalog: SkillCatalogSnapshot,
    },
    SkillResolved {
        invocation: PreparedSkillInvocation,
    },
    Error {
        error: HarnessError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudHostRequest {
    ParkActivity {
        activity_revision: u64,
        dependencies: Vec<ternilo_protocol::AcceptedSubagentRun>,
    },
    ResumeActivity {
        activity_revision: u64,
        parked_revision: u64,
    },
    CreateSubagent {
        subagent_id: SubagentId,
        provider: String,
        label: String,
        task: String,
        transcript_kind: SubagentTranscriptKind,
    },
    EnqueueSubagent {
        session_id: SessionId,
        run_id: RunId,
        input: String,
    },
    WaitSubagent {
        session_id: SessionId,
        run_id: RunId,
    },
    CancelSubagent {
        session_id: SessionId,
        run_id: RunId,
    },
    AppendSubagentLifecycle {
        session_id: SessionId,
        run_id: RunId,
        snapshot: Box<SubagentSnapshot>,
    },
    AgentTeamSnapshot,
    AgentTeamTaskCreate {
        request: AgentTeamTaskCreate,
    },
    AgentTeamTaskReplace {
        task_id: AgentTeamTaskId,
        request: AgentTeamTaskReplace,
    },
    AgentTeamTaskDelete {
        task_id: AgentTeamTaskId,
        expected_revision: u64,
    },
    AgentTeamMessageSend {
        request: AgentTeamMessageSend,
    },
    AgentTeamMessageRead {
        message_id: AgentTeamMessageId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudHostOutcome {
    Ok { value: serde_json::Value },
    Error { error: HarnessError },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParentToChildFrame {
    ModelDelta {
        request_id: u64,
        delta: String,
    },
    ModelReasoningDelta {
        request_id: u64,
        delta: String,
    },
    ModelRetryScheduled {
        request_id: u64,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    },
    ModelRetryStarted {
        request_id: u64,
        retry: u32,
    },
    ModelRetryCancelled {
        request_id: u64,
        retry: u32,
    },
    ModelComplete {
        request_id: u64,
        response: ModelResponse,
    },
    ModelError {
        request_id: u64,
        error: HarnessError,
    },
    QuestionAnswer {
        answer: UserAnswer,
    },
    QuestionError {
        question_id: String,
        error: HarnessError,
    },
    SessionCommand {
        command_id: CommandId,
        command: ChildSessionCommand,
    },
    HostReply {
        request_id: u64,
        outcome: CloudHostOutcome,
    },
}

pub struct ParentProtocol {
    input: Mutex<Option<ChildStdin>>,
}

impl ParentProtocol {
    #[must_use]
    pub fn new(input: ChildStdin) -> Self {
        Self {
            input: Mutex::new(Some(input)),
        }
    }

    pub async fn send(&self, frame: &ParentToChildFrame) -> Result<(), HarnessError> {
        let mut encoded = serde_json::to_vec(frame).map_err(|error| {
            HarnessError::execution(format!("encode parent-to-child frame: {error}"))
        })?;
        encoded.push(b'\n');
        let mut input = self.input.lock().await;
        let input = input
            .as_mut()
            .ok_or_else(|| HarnessError::execution("cloud child input is closed"))?;
        input.write_all(&encoded).await.map_err(|error| {
            HarnessError::execution(format!("write parent-to-child frame: {error}"))
        })?;
        input.flush().await.map_err(|error| {
            HarnessError::execution(format!("flush parent-to-child frame: {error}"))
        })
    }

    pub async fn close(&self) -> Result<(), HarnessError> {
        let input = self.input.lock().await.take();
        if let Some(mut input) = input {
            input.shutdown().await.map_err(|error| {
                HarnessError::execution(format!("close cloud child input: {error}"))
            })?;
        }
        Ok(())
    }
}

pub struct ChildProtocol {
    output: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
}

impl Default for ChildProtocol {
    fn default() -> Self {
        Self {
            output: Mutex::new(Box::new(tokio::io::stdout())),
        }
    }
}

impl ChildProtocol {
    #[cfg(test)]
    pub(crate) fn with_output(output: impl AsyncWrite + Send + Unpin + 'static) -> Self {
        Self {
            output: Mutex::new(Box::new(output)),
        }
    }

    pub async fn emit(&self, frame: &ChildToParentFrame) -> Result<(), HarnessError> {
        let mut encoded = serde_json::to_vec(frame).map_err(|error| {
            HarnessError::execution(format!("encode child-to-parent frame: {error}"))
        })?;
        encoded.push(b'\n');
        let mut output = self.output.lock().await;
        output.write_all(&encoded).await.map_err(|error| {
            HarnessError::execution(format!("write child-to-parent frame: {error}"))
        })?;
        output.flush().await.map_err(|error| {
            HarnessError::execution(format!("flush child-to-parent frame: {error}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{SubmissionDelivery, SubmissionId, UserMessageSource};

    #[test]
    fn steering_and_cancel_frames_are_directional_and_round_trip() {
        let steer = ParentToChildFrame::SessionCommand {
            command_id: CommandId::new("steer-1"),
            command: ChildSessionCommand::Steer {
                input: Box::new(SteeringInput {
                    provenance: None,
                    submission_id: SubmissionId::new("submission"),
                    input: "continue".to_owned(),
                    display_input: None,
                    source: UserMessageSource::Submission {
                        regenerate_from: None,
                        submission_id: SubmissionId::new("submission"),
                        created_at_ms: 1,
                        delivery: SubmissionDelivery::Steer,
                        skill_name: None,
                    },
                    references: Vec::new(),
                    reference_contexts: Vec::new(),
                    attachments: Vec::new(),
                }),
            },
        };
        let encoded = serde_json::to_string(&steer).unwrap();
        assert_eq!(
            serde_json::from_str::<ParentToChildFrame>(&encoded).unwrap(),
            steer
        );
        let reply = ChildToParentFrame::SessionCommandReply {
            command_id: CommandId::new("steer-1"),
            outcome: ChildSessionCommandOutcome::Steer { accepted: false },
        };
        assert_eq!(
            serde_json::from_str::<ChildToParentFrame>(&serde_json::to_string(&reply).unwrap())
                .unwrap(),
            reply
        );
    }

    #[test]
    fn cloud_subagent_host_frames_round_trip_without_exposing_database_state() {
        let request = ChildToParentFrame::HostRequest {
            request_id: 7,
            request: CloudHostRequest::CreateSubagent {
                subagent_id: ternilo_protocol::SubagentId::new("researcher"),
                provider: "in-process".to_owned(),
                label: "Researcher".to_owned(),
                task: "inspect the project".to_owned(),
                transcript_kind: ternilo_protocol::SubagentTranscriptKind::Conversation,
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<ChildToParentFrame>(&encoded).unwrap(),
            request,
        );
        assert!(!encoded.contains("database_url"));
        let reply = ParentToChildFrame::HostReply {
            request_id: 7,
            outcome: CloudHostOutcome::Ok {
                value: serde_json::json!("child-session"),
            },
        };
        assert_eq!(
            serde_json::from_str::<ParentToChildFrame>(&serde_json::to_string(&reply).unwrap(),)
                .unwrap(),
            reply,
        );
    }

    #[test]
    fn every_agent_team_host_operation_round_trips_on_the_shared_pipe() {
        let task_id = ternilo_protocol::AgentTeamTaskId::new("task-1");
        let requests = vec![
            CloudHostRequest::AgentTeamSnapshot,
            CloudHostRequest::AgentTeamTaskCreate {
                request: ternilo_protocol::AgentTeamTaskCreate {
                    subject: "Review".to_owned(),
                    description: String::new(),
                    status: ternilo_protocol::AgentTeamTaskStatus::Pending,
                    dependencies: Vec::new(),
                    owner: None,
                },
            },
            CloudHostRequest::AgentTeamTaskReplace {
                task_id: task_id.clone(),
                request: ternilo_protocol::AgentTeamTaskReplace {
                    expected_revision: 1,
                    subject: "Review updated".to_owned(),
                    description: String::new(),
                    status: ternilo_protocol::AgentTeamTaskStatus::InProgress,
                    dependencies: Vec::new(),
                    owner: None,
                },
            },
            CloudHostRequest::AgentTeamTaskDelete {
                task_id,
                expected_revision: 2,
            },
            CloudHostRequest::AgentTeamMessageSend {
                request: ternilo_protocol::AgentTeamMessageSend {
                    to: ternilo_protocol::AgentTeamMemberId::new("member-2"),
                    content: "Ready".to_owned(),
                },
            },
            CloudHostRequest::AgentTeamMessageRead {
                message_id: ternilo_protocol::AgentTeamMessageId::new("message-1"),
            },
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let frame = ChildToParentFrame::HostRequest {
                request_id: u64::try_from(index).unwrap() + 1,
                request,
            };
            let encoded = serde_json::to_string(&frame).unwrap();
            assert_eq!(
                serde_json::from_str::<ChildToParentFrame>(&encoded).unwrap(),
                frame,
            );
            assert!(!encoded.contains("tenant_id"));
            assert!(!encoded.contains("database_url"));
        }
    }
}
