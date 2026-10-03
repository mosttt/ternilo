use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use ternilo_kernel::{
    ActivityBranch, Agents, AgentsProvider, CommandResolution, Commands, CommandsClient, Contexts,
    ContextsClient, ExecutionActivityOutput, HarnessPlugin, Hooks, HooksClient, ModelOutput,
    Models, ModelsClient, PluginFactory, PluginManifest, Prompts, PromptsClient, ResolvedCommand,
    RunCancellation, RunEnvironment, RunEnvironmentClient, Sessions, SessionsClient, Tools,
    ToolsClient,
};
use ternilo_protocol::{
    AgentInput, ExecutionActivityPhase, HarnessError, HookDecision, HookPoint, HookRequest,
    HookResult, ModelFinishReason, ModelRequest, ModelRetryFailure, RunId, RunOutcome,
    SessionCommandOutcome, SessionCommandOutcomeKind, SessionEventKind, SteeringInput, ToolCall,
    ToolOutput, TurnFinishReason, UserQuestion, UserQuestionOption,
};

use crate::{effective_count_limit, factory as make_factory, parse_config};

pub const KIND: &str = "ternilo.agent.react";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-react-agent@1",
        requires: [RunEnvironment, Sessions, Prompts, Tools, Commands, Models, Contexts, Hooks],
        provides: [Agents],
    }
}

#[derive(Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentConfig {
    /// Maximum Agent steps requested per turn; 0 means no plugin limit. The host may impose a lower ceiling.
    #[serde(default = "default_max_steps")]
    max_steps: u32,
    /// Maximum tool calls requested per turn; 0 means no plugin limit. The host may impose a lower ceiling.
    #[serde(default = "default_max_tool_calls")]
    max_tool_calls: u32,
    #[serde(default = "default_max_goal_rounds", rename = "max_goal_rounds")]
    goal_round_limit: u32,
}

const fn default_max_goal_rounds() -> u32 {
    256
}

const fn default_max_tool_calls() -> u32 {
    512
}

const fn default_max_steps() -> u32 {
    0
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[
                "ternilo/run-environment@1",
                "ternilo/sessions@1",
                "ternilo/prompts@1",
                "ternilo/tools@1",
                "ternilo/commands@1",
                "ternilo/models@3",
                "ternilo/contexts@1",
                "ternilo/hooks@1",
            ],
            provides: &["ternilo/agents@3"],
        },
        |value| {
            let config: AgentConfig = parse_config(value)?;
            Ok(Arc::new(ReactAgentPlugin { config }))
        },
    )
    .with_config_schema::<AgentConfig>()
}

struct ReactAgentPlugin {
    config: AgentConfig,
}

impl HarnessPlugin for ReactAgentPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let provider: Arc<dyn AgentsProvider> = Arc::new(ReactAgent {
            max_steps: self.config.max_steps,
            max_tool_calls: self.config.max_tool_calls,
            max_goal_rounds: self.config.goal_round_limit,
            environment: context
                .context()
                .service::<RunEnvironment>()
                .expect("agent declares RunEnvironment"),
            sessions: context
                .context()
                .service::<Sessions>()
                .expect("agent declares Sessions"),
            prompts: context
                .context()
                .service::<Prompts>()
                .expect("agent declares Prompts"),
            tools: context
                .context()
                .service::<Tools>()
                .expect("agent declares Tools"),
            commands: context
                .context()
                .service::<Commands>()
                .expect("agent declares Commands"),
            models: context
                .context()
                .service::<Models>()
                .expect("agent declares Models"),
            contexts: context
                .context()
                .service::<Contexts>()
                .expect("agent declares Contexts"),
            hooks: context
                .context()
                .service::<Hooks>()
                .expect("agent declares Hooks"),
            driver: tokio::sync::Mutex::new(()),
            active: tokio::sync::Mutex::new(None),
            session_started: AtomicBool::new(false),
        });
        let route = context.context().clone();
        let scope = context.scope().clone();
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Agents>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide agent loop: {error}"))
                })?;
            Ok(None)
        }))
    }
}

struct ReactAgent {
    max_steps: u32,
    max_tool_calls: u32,
    max_goal_rounds: u32,
    environment: RunEnvironmentClient,
    sessions: SessionsClient,
    prompts: PromptsClient,
    tools: ToolsClient,
    commands: CommandsClient,
    models: ModelsClient,
    contexts: ContextsClient,
    hooks: HooksClient,
    driver: tokio::sync::Mutex<()>,
    active: tokio::sync::Mutex<Option<ActiveRun>>,
    session_started: AtomicBool,
}

struct ActiveRun {
    run_id: RunId,
    cancellation: RunCancellation,
    accepting_steering: bool,
    steering: VecDeque<SteeringInput>,
}

enum DirectCommandOutcome {
    Finished(RunOutcome),
    Continue { steps: u32, tool_calls: u32 },
}

struct SessionActivityOutput {
    sessions: SessionsClient,
    run_id: RunId,
}

impl ExecutionActivityOutput for SessionActivityOutput {
    fn changed<'a>(
        &'a self,
        phase: ExecutionActivityPhase,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ExecutionActivityChanged { phase },
                )
                .await
                .map(|_| ())
        })
    }
}

struct SessionModelOutput {
    run_id: RunId,
    step: u32,
    sessions: SessionsClient,
    cancellation: RunCancellation,
}

impl ModelOutput for SessionModelOutput {
    fn emit<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if delta.is_empty() {
                return Ok(());
            }
            self.cancellation.check()?;
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::AssistantMessageDelta {
                        step: self.step,
                        delta,
                    },
                )
                .await
                .map(|_| ())
        })
    }

    fn emit_reasoning<'a>(
        &'a self,
        delta: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if delta.is_empty() {
                return Ok(());
            }
            self.cancellation.check()?;
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::AssistantReasoningDelta {
                        step: self.step,
                        delta,
                    },
                )
                .await
                .map(|_| ())
        })
    }

    fn retry_scheduled<'a>(
        &'a self,
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ModelRetryScheduled {
                        retry_id: self.retry_id(retry),
                        retry,
                        max_retries,
                        delay_ms,
                        failure,
                    },
                )
                .await
                .map(|_| ())
        })
    }

    fn retry_started<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ModelRetryStarted {
                        retry_id: self.retry_id(retry),
                        retry,
                    },
                )
                .await
                .map(|_| ())
        })
    }

    fn retry_cancelled<'a>(
        &'a self,
        retry: u32,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.sessions
                .append(
                    self.run_id.clone(),
                    SessionEventKind::ModelRetryCancelled {
                        retry_id: self.retry_id(retry),
                        retry,
                    },
                )
                .await
                .map(|_| ())
        })
    }
}

impl SessionModelOutput {
    fn retry_id(&self, retry: u32) -> String {
        format!("{}:{}:{retry}", self.run_id.as_str(), self.step)
    }
}

impl ReactAgent {
    async fn append(&self, run_id: &RunId, kind: SessionEventKind) -> Result<(), HarnessError> {
        self.sessions.append(run_id.clone(), kind).await.map(|_| ())
    }

    async fn fail(&self, run_id: &RunId, error: HarnessError) -> HarnessError {
        for (call, started) in
            crate::session::pending_tool_calls(&self.sessions.events().await, run_id)
        {
            let _ = self
                .append(
                    run_id,
                    SessionEventKind::ToolCallFinished {
                        call_id: call.id,
                        name: call.name,
                        output: crate::session::interrupted_model_tool_output(
                            &error.to_string(),
                            started,
                        ),
                        retained_output: None,
                    },
                )
                .await;
        }
        let kind = if error.is_cancelled() {
            SessionEventKind::TurnCancelled
        } else {
            SessionEventKind::TurnFailed {
                message: error.to_string(),
            }
        };
        let _ = self.append(run_id, kind).await;
        error
    }

    async fn append_steering(
        &self,
        run_id: &RunId,
        input: SteeringInput,
        cancellation: &RunCancellation,
    ) -> Result<(), HarnessError> {
        let reference_contexts = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(cancelled_error(run_id)),
            result = self.environment.resolve_input_references(
                input.references.clone(), input.reference_contexts,
            ) => result,
        }?;
        let hook_results = self
            .dispatch_hooks(HookRequest {
                point: HookPoint::UserPromptSubmit,
                run_id: run_id.clone(),
                prompt: Some(input.input.clone()),
                tool_call: None,
                tool_output: None,
                answer: None,
            })
            .await;
        self.enforce_hooks(run_id, HookPoint::UserPromptSubmit, &hook_results)
            .await?;
        for context in reference_contexts {
            let handler_id = context.handler_id().to_owned();
            let dialect = context.dialect().to_owned();
            self.append(
                run_id,
                SessionEventKind::HookContextAdded {
                    handler_id,
                    dialect,
                    content: context.content,
                    reference: Some(context.reference),
                    completeness: context.completeness,
                },
            )
            .await?;
        }
        self.append(
            run_id,
            SessionEventKind::UserMessage {
                provenance: input.provenance,
                content: input.input,
                display_content: input.display_input,
                source: Some(input.source),
                references: input.references,
                attachments: input.attachments,
            },
        )
        .await
    }

    async fn take_steering(&self, run_id: &RunId, close_if_empty: bool) -> Vec<SteeringInput> {
        let mut active = self.active.lock().await;
        let Some(active) = active.as_mut().filter(|active| &active.run_id == run_id) else {
            return Vec::new();
        };
        let pending = active.steering.drain(..).collect::<Vec<_>>();
        if close_if_empty && pending.is_empty() {
            active.accepting_steering = false;
        }
        pending
    }

    async fn append_pending_steering(
        &self,
        run_id: &RunId,
        close_if_empty: bool,
        cancellation: &RunCancellation,
    ) -> Result<bool, HarnessError> {
        let pending = self.take_steering(run_id, close_if_empty).await;
        if pending.is_empty() {
            return Ok(false);
        }
        for input in pending {
            self.append_steering(run_id, input, cancellation).await?;
        }
        Ok(true)
    }

    async fn dispatch_hooks(&self, request: HookRequest) -> Vec<HookResult> {
        let run_id = request.run_id.clone();
        let results = self.hooks.run(request).await;
        for result in &results {
            let _ = self
                .append(
                    &run_id,
                    SessionEventKind::HookResult {
                        result: result.clone(),
                    },
                )
                .await;
            if let Some(content) = result
                .additional_context
                .as_deref()
                .filter(|content| !content.trim().is_empty())
            {
                let _ = self
                    .append(
                        &run_id,
                        SessionEventKind::HookContextAdded {
                            handler_id: result.handler_id.clone(),
                            dialect: result.dialect.clone(),
                            content: content.to_owned(),
                            reference: None,
                            completeness: None,
                        },
                    )
                    .await;
            }
        }
        results
    }

    async fn enforce_hooks(
        &self,
        run_id: &RunId,
        point: HookPoint,
        results: &[HookResult],
    ) -> Result<(), HarnessError> {
        let denied = results
            .iter()
            .filter(|result| result.stop || result.decision == HookDecision::Deny)
            .collect::<Vec<_>>();
        if !denied.is_empty() {
            let reason = joined_hook_reasons(&denied)
                .unwrap_or_else(|| format!("{} hook blocked the operation", point.wire_name()));
            self.append(
                run_id,
                SessionEventKind::HookContextAdded {
                    handler_id: "hook-policy".to_owned(),
                    dialect: "native".to_owned(),
                    content: reason.clone(),
                    reference: None,
                    completeness: None,
                },
            )
            .await?;
            return Err(HarnessError::policy(reason));
        }
        let asks = results
            .iter()
            .filter(|result| result.decision == HookDecision::Ask)
            .collect::<Vec<_>>();
        if asks.is_empty() {
            return Ok(());
        }
        let reason = joined_hook_reasons(&asks)
            .unwrap_or_else(|| format!("{} hook requests confirmation", point.wire_name()));
        let answer = self
            .environment
            .ask_user(UserQuestion {
                id: format!("hook-{}-{run_id}", point.wire_name()),
                question: reason,
                detail: None,
                header: None,
                options: vec![
                    UserQuestionOption {
                        label: "Allow once".to_owned(),
                        description: None,
                    },
                    UserQuestionOption {
                        label: "Deny".to_owned(),
                        description: None,
                    },
                ],
                multi_select: false,
                presentation: None,
                tool_approval: None,
            })
            .await?;
        if answer.chose("Allow once") {
            Ok(())
        } else {
            Err(HarnessError::policy(format!(
                "user denied the {} hook request",
                point.wire_name()
            )))
        }
    }

    async fn direct_command_outcome(
        &self,
        run_id: &RunId,
        command_id: &str,
        tool_name: Option<&str>,
        output: ToolOutput,
        tool_calls: u32,
        continue_model: bool,
    ) -> Result<DirectCommandOutcome, HarnessError> {
        let continue_model = continue_model && !output.is_error;
        if !continue_model {
            let stop_results = self
                .dispatch_hooks(HookRequest {
                    point: HookPoint::Stop,
                    run_id: run_id.clone(),
                    prompt: None,
                    tool_call: None,
                    tool_output: None,
                    answer: Some(output.content.clone()),
                })
                .await;
            if let Err(error) = self
                .enforce_hooks(run_id, HookPoint::Stop, &stop_results)
                .await
            {
                return Err(self.fail_direct_command(run_id, command_id, error).await);
            }
        }
        let mut parameters = BTreeMap::new();
        if let Some(tool_name) = tool_name {
            parameters.insert("tool".to_owned(), tool_name.to_owned());
        }
        if output.is_error {
            parameters.insert("message".to_owned(), output.content.clone());
        }
        self.append(
            run_id,
            SessionEventKind::CommandFinished {
                command_id: command_id.to_owned(),
                outcome: SessionCommandOutcome {
                    kind: if output.is_error {
                        SessionCommandOutcomeKind::Error
                    } else {
                        SessionCommandOutcomeKind::Success
                    },
                    code: if output.is_error {
                        "direct_command_failed"
                    } else if continue_model {
                        "goal_execution_started"
                    } else {
                        "direct_command_completed"
                    }
                    .to_owned(),
                    parameters,
                },
            },
        )
        .await?;
        if continue_model {
            return Ok(DirectCommandOutcome::Continue {
                steps: tool_calls,
                tool_calls,
            });
        }
        self.append(
            run_id,
            SessionEventKind::TurnFinished {
                answer: output.content.clone(),
                finish_reason: TurnFinishReason::Completed,
            },
        )
        .await?;
        let events = self
            .sessions
            .events()
            .await
            .into_iter()
            .filter(|event| event.run_id == *run_id)
            .collect();
        Ok(DirectCommandOutcome::Finished(RunOutcome {
            answer: output.content,
            steps: tool_calls,
            tool_calls,
            events,
            generated_title: None,
        }))
    }

    async fn fail_direct_command(
        &self,
        run_id: &RunId,
        command_id: &str,
        error: HarnessError,
    ) -> HarnessError {
        let mut parameters = BTreeMap::new();
        parameters.insert("message".to_owned(), error.to_string());
        let _ = self
            .append(
                run_id,
                SessionEventKind::CommandFinished {
                    command_id: command_id.to_owned(),
                    outcome: SessionCommandOutcome {
                        kind: SessionCommandOutcomeKind::Error,
                        code: if error.is_cancelled() {
                            "direct_command_cancelled"
                        } else {
                            "direct_command_failed"
                        }
                        .to_owned(),
                        parameters,
                    },
                },
            )
            .await;
        self.fail(run_id, error).await
    }

    async fn record_interrupted_tool(
        &self,
        run_id: &RunId,
        call: &ternilo_protocol::ToolCall,
        error: &HarnessError,
    ) -> Result<(), HarnessError> {
        self.append(
            run_id,
            SessionEventKind::ToolCallFinished {
                call_id: call.id.clone(),
                name: call.name.clone(),
                output: ToolOutput {
                    content: if error.is_cancelled() {
                        "tool_call_cancelled".to_owned()
                    } else {
                        error.to_string()
                    },
                    is_error: true,
                },
                retained_output: None,
            },
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn run_direct_command(
        &self,
        run_id: &RunId,
        resolution: CommandResolution,
        has_attachments: bool,
        cancellation: RunCancellation,
        activity: ActivityBranch,
        continue_model: bool,
    ) -> Result<DirectCommandOutcome, HarnessError> {
        let CommandResolution {
            command_name,
            result,
        } = resolution;
        let command_id = format!("direct-{}", run_id.as_str());
        self.append(
            run_id,
            SessionEventKind::CommandStarted {
                command_id: command_id.clone(),
                command_name: command_name.clone(),
            },
        )
        .await?;
        if let Err(error) = cancellation.check() {
            return Err(self.fail_direct_command(run_id, &command_id, error).await);
        }
        if has_attachments {
            return self
                .direct_command_outcome(
                    run_id,
                    &command_id,
                    None,
                    ToolOutput {
                        content: format!(
                            "/{command_name} does not accept attachments; remove them and run the command again"
                        ),
                        is_error: true,
                    },
                    0,
                    false,
                )
                .await;
        }
        let ResolvedCommand {
            tool_name,
            arguments,
        } = match result {
            Ok(command) => command,
            Err(error) => {
                return self
                    .direct_command_outcome(
                        run_id,
                        &command_id,
                        None,
                        ToolOutput {
                            content: error.to_string(),
                            is_error: true,
                        },
                        0,
                        false,
                    )
                    .await;
            }
        };
        self.append(run_id, SessionEventKind::StepStarted { step: 1 })
            .await?;
        let presentation = match self.tools.present().await {
            Ok(presentation) => presentation,
            Err(error) => {
                self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                    .await?;
                return Err(self.fail_direct_command(run_id, &command_id, error).await);
            }
        };
        let mut call = ToolCall {
            id: format!("direct-{tool_name}-{}", run_id.as_str()),
            name: tool_name.clone(),
            arguments,
            presentation: None,
        };
        call.presentation = self.tools.describe(call.name.clone()).await;
        self.append(
            run_id,
            SessionEventKind::ToolCallStarted { call: call.clone() },
        )
        .await?;
        let visible = presentation.tools.iter().any(|tool| tool.name == call.name);
        if !visible {
            let content = if presentation.code_only {
                format!(
                    "/{command_name} cannot call `{tool_name}` in the current code-only tool presentation"
                )
            } else {
                format!(
                    "/{command_name} requires the `{tool_name}` tool, but it is not enabled for this session"
                )
            };
            let output = ToolOutput {
                content,
                is_error: true,
            };
            self.append(
                run_id,
                SessionEventKind::ToolCallFinished {
                    call_id: call.id,
                    name: call.name,
                    output: output.clone(),
                    retained_output: None,
                },
            )
            .await?;
            self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                .await?;
            return self
                .direct_command_outcome(run_id, &command_id, Some(&tool_name), output, 1, false)
                .await;
        }

        let pre_results = self
            .dispatch_hooks(HookRequest {
                point: HookPoint::PreToolUse,
                run_id: run_id.clone(),
                prompt: None,
                tool_call: Some(call.clone()),
                tool_output: None,
                answer: None,
            })
            .await;
        let output = match self
            .enforce_hooks(run_id, HookPoint::PreToolUse, &pre_results)
            .await
        {
            Ok(()) => {
                let delegation = activity.delegate();
                let result = {
                    let execution = self.tools.execute(
                        run_id.clone(),
                        call.clone(),
                        cancellation.clone(),
                        delegation.branch(),
                    );
                    tokio::pin!(execution);
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Err(cancelled_error(run_id)),
                        result = &mut execution => result,
                    }
                };
                if let Err(error) = delegation.finish().await {
                    self.record_interrupted_tool(run_id, &call, &error).await?;
                    self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                        .await?;
                    return Err(self.fail_direct_command(run_id, &command_id, error).await);
                }
                match result {
                    Ok(output) => output,
                    Err(error) if error.is_cancelled() => {
                        self.append(
                            run_id,
                            SessionEventKind::ToolCallFinished {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                output: ToolOutput {
                                    content: "tool_call_cancelled".to_owned(),
                                    is_error: true,
                                },
                                retained_output: None,
                            },
                        )
                        .await?;
                        self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                            .await?;
                        return Err(self.fail_direct_command(run_id, &command_id, error).await);
                    }
                    Err(error) => ToolOutput {
                        content: error.to_string(),
                        is_error: true,
                    },
                }
            }
            Err(error) if pre_results.iter().any(|result| result.stop) => {
                let output = ToolOutput {
                    content: error.to_string(),
                    is_error: true,
                };
                self.append(
                    run_id,
                    SessionEventKind::ToolCallFinished {
                        call_id: call.id,
                        name: call.name,
                        output,
                        retained_output: None,
                    },
                )
                .await?;
                self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                    .await?;
                return Err(self.fail_direct_command(run_id, &command_id, error).await);
            }
            Err(error) => ToolOutput {
                content: error.to_string(),
                is_error: true,
            },
        };
        self.append(
            run_id,
            SessionEventKind::ToolCallFinished {
                call_id: call.id.clone(),
                name: call.name.clone(),
                output: output.clone(),
                retained_output: None,
            },
        )
        .await?;
        if !output.is_error {
            let post_results = self
                .dispatch_hooks(HookRequest {
                    point: HookPoint::PostToolUse,
                    run_id: run_id.clone(),
                    prompt: None,
                    tool_call: Some(call),
                    tool_output: Some(output.clone()),
                    answer: None,
                })
                .await;
            if let Err(error) = self
                .enforce_hooks(run_id, HookPoint::PostToolUse, &post_results)
                .await
                && post_results.iter().any(|result| result.stop)
            {
                self.append(run_id, SessionEventKind::StepFinished { step: 1 })
                    .await?;
                return Err(self.fail_direct_command(run_id, &command_id, error).await);
            }
        }
        self.append(run_id, SessionEventKind::StepFinished { step: 1 })
            .await?;
        self.direct_command_outcome(
            run_id,
            &command_id,
            Some(&tool_name),
            output,
            1,
            continue_model,
        )
        .await
    }

    #[allow(clippy::too_many_lines)]
    async fn run_active(
        &self,
        input: AgentInput,
        cancellation: RunCancellation,
        direct_command: Option<CommandResolution>,
        activity: ActivityBranch,
    ) -> Result<RunOutcome, HarnessError> {
        activity.ensure_running().await?;
        let run_id = input.run_id;
        let prompt = input.input;
        let display_prompt = input.display_input;
        let source = input.source;
        let goal_execution =
            crate::agent_goal::starts_goal_execution(&prompt, direct_command.as_ref());
        if let Some(ternilo_protocol::UserMessageSource::Submission {
            regenerate_from: Some(target),
            ..
        }) = &source
        {
            ternilo_protocol::validate_regeneration(&self.sessions.events().await, *target)?;
        }
        let provenance = input.provenance;
        let references = input.references;
        let reference_contexts = input.reference_contexts;
        let attachments = input.attachments;
        let additional_inputs = input.additional_inputs;
        let direct_has_attachments = !attachments.is_empty();
        if self
            .sessions
            .events()
            .await
            .iter()
            .any(|event| event.run_id == run_id)
        {
            return Err(HarnessError::invalid(format!(
                "run id {run_id:?} already exists in this session"
            )));
        }
        self.append(&run_id, SessionEventKind::TurnStarted).await?;
        self.append(
            &run_id,
            SessionEventKind::UserMessage {
                provenance,
                content: prompt.clone(),
                display_content: display_prompt,
                source,
                references: references.clone(),
                attachments,
            },
        )
        .await?;
        let admission = async {
            cancellation.check()?;
            self.environment.check_run_authorization(run_id.clone()).await?;
            let available = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(HarnessError::cancelled("run was cancelled")),
                lease = self.environment.try_acquire_workspace() => lease?,
            };
            if let Some(lease) = available {
                return Ok(lease);
            }
            self.append(&run_id, SessionEventKind::WorkspaceExecutionWaiting)
                .await?;
            let lease = self
                .environment
                .acquire_workspace(cancellation.clone())
                .await?;
            self.append(&run_id, SessionEventKind::WorkspaceExecutionAcquired)
                .await?;
            Ok(lease)
        }
        .await;
        let _workspace_lease = match admission {
            Ok(lease) => lease,
            Err(error) => return Err(self.fail(&run_id, error).await),
        };
        if let Err(error) = self.tools.prepare(cancellation.clone()).await {
            return Err(self.fail(&run_id, error).await);
        }
        let reference_contexts = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(HarnessError::cancelled("run was cancelled")),
            result = self.environment.resolve_input_references(references, reference_contexts) => result,
        };
        let reference_contexts = match reference_contexts {
            Ok(contexts) => contexts,
            Err(error) => return Err(self.fail(&run_id, error).await),
        };
        if !self.session_started.swap(true, Ordering::AcqRel) {
            let results = self
                .dispatch_hooks(HookRequest {
                    point: HookPoint::SessionStart,
                    run_id: run_id.clone(),
                    prompt: None,
                    tool_call: None,
                    tool_output: None,
                    answer: None,
                })
                .await;
            if let Err(error) = self
                .enforce_hooks(&run_id, HookPoint::SessionStart, &results)
                .await
            {
                return Err(self.fail(&run_id, error).await);
            }
        }
        let results = self
            .dispatch_hooks(HookRequest {
                point: HookPoint::UserPromptSubmit,
                run_id: run_id.clone(),
                prompt: Some(prompt.clone()),
                tool_call: None,
                tool_output: None,
                answer: None,
            })
            .await;
        if let Err(error) = self
            .enforce_hooks(&run_id, HookPoint::UserPromptSubmit, &results)
            .await
        {
            return Err(self.fail(&run_id, error).await);
        }
        for context in reference_contexts {
            let handler_id = context.handler_id().to_owned();
            let dialect = context.dialect().to_owned();
            self.append(
                &run_id,
                SessionEventKind::HookContextAdded {
                    handler_id,
                    dialect,
                    content: context.content,
                    reference: Some(context.reference),
                    completeness: context.completeness,
                },
            )
            .await?;
        }
        let mut initial_steps = 0;
        let mut initial_tool_calls = 0;
        if direct_command.is_some() {
            if let Err(error) = self.tools.prepare(cancellation.clone()).await {
                return Err(self.fail(&run_id, error).await);
            }
            // Hooks and newly prepared sources may change the target schema.
            let Some(resolution) = self.commands.resolve(prompt.clone()).await else {
                return Err(self
                    .fail(
                        &run_id,
                        HarnessError::invalid("direct command is no longer registered"),
                    )
                    .await);
            };
            match self
                .run_direct_command(
                    &run_id,
                    resolution,
                    direct_has_attachments,
                    cancellation.clone(),
                    activity.clone(),
                    goal_execution,
                )
                .await?
            {
                DirectCommandOutcome::Finished(outcome) => return Ok(outcome),
                DirectCommandOutcome::Continue { steps, tool_calls } => {
                    initial_steps = steps;
                    initial_tool_calls = tool_calls;
                }
            }
        }
        let mut goal_round = 1;
        if goal_execution {
            let (objective, _) = crate::agent_goal::current_goal(&self.sessions.events().await)
                .ok_or_else(|| HarnessError::execution("goal command did not save a goal"))?;
            self.append(
                &run_id,
                SessionEventKind::GoalRoundStarted {
                    objective,
                    round: goal_round,
                    max_rounds: self.max_goal_rounds,
                },
            )
            .await?;
        }
        for input in additional_inputs {
            if let Err(error) = self.append_steering(&run_id, input, &cancellation).await {
                return Err(self.fail(&run_id, error).await);
            }
        }

        let limits = self.environment.limits().await;
        let max_steps = effective_count_limit(self.max_steps, limits.max_steps);
        let max_tool_calls = effective_count_limit(self.max_tool_calls, limits.max_tool_calls);
        let mut tool_calls = initial_tool_calls;
        let mut step = next_step(initial_steps);
        loop {
            if exceeds_step_limit(max_steps, step) {
                let error = HarnessError::policy(format!("turn exceeded max_steps ({max_steps})"));
                return Err(self.fail(&run_id, error).await);
            }
            if let Err(error) = cancellation.check() {
                return Err(self.fail(&run_id, error).await);
            }
            if let Err(error) = self
                .append_pending_steering(&run_id, false, &cancellation)
                .await
            {
                return Err(self.fail(&run_id, error).await);
            }
            self.append(&run_id, SessionEventKind::StepStarted { step })
                .await?;
            let context = self.contexts.prepare(run_id.clone());
            tokio::pin!(context);
            let context_result = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(cancelled_error(&run_id)),
                result = &mut context => result,
            };
            if let Err(error) = context_result {
                return Err(self.fail(&run_id, error).await);
            }
            if let Err(error) = self.tools.prepare(cancellation.clone()).await {
                return Err(self.fail(&run_id, error).await);
            }
            let presentation = match self.tools.present().await {
                Ok(presentation) => presentation,
                Err(error) => return Err(self.fail(&run_id, error).await),
            };
            let visible_tools = presentation
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<BTreeSet<_>>();
            let code_only = presentation.code_only;
            let mut system_prompt = self.prompts.assemble().await;
            if let Some(section) = presentation.system_prompt {
                if !system_prompt.is_empty() {
                    system_prompt.push_str("\n\n");
                }
                system_prompt.push_str(&section);
            }
            self.append(
                &run_id,
                SessionEventKind::ModelRequestStarted {
                    step,
                    system_prompt: system_prompt.clone(),
                },
            )
            .await?;
            let request = ModelRequest {
                run_id: run_id.clone(),
                system_prompt,
                messages: self.sessions.derive_messages().await,
                tools: presentation.tools,
                step,
            };
            let output: Arc<dyn ModelOutput> = Arc::new(SessionModelOutput {
                run_id: run_id.clone(),
                step,
                sessions: self.sessions.clone(),
                cancellation: cancellation.clone(),
            });
            let response = match self
                .models
                .complete(request, output, cancellation.clone())
                .await
            {
                Ok(response) => response,
                Err(error) => return Err(self.fail(&run_id, error).await),
            };
            if let Err(error) = cancellation.check() {
                return Err(self.fail(&run_id, error).await);
            }
            self.append(
                &run_id,
                SessionEventKind::AssistantMessage {
                    step,
                    response: response.clone(),
                },
            )
            .await?;

            if response.finish_reason == ModelFinishReason::Pause {
                step = next_step(step);
                continue;
            }

            if response.tool_calls.is_empty() {
                if response.content.is_empty() {
                    let error =
                        HarnessError::execution("model returned neither content nor tool calls");
                    return Err(self.fail(&run_id, error).await);
                }
                let results = self
                    .dispatch_hooks(HookRequest {
                        point: HookPoint::Stop,
                        run_id: run_id.clone(),
                        prompt: None,
                        tool_call: None,
                        tool_output: None,
                        answer: Some(response.content.clone()),
                    })
                    .await;
                if let Err(error) = self.enforce_hooks(&run_id, HookPoint::Stop, &results).await {
                    if results.iter().any(|result| result.stop) {
                        return Err(self.fail(&run_id, error).await);
                    }
                    self.append(&run_id, SessionEventKind::StepFinished { step })
                        .await?;
                    step = next_step(step);
                    continue;
                }
                self.append(&run_id, SessionEventKind::StepFinished { step })
                    .await?;
                let goal = if goal_execution {
                    crate::agent_goal::current_goal(&self.sessions.events().await)
                } else {
                    None
                };
                let continue_goal = goal
                    .as_ref()
                    .is_some_and(|(_, status)| *status == ternilo_protocol::GoalStatus::Active)
                    && response.finish_reason != ModelFinishReason::MaxTokens;
                match self
                    .append_pending_steering(&run_id, !continue_goal, &cancellation)
                    .await
                {
                    Ok(true) => {
                        step = next_step(step);
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => return Err(self.fail(&run_id, error).await),
                }
                if let Some((objective, ternilo_protocol::GoalStatus::Active)) = goal {
                    if continue_goal {
                        if self.max_goal_rounds != 0 && goal_round >= self.max_goal_rounds {
                            return Err(self.fail(&run_id, HarnessError::policy(format!(
                                "goal reached its configured limit of {} rounds; review progress before resuming",
                                self.max_goal_rounds,
                            ))).await);
                        }
                        goal_round = goal_round.saturating_add(1);
                        self.append(
                            &run_id,
                            SessionEventKind::GoalRoundStarted {
                                objective,
                                round: goal_round,
                                max_rounds: self.max_goal_rounds,
                            },
                        )
                        .await?;
                        step = next_step(step);
                        continue;
                    }
                    self.append(
                        &run_id,
                        SessionEventKind::GoalUpdated {
                            objective,
                            status: ternilo_protocol::GoalStatus::Blocked,
                        },
                    )
                    .await?;
                }
                self.append(
                    &run_id,
                    SessionEventKind::TurnFinished {
                        answer: response.content.clone(),
                        finish_reason: if response.finish_reason == ModelFinishReason::MaxTokens {
                            TurnFinishReason::MaxTokens
                        } else {
                            TurnFinishReason::Completed
                        },
                    },
                )
                .await?;
                let events = self
                    .sessions
                    .events()
                    .await
                    .into_iter()
                    .filter(|event| event.run_id == run_id)
                    .collect();
                return Ok(RunOutcome {
                    answer: response.content,
                    steps: step,
                    tool_calls,
                    events,
                    generated_title: None,
                });
            }

            for mut call in response.tool_calls {
                if max_tool_calls != 0 && tool_calls >= max_tool_calls {
                    let error = HarnessError::policy(format!(
                        "turn exceeded max_tool_calls ({max_tool_calls})",
                    ));
                    return Err(self.fail(&run_id, error).await);
                }
                tool_calls = tool_calls.saturating_add(1);
                call.presentation = self.tools.describe(call.name.clone()).await;
                self.append(
                    &run_id,
                    SessionEventKind::ToolCallStarted { call: call.clone() },
                )
                .await?;
                if !visible_tools.contains(&call.name) {
                    let unavailable_message = if code_only {
                        format!(
                            "only `run_code` is callable directly; call {:?} from inside a `run_code` program",
                            call.name
                        )
                    } else {
                        format!(
                            "tool {:?} is not available in this agent presentation",
                            call.name
                        )
                    };
                    self.append(
                        &run_id,
                        SessionEventKind::ToolCallFinished {
                            call_id: call.id,
                            name: call.name,
                            output: ToolOutput {
                                content: unavailable_message,
                                is_error: true,
                            },
                            retained_output: None,
                        },
                    )
                    .await?;
                    continue;
                }
                let pre_results = self
                    .dispatch_hooks(HookRequest {
                        point: HookPoint::PreToolUse,
                        run_id: run_id.clone(),
                        prompt: None,
                        tool_call: Some(call.clone()),
                        tool_output: None,
                        answer: None,
                    })
                    .await;
                let output = match self
                    .enforce_hooks(&run_id, HookPoint::PreToolUse, &pre_results)
                    .await
                {
                    Ok(()) => {
                        let delegation = activity.delegate();
                        let result = {
                            let execution = self.tools.execute(
                                run_id.clone(),
                                call.clone(),
                                cancellation.clone(),
                                delegation.branch(),
                            );
                            tokio::pin!(execution);
                            tokio::select! {
                                biased;
                                () = cancellation.cancelled() => Err(cancelled_error(&run_id)),
                                result = &mut execution => result,
                            }
                        };
                        if let Err(error) = delegation.finish().await {
                            self.record_interrupted_tool(&run_id, &call, &error).await?;
                            return Err(self.fail(&run_id, error).await);
                        }
                        match result {
                            Ok(output) => output,
                            Err(error) if error.is_cancelled() => {
                                // A started call must have one durable terminal
                                // fact even when the turn is cancelled mid-call.
                                // Otherwise projections keep presenting this
                                // tool as running after TurnCancelled.
                                self.append(
                                    &run_id,
                                    SessionEventKind::ToolCallFinished {
                                        call_id: call.id.clone(),
                                        name: call.name.clone(),
                                        output: ToolOutput {
                                            content: "tool_call_cancelled".to_owned(),
                                            is_error: true,
                                        },
                                        retained_output: None,
                                    },
                                )
                                .await?;
                                return Err(self.fail(&run_id, error).await);
                            }
                            Err(error) => ToolOutput {
                                content: error.to_string(),
                                is_error: true,
                            },
                        }
                    }
                    Err(error) if pre_results.iter().any(|result| result.stop) => {
                        return Err(self.fail(&run_id, error).await);
                    }
                    Err(error) => ToolOutput {
                        content: error.to_string(),
                        is_error: true,
                    },
                };
                self.append(
                    &run_id,
                    SessionEventKind::ToolCallFinished {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        output: output.clone(),
                        retained_output: None,
                    },
                )
                .await?;
                if !output.is_error {
                    let post_results = self
                        .dispatch_hooks(HookRequest {
                            point: HookPoint::PostToolUse,
                            run_id: run_id.clone(),
                            prompt: None,
                            tool_call: Some(call),
                            tool_output: Some(output),
                            answer: None,
                        })
                        .await;
                    if let Err(error) = self
                        .enforce_hooks(&run_id, HookPoint::PostToolUse, &post_results)
                        .await
                        && post_results.iter().any(|result| result.stop)
                    {
                        return Err(self.fail(&run_id, error).await);
                    }
                }
            }
            self.append(&run_id, SessionEventKind::StepFinished { step })
                .await?;
            step = next_step(step);
        }
    }
}

impl AgentsProvider for ReactAgent {
    fn run<'a>(
        &'a self,
        _: CallContext<()>,
        input: AgentInput,
    ) -> Pin<Box<dyn Future<Output = Result<RunOutcome, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            input.validate()?;
            let _driver = self.driver.lock().await;
            let run_id = input.run_id.clone();
            let cancellation = RunCancellation::new();
            let direct_command = self.commands.resolve(input.input.clone()).await;
            let goal_execution =
                crate::agent_goal::starts_goal_execution(&input.input, direct_command.as_ref());
            let accepting_steering = direct_command.is_none() || goal_execution;
            let activity = self
                .environment
                .begin_activity(
                    run_id.clone(),
                    cancellation.clone(),
                    Arc::new(SessionActivityOutput {
                        sessions: self.sessions.clone(),
                        run_id: run_id.clone(),
                    }),
                )
                .await?;
            *self.active.lock().await = Some(ActiveRun {
                run_id: run_id.clone(),
                cancellation: cancellation.clone(),
                accepting_steering,
                steering: VecDeque::new(),
            });
            let result = self
                .run_active(input, cancellation, direct_command, activity)
                .await;
            if goal_execution
                && result
                    .as_ref()
                    .is_err_and(|error| error.code != ternilo_protocol::ErrorCode::Cancelled)
            {
                let events = self.sessions.events().await;
                if events.iter().any(|event| {
                    event.run_id == run_id
                        && matches!(event.kind, SessionEventKind::GoalRoundStarted { .. })
                }) && let Some((objective, ternilo_protocol::GoalStatus::Active)) =
                    crate::agent_goal::current_goal(&events)
                {
                    self.append(
                        &run_id,
                        SessionEventKind::GoalUpdated {
                            objective,
                            status: ternilo_protocol::GoalStatus::Blocked,
                        },
                    )
                    .await?;
                }
            }
            let mut active = self.active.lock().await;
            if active
                .as_ref()
                .is_some_and(|active| active.run_id == run_id)
            {
                *active = None;
            }
            result
        })
    }

    fn cancel<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            run_id.validate()?;
            let active = self.active.lock().await;
            if let Some(active) = active.as_ref()
                && active.run_id == run_id
            {
                active.cancellation.cancel();
                return Ok(());
            }
            drop(active);
            if self.sessions.events().await.iter().any(|event| {
                event.run_id == run_id
                    && matches!(
                        event.kind,
                        SessionEventKind::TurnFinished { .. }
                            | SessionEventKind::TurnFailed { .. }
                            | SessionEventKind::TurnCancelled
                    )
            }) {
                return Ok(());
            }
            Err(HarnessError::invalid(format!(
                "run {run_id} is not active in this session"
            )))
        })
    }

    fn active_run<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<RunId>> + Send + 'a>> {
        Box::pin(async move {
            self.active
                .lock()
                .await
                .as_ref()
                .map(|active| active.run_id.clone())
        })
    }

    fn steer<'a>(
        &'a self,
        _: CallContext<()>,
        input: SteeringInput,
    ) -> Pin<Box<dyn Future<Output = Result<bool, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            input.validate()?;
            if self.commands.resolve(input.input.clone()).await.is_some() {
                return Ok(false);
            }
            let mut active = self.active.lock().await;
            let Some(active) = active.as_mut() else {
                return Ok(false);
            };
            if !active.accepting_steering || active.cancellation.is_cancelled() {
                return Ok(false);
            }
            active.steering.push_back(input);
            Ok(true)
        })
    }
}

fn cancelled_error(run_id: &RunId) -> HarnessError {
    HarnessError::cancelled(format!("run {run_id} was cancelled"))
}

fn joined_hook_reasons(results: &[&HookResult]) -> Option<String> {
    let reasons = results
        .iter()
        .filter_map(|result| result.reason.as_deref().or(result.stop_reason.as_deref()))
        .filter(|reason| !reason.trim().is_empty())
        .collect::<Vec<_>>();
    (!reasons.is_empty()).then(|| reasons.join("\n\n"))
}

const fn exceeds_step_limit(max_steps: u32, step: u32) -> bool {
    max_steps != 0 && step > max_steps
}

const fn next_step(step: u32) -> u32 {
    step.saturating_add(1)
}

#[cfg(test)]
mod step_limit_tests {
    use serde_json::json;
    use ternilo_kernel::compose_profiles;
    use ternilo_protocol::{PluginEntry, Profile};

    use super::{AgentConfig, effective_count_limit, exceeds_step_limit, factory, next_step};

    #[test]
    fn agent_schema_exposes_unlimited_steps_and_bounded_tool_calls() {
        let config: AgentConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(config.max_steps, 0);
        assert_eq!(config.max_tool_calls, 512);
        assert_eq!(
            factory().config_schema["properties"]["max_tool_calls"]["default"],
            512
        );
        assert_eq!(
            factory().config_schema["properties"]["max_steps"]["default"],
            0
        );
    }

    #[test]
    fn plugin_request_and_host_ceiling_have_four_zero_combinations() {
        assert_eq!(effective_count_limit(0, 0), 0);
        assert_eq!(effective_count_limit(12, 0), 12);
        assert_eq!(effective_count_limit(0, 8), 8);
        assert_eq!(effective_count_limit(12, 8), 8);
        assert_eq!(effective_count_limit(4, 8), 4);
    }

    #[test]
    fn profile_overlay_can_set_the_agent_step_request() {
        let profile = compose_profiles([
            crate::local_profile(),
            Profile {
                plugins: vec![PluginEntry {
                    id: "agent-loop".to_owned(),
                    kind: super::KIND.to_owned(),
                    enabled: true,
                    config: json!({ "max_steps": 24 }),
                }],
            },
        ]);
        let entry = profile
            .plugins
            .iter()
            .find(|entry| entry.id == "agent-loop")
            .unwrap();
        let config: AgentConfig = serde_json::from_value(entry.config.clone()).unwrap();
        assert_eq!(config.max_steps, 24);
    }

    #[test]
    fn positive_step_limit_stops_after_the_configured_step() {
        assert!(!exceeds_step_limit(8, 8));
        assert!(exceeds_step_limit(8, 9));
    }

    #[test]
    fn continued_model_decisions_advance_before_rechecking_the_limit() {
        let continued = next_step(1);
        assert_eq!(continued, 2);
        assert!(exceeds_step_limit(1, continued));
    }

    #[test]
    fn zero_step_limit_allows_steps_beyond_the_previous_default() {
        assert!(!exceeds_step_limit(0, 9));
        assert!(!exceeds_step_limit(0, u32::MAX));
    }
}
