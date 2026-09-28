use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    AgentTeam, AgentTeamClient, HarnessPlugin, PluginFactory, PluginManifest, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools, ToolsClient,
};
use ternilo_protocol::{
    AgentTeamMessageId, AgentTeamMessageSend, AgentTeamTaskCreate, AgentTeamTaskId,
    AgentTeamTaskReplace, HarnessError, ToolOutput, ToolSpec,
};

use crate::{EmptyConfig, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.tools.agent_team";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-agent-team-tools@1",
        requires: [Tools, AgentTeam],
        provides: [],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1", "ternilo/agent-team@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(AgentTeamToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

struct AgentTeamToolsPlugin;

impl HarnessPlugin for AgentTeamToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("Agent Team tools declares Tools");
        let agent_team = context
            .context()
            .service::<AgentTeam>()
            .expect("Agent Team tools declares AgentTeam");
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for (spec, operation) in definitions() {
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: operation.effect(),
                            handler: Arc::new(AgentTeamTool {
                                agent_team: agent_team.clone(),
                                operation,
                            }),
                        })
                        .await
                        .map_err(|error| {
                            linorun_core::ActivationFailure::user(error.to_string())
                        })?,
                );
            }
            Ok(Some(effect::inverse(move || async move {
                unregister_all(&tools, registrations).await
            })))
        }))
    }
}

fn definitions() -> Vec<(ToolSpec, AgentTeamOperation)> {
    vec![
        (
            ToolSpec {
                name: "team_task_list".to_owned(),
                description: "List the current Agent Team roster and shared tasks.".to_owned(),
                input_schema: empty_schema(),
            },
            AgentTeamOperation::ListTasks,
        ),
        (
            ToolSpec {
                name: "team_task_create".to_owned(),
                description: "Create a shared Agent Team task, optionally assigned to a member."
                    .to_owned(),
                input_schema: task_create_schema(),
            },
            AgentTeamOperation::CreateTask,
        ),
        (
            ToolSpec {
                name: "team_task_update".to_owned(),
                description:
                    "Replace a shared Agent Team task using its current revision for conflict detection."
                        .to_owned(),
                input_schema: task_update_schema(),
            },
            AgentTeamOperation::UpdateTask,
        ),
        (
            ToolSpec {
                name: "team_task_delete".to_owned(),
                description: "Delete a shared Agent Team task at its current revision.".to_owned(),
                input_schema: task_delete_schema(),
            },
            AgentTeamOperation::DeleteTask,
        ),
        (
            ToolSpec {
                name: "team_mailbox".to_owned(),
                description: "Read messages sent by or to the current Agent Team member.".to_owned(),
                input_schema: empty_schema(),
            },
            AgentTeamOperation::Mailbox,
        ),
        (
            ToolSpec {
                name: "team_message_send".to_owned(),
                description: "Send a persistent message to another Agent Team member.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "to": { "type": "string", "description": "Opaque member ID from team_task_list or team_mailbox." },
                        "content": { "type": "string", "minLength": 1 }
                    },
                    "required": ["to", "content"],
                    "additionalProperties": false
                }),
            },
            AgentTeamOperation::SendMessage,
        ),
        (
            ToolSpec {
                name: "team_message_read".to_owned(),
                description: "Mark one message addressed to the current Team member as read."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "message_id": { "type": "string" } },
                    "required": ["message_id"],
                    "additionalProperties": false
                }),
            },
            AgentTeamOperation::ReadMessage,
        ),
    ]
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn status_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["pending", "in_progress", "blocked", "completed", "cancelled"]
    })
}

fn task_fields() -> Value {
    json!({
        "subject": { "type": "string", "minLength": 1 },
        "description": { "type": "string", "default": "" },
        "status": status_schema(),
        "dependencies": {
            "type": "array",
            "items": { "type": "string" },
            "uniqueItems": true,
            "default": []
        },
        "owner": {
            "type": ["string", "null"],
            "description": "Opaque member ID, or null for an unassigned task."
        }
    })
}

fn task_create_schema() -> Value {
    json!({
        "type": "object",
        "properties": task_fields(),
        "required": ["subject"],
        "additionalProperties": false
    })
}

fn task_update_schema() -> Value {
    let mut fields = task_fields();
    let fields = fields.as_object_mut().expect("task fields is an object");
    fields.insert("task_id".to_owned(), json!({ "type": "string" }));
    fields.insert(
        "expected_revision".to_owned(),
        json!({ "type": "integer", "minimum": 1 }),
    );
    json!({
        "type": "object",
        "properties": fields,
        "required": ["task_id", "expected_revision", "subject", "status"],
        "additionalProperties": false
    })
}

fn task_delete_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task_id": { "type": "string" },
            "expected_revision": { "type": "integer", "minimum": 1 }
        },
        "required": ["task_id", "expected_revision"],
        "additionalProperties": false
    })
}

#[derive(Clone, Copy)]
enum AgentTeamOperation {
    ListTasks,
    CreateTask,
    UpdateTask,
    DeleteTask,
    Mailbox,
    SendMessage,
    ReadMessage,
}

impl AgentTeamOperation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::ListTasks | Self::Mailbox => ternilo_kernel::ToolEffect::ReadOnly,
            Self::CreateTask
            | Self::UpdateTask
            | Self::DeleteTask
            | Self::SendMessage
            | Self::ReadMessage => ternilo_kernel::ToolEffect::Mutating,
        }
    }
}

struct AgentTeamTool {
    agent_team: AgentTeamClient,
    operation: AgentTeamOperation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskUpdateArguments {
    task_id: AgentTeamTaskId,
    expected_revision: u64,
    subject: String,
    #[serde(default)]
    description: String,
    status: ternilo_protocol::AgentTeamTaskStatus,
    #[serde(default)]
    dependencies: Vec<AgentTeamTaskId>,
    #[serde(default)]
    owner: Option<ternilo_protocol::AgentTeamMemberId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskDeleteArguments {
    task_id: AgentTeamTaskId,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageReadArguments {
    message_id: AgentTeamMessageId,
}

impl ToolHandler for AgentTeamTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = match self.operation {
                AgentTeamOperation::ListTasks => {
                    let _: EmptyConfig = parse_arguments(arguments)?;
                    let snapshot = self.agent_team.snapshot().await?;
                    json!({
                        "team_id": snapshot.team_id,
                        "current_member_id": snapshot.current_member_id,
                        "members": snapshot.members,
                        "tasks": snapshot.tasks
                    })
                }
                AgentTeamOperation::CreateTask => {
                    let request: AgentTeamTaskCreate = parse_arguments(arguments)?;
                    serde_json::to_value(self.agent_team.create_task(request).await?)
                        .map_err(serialize_error)?
                }
                AgentTeamOperation::UpdateTask => {
                    let arguments: TaskUpdateArguments = parse_arguments(arguments)?;
                    let request = AgentTeamTaskReplace {
                        expected_revision: arguments.expected_revision,
                        subject: arguments.subject,
                        description: arguments.description,
                        status: arguments.status,
                        dependencies: arguments.dependencies,
                        owner: arguments.owner,
                    };
                    serde_json::to_value(
                        self.agent_team
                            .replace_task(arguments.task_id, request)
                            .await?,
                    )
                    .map_err(serialize_error)?
                }
                AgentTeamOperation::DeleteTask => {
                    let arguments: TaskDeleteArguments = parse_arguments(arguments)?;
                    self.agent_team
                        .delete_task(arguments.task_id, arguments.expected_revision)
                        .await?;
                    json!({ "deleted": true })
                }
                AgentTeamOperation::Mailbox => {
                    let _: EmptyConfig = parse_arguments(arguments)?;
                    let snapshot = self.agent_team.snapshot().await?;
                    json!({
                        "team_id": snapshot.team_id,
                        "current_member_id": snapshot.current_member_id,
                        "members": snapshot.members,
                        "messages": snapshot.messages
                    })
                }
                AgentTeamOperation::SendMessage => {
                    let request: AgentTeamMessageSend = parse_arguments(arguments)?;
                    serde_json::to_value(self.agent_team.send_message(request).await?)
                        .map_err(serialize_error)?
                }
                AgentTeamOperation::ReadMessage => {
                    let arguments: MessageReadArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.agent_team
                            .mark_message_read(arguments.message_id)
                            .await?,
                    )
                    .map_err(serialize_error)?
                }
            };
            Ok(ToolOutput {
                content: serde_json::to_string_pretty(&result).map_err(|error| {
                    HarnessError::execution(format!("render Agent Team result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

fn parse_arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::invalid(format!("invalid Agent Team arguments: {error}")))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "used directly as a Result::map_err conversion"
)]
fn serialize_error(error: serde_json::Error) -> HarnessError {
    HarnessError::execution(format!("serialize Agent Team result: {error}"))
}

async fn unregister_all(tools: &ToolsClient, registrations: Vec<u64>) -> Result<(), CleanupError> {
    for registration in registrations.into_iter().rev() {
        tools
            .unregister_tool(registration)
            .await
            .map_err(|error| CleanupError::user(error.to_string()))?;
    }
    Ok(())
}
