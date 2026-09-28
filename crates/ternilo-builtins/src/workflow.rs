use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures_util::{StreamExt as _, stream};
use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use rhai::{Array, Dynamic, Engine, EvalAltResult, ImmutableString, Map, Position, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ternilo_kernel::{
    ActivityBranch, HarnessPlugin, PluginFactory, PluginManifest, RunCancellation,
    SessionProjectionUnit, Sessions, SessionsClient, Subagents, SubagentsClient, ToolEffect,
    ToolExecutionContext, ToolHandler, ToolRegistration, Tools, WorkflowRunRequest,
    WorkflowRunResult, Workflows, WorkflowsClient, WorkflowsProvider,
};
use ternilo_protocol::{
    HarnessError, SessionEvent, SessionEventKind, SubagentId, SubagentSnapshot, SubagentStatus,
    ToolOutput, ToolSpec, WorkflowAgentOutcome, WorkflowMeta, WorkflowRunId, WorkflowStopReason,
};

use crate::{factory as make_factory, parse_config};

pub const ENGINE_KIND: &str = "ternilo.workflow.rhai";
pub const TOOL_KIND: &str = "ternilo.tools.workflow";

component_descriptor! {
    static ENGINE_DESCRIPTOR: () {
        id: "ternilo/rhai-workflow-engine@1",
        requires: [Subagents, Sessions],
        provides: [Workflows],
    }
}

component_descriptor! {
    static TOOL_DESCRIPTOR: () {
        id: "ternilo/workflow-tool@1",
        requires: [Tools, Workflows],
        provides: [],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EngineConfig {
    #[serde(default = "default_max_operations")]
    max_operations: u64,
    #[serde(default = "default_max_wall_ms")]
    max_wall_ms: u64,
    #[serde(default = "default_max_result_bytes")]
    max_result_bytes: usize,
    #[serde(default = "default_max_items_per_call")]
    max_items_per_call: usize,
    #[serde(default = "default_max_total_agents")]
    max_total_agents: u32,
    #[serde(default = "default_max_concurrent_agents")]
    max_concurrent_agents: usize,
    #[serde(default = "default_agent_timeout_ms")]
    agent_timeout_ms: u64,
}

const fn default_max_operations() -> u64 {
    1_000_000
}

const fn default_max_wall_ms() -> u64 {
    120_000
}

const fn default_max_result_bytes() -> usize {
    1024 * 1024
}

const fn default_max_items_per_call() -> usize {
    512
}

const fn default_max_total_agents() -> u32 {
    128
}

const fn default_max_concurrent_agents() -> usize {
    8
}

const fn default_agent_timeout_ms() -> u64 {
    300_000
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolConfig {
    #[serde(default = "default_max_result_chars")]
    max_result_chars: usize,
}

const fn default_max_result_chars() -> usize {
    50_000
}

pub fn engine_factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: ENGINE_KIND,
            requires: &["ternilo/subagents@2", "ternilo/sessions@1"],
            provides: &["ternilo/workflows@1"],
        },
        |value| {
            let config: EngineConfig = parse_config(value)?;
            if config.max_operations == 0
                || config.max_wall_ms == 0
                || config.max_result_bytes == 0
                || config.max_items_per_call == 0
                || config.max_total_agents == 0
                || config.max_concurrent_agents == 0
                || config.agent_timeout_ms == 0
            {
                return Err(HarnessError::composition(
                    "workflow engine limits must be positive",
                ));
            }
            Ok(Arc::new(WorkflowEnginePlugin { config }))
        },
    )
    .with_config_schema::<EngineConfig>()
    .with_projection_unit(workflow_projection_unit())
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: TOOL_KIND,
            requires: &["ternilo/tools@1", "ternilo/workflows@1"],
            provides: &[],
        },
        |value| {
            let config: ToolConfig = parse_config(value)?;
            if config.max_result_chars == 0 {
                return Err(HarnessError::composition(
                    "workflow tool max_result_chars must be positive",
                ));
            }
            Ok(Arc::new(WorkflowToolPlugin { config }))
        },
    )
    .with_config_schema::<ToolConfig>()
}

struct WorkflowEnginePlugin {
    config: EngineConfig,
}

impl HarnessPlugin for WorkflowEnginePlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &ENGINE_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let provider: Arc<dyn WorkflowsProvider> = Arc::new(RhaiWorkflowEngine {
            config: self.config.clone(),
            sessions: context
                .context()
                .service::<Sessions>()
                .expect("workflow engine declares Sessions"),
            subagents: context
                .context()
                .service::<Subagents>()
                .expect("workflow engine declares Subagents"),
            next_run: AtomicU64::new(1),
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Workflows>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide workflow engine: {error}"
                    ))
                })?;
            Ok(None)
        }))
    }
}

struct WorkflowToolPlugin {
    config: ToolConfig,
}

impl HarnessPlugin for WorkflowToolPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &TOOL_DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("workflow tool declares Tools");
        let workflows = context
            .context()
            .service::<Workflows>()
            .expect("workflow tool declares Workflows");
        let handler = Arc::new(WorkflowTool {
            workflows,
            max_result_chars: self.config.max_result_chars,
        });
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: workflow_tool_spec(),
                    effect: ToolEffect::Dangerous,
                    handler,
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

fn workflow_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "workflow".to_owned(),
        description: "Run a bounded Rhai orchestration script over subagents. The script receives `args` and the functions `agent(prompt, options?)`, `task(prompt, options?)`, `parallel(tasks)`, `stage(prompt, options?)`, `pipeline(items, stages)`, `phase(title)`, and `log(message)`. `agent` runs one child immediately. `parallel` accepts task descriptors and preserves result order. `pipeline` runs every item through prompt-template stages without a cross-stage barrier; templates may use {{item}}, {{prev}}, and {{index}}. Child failures become null; invalid scripts, infrastructure failures, caps, and cancellation fail the workflow. The runtime exposes no filesystem, network, process, environment, module, timer, or persistent-state API.".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "meta": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short lowercase kebab-case identity" },
                        "description": { "type": "string" },
                        "when_to_use": { "type": "string" },
                        "phases": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string" },
                                    "detail": { "type": "string" }
                                },
                                "required": ["title"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["name", "description"],
                    "additionalProperties": false
                },
                "script": { "type": "string", "description": "Rhai script body; its final expression is returned as JSON" },
                "args": { "type": "object", "default": {} }
            },
            "required": ["meta", "script"],
            "additionalProperties": false
        }),
    }
}

struct WorkflowTool {
    workflows: WorkflowsClient,
    max_result_chars: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowArguments {
    meta: WorkflowMeta,
    script: String,
    #[serde(default = "empty_object")]
    args: Value,
}

fn empty_object() -> Value {
    json!({})
}

impl ToolHandler for WorkflowTool {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: WorkflowArguments =
                serde_json::from_value(arguments).map_err(|error| {
                    HarnessError::invalid(format!("invalid workflow arguments: {error}"))
                })?;
            let result = self
                .workflows
                .run(WorkflowRunRequest {
                    parent_run_id: context.run_id,
                    meta: arguments.meta,
                    script: arguments.script,
                    args: arguments.args,
                    cancellation: context.cancellation,
                    activity: context.activity,
                })
                .await?;
            let is_error = result.stop_reason != WorkflowStopReason::Completed;
            let rendered_result = match result.stop_reason {
                WorkflowStopReason::Completed => {
                    let value = json!({
                        "workflow_id": result.workflow_id,
                        "agents_started": result.agents_started,
                        "result": result.value,
                    });
                    serde_json::to_string_pretty(&value).map_err(|error| {
                        HarnessError::execution(format!("serialize workflow result: {error}"))
                    })?
                }
                WorkflowStopReason::Cancelled => format!(
                    "workflow was cancelled{}",
                    result
                        .error
                        .as_deref()
                        .map(|error| format!(": {error}"))
                        .unwrap_or_default()
                ),
                WorkflowStopReason::Error => format!(
                    "workflow failed: {}",
                    result.error.as_deref().unwrap_or("unknown error")
                ),
            };
            Ok(ToolOutput {
                content: truncate_chars(rendered_result, self.max_result_chars),
                is_error,
            })
        })
    }
}

struct RhaiWorkflowEngine {
    config: EngineConfig,
    sessions: SessionsClient,
    subagents: SubagentsClient,
    next_run: AtomicU64,
}

impl WorkflowsProvider for RhaiWorkflowEngine {
    fn run<'a>(
        &'a self,
        _: CallContext<()>,
        request: WorkflowRunRequest,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowRunResult, HarnessError>> + Send + 'a>> {
        Box::pin(async move { self.run_workflow(request).await })
    }
}

impl RhaiWorkflowEngine {
    async fn run_workflow(
        &self,
        request: WorkflowRunRequest,
    ) -> Result<WorkflowRunResult, HarnessError> {
        request.activity.ensure_running().await?;
        validate_request(&request)?;
        let cancellation = RunCancellation::new();
        let mut cancellation_guard = WorkflowCancellationGuard {
            cancellation: cancellation.clone(),
            armed: true,
        };
        let ordinal = self.next_run.fetch_add(1, Ordering::Relaxed);
        let workflow_id = WorkflowRunId::new(format!("workflow-{}-{ordinal}", now_ms()?));
        self.sessions
            .append(
                request.parent_run_id.clone(),
                SessionEventKind::WorkflowRunStarted {
                    workflow_id: workflow_id.clone(),
                    meta: request.meta.clone(),
                },
            )
            .await?;
        let deadline = Instant::now() + Duration::from_millis(self.config.max_wall_ms);
        let activity = request.activity;
        let worker = activity.delegate();
        let bridge = Arc::new(WorkflowBridge {
            workflow_id: workflow_id.clone(),
            parent_run_id: request.parent_run_id,
            sessions: self.sessions.clone(),
            subagents: self.subagents.clone(),
            cancellation,
            parent_cancellation: request.cancellation,
            activity: worker.branch(),
            deadline,
            max_wall_ms: self.config.max_wall_ms,
            max_items_per_call: self.config.max_items_per_call,
            max_total_agents: self.config.max_total_agents,
            max_concurrent_agents: self.config.max_concurrent_agents,
            agent_timeout_ms: self.config.agent_timeout_ms,
            reserved_agents: AtomicU32::new(0),
            started_agents: AtomicU32::new(0),
            current_phase: Mutex::new(None),
            active: tokio::sync::Mutex::new(BTreeSet::new()),
        });
        let config = self.config.clone();
        let script = request.script;
        let args = request.args;
        let runtime = tokio::runtime::Handle::current();
        let worker_bridge = bridge.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let result = execute_script(&config, script, args, &worker_bridge);
            runtime.block_on(worker_bridge.finish(&result));
            drop(worker_bridge);
            runtime.block_on(worker.finish())?;
            Ok(result)
        })
        .await;
        let result = match joined {
            Ok(result) => result,
            Err(error) => {
                let result = WorkflowRunResult {
                    workflow_id,
                    stop_reason: WorkflowStopReason::Error,
                    agents_started: bridge.started_agents.load(Ordering::Acquire),
                    value: None,
                    error: Some(format!("workflow runtime task failed: {error}")),
                };
                bridge.finish(&result).await;
                Ok(result)
            }
        };
        activity.ensure_running().await?;
        cancellation_guard.armed = false;
        result
    }
}

struct WorkflowCancellationGuard {
    cancellation: RunCancellation,
    armed: bool,
}

impl Drop for WorkflowCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

fn validate_request(request: &WorkflowRunRequest) -> Result<(), HarnessError> {
    let name = request.meta.name.as_str();
    if name.is_empty()
        || name.len() > 80
        || name.split('-').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err(HarnessError::invalid(
            "workflow meta.name must be lowercase kebab-case",
        ));
    }
    if request.meta.description.trim().is_empty() || request.meta.description.len() > 2_000 {
        return Err(HarnessError::invalid(
            "workflow meta.description must contain 1 to 2000 bytes",
        ));
    }
    if request.meta.phases.len() > 64
        || request
            .meta
            .phases
            .iter()
            .any(|phase| phase.title.trim().is_empty() || phase.title.len() > 200)
    {
        return Err(HarnessError::invalid(
            "workflow phases must contain at most 64 non-empty titles",
        ));
    }
    if request.script.trim().is_empty() || request.script.len() > 256 * 1024 {
        return Err(HarnessError::invalid(
            "workflow script must contain 1 to 262144 bytes",
        ));
    }
    if !request.args.is_object() {
        return Err(HarnessError::invalid("workflow args must be a JSON object"));
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTask {
    prompt: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    json: bool,
}

struct PreparedAgentTask {
    sequence: u32,
    prompt: String,
    label: String,
    phase: Option<String>,
    provider: Option<String>,
    json: bool,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentOptions {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    json: bool,
}

impl AgentTask {
    fn from_prompt(prompt: String, options: AgentOptions) -> Self {
        Self {
            prompt,
            label: options.label,
            phase: options.phase,
            provider: options.provider,
            json: options.json,
        }
    }
}

struct WorkflowBridge {
    workflow_id: WorkflowRunId,
    parent_run_id: ternilo_protocol::RunId,
    sessions: SessionsClient,
    subagents: SubagentsClient,
    cancellation: RunCancellation,
    parent_cancellation: RunCancellation,
    activity: ActivityBranch,
    deadline: Instant,
    max_wall_ms: u64,
    max_items_per_call: usize,
    max_total_agents: u32,
    max_concurrent_agents: usize,
    agent_timeout_ms: u64,
    reserved_agents: AtomicU32,
    started_agents: AtomicU32,
    current_phase: Mutex<Option<String>>,
    active: tokio::sync::Mutex<BTreeSet<SubagentId>>,
}

impl WorkflowBridge {
    fn check_running(&self) -> Result<(), HarnessError> {
        self.cancellation.check()?;
        self.parent_cancellation.check()?;
        if Instant::now() >= self.deadline {
            Err(HarnessError::execution(format!(
                "workflow exceeded max_wall_ms ({})",
                self.max_wall_ms
            )))
        } else {
            Ok(())
        }
    }

    async fn append(&self, kind: SessionEventKind) -> Result<(), HarnessError> {
        self.sessions
            .append(self.parent_run_id.clone(), kind)
            .await
            .map(|_| ())
    }

    async fn phase(&self, title: String) -> Result<(), HarnessError> {
        let title = title.trim().to_owned();
        if title.is_empty() || title.len() > 200 {
            return Err(HarnessError::invalid(
                "phase() requires a title of 1 to 200 bytes",
            ));
        }
        *self
            .current_phase
            .lock()
            .map_err(|_| HarnessError::execution("workflow phase lock poisoned"))? =
            Some(title.clone());
        self.append(SessionEventKind::WorkflowPhaseChanged {
            workflow_id: self.workflow_id.clone(),
            title,
        })
        .await
    }

    async fn log(&self, message: String) -> Result<(), HarnessError> {
        if message.is_empty() || message.len() > 8_192 {
            return Err(HarnessError::invalid(
                "log() requires a message of 1 to 8192 bytes",
            ));
        }
        self.append(SessionEventKind::WorkflowLogEmitted {
            workflow_id: self.workflow_id.clone(),
            message,
        })
        .await
    }

    fn reserve_agent(&self) -> Result<u32, HarnessError> {
        self.reserved_agents
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.max_total_agents).then_some(current + 1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| {
                HarnessError::policy(format!(
                    "workflow exceeded max_total_agents ({})",
                    self.max_total_agents
                ))
            })
    }

    fn prepare_agent_task(&self, mut task: AgentTask) -> Result<PreparedAgentTask, HarnessError> {
        self.check_running()?;
        task.prompt = task.prompt.trim().to_owned();
        if task.prompt.is_empty() || task.prompt.len() > 100_000 {
            return Err(HarnessError::invalid(
                "agent() requires a prompt of 1 to 100000 bytes",
            ));
        }
        let sequence = self.reserve_agent()?;
        let phase = task.phase.take().or_else(|| {
            self.current_phase
                .lock()
                .ok()
                .and_then(|phase| phase.clone())
        });
        let label = task
            .label
            .take()
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| short_label(&task.prompt));
        let provider = task
            .provider
            .take()
            .map(|provider| provider.trim().to_owned());
        if provider.as_deref() == Some("") {
            return Err(HarnessError::invalid("agent provider must not be empty"));
        }
        Ok(PreparedAgentTask {
            sequence,
            prompt: task.prompt,
            label,
            phase,
            provider,
            json: task.json,
        })
    }

    async fn spawn_agent_task(
        &self,
        task: &PreparedAgentTask,
        activity: ActivityBranch,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let spawn = async {
            match &task.provider {
                Some(provider) => {
                    self.subagents
                        .spawn_on(
                            provider.clone(),
                            self.parent_run_id.clone(),
                            task.prompt.clone(),
                            Some(task.label.clone()),
                            true,
                            activity.clone(),
                        )
                        .await
                }
                None => {
                    self.subagents
                        .spawn(
                            self.parent_run_id.clone(),
                            task.prompt.clone(),
                            Some(task.label.clone()),
                            true,
                            activity.clone(),
                        )
                        .await
                }
            }
        };
        let snapshot = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                return Err(HarnessError::cancelled("workflow was cancelled"));
            }
            () = self.parent_cancellation.cancelled() => {
                return Err(HarnessError::cancelled("workflow was cancelled"));
            }
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => {
                return Err(HarnessError::execution(format!(
                    "workflow exceeded max_wall_ms ({})",
                    self.max_wall_ms
                )));
            }
            result = spawn => result?,
        };
        Ok(snapshot)
    }

    async fn wait_for_agent(
        &self,
        subagent_id: &SubagentId,
        activity: ActivityBranch,
    ) -> Result<SubagentSnapshot, HarnessError> {
        let result = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                let _ = self.subagents.interrupt(self.parent_run_id.clone(), subagent_id.clone()).await;
                Err(HarnessError::cancelled("workflow was cancelled"))
            }
            () = self.parent_cancellation.cancelled() => {
                let _ = self.subagents.interrupt(self.parent_run_id.clone(), subagent_id.clone()).await;
                Err(HarnessError::cancelled("workflow was cancelled"))
            }
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => {
                let _ = self.subagents.interrupt(self.parent_run_id.clone(), subagent_id.clone()).await;
                Err(HarnessError::execution(format!(
                    "workflow exceeded max_wall_ms ({})",
                    self.max_wall_ms
                )))
            }
            result = self.subagents.wait(subagent_id.clone(), self.agent_timeout_ms, activity.clone()) => result,
        };
        activity.ensure_running().await?;
        result
    }

    async fn run_agent(
        &self,
        task: AgentTask,
        activity: ActivityBranch,
    ) -> Result<Value, HarnessError> {
        activity.ensure_running().await?;
        let task = self.prepare_agent_task(task)?;
        let snapshot = self.spawn_agent_task(&task, activity.clone()).await?;
        let subagent_id = snapshot.subagent_id;
        self.active.lock().await.insert(subagent_id.clone());
        self.started_agents.fetch_add(1, Ordering::AcqRel);
        if let Err(error) = self
            .append(SessionEventKind::WorkflowAgentStarted {
                workflow_id: self.workflow_id.clone(),
                sequence: task.sequence,
                label: task.label,
                phase: task.phase,
                subagent_id: subagent_id.clone(),
            })
            .await
        {
            let _ = self.dispose_child(&subagent_id).await;
            return Err(error);
        }

        let waited = self.wait_for_agent(&subagent_id, activity.clone()).await;
        activity.ensure_running().await?;
        let (outcome, value, fatal) = classify_agent_result(waited, task.json);
        let append = self
            .append(SessionEventKind::WorkflowAgentFinished {
                workflow_id: self.workflow_id.clone(),
                sequence: task.sequence,
                outcome,
            })
            .await;
        let dispose = self.dispose_child(&subagent_id).await;
        append?;
        dispose?;
        if let Some(error) = fatal {
            return Err(error);
        }
        Ok(value)
    }

    async fn dispose_child(&self, subagent_id: &SubagentId) -> Result<(), HarnessError> {
        let result = self
            .subagents
            .dispose(self.parent_run_id.clone(), subagent_id.clone())
            .await
            .map(|_| ());
        self.active.lock().await.remove(subagent_id);
        result
    }

    async fn parallel(
        self: &Arc<Self>,
        tasks: Vec<AgentTask>,
        activity: ActivityBranch,
    ) -> Result<Value, HarnessError> {
        if tasks.len() > self.max_items_per_call {
            return Err(HarnessError::policy(format!(
                "parallel() exceeded max_items_per_call ({})",
                self.max_items_per_call
            )));
        }
        let results = stream::iter(tasks.into_iter().map(|task| {
            let bridge = self.clone();
            let activity = activity.clone();
            async move {
                let task_scope = activity.delegate();
                let result = bridge.run_agent(task, task_scope.branch()).await;
                task_scope.finish().await?;
                result
            }
        }))
        .buffered(self.max_concurrent_agents)
        .collect::<Vec<_>>()
        .await;
        Ok(Value::Array(
            results.into_iter().collect::<Result<Vec<_>, _>>()?,
        ))
    }

    async fn pipeline(
        self: &Arc<Self>,
        items: Vec<Value>,
        stages: Vec<AgentTask>,
        activity: ActivityBranch,
    ) -> Result<Value, HarnessError> {
        if items.len() > self.max_items_per_call || stages.len() > 32 {
            return Err(HarnessError::policy(format!(
                "pipeline() accepts at most {} items and 32 stages",
                self.max_items_per_call
            )));
        }
        if stages.is_empty() {
            return Err(HarnessError::invalid(
                "pipeline() requires at least one stage",
            ));
        }
        let results = stream::iter(items.into_iter().enumerate().map(|(index, item)| {
            let bridge = self.clone();
            let stages = stages.clone();
            let activity = activity.clone();
            async move {
                let item_scope = activity.delegate();
                let item_activity = item_scope.branch();
                let result = async {
                    let mut previous = item.clone();
                    for mut stage in stages {
                        item_activity.ensure_running().await?;
                        stage.prompt = render_template(&stage.prompt, &item, &previous, index);
                        previous = bridge.run_agent(stage, item_activity.clone()).await?;
                        if previous.is_null() {
                            break;
                        }
                    }
                    Ok(previous)
                }
                .await;
                drop(item_activity);
                item_scope.finish().await?;
                result
            }
        }))
        .buffered(self.max_concurrent_agents)
        .collect::<Vec<Result<Value, HarnessError>>>()
        .await;
        Ok(Value::Array(
            results.into_iter().collect::<Result<Vec<_>, _>>()?,
        ))
    }

    async fn finish(&self, result: &WorkflowRunResult) {
        let active = self.active.lock().await.iter().cloned().collect::<Vec<_>>();
        for subagent_id in active {
            let _ = self
                .subagents
                .interrupt(self.parent_run_id.clone(), subagent_id.clone())
                .await;
            let _ = self.dispose_child(&subagent_id).await;
        }
        let _ = self
            .append(SessionEventKind::WorkflowRunFinished {
                workflow_id: self.workflow_id.clone(),
                stop_reason: result.stop_reason,
                agents_started: result.agents_started,
                error: result.error.clone(),
            })
            .await;
    }
}

fn classify_agent_result(
    result: Result<SubagentSnapshot, HarnessError>,
    json_output: bool,
) -> (WorkflowAgentOutcome, Value, Option<HarnessError>) {
    match result {
        Ok(snapshot)
            if matches!(
                snapshot.status,
                SubagentStatus::Idle | SubagentStatus::Completed
            ) =>
        {
            let output = snapshot.output.unwrap_or_default();
            if json_output {
                match serde_json::from_str(&output) {
                    Ok(value) => (WorkflowAgentOutcome::Completed, value, None),
                    Err(_) => (WorkflowAgentOutcome::Failed, Value::Null, None),
                }
            } else {
                (WorkflowAgentOutcome::Completed, Value::String(output), None)
            }
        }
        Ok(snapshot) if snapshot.status == SubagentStatus::Cancelled => {
            (WorkflowAgentOutcome::Cancelled, Value::Null, None)
        }
        Ok(_) => (WorkflowAgentOutcome::Failed, Value::Null, None),
        Err(error) if error.is_cancelled() => {
            (WorkflowAgentOutcome::Cancelled, Value::Null, Some(error))
        }
        Err(error) => (WorkflowAgentOutcome::Failed, Value::Null, Some(error)),
    }
}

fn execute_script(
    config: &EngineConfig,
    script: String,
    args: Value,
    bridge: &Arc<WorkflowBridge>,
) -> WorkflowRunResult {
    let mut engine = Engine::new();
    let deadline = bridge.deadline;
    let progress_cancellation = bridge.cancellation.clone();
    let progress_parent_cancellation = bridge.parent_cancellation.clone();
    engine
        .set_max_operations(config.max_operations)
        .set_max_string_size(config.max_result_bytes)
        .set_max_array_size(config.max_items_per_call.saturating_mul(64))
        .set_max_map_size(config.max_items_per_call.saturating_mul(64))
        .set_max_call_levels(64)
        .set_max_expr_depths(64, 64)
        .disable_symbol("eval")
        .disable_symbol("import")
        .on_progress(move |_| {
            if progress_cancellation.is_cancelled() || progress_parent_cancellation.is_cancelled() {
                Some(Dynamic::from("cancelled"))
            } else if Instant::now() >= deadline {
                Some(Dynamic::from("wall-time"))
            } else {
                None
            }
        });
    register_workflow_functions(&mut engine, bridge.clone());

    let result = (|| -> Result<Option<Value>, HarnessError> {
        bridge.check_running()?;
        let ast = engine.compile(script).map_err(|error| {
            HarnessError::invalid(format!("workflow script does not parse: {error}"))
        })?;
        let mut scope = Scope::new();
        let args = rhai::serde::to_dynamic(args).map_err(|error| {
            HarnessError::invalid(format!(
                "workflow args are not representable in Rhai: {error}"
            ))
        })?;
        scope.push_dynamic("args", args);
        let value = engine
            .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
            .map_err(|error| {
                if bridge.cancellation.is_cancelled() || bridge.parent_cancellation.is_cancelled() {
                    HarnessError::cancelled("workflow was cancelled")
                } else if Instant::now() >= deadline {
                    HarnessError::execution(format!(
                        "workflow exceeded max_wall_ms ({})",
                        config.max_wall_ms
                    ))
                } else {
                    HarnessError::execution(format!("workflow script failed: {error}"))
                }
            })?;
        bridge.check_running()?;
        if value.is_unit() {
            return Ok(None);
        }
        let value = rhai::serde::from_dynamic::<Value>(&value).map_err(|error| {
            HarnessError::invalid(format!("workflow result must be lossless JSON: {error}"))
        })?;
        let encoded = serde_json::to_vec(&value).map_err(|error| {
            HarnessError::execution(format!("serialize workflow result: {error}"))
        })?;
        if encoded.len() > config.max_result_bytes {
            return Err(HarnessError::policy(format!(
                "workflow result exceeded max_result_bytes ({})",
                config.max_result_bytes
            )));
        }
        Ok(Some(value))
    })();

    match result {
        Ok(value) => WorkflowRunResult {
            workflow_id: bridge.workflow_id.clone(),
            stop_reason: WorkflowStopReason::Completed,
            agents_started: bridge.started_agents.load(Ordering::Acquire),
            value,
            error: None,
        },
        Err(error) => WorkflowRunResult {
            workflow_id: bridge.workflow_id.clone(),
            stop_reason: if error.is_cancelled() {
                WorkflowStopReason::Cancelled
            } else {
                WorkflowStopReason::Error
            },
            agents_started: bridge.started_agents.load(Ordering::Acquire),
            value: None,
            error: Some(error.to_string()),
        },
    }
}

fn register_workflow_functions(engine: &mut Engine, bridge: Arc<WorkflowBridge>) {
    engine.register_fn("task", |prompt: ImmutableString| {
        task_map(prompt, Map::new())
    });
    engine.register_fn("task", |prompt: ImmutableString, options: Map| {
        task_map(prompt, options)
    });
    engine.register_fn("stage", |prompt: ImmutableString| {
        task_map(prompt, Map::new())
    });
    engine.register_fn("stage", |prompt: ImmutableString, options: Map| {
        task_map(prompt, options)
    });

    let runtime = tokio::runtime::Handle::current();
    let direct_bridge = bridge.clone();
    let direct_runtime = runtime.clone();
    engine.register_fn(
        "agent",
        move |prompt: ImmutableString| -> Result<Dynamic, Box<EvalAltResult>> {
            let task = AgentTask::from_prompt(prompt.to_string(), AgentOptions::default());
            block_on_value(&direct_runtime, &direct_bridge.activity, |activity| {
                direct_bridge.run_agent(task, activity)
            })
        },
    );
    let options_bridge = bridge.clone();
    let options_runtime = runtime.clone();
    engine.register_fn(
        "agent",
        move |prompt: ImmutableString, options: Map| -> Result<Dynamic, Box<EvalAltResult>> {
            let options = parse_options(options)?;
            let task = AgentTask::from_prompt(prompt.to_string(), options);
            block_on_value(&options_runtime, &options_bridge.activity, |activity| {
                options_bridge.run_agent(task, activity)
            })
        },
    );
    let parallel_bridge = bridge.clone();
    let parallel_runtime = runtime.clone();
    engine.register_fn(
        "parallel",
        move |tasks: Array| -> Result<Dynamic, Box<EvalAltResult>> {
            let tasks = parse_tasks(tasks)?;
            block_on_value(&parallel_runtime, &parallel_bridge.activity, |activity| {
                parallel_bridge.parallel(tasks, activity)
            })
        },
    );
    let pipeline_bridge = bridge.clone();
    let pipeline_runtime = runtime.clone();
    engine.register_fn(
        "pipeline",
        move |items: Array, stages: Array| -> Result<Dynamic, Box<EvalAltResult>> {
            let items = dynamic_array_to_values(items)?;
            let stages = parse_tasks(stages)?;
            block_on_value(&pipeline_runtime, &pipeline_bridge.activity, |activity| {
                pipeline_bridge.pipeline(items, stages, activity)
            })
        },
    );
    let phase_bridge = bridge.clone();
    let phase_runtime = runtime.clone();
    engine.register_fn(
        "phase",
        move |title: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            block_on_unit(&phase_runtime, &phase_bridge.activity, |_activity| {
                phase_bridge.phase(title.to_string())
            })
        },
    );
    let log_runtime = runtime;
    engine.register_fn(
        "log",
        move |message: ImmutableString| -> Result<(), Box<EvalAltResult>> {
            block_on_unit(&log_runtime, &bridge.activity, |_activity| {
                bridge.log(message.to_string())
            })
        },
    );
}

fn task_map(prompt: ImmutableString, mut options: Map) -> Map {
    options.insert("prompt".into(), Dynamic::from(prompt));
    options
}

fn parse_options(options: Map) -> Result<AgentOptions, Box<EvalAltResult>> {
    let value =
        rhai::serde::from_dynamic::<Value>(&Dynamic::from_map(options)).map_err(|error| {
            Box::new(rhai_runtime_error(format!(
                "invalid agent options: {error}"
            )))
        })?;
    serde_json::from_value(value).map_err(|error| {
        Box::new(rhai_runtime_error(format!(
            "invalid agent options: {error}"
        )))
    })
}

fn parse_tasks(tasks: Array) -> Result<Vec<AgentTask>, Box<EvalAltResult>> {
    dynamic_array_to_values(tasks)?
        .into_iter()
        .map(|value| match value {
            Value::String(prompt) => Ok(AgentTask::from_prompt(prompt, AgentOptions::default())),
            value => serde_json::from_value(value).map_err(|error| {
                Box::new(rhai_runtime_error(format!(
                    "invalid task descriptor: {error}"
                )))
            }),
        })
        .collect()
}

fn dynamic_array_to_values(items: Array) -> Result<Vec<Value>, Box<EvalAltResult>> {
    rhai::serde::from_dynamic::<Vec<Value>>(&Dynamic::from_array(items)).map_err(|error| {
        Box::new(rhai_runtime_error(format!(
            "value must be lossless JSON: {error}"
        )))
    })
}

fn block_on_value<F>(
    runtime: &tokio::runtime::Handle,
    activity: &ActivityBranch,
    operation: impl FnOnce(ActivityBranch) -> F,
) -> Result<Dynamic, Box<EvalAltResult>>
where
    F: Future<Output = Result<Value, HarnessError>>,
{
    let value = block_on_activity(runtime, activity, operation)?;
    rhai::serde::to_dynamic(value).map_err(|error| {
        Box::new(rhai_runtime_error(format!(
            "workflow value is not representable: {error}"
        )))
    })
}

fn block_on_unit<F>(
    runtime: &tokio::runtime::Handle,
    activity: &ActivityBranch,
    operation: impl FnOnce(ActivityBranch) -> F,
) -> Result<(), Box<EvalAltResult>>
where
    F: Future<Output = Result<(), HarnessError>>,
{
    block_on_activity(runtime, activity, operation)
}

fn block_on_activity<T, F>(
    runtime: &tokio::runtime::Handle,
    activity: &ActivityBranch,
    operation: impl FnOnce(ActivityBranch) -> F,
) -> Result<T, Box<EvalAltResult>>
where
    F: Future<Output = Result<T, HarnessError>>,
{
    runtime
        .block_on(async {
            activity.ensure_running().await?;
            let callback = activity.delegate();
            let result = operation(callback.branch()).await;
            // Restore admission even when an outer select discarded a child wait.
            // Admission failures must terminate Rhai instead of entering try/catch.
            callback.finish().await?;
            activity.ensure_running().await?;
            Ok::<_, HarnessError>(result)
        })
        .map_err(|error| {
            Box::new(EvalAltResult::ErrorTerminated(
                error.to_string().into(),
                Position::NONE,
            ))
        })?
        .map_err(|error| Box::new(harness_runtime_error(&error)))
}

fn harness_runtime_error(error: &HarnessError) -> EvalAltResult {
    rhai_runtime_error(error.to_string())
}

fn rhai_runtime_error(message: impl Into<String>) -> EvalAltResult {
    EvalAltResult::ErrorRuntime(message.into().into(), Position::NONE)
}

fn render_template(template: &str, item: &Value, previous: &Value, index: usize) -> String {
    template
        .replace("{{item}}", &template_value(item))
        .replace("{{prev}}", &template_value(previous))
        .replace("{{index}}", &index.to_string())
}

fn template_value(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
        str::to_owned,
    )
}

fn short_label(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or(prompt).trim();
    let mut label = first.chars().take(77).collect::<String>();
    if first.chars().count() > 77 {
        label.push_str("...");
    }
    label
}

fn truncate_chars(content: String, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content;
    }
    let retained = content.chars().take(max_chars).collect::<String>();
    let omitted = content.chars().count().saturating_sub(max_chars);
    format!("{retained}\n… [truncated: {omitted} more characters]")
}

fn now_ms() -> Result<u64, HarnessError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock before epoch: {error}")))?
        .as_millis();
    u64::try_from(millis).map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowProjectionState {
    runs: BTreeMap<String, WorkflowProjectionRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowProjectionRun {
    meta: WorkflowMeta,
    status: String,
    phase: Option<String>,
    latest_log: Option<String>,
    active_agents: BTreeMap<u32, String>,
    agents_started: u32,
    error: Option<String>,
}

fn workflow_projection_unit() -> Arc<dyn SessionProjectionUnit> {
    Arc::new(WorkflowProjectionUnit)
}

struct WorkflowProjectionUnit;

impl SessionProjectionUnit for WorkflowProjectionUnit {
    fn key(&self) -> &'static str {
        "workflows"
    }

    fn version(&self) -> u32 {
        1
    }

    fn initial(&self) -> Value {
        serde_json::to_value(WorkflowProjectionState::default())
            .expect("workflow projection is JSON")
    }

    fn valid_state(&self, state: &Value) -> bool {
        serde_json::from_value::<WorkflowProjectionState>(state.clone()).is_ok()
    }

    fn apply(&self, state: &mut Value, event: &SessionEvent) -> Result<(), HarnessError> {
        let mut decoded: WorkflowProjectionState =
            serde_json::from_value(state.clone()).map_err(|error| {
                HarnessError::execution(format!("decode workflow projection: {error}"))
            })?;
        match &event.kind {
            SessionEventKind::WorkflowRunStarted { workflow_id, meta } => {
                decoded.runs.insert(
                    workflow_id.as_str().to_owned(),
                    WorkflowProjectionRun {
                        meta: meta.clone(),
                        status: "running".to_owned(),
                        phase: None,
                        latest_log: None,
                        active_agents: BTreeMap::new(),
                        agents_started: 0,
                        error: None,
                    },
                );
            }
            SessionEventKind::WorkflowPhaseChanged { workflow_id, title } => {
                if let Some(run) = decoded.runs.get_mut(workflow_id.as_str()) {
                    run.phase = Some(title.clone());
                }
            }
            SessionEventKind::WorkflowLogEmitted {
                workflow_id,
                message,
            } => {
                if let Some(run) = decoded.runs.get_mut(workflow_id.as_str()) {
                    run.latest_log = Some(message.clone());
                }
            }
            SessionEventKind::WorkflowAgentStarted {
                workflow_id,
                sequence,
                label,
                ..
            } => {
                if let Some(run) = decoded.runs.get_mut(workflow_id.as_str()) {
                    run.active_agents.insert(*sequence, label.clone());
                    run.agents_started = run.agents_started.saturating_add(1);
                }
            }
            SessionEventKind::WorkflowAgentFinished {
                workflow_id,
                sequence,
                ..
            } => {
                if let Some(run) = decoded.runs.get_mut(workflow_id.as_str()) {
                    run.active_agents.remove(sequence);
                }
            }
            SessionEventKind::WorkflowRunFinished {
                workflow_id,
                stop_reason,
                agents_started,
                error,
            } => {
                if let Some(run) = decoded.runs.get_mut(workflow_id.as_str()) {
                    match stop_reason {
                        WorkflowStopReason::Completed => "completed",
                        WorkflowStopReason::Error => "error",
                        WorkflowStopReason::Cancelled => "cancelled",
                    }
                    .clone_into(&mut run.status);
                    run.active_agents.clear();
                    run.agents_started = *agents_started;
                    run.error.clone_from(error);
                }
            }
            _ => {}
        }
        *state = serde_json::to_value(decoded).map_err(|error| {
            HarnessError::execution(format!("encode workflow projection: {error}"))
        })?;
        Ok(())
    }

    fn view(&self, state: &Value) -> Result<Value, HarnessError> {
        serde_json::from_value::<WorkflowProjectionState>(state.clone())
            .map_err(|error| {
                HarnessError::execution(format!("decode workflow projection: {error}"))
            })
            .and_then(|state| {
                serde_json::to_value(state).map_err(|error| {
                    HarnessError::execution(format!("encode workflow projection view: {error}"))
                })
            })
    }
}
