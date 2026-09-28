use std::{
    fmt::Write as _,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    Attachments, AttachmentsClient, CommandRegistration, CommandResolver, HarnessPlugin,
    ModelOutput, Models, ModelsProvider, PluginFactory, PluginManifest, RunCancellation,
};
use ternilo_protocol::{
    CommandDescriptor, CommandInputDescriptor, HarnessError, MessageRole, ModelFinishReason,
    ModelRequest, ModelResponse, ToolCall,
};

use crate::{factory as make_factory, model_request_digest, parse_config};

pub const KIND: &str = "ternilo.model.rule";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-rule-model@1",
        requires: [Attachments],
        provides: [Models],
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RuleModelConfig {
    #[serde(default = "default_prefix")]
    prefix: String,
}

fn default_prefix() -> String {
    "ternilo: ".to_owned()
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/attachments@1"],
            provides: &["ternilo/models@3"],
        },
        |value| {
            let config: RuleModelConfig = parse_config(value)?;
            Ok(Arc::new(RuleModelPlugin {
                prefix: config.prefix,
            }))
        },
    )
    .with_config_schema::<RuleModelConfig>()
}

struct RuleModelPlugin {
    prefix: String,
}

impl HarnessPlugin for RuleModelPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("rule model declares Attachments");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let provider: Arc<dyn ModelsProvider> = Arc::new(RuleModel {
            prefix: self.prefix.clone(),
            next_call: AtomicU64::new(1),
            attachments,
        });
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Models>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide rule model: {error}"))
                })?;
            Ok(None)
        }))
    }
}

struct RuleModel {
    prefix: String,
    next_call: AtomicU64,
    attachments: AttachmentsClient,
}

impl ModelsProvider for RuleModel {
    fn context_window<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async { None })
    }

    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        mut request: ModelRequest,
        output: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            cancellation.check()?;
            let request_digest = model_request_digest(&request)?;
            for message in &mut request.messages {
                for attachment in &mut message.attachments {
                    *attachment = self.attachments.resolve(attachment.clone()).await?;
                }
            }
            let mut response = self.complete_request(&request)?;
            response.request_digest = Some(request_digest);
            if !response.content.is_empty() {
                output.emit(response.content.clone()).await?;
            }
            cancellation.check()?;
            Ok(response)
        })
    }
}

/// A deterministic slash command resolved to one registered Harness tool.
///
/// The parser is shared by the offline rule model and the real React agent so
/// command semantics never depend on a model deciding to reproduce a tool
/// call.
pub type RuleCommand = (&'static str, Value);

impl RuleModel {
    fn complete_request(&self, request: &ModelRequest) -> Result<ModelResponse, HarnessError> {
        let last = request
            .messages
            .last()
            .ok_or_else(|| HarnessError::execution("model request has no messages"))?;
        if last.role == MessageRole::Tool {
            return Ok(ModelResponse {
                provider: "ternilo".to_owned(),
                model: "rule".to_owned(),
                content: format!("{}{}", self.prefix, last.content),
                reasoning_content: None,
                provider_state: None,
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: ModelFinishReason::Stop,
                provider_request_id: None,
                attempts: 1,
                request_digest: None,
                replayed: false,
            });
        }
        if last.role != MessageRole::User {
            return Err(HarnessError::execution(
                "rule model expected a user or tool message",
            ));
        }
        if request.system_prompt == crate::session_title::SYSTEM_PROMPT {
            return Ok(ModelResponse {
                provider: "ternilo".to_owned(),
                model: "rule".to_owned(),
                content: crate::session_title::rule_title(&last.content)?,
                reasoning_content: None,
                provider_state: None,
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: ModelFinishReason::Stop,
                provider_request_id: None,
                attempts: 1,
                request_digest: None,
                replayed: false,
            });
        }

        let command = if let Some(raw) = last.content.strip_prefix("/lsp ") {
            Some(parse_lsp_command(raw, request)?)
        } else {
            parse_rule_command(&last.content)?.map(|(name, arguments)| (name.to_owned(), arguments))
        };
        let Some((name, arguments)) = command else {
            return Ok(ModelResponse {
                provider: "ternilo".to_owned(),
                model: "rule".to_owned(),
                content: format!("{}{}", self.prefix, rule_visible_content(last)),
                reasoning_content: None,
                provider_state: None,
                tool_calls: Vec::new(),
                usage: None,
                finish_reason: ModelFinishReason::Stop,
                provider_request_id: None,
                attempts: 1,
                request_digest: None,
                replayed: false,
            });
        };
        if !request.tools.iter().any(|tool| tool.name == name) {
            return Err(HarnessError::execution(format!(
                "{name} tool is not available"
            )));
        }
        let call_id = self.next_call.fetch_add(1, Ordering::Relaxed);
        Ok(ModelResponse {
            provider: "ternilo".to_owned(),
            model: "rule".to_owned(),
            content: String::new(),
            reasoning_content: None,
            provider_state: None,
            tool_calls: vec![ToolCall {
                id: format!("{name}-{call_id}"),
                name,
                arguments,
                presentation: None,
            }],
            usage: None,
            finish_reason: ModelFinishReason::ToolCalls,
            provider_request_id: None,
            attempts: 1,
            request_digest: None,
            replayed: false,
        })
    }
}

fn parse_lsp_command(raw: &str, request: &ModelRequest) -> Result<(String, Value), HarnessError> {
    let (method, params) = raw.split_once(' ').unwrap_or((raw, "{}"));
    let params: Value = serde_json::from_str(params)
        .map_err(|error| HarnessError::invalid(format!("/lsp params must be JSON: {error}")))?;
    let name = request
        .tools
        .iter()
        .find(|tool| tool.name.starts_with("lsp__"))
        .map(|tool| tool.name.clone())
        .ok_or_else(|| HarnessError::execution("no LSP tool is configured"))?;
    Ok((name, json!({ "method": method, "params": params })))
}

pub fn parse_rule_command(content: &str) -> Result<Option<RuleCommand>, HarnessError> {
    let content = content.trim();
    if let Some(command) = parse_workspace_command(content) {
        return Ok(Some(command));
    }
    if let Some(command) = parse_terminal_command(content) {
        return Ok(Some(command));
    }
    if let Some(command) = parse_session_command(content) {
        return Ok(Some(command));
    }
    if let Some(command) = parse_subagent_command(content) {
        return Ok(Some(command));
    }
    if let Some(command) = parse_integration_command(content)? {
        return Ok(Some(command));
    }
    if let Some(name) = rule_command_name(content) {
        return Err(HarnessError::invalid(format!(
            "invalid /{name} command; usage: {}",
            rule_command_usage(name)
        )));
    }
    Ok(None)
}

/// Return the stable name of a slash command owned by the direct tool command
/// plane. `/feedback`, `/plan`, and `/skill` intentionally belong to their
/// dedicated typed Web/application paths and are therefore not returned.
#[must_use]
pub fn rule_command_name(content: &str) -> Option<&'static str> {
    let token = content.split_whitespace().next()?;
    let name = token.strip_prefix('/')?;
    DIRECT_RULE_COMMANDS
        .iter()
        .copied()
        .find(|candidate| *candidate == name)
}

const DIRECT_RULE_COMMANDS: &[&str] = &[
    "read",
    "write",
    "glob",
    "grep",
    "shell",
    "shell!",
    "terminal-open",
    "terminals",
    "terminal-send",
    "terminal-read",
    "terminal-close",
    "ask",
    "goal",
    "update-plan",
    "exit-plan",
    "todo",
    "session-search",
    "session-trace",
    "session-read",
    "schedule-after",
    "schedules",
    "schedule-delete",
    "compact",
    "agent-bg-on",
    "agent-on",
    "agent",
    "agents",
    "agent-send",
    "agent-wait",
    "agent-stop",
    "skills",
    "jobs",
    "job-output",
    "job-kill",
    "job",
    "fetch",
    "search",
    "workflow",
    "code",
    "extensions",
    "extension-enable",
    "extension-disable",
    "extension-mount",
    "extension-unmount",
    "extension-revoke",
];

pub(crate) fn builtin_command_registrations() -> Vec<CommandRegistration> {
    DIRECT_RULE_COMMANDS
        .iter()
        .map(|name| {
            let tool = rule_command_tool(name);
            let usage = rule_command_usage(name);
            let input = usage
                .split_once(' ')
                .map(|(_, hint)| CommandInputDescriptor {
                    hint: hint.to_owned(),
                    images: false,
                });
            CommandRegistration {
                descriptor: CommandDescriptor {
                    name: (*name).to_owned(),
                    description: rule_command_description(name).to_owned(),
                    input,
                },
                tool_name: tool.to_owned(),
                resolver: Arc::new(BuiltinCommandResolver {
                    command_name: name,
                    tool_name: tool,
                }),
            }
        })
        .collect()
}

struct BuiltinCommandResolver {
    command_name: &'static str,
    tool_name: &'static str,
}

impl CommandResolver for BuiltinCommandResolver {
    fn resolve(&self, input: &str) -> Result<Value, HarnessError> {
        let command = if input.is_empty() {
            format!("/{}", self.command_name)
        } else {
            format!("/{} {input}", self.command_name)
        };
        let Some((tool_name, arguments)) = parse_rule_command(&command)? else {
            return Err(HarnessError::invalid(format!(
                "invalid /{} command; usage: {}",
                self.command_name,
                rule_command_usage(self.command_name)
            )));
        };
        if tool_name != self.tool_name {
            return Err(HarnessError::execution(format!(
                "builtin /{} resolved to unexpected tool {tool_name:?}",
                self.command_name
            )));
        }
        Ok(arguments)
    }
}

fn rule_command_description(name: &str) -> &'static str {
    match name {
        "read" => "Read a workspace file",
        "write" => "Create or overwrite a workspace file",
        "glob" => "Find workspace files by glob",
        "grep" => "Search workspace text",
        "shell" => "Run a command in the workspace sandbox",
        "shell!" => "Run a command with full host access",
        "terminal-open" => "Open a persistent terminal",
        "terminals" => "List persistent terminals",
        "terminal-send" => "Send input to a persistent terminal",
        "terminal-read" => "Read persistent terminal output",
        "terminal-close" => "Close a persistent terminal",
        "ask" => "Ask the user a resumable question",
        "goal" => "Set and execute the Session goal, resume it, or manage its state",
        "update-plan" => "Replace the current execution plan",
        "exit-plan" => "Submit a plan and return to execution",
        "todo" => "Replace the current task list",
        "session-search" => "Search Session history",
        "session-trace" => "Inspect a Session trace summary",
        "session-read" => "Read events from another Session",
        "schedule-after" => "Create a delayed reminder",
        "schedules" => "List active reminders",
        "schedule-delete" => "Delete a reminder",
        "compact" => "Compact earlier conversation context",
        "agent-bg-on" => "Start a background sub-Agent with a provider",
        "agent-on" => "Start and wait for a sub-Agent with a provider",
        "agent" => "Start a background sub-Agent",
        "agents" => "List sub-Agents",
        "agent-send" => "Send a message to a sub-Agent",
        "agent-wait" => "Wait for a sub-Agent",
        "agent-stop" => "Stop a sub-Agent",
        "skills" => "List available Skills",
        "jobs" => "List background jobs",
        "job-output" => "Read background job output",
        "job-kill" => "Stop a background job",
        "job" => "Start a background job",
        "fetch" => "Read a web page",
        "search" => "Search the web",
        "workflow" => "Run a multi-Agent Workflow",
        "code" => "Run Rhai code",
        "extensions" => "Inspect runtime extensions",
        "extension-enable" => "Enable a runtime extension",
        "extension-disable" => "Disable a runtime extension",
        "extension-mount" => "Mount a runtime extension in this Session",
        "extension-unmount" => "Unmount a runtime extension from this Session",
        "extension-revoke" => "Revoke an installed runtime extension",
        _ => "Run a direct Session command",
    }
}

fn rule_command_tool(name: &str) -> &'static str {
    match name {
        "read" => "read_file",
        "write" => "write_file",
        "glob" => "glob_files",
        "grep" => "search_files",
        "shell" | "shell!" => "shell",
        "terminal-open" => "terminal_open",
        "terminals" => "terminal_list",
        "terminal-send" => "terminal_send",
        "terminal-read" => "terminal_read",
        "terminal-close" => "terminal_close",
        "ask" => "ask_user",
        "goal" => "update_goal",
        "update-plan" => "update_plan",
        "exit-plan" => "exit_plan_mode",
        "todo" => "todo_write",
        "session-search" => "session_search",
        "session-trace" => "session_trace",
        "session-read" => "session_event_read",
        "schedule-after" => "schedule_create",
        "schedules" => "schedule_list",
        "schedule-delete" => "schedule_delete",
        "compact" => "compact_context",
        "agent-bg-on" | "agent-on" | "agent" => "spawn_agent",
        "agents" => "list_agents",
        "agent-send" => "send_agent_message",
        "agent-wait" => "wait_agent",
        "agent-stop" => "interrupt_agent",
        "skills" => "list_skills",
        "jobs" => "job_list",
        "job-output" => "job_output",
        "job-kill" => "job_kill",
        "job" => "job_start",
        "fetch" => "web_fetch",
        "search" => "web_search",
        "workflow" => "workflow",
        "code" => "run_code",
        "extensions" => "extension_inspect",
        "extension-enable" | "extension-disable" => "extension_set_enabled",
        "extension-mount" | "extension-unmount" => "extension_set_mounted",
        "extension-revoke" => "extension_revoke",
        _ => "",
    }
}

fn rule_command_usage(name: &str) -> &'static str {
    match name {
        "read" => "/read <path>",
        "write" => "/write <path> <content>",
        "glob" => "/glob <pattern>",
        "grep" => "/grep <pattern>",
        "shell" => "/shell <command>",
        "shell!" => "/shell! <command>",
        "terminal-open" => "/terminal-open [name]",
        "terminals" => "/terminals",
        "terminal-send" => "/terminal-send <terminal-id> <input>",
        "terminal-read" => "/terminal-read <terminal-id>",
        "terminal-close" => "/terminal-close <terminal-id>",
        "ask" => "/ask <question>",
        "goal" => "/goal [edit|resume|complete|blocked] <objective>",
        "update-plan" => "/update-plan <step; step>",
        "exit-plan" => "/exit-plan <plan>",
        "todo" => "/todo <step; step>",
        "session-search" => "/session-search <query>",
        "session-trace" => "/session-trace <session-id>",
        "session-read" => "/session-read <session-id> [start-seq]",
        "schedule-after" => "/schedule-after <seconds> <prompt>",
        "schedules" => "/schedules",
        "schedule-delete" => "/schedule-delete <schedule-id>",
        "compact" => "/compact",
        "agent-bg-on" => "/agent-bg-on <provider> <task>",
        "agent-on" => "/agent-on <provider> <task>",
        "agent" => "/agent <task>",
        "agents" => "/agents",
        "agent-send" => "/agent-send <subagent-id> <message>",
        "agent-wait" => "/agent-wait <subagent-id>",
        "agent-stop" => "/agent-stop <subagent-id>",
        "skills" => "/skills",
        "jobs" => "/jobs",
        "job-output" => "/job-output <job-id>",
        "job-kill" => "/job-kill <job-id>",
        "job" => "/job <command>",
        "fetch" => "/fetch <url>",
        "search" => "/search <query>",
        "workflow" => "/workflow <json-object>",
        "code" => "/code <rhai>",
        "extensions" => "/extensions",
        "extension-enable" => "/extension-enable <package-id> <version>",
        "extension-disable" => "/extension-disable <package-id> <version>",
        "extension-mount" => "/extension-mount <package-id> <version> <settings-json>",
        "extension-unmount" => "/extension-unmount <package-id> <version>",
        "extension-revoke" => "/extension-revoke <package-id> <version>",
        _ => "/<command>",
    }
}

fn parse_workspace_command(content: &str) -> Option<RuleCommand> {
    if let Some(path) = content.strip_prefix("/read ") {
        Some(("read_file", json!({ "path": path.trim() })))
    } else if let Some(pattern) = content.strip_prefix("/glob ") {
        Some(("glob_files", json!({ "pattern": pattern.trim() })))
    } else if let Some(pattern) = content.strip_prefix("/grep ") {
        Some(("search_files", json!({ "pattern": pattern.trim() })))
    } else if let Some(command) = content.strip_prefix("/shell! ") {
        Some(("shell", json!({ "command": command, "full_access": true })))
    } else if let Some(command) = content.strip_prefix("/shell ") {
        Some(("shell", json!({ "command": command })))
    } else if let Some(arguments) = content.strip_prefix("/write ") {
        arguments
            .split_once(' ')
            .map(|(path, body)| ("write_file", json!({ "path": path, "content": body })))
    } else {
        None
    }
}

fn parse_terminal_command(content: &str) -> Option<RuleCommand> {
    if content == "/terminal-open" {
        Some(("terminal_open", json!({})))
    } else if let Some(name) = content.strip_prefix("/terminal-open ") {
        Some(("terminal_open", json!({ "name": name.trim() })))
    } else if content == "/terminals" {
        Some(("terminal_list", json!({})))
    } else if let Some(arguments) = content.strip_prefix("/terminal-send ") {
        arguments.split_once(' ').map(|(terminal_id, input)| {
            (
                "terminal_send",
                json!({ "terminal_id": terminal_id, "input": input }),
            )
        })
    } else if let Some(terminal_id) = content.strip_prefix("/terminal-read ") {
        Some((
            "terminal_read",
            json!({ "terminal_id": terminal_id.trim() }),
        ))
    } else if let Some(terminal_id) = content.strip_prefix("/terminal-close ") {
        Some((
            "terminal_close",
            json!({ "terminal_id": terminal_id.trim() }),
        ))
    } else {
        None
    }
}

fn parse_session_command(content: &str) -> Option<RuleCommand> {
    if let Some(prompt) = content.strip_prefix("/ask ") {
        Some((
            "ask_user",
            json!({
                "questions": [{
                    "id": "question",
                    "question": prompt,
                    "options": []
                }]
            }),
        ))
    } else if let Some(arguments) = content.strip_prefix("/goal ") {
        parse_goal_command(arguments)
    } else if let Some(steps) = content.strip_prefix("/update-plan ") {
        Some(("update_plan", json!({ "items": task_items(steps) })))
    } else if let Some(plan) = content.strip_prefix("/exit-plan ") {
        Some(("exit_plan_mode", json!({ "plan": plan })))
    } else if let Some(steps) = content.strip_prefix("/todo ") {
        Some(("todo_write", json!({ "items": task_items(steps) })))
    } else if let Some(query) = content.strip_prefix("/session-search ") {
        Some(("session_search", json!({ "query": query.trim() })))
    } else if let Some(session_id) = content.strip_prefix("/session-trace ") {
        Some(("session_trace", json!({ "session_id": session_id.trim() })))
    } else if let Some(arguments) = content.strip_prefix("/session-read ") {
        parse_session_read(arguments)
    } else if let Some(arguments) = content.strip_prefix("/schedule-after ") {
        parse_schedule_after(arguments)
    } else if content == "/schedules" {
        Some(("schedule_list", json!({})))
    } else if let Some(id) = content.strip_prefix("/schedule-delete ") {
        Some(("schedule_delete", json!({ "id": id.trim() })))
    } else if content == "/compact" {
        Some(("compact_context", json!({ "deterministic": true })))
    } else {
        None
    }
}

fn parse_goal_command(arguments: &str) -> Option<RuleCommand> {
    let arguments = arguments.trim();
    if matches!(arguments, "edit" | "resume" | "complete" | "blocked") {
        return None;
    }
    let (status, objective) = if let Some(objective) = arguments.strip_prefix("edit ") {
        ("active", objective)
    } else if let Some(objective) = arguments.strip_prefix("resume ") {
        ("active", objective)
    } else if let Some(objective) = arguments.strip_prefix("complete ") {
        ("complete", objective)
    } else if let Some(objective) = arguments.strip_prefix("blocked ") {
        ("blocked", objective)
    } else {
        ("active", arguments)
    };
    let objective = objective.trim();
    (!objective.is_empty()).then(|| {
        (
            "update_goal",
            json!({ "objective": objective, "status": status }),
        )
    })
}

fn parse_session_read(arguments: &str) -> Option<RuleCommand> {
    let mut fields = arguments.split_whitespace();
    fields.next().map(|session_id| {
        let start_seq = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        (
            "session_event_read",
            json!({ "session_id": session_id, "start_seq": start_seq }),
        )
    })
}

fn parse_schedule_after(arguments: &str) -> Option<RuleCommand> {
    arguments.split_once(' ').and_then(|(seconds, prompt)| {
        seconds.parse::<u64>().ok().map(|after_seconds| {
            (
                "schedule_create",
                json!({ "prompt": prompt, "after_seconds": after_seconds }),
            )
        })
    })
}

fn parse_subagent_command(content: &str) -> Option<RuleCommand> {
    if let Some(arguments) = content.strip_prefix("/agent-bg-on ") {
        arguments.split_once(' ').map(|(provider, task)| {
            (
                "spawn_agent",
                json!({ "provider": provider, "task": task, "background": true }),
            )
        })
    } else if let Some(arguments) = content.strip_prefix("/agent-on ") {
        arguments.split_once(' ').map(|(provider, task)| {
            (
                "spawn_agent",
                json!({ "provider": provider, "task": task, "background": false }),
            )
        })
    } else if let Some(task) = content.strip_prefix("/agent ") {
        Some(("spawn_agent", json!({ "task": task, "background": true })))
    } else if content == "/agents" {
        Some(("list_agents", json!({})))
    } else if let Some(arguments) = content.strip_prefix("/agent-send ") {
        arguments.split_once(' ').map(|(subagent_id, message)| {
            (
                "send_agent_message",
                json!({ "subagent_id": subagent_id, "message": message }),
            )
        })
    } else if let Some(subagent_id) = content.strip_prefix("/agent-wait ") {
        Some(("wait_agent", json!({ "subagent_id": subagent_id.trim() })))
    } else if let Some(subagent_id) = content.strip_prefix("/agent-stop ") {
        Some((
            "interrupt_agent",
            json!({ "subagent_id": subagent_id.trim() }),
        ))
    } else {
        None
    }
}

fn parse_integration_command(content: &str) -> Result<Option<RuleCommand>, HarnessError> {
    let command = if content == "/skills" {
        Some(("list_skills", json!({})))
    } else if content == "/jobs" {
        Some(("job_list", json!({})))
    } else if let Some(job_id) = content.strip_prefix("/job-output ") {
        Some(("job_output", json!({ "job_id": job_id.trim() })))
    } else if let Some(job_id) = content.strip_prefix("/job-kill ") {
        Some(("job_kill", json!({ "job_id": job_id.trim() })))
    } else if let Some(command) = content.strip_prefix("/job ") {
        Some(("job_start", json!({ "command": command })))
    } else if let Some(url) = content.strip_prefix("/fetch ") {
        Some(("web_fetch", json!({ "url": url.trim() })))
    } else if let Some(query) = content.strip_prefix("/search ") {
        Some(("web_search", json!({ "query": query.trim() })))
    } else if let Some(raw) = content.strip_prefix("/workflow ") {
        let arguments: Value = serde_json::from_str(raw).map_err(|error| {
            HarnessError::invalid(format!("/workflow expects a JSON object: {error}"))
        })?;
        if !arguments.is_object() {
            return Err(HarnessError::invalid(
                "/workflow expects a JSON object with meta and script",
            ));
        }
        Some(("workflow", arguments))
    } else if let Some(code) = content.strip_prefix("/code ") {
        Some((
            "run_code",
            json!({ "code": code, "description": "Run the supplied Rhai program" }),
        ))
    } else if content == "/extensions" {
        Some(("extension_inspect", json!({})))
    } else if let Some(raw) = content.strip_prefix("/extension-enable ") {
        parse_extension_state_command(raw, true)
    } else if let Some(raw) = content.strip_prefix("/extension-disable ") {
        parse_extension_state_command(raw, false)
    } else if let Some(raw) = content.strip_prefix("/extension-mount ") {
        parse_extension_mount_command(raw, true)
    } else if let Some(raw) = content.strip_prefix("/extension-unmount ") {
        parse_extension_mount_command(raw, false)
    } else if let Some(raw) = content.strip_prefix("/extension-revoke ") {
        raw.split_once(' ').map(|(package_id, version)| {
            (
                "extension_revoke",
                json!({
                    "package_id": package_id,
                    "version": version.trim(),
                    "reason": "Requested by the user through the local extension command"
                }),
            )
        })
    } else {
        None
    };
    Ok(command)
}

fn parse_extension_state_command(raw: &str, enabled: bool) -> Option<RuleCommand> {
    raw.split_once(' ').map(|(package_id, version)| {
        (
            "extension_set_enabled",
            json!({
                "package_id": package_id,
                "version": version.trim(),
                "enabled": enabled,
                "reason": "Requested by the user through the local extension command"
            }),
        )
    })
}

fn parse_extension_mount_command(raw: &str, mounted: bool) -> Option<RuleCommand> {
    let mut parts = raw.splitn(3, ' ');
    let package_id = parts.next()?;
    let version = parts.next()?.trim();
    if package_id.is_empty() || version.is_empty() {
        return None;
    }
    let settings = if mounted {
        serde_json::from_str(parts.next()?.trim()).ok()?
    } else {
        json!({})
    };
    Some((
        "extension_set_mounted",
        json!({
            "package_id": package_id,
            "version": version,
            "mounted": mounted,
            "settings": settings,
            "reason": "Requested by the user through the local extension command"
        }),
    ))
}

fn rule_visible_content(message: &ternilo_protocol::ModelMessage) -> String {
    let mut content = message.content.clone();
    for attachment in &message.attachments {
        if attachment.media_type.starts_with("image/") {
            write!(
                content,
                "\n\n[image attachment: {} ({})]",
                attachment.name, attachment.media_type
            )
            .expect("writing to String cannot fail");
        } else {
            write!(
                content,
                "\n\n<attachment name={:?} media_type={:?}>\n{}\n</attachment>",
                attachment.name, attachment.media_type, attachment.content
            )
            .expect("writing to String cannot fail");
        }
    }
    content
}

fn task_items(value: &str) -> Vec<serde_json::Value> {
    value
        .split(';')
        .map(str::trim)
        .filter(|step| !step.is_empty())
        .enumerate()
        .map(|(index, step)| {
            json!({
                "step": step,
                "status": if index == 0 { "in_progress" } else { "pending" }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_parser_resolves_tools_and_rejects_malformed_known_commands() {
        assert_eq!(
            parse_rule_command("/read src/lib.rs").unwrap(),
            Some(("read_file", json!({ "path": "src/lib.rs" })))
        );
        assert_eq!(
            parse_rule_command("/compact").unwrap(),
            Some(("compact_context", json!({ "deterministic": true })))
        );
        let error = parse_rule_command("/read").unwrap_err();
        assert!(error.message.contains("usage: /read <path>"));
        assert_eq!(parse_rule_command("/not-a-command").unwrap(), None);
        assert_eq!(
            parse_rule_command(
                "/extension-mount dev.example 1.0.0 {\"endpoint\":\"https://example.test\"}"
            )
            .unwrap(),
            Some((
                "extension_set_mounted",
                json!({
                    "package_id": "dev.example",
                    "version": "1.0.0",
                    "mounted": true,
                    "settings": { "endpoint": "https://example.test" },
                    "reason": "Requested by the user through the local extension command"
                }),
            )),
        );
    }

    #[test]
    fn direct_goal_actions_preserve_the_objective_and_select_the_real_status() {
        for (input, objective, status) in [
            ("/goal ship Web", "ship Web", "active"),
            (
                "/goal edit ship the whole Web",
                "ship the whole Web",
                "active",
            ),
            ("/goal resume ship Web", "ship Web", "active"),
            ("/goal complete ship Web", "ship Web", "complete"),
            (
                "/goal blocked waiting for credentials",
                "waiting for credentials",
                "blocked",
            ),
        ] {
            assert_eq!(
                parse_rule_command(input).unwrap(),
                Some((
                    "update_goal",
                    json!({ "objective": objective, "status": status }),
                )),
                "{input}",
            );
        }
        let error = parse_rule_command("/goal complete").unwrap_err();
        assert!(
            error
                .message
                .contains("usage: /goal [edit|resume|complete|blocked] <objective>")
        );
    }

    #[test]
    fn typed_commands_are_not_owned_by_the_direct_parser() {
        for command in ["/feedback text", "/plan", "/skill release-check task"] {
            assert_eq!(rule_command_name(command), None);
            assert_eq!(parse_rule_command(command).unwrap(), None);
        }
    }

    #[test]
    fn builtin_command_registrations_preserve_catalog_metadata() {
        let commands = builtin_command_registrations();
        let commands = commands
            .iter()
            .filter(|command| matches!(command.descriptor.name.as_str(), "goal" | "read"))
            .collect::<Vec<_>>();
        assert_eq!(
            commands
                .iter()
                .map(|command| command.descriptor.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "goal"]
        );
        assert_eq!(
            commands[1].descriptor.input.as_ref().unwrap().hint,
            "[edit|resume|complete|blocked] <objective>"
        );
        assert!(!commands[1].descriptor.input.as_ref().unwrap().images);
        assert_eq!(commands[0].tool_name, "read_file");
    }
}
