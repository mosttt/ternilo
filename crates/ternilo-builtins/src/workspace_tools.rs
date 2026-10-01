use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    Attachments, AttachmentsClient, HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment,
    RunEnvironmentClient, Sessions, SessionsClient, Shell, ShellClient, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools, ToolsClient, WorkspaceFiles, WorkspaceFilesClient,
};
use ternilo_protocol::{
    Attachment, FileListRequest, FileReadRequest, FileReplaceRequest, FileSearchRequest,
    FileWriteRequest, GoalStatus, HarnessError, PlanItem, SessionEventKind, SessionMode,
    ShellRequest, ToolOutput, ToolSpec, UserQuestion, UserQuestionOption, UserQuestionPresentation,
};

use crate::{EmptyConfig, factory, parse_config};

pub const FILE_TOOLS_KIND: &str = "ternilo.tools.files";
pub const SHELL_TOOL_KIND: &str = "ternilo.tool.shell";
pub const ASK_USER_TOOL_KIND: &str = "ternilo.tool.ask_user";
pub const PLAN_TOOL_KIND: &str = "ternilo.tool.plan";

component_descriptor! {
    static FILE_TOOLS_DESCRIPTOR: () {
        id: "ternilo/builtin-file-tools@1",
        requires: [Tools, WorkspaceFiles, Sessions, Attachments],
        provides: [],
    }
}

component_descriptor! {
    static SHELL_TOOL_DESCRIPTOR: () {
        id: "ternilo/builtin-shell-tool@1",
        requires: [Tools, Shell],
        provides: [],
    }
}

component_descriptor! {
    static ASK_USER_TOOL_DESCRIPTOR: () {
        id: "ternilo/builtin-ask-user-tool@1",
        requires: [Tools, Sessions, RunEnvironment],
        provides: [],
    }
}

component_descriptor! {
    static PLAN_TOOL_DESCRIPTOR: () {
        id: "ternilo/builtin-plan-tool@1",
        requires: [Tools, Sessions, RunEnvironment],
        provides: [],
    }
}

pub fn file_tools_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: FILE_TOOLS_KIND,
            requires: &[
                "ternilo/tools@1",
                "ternilo/workspace-files@2",
                "ternilo/sessions@1",
                "ternilo/attachments@1",
            ],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(FileToolsPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

pub fn shell_tool_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: SHELL_TOOL_KIND,
            requires: &["ternilo/tools@1", "ternilo/shell@1"],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(ShellToolPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

pub fn ask_user_tool_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: ASK_USER_TOOL_KIND,
            requires: &[
                "ternilo/tools@1",
                "ternilo/sessions@1",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(AskUserToolPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
}

pub fn plan_tool_factory() -> PluginFactory {
    factory(
        PluginManifest {
            kind: PLAN_TOOL_KIND,
            requires: &[
                "ternilo/tools@1",
                "ternilo/sessions@1",
                "ternilo/run-environment@1",
            ],
            provides: &[],
        },
        |value| {
            let _: EmptyConfig = parse_config(value)?;
            Ok(Arc::new(PlanToolPlugin))
        },
    )
    .with_config_schema::<EmptyConfig>()
    .with_projection_unit(crate::projection::latest_unit("plan", 1, plan_value))
    .with_projection_unit(crate::projection::latest_unit(
        "plan_review",
        1,
        plan_review_value,
    ))
    .with_projection_unit(crate::projection::latest_unit("todos", 1, todo_value))
    .with_projection_unit(crate::projection::latest_unit("goal", 1, goal_value))
}

fn plan_value(kind: &SessionEventKind) -> Option<Value> {
    match kind {
        SessionEventKind::PlanUpdated { explanation, items } => Some(json!({
            "explanation": explanation,
            "items": items,
        })),
        _ => None,
    }
}

fn plan_review_value(kind: &SessionEventKind) -> Option<Value> {
    match kind {
        SessionEventKind::PlanReviewCompleted {
            plan,
            approved,
            feedback,
        } => Some(json!({
            "plan": plan,
            "approved": approved,
            "feedback": feedback,
        })),
        _ => None,
    }
}

fn todo_value(kind: &SessionEventKind) -> Option<Value> {
    match kind {
        SessionEventKind::TodoUpdated { items } => Some(json!({ "items": items })),
        _ => None,
    }
}

fn goal_value(kind: &SessionEventKind) -> Option<Value> {
    match kind {
        SessionEventKind::GoalUpdated { objective, status } => Some(json!({
            "objective": objective,
            "status": status,
        })),
        _ => None,
    }
}

struct FileToolsPlugin;

impl HarnessPlugin for FileToolsPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &FILE_TOOLS_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("file tools declares Tools");
        let files = context
            .context()
            .service::<WorkspaceFiles>()
            .expect("file tools declares WorkspaceFiles");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("file tools declares Sessions");
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("file tools declares Attachments");
        Activation::Once(Box::pin(async move {
            let registrations = register_file_tools(&tools, files, sessions, attachments)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                unregister_all(&tools, registrations).await
            })))
        }))
    }
}

async fn register_file_tools(
    tools: &ToolsClient,
    files: WorkspaceFilesClient,
    sessions: SessionsClient,
    attachments: AttachmentsClient,
) -> Result<Vec<u64>, HarnessError> {
    let mut registrations = Vec::new();
    for (spec, operation) in file_tool_definitions() {
        registrations.push(
            tools
                .register_tool(ToolRegistration {
                    spec,
                    effect: operation.effect(),
                    handler: Arc::new(FileTool {
                        files: files.clone(),
                        sessions: sessions.clone(),
                        attachments: attachments.clone(),
                        operation,
                    }),
                })
                .await?,
        );
    }
    Ok(registrations)
}

fn file_tool_definitions() -> [(ToolSpec, FileOperation); 5] {
    [
        (
            ToolSpec {
                name: "read_file".to_owned(),
                description: "Read a UTF-8 text file inside the current workspace with optional one-based line bounds.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "start_line": { "type": "integer", "minimum": 1 },
                        "line_count": { "type": "integer", "minimum": 1, "maximum": 2000 }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            },
            FileOperation::Read,
        ),
        (
            ToolSpec {
                name: "write_file".to_owned(),
                description: "Create or overwrite a UTF-8 text file inside the current workspace.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            },
            FileOperation::Write,
        ),
        (
            ToolSpec {
                name: "replace_in_file".to_owned(),
                description: "Replace literal text in a workspace file; old text must be unique unless replace_all is true.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old": { "type": "string" },
                        "new": { "type": "string" },
                        "replace_all": { "type": "boolean", "default": false }
                    },
                    "required": ["path", "old", "new"],
                    "additionalProperties": false
                }),
            },
            FileOperation::Replace,
        ),
        (
            ToolSpec {
                name: "glob_files".to_owned(),
                description: "List workspace files matching a gitignore-style glob using ripgrep. Returns files, warnings for unreadable paths, and a truncated flag; warnings mean the listing may be incomplete.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 200 }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            },
            FileOperation::Glob,
        ),
        (
            ToolSpec {
                name: "search_files".to_owned(),
                description: "Search text in workspace files using a ripgrep regular expression. Returns matches, warnings for unreadable paths, and a truncated flag. An empty result with warnings does not prove the pattern is absent.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "file_glob": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
                    },
                    "required": ["pattern"],
                    "additionalProperties": false
                }),
            },
            FileOperation::Search,
        ),
    ]
}

#[derive(Clone, Copy)]
enum FileOperation {
    Read,
    Write,
    Replace,
    Glob,
    Search,
}

impl FileOperation {
    const fn effect(self) -> ternilo_kernel::ToolEffect {
        match self {
            Self::Read | Self::Glob | Self::Search => ternilo_kernel::ToolEffect::ReadOnly,
            Self::Write | Self::Replace => ternilo_kernel::ToolEffect::Mutating,
        }
    }
}

struct FileTool {
    files: WorkspaceFilesClient,
    sessions: SessionsClient,
    attachments: AttachmentsClient,
    operation: FileOperation,
}

impl ToolHandler for FileTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let result = match self.operation {
                FileOperation::Read => {
                    let request: FileReadRequest = parse_arguments(arguments)?;
                    serde_json::to_value(self.files.read_text(request).await?)
                }
                FileOperation::Write => {
                    let request: FileWriteRequest = parse_arguments(arguments)?;
                    let written_content = request.content.clone();
                    let result = self.files.write_text(request).await?;
                    let attachment = self
                        .attachments
                        .store(Attachment {
                            name: result.path.clone(),
                            media_type: "text/plain; charset=utf-8".to_owned(),
                            content: written_content,
                        })
                        .await?;
                    self.sessions
                        .append(
                            context.run_id.clone(),
                            SessionEventKind::DeliverableProduced {
                                path: result.path.clone(),
                                operation: "write".to_owned(),
                                attachment,
                            },
                        )
                        .await?;
                    serde_json::to_value(result)
                }
                FileOperation::Replace => {
                    let request: ReplaceArguments = parse_arguments(arguments)?;
                    let result = self
                        .files
                        .replace_text(FileReplaceRequest {
                            path: request.path,
                            old: request.old,
                            new: request.new,
                            replace_all: request.replace_all,
                        })
                        .await?;
                    let updated_file = self
                        .files
                        .read_text(FileReadRequest {
                            path: result.path.clone(),
                            start_line: None,
                            line_count: None,
                        })
                        .await?;
                    let attachment = self
                        .attachments
                        .store(Attachment {
                            name: result.path.clone(),
                            media_type: "text/plain; charset=utf-8".to_owned(),
                            content: updated_file.content,
                        })
                        .await?;
                    self.sessions
                        .append(
                            context.run_id.clone(),
                            SessionEventKind::DeliverableProduced {
                                path: result.path.clone(),
                                operation: "replace".to_owned(),
                                attachment,
                            },
                        )
                        .await?;
                    serde_json::to_value(result)
                }
                FileOperation::Glob => {
                    let request: GlobArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.files
                            .list_files(FileListRequest {
                                pattern: request.pattern,
                                limit: request.limit,
                            })
                            .await?,
                    )
                }
                FileOperation::Search => {
                    let request: SearchArguments = parse_arguments(arguments)?;
                    serde_json::to_value(
                        self.files
                            .search_text(FileSearchRequest {
                                pattern: request.pattern,
                                file_glob: request.file_glob,
                                limit: request.limit,
                            })
                            .await?,
                    )
                }
            }
            .map_err(|error| HarnessError::execution(format!("serialize tool result: {error}")))?;
            json_output(&result)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceArguments {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArguments {
    pattern: String,
    #[serde(default = "default_result_limit")]
    limit: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    pattern: String,
    file_glob: Option<String>,
    #[serde(default = "default_result_limit")]
    limit: u32,
}

const fn default_result_limit() -> u32 {
    200
}

struct ShellToolPlugin;

impl HarnessPlugin for ShellToolPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &SHELL_TOOL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("shell tool declares Tools");
        let shell = context
            .context()
            .service::<Shell>()
            .expect("shell tool declares Shell");
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "shell".to_owned(),
                        description: "Run a platform shell command in the workspace sandbox. Set full_access only when the task requires host access; the user may be asked to approve it.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "command": { "type": "string" },
                                "timeout_ms": { "type": "integer", "minimum": 100, "maximum": 300_000, "default": 30000 },
                                "full_access": { "type": "boolean", "default": false }
                            },
                            "required": ["command"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::Mutating,
                    handler: Arc::new(ShellTool { shell }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct ShellTool {
    shell: ShellClient,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArguments {
    command: String,
    #[serde(default = "default_shell_timeout")]
    timeout_ms: u64,
    #[serde(default)]
    full_access: bool,
}

const fn default_shell_timeout() -> u64 {
    30_000
}

impl ToolHandler for ShellTool {
    fn approval_reason(&self, arguments: &Value) -> Option<String> {
        if !arguments
            .get("full_access")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return None;
        }
        let command = arguments
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("");
        let command = command.chars().take(2_000).collect::<String>();
        Some(format!(
            "the command requests access outside the workspace sandbox:\n\n{command}"
        ))
    }

    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: ShellArguments = parse_arguments(arguments)?;
            let result = self
                .shell
                .execute(
                    context.run_id.clone(),
                    ShellRequest {
                        command: arguments.command,
                        timeout_ms: arguments.timeout_ms,
                        full_access: arguments.full_access,
                        stdin: None,
                        env: std::collections::BTreeMap::new(),
                    },
                )
                .await?;
            let value = serde_json::to_value(result).map_err(|error| {
                HarnessError::execution(format!("serialize shell result: {error}"))
            })?;
            json_output(&value)
        })
    }
}

struct AskUserToolPlugin;

impl HarnessPlugin for AskUserToolPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ASK_USER_TOOL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("ask user tool declares Tools");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("ask user tool declares Sessions");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("ask user tool declares RunEnvironment");
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "ask_user".to_owned(),
                        description: "Pause the run and ask the user one or more concise structured questions before continuing.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "questions": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "id": { "type": "string" },
                                            "question": { "type": "string" },
                                            "detail": { "type": "string" },
                                            "header": { "type": "string" },
                                            "options": {
                                                "type": "array",
                                                "items": {
                                                    "type": "object",
                                                    "properties": {
                                                        "label": { "type": "string" },
                                                        "description": { "type": "string" }
                                                    },
                                                    "required": ["label"],
                                                    "additionalProperties": false
                                                }
                                            },
                                            "multi_select": { "type": "boolean" }
                                        },
                                        "required": ["id", "question"],
                                        "additionalProperties": false
                                    }
                                }
                            },
                            "required": ["questions"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: Arc::new(AskUserTool {
                        sessions,
                        environment,
                        next_question: AtomicU64::new(1),
                    }),
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct AskUserTool {
    sessions: SessionsClient,
    environment: RunEnvironmentClient,
    next_question: AtomicU64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserArguments {
    questions: Vec<AskUserQuestionArguments>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AskUserQuestionArguments {
    id: String,
    question: String,
    detail: Option<String>,
    header: Option<String>,
    #[serde(default)]
    options: Vec<UserQuestionOption>,
    #[serde(default)]
    multi_select: bool,
}

impl ToolHandler for AskUserTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: AskUserArguments = parse_arguments(arguments)?;
            if arguments.questions.is_empty()
                || arguments
                    .questions
                    .iter()
                    .any(|item| item.id.trim().is_empty() || item.question.trim().is_empty())
            {
                return Err(HarnessError::invalid(
                    "ask_user requires non-empty questions",
                ));
            }
            let sequence = self.next_question.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
                .as_millis();
            let questions = arguments
                .questions
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    let caller_id = item.id;
                    let question = UserQuestion {
                        id: format!("question-{timestamp}-{sequence}-{index}"),
                        question: item.question,
                        detail: item.detail,
                        header: item.header,
                        options: item.options,
                        multi_select: item.multi_select,
                        presentation: None,
                        tool_approval: None,
                    };
                    (caller_id, question)
                })
                .collect::<Vec<_>>();
            for (_, question) in &questions {
                self.sessions
                    .append(
                        context.run_id.clone(),
                        SessionEventKind::UserQuestionAsked {
                            question: question.clone(),
                        },
                    )
                    .await?;
            }
            let answers = futures_util::future::try_join_all(
                questions
                    .iter()
                    .map(|(_, question)| self.environment.ask_user(question.clone())),
            )
            .await?;
            for answer in &answers {
                self.sessions
                    .append(
                        context.run_id.clone(),
                        SessionEventKind::UserQuestionAnswered {
                            answer: answer.clone(),
                        },
                    )
                    .await?;
            }
            let answers = questions
                .iter()
                .zip(answers)
                .map(|((caller_id, _), answer)| {
                    let mut value = json!({
                        "id": caller_id,
                        "selected": answer.selected,
                    });
                    if let Some(custom) = answer.custom {
                        value["custom"] = Value::String(custom);
                    }
                    value
                })
                .collect::<Vec<_>>();
            Ok(ToolOutput {
                content: serde_json::to_string(&json!({ "answers": answers }))
                    .map_err(|error| HarnessError::execution(error.to_string()))?,
                is_error: false,
            })
        })
    }
}

struct PlanToolPlugin;

impl HarnessPlugin for PlanToolPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &PLAN_TOOL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("plan tool declares Tools");
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("plan tool declares Sessions");
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("plan tool declares RunEnvironment");
        Activation::Once(Box::pin(async move {
            let mut registrations = Vec::new();
            for (spec, operation) in planning_tool_definitions() {
                registrations.push(
                    tools
                        .register_tool(ToolRegistration {
                            spec,
                            effect: ternilo_kernel::ToolEffect::ReadOnly,
                            handler: Arc::new(PlanningTool {
                                sessions: sessions.clone(),
                                environment: environment.clone(),
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

fn planning_tool_definitions() -> [(ToolSpec, PlanningOperation); 4] {
    let item_schema = json!({
        "type": "object",
        "properties": {
            "step": { "type": "string" },
            "status": { "enum": ["pending", "in_progress", "completed"] }
        },
        "required": ["step", "status"],
        "additionalProperties": false
    });
    [
        (
            ToolSpec {
                name: "update_plan".to_owned(),
                description: "Publish the current task plan. Exactly one item may be in_progress."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "explanation": { "type": "string" },
                        "items": { "type": "array", "items": item_schema.clone() }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }),
            },
            PlanningOperation::Plan,
        ),
        (
            ToolSpec {
                name: "todo_write".to_owned(),
                description: "Publish the current concise todo list.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "items": { "type": "array", "items": item_schema }
                    },
                    "required": ["items"],
                    "additionalProperties": false
                }),
            },
            PlanningOperation::Todo,
        ),
        (
            ToolSpec {
                name: "update_goal".to_owned(),
                description: "Update the goal explicitly enabled by the user with /goal. Never start or resume goals on your own."
                    .to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "objective": { "type": "string" },
                        "status": { "enum": ["active", "complete", "blocked"] }
                    },
                    "required": ["objective", "status"],
                    "additionalProperties": false
                }),
            },
            PlanningOperation::Goal,
        ),
        (
            ToolSpec {
                name: "exit_plan_mode".to_owned(),
                description: "Use only in plan mode. Present the complete Markdown plan for explicit user review. Approval leaves plan mode after this turn; feedback keeps plan mode active.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "plan": {
                            "type": "string",
                            "description": "The complete decision-ready Markdown plan, beginning with a heading."
                        }
                    },
                    "required": ["plan"],
                    "additionalProperties": false
                }),
            },
            PlanningOperation::Exit,
        ),
    ]
}

#[derive(Clone, Copy)]
enum PlanningOperation {
    Plan,
    Todo,
    Goal,
    Exit,
}

struct PlanningTool {
    sessions: SessionsClient,
    environment: RunEnvironmentClient,
    operation: PlanningOperation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanArguments {
    explanation: Option<String>,
    items: Vec<PlanItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoArguments {
    items: Vec<PlanItem>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalArguments {
    objective: String,
    status: GoalStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitPlanArguments {
    plan: String,
}

impl ToolHandler for PlanningTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if matches!(self.operation, PlanningOperation::Exit) {
                return self.exit_plan_mode(context, arguments).await;
            }
            let (kind, result) = match self.operation {
                PlanningOperation::Plan => {
                    let arguments: PlanArguments = parse_arguments(arguments)?;
                    validate_plan_items(&arguments.items)?;
                    (
                        SessionEventKind::PlanUpdated {
                            explanation: arguments.explanation,
                            items: arguments.items,
                        },
                        "plan updated",
                    )
                }
                PlanningOperation::Todo => {
                    let arguments: TodoArguments = parse_arguments(arguments)?;
                    validate_plan_items(&arguments.items)?;
                    (
                        SessionEventKind::TodoUpdated {
                            items: arguments.items,
                        },
                        "todo list updated",
                    )
                }
                PlanningOperation::Goal => {
                    if !crate::agent_goal::goal_tool_enabled(&self.sessions.events().await) {
                        return Err(HarnessError::policy(
                            "goals must be enabled by the user with /goal",
                        ));
                    }
                    let arguments: GoalArguments = parse_arguments(arguments)?;
                    if arguments.objective.trim().is_empty() {
                        return Err(HarnessError::invalid("goal objective must not be empty"));
                    }
                    (
                        SessionEventKind::GoalUpdated {
                            objective: arguments.objective,
                            status: arguments.status,
                        },
                        "goal updated",
                    )
                }
                PlanningOperation::Exit => unreachable!("exit is handled before this match"),
            };
            self.sessions.append(context.run_id, kind).await?;
            Ok(ToolOutput {
                content: result.to_owned(),
                is_error: false,
            })
        })
    }
}

impl PlanningTool {
    async fn exit_plan_mode(
        &self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Result<ToolOutput, HarnessError> {
        if self.environment.session_mode().await != SessionMode::Plan {
            return Err(HarnessError::policy(
                "exit_plan_mode may only be used while plan mode is active",
            ));
        }
        let arguments: ExitPlanArguments = parse_arguments(arguments)?;
        let plan = arguments.plan.trim().to_owned();
        if plan.is_empty() || plan.chars().count() > 100_000 {
            return Err(HarnessError::invalid(
                "exit_plan_mode plan must contain 1 to 100000 characters",
            ));
        }
        let title = plan
            .lines()
            .find_map(|line| {
                let heading = line.trim_start().trim_start_matches('#').trim();
                line.trim_start()
                    .starts_with('#')
                    .then(|| heading.to_owned())
            })
            .filter(|heading| !heading.is_empty())
            .ok_or_else(|| {
                HarnessError::invalid("exit_plan_mode plan must include a Markdown heading")
            })?;
        let question = UserQuestion {
            id: format!("plan-review-{}", context.call_id),
            question: "请审阅完整计划。批准后，Ternilo 会在本轮结束时切回执行模式；也可以选择继续规划或直接填写修改意见。".to_owned(),
            detail: None,
            header: None,
            options: vec![
                UserQuestionOption {
                    label: "Approve".to_owned(),
                    description: None,
                },
                UserQuestionOption {
                    label: "Keep planning".to_owned(),
                    description: None,
                },
            ],
            multi_select: false,
            presentation: Some(UserQuestionPresentation::PlanReview {
                title,
                plan: plan.clone(),
                approve_label: "Approve".to_owned(),
            }),
            tool_approval: None,
        };
        self.sessions
            .append(
                context.run_id.clone(),
                SessionEventKind::UserQuestionAsked {
                    question: question.clone(),
                },
            )
            .await?;
        let answer = self.environment.ask_user(question).await?;
        self.sessions
            .append(
                context.run_id.clone(),
                SessionEventKind::UserQuestionAnswered {
                    answer: answer.clone(),
                },
            )
            .await?;
        let approved = answer.chose("Approve");
        let feedback = (!approved).then(|| answer.display_text());
        self.sessions
            .append(
                context.run_id,
                SessionEventKind::PlanReviewCompleted {
                    plan,
                    approved,
                    feedback: feedback.clone(),
                },
            )
            .await?;
        Ok(ToolOutput {
            content: if approved {
                serde_json::to_string_pretty(&json!({
                    "approved": true,
                    "message": "Plan approved. Plan mode will end after this turn; begin implementation on the next turn."
                }))
            } else {
                serde_json::to_string_pretty(&json!({
                    "approved": false,
                    "feedback": feedback,
                    "message": "Remain in plan mode, revise the plan, and present it again."
                }))
            }
            .map_err(|error| HarnessError::execution(format!("serialize plan review: {error}")))?,
            is_error: !approved,
        })
    }
}

fn validate_plan_items(items: &[PlanItem]) -> Result<(), HarnessError> {
    if items
        .iter()
        .filter(|item| matches!(item.status, ternilo_protocol::PlanItemStatus::InProgress))
        .count()
        > 1
    {
        Err(HarnessError::invalid(
            "at most one plan item may be in_progress",
        ))
    } else if items.iter().any(|item| item.step.trim().is_empty()) {
        Err(HarnessError::invalid("plan steps must not be empty"))
    } else {
        Ok(())
    }
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

fn parse_arguments<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HarnessError> {
    serde_json::from_value(value)
        .map_err(|error| HarnessError::invalid(format!("invalid tool arguments: {error}")))
}

fn json_output(value: &Value) -> Result<ToolOutput, HarnessError> {
    Ok(ToolOutput {
        content: serde_json::to_string_pretty(&value)
            .map_err(|error| HarnessError::execution(format!("serialize tool output: {error}")))?,
        is_error: false,
    })
}
