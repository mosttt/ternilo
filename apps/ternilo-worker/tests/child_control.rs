use std::{path::PathBuf, process::Stdio, time::Duration};

use serde_json::{Value, json};
use ternilo_cloud::{CloudRunDraft, ExecutionEnvelope, WorkerPolicy};
use ternilo_protocol::{
    AgentId, ModelFinishReason, ModelResponse, PermissionPreset, PreparedSkillInvocation, RunId,
    RunLimits, SessionId, SessionMode, SteeringInput, SubmissionDelivery, SubmissionId, TenantId,
    UserId, UserMessageSource, WorkspaceBinding, WorkspaceId,
};
use ternilo_transport::CommandId;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
};

#[path = "support/child_control/fixture.rs"]
mod fixture;
#[path = "support/child_control/live.rs"]
mod live;
#[path = "support/child_control/subagents.rs"]
mod subagents;
#[path = "support/child_control/team.rs"]
mod team;

use fixture::*;
