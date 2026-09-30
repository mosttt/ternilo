use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use ternilo_kernel::{
    Attachments, AttachmentsClient, HarnessPlugin, PluginFactory, PluginManifest, RunEnvironment,
    RunEnvironmentClient, SessionTelemetry, SessionTelemetryClient, Sessions, SessionsProvider,
};
use ternilo_protocol::{
    Attachment, HarnessError, MessageRole, ModelMessage, RunId, SessionCommandOutcome,
    SessionCommandOutcomeKind, SessionEvent, SessionEventKind, ToolCall, ToolOutput,
};

use crate::{factory as make_factory, parse_config, projection};

pub const KIND: &str = "ternilo.session.log";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-session-log@1",
        requires: [RunEnvironment, Attachments, SessionTelemetry],
        provides: [Sessions],
    }
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &[
                "ternilo/run-environment@1",
                "ternilo/attachments@1",
                "ternilo/session-telemetry@1",
            ],
            provides: &["ternilo/sessions@1"],
        },
        |value| {
            let config: SessionLogConfig = parse_config(value)?;
            if config.max_tool_result_chars < 256 {
                return Err(HarnessError::composition(
                    "session max_tool_result_chars must be at least 256",
                ));
            }
            Ok(Arc::new(SessionLogPlugin { config }))
        },
    )
    .with_config_schema::<SessionLogConfig>()
    .with_projection_unit(projection::stats_unit())
    .with_projection_unit(projection::feedback_unit())
}

#[derive(Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionLogConfig {
    #[serde(default = "default_max_tool_result_chars")]
    max_tool_result_chars: usize,
}

const fn default_max_tool_result_chars() -> usize {
    12_000
}

struct SessionLogPlugin {
    config: SessionLogConfig,
}

impl HarnessPlugin for SessionLogPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let environment = context
            .context()
            .service::<RunEnvironment>()
            .expect("session log declares RunEnvironment");
        let attachments = context
            .context()
            .service::<Attachments>()
            .expect("session log declares Attachments");
        let telemetry = context
            .context()
            .service::<SessionTelemetry>()
            .expect("session log declares SessionTelemetry");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let max_tool_result_chars = self.config.max_tool_result_chars;
        Activation::Once(Box::pin(async move {
            let mut events = environment
                .load_events()
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            validate_history(&events)
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let identity = environment.identity().await;
            repair_incomplete_history(&environment, &telemetry, &identity, &mut events)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            let commits = Arc::new(SessionCommits::default());
            let provider: Arc<dyn SessionsProvider> = Arc::new(SessionLog {
                environment,
                attachments,
                telemetry,
                identity,
                state: Arc::new(tokio::sync::Mutex::new(SessionLogState {
                    events,
                    committing: false,
                    failure: None,
                })),
                commits: Arc::clone(&commits),
                max_tool_result_chars,
            });
            scope
                .provide::<Sessions>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!("provide session log: {error}"))
                })?;
            Ok(Some(effect::inverse(move || async move {
                commits
                    .shutdown()
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

#[derive(Default)]
struct RunRecoveryState {
    first_seq: u64,
    turn_started: bool,
    turn_terminal: bool,
    commands: Vec<String>,
    tools: Vec<(String, String)>,
    code_dispatches: Vec<(String, String, String)>,
}

impl RunRecoveryState {
    fn observe(&mut self, kind: &SessionEventKind) {
        match kind {
            SessionEventKind::TurnStarted => self.turn_started = true,
            SessionEventKind::TurnFinished { .. }
            | SessionEventKind::TurnFailed { .. }
            | SessionEventKind::TurnCancelled => self.turn_terminal = true,
            SessionEventKind::CommandStarted { command_id, .. } => {
                self.commands.retain(|pending| pending != command_id);
                self.commands.push(command_id.clone());
            }
            SessionEventKind::CommandFinished { command_id, .. } => {
                self.commands.retain(|pending| pending != command_id);
            }
            SessionEventKind::ToolCallStarted { call } => {
                self.tools.retain(|(call_id, _)| call_id != &call.id);
                self.tools.push((call.id.clone(), call.name.clone()));
            }
            SessionEventKind::ToolCallFinished { call_id, .. } => {
                self.tools.retain(|(pending, _)| pending != call_id);
            }
            SessionEventKind::CodeDispatchStarted {
                parent_call_id,
                call,
            } => {
                self.code_dispatches
                    .retain(|(_, call_id, _)| call_id != &call.id);
                self.code_dispatches.push((
                    parent_call_id.clone(),
                    call.id.clone(),
                    call.name.clone(),
                ));
            }
            SessionEventKind::CodeDispatchFinished { call_id, .. } => {
                self.code_dispatches
                    .retain(|(_, pending, _)| pending != call_id);
            }
            _ => {}
        }
    }

    fn is_unfinished(&self) -> bool {
        !self.turn_terminal
            && (self.turn_started
                || !self.commands.is_empty()
                || !self.tools.is_empty()
                || !self.code_dispatches.is_empty())
    }

    fn repair_events(self, run_id: RunId) -> Vec<(RunId, SessionEventKind)> {
        let mut repairs = Vec::new();
        for (parent_call_id, call_id, name) in self.code_dispatches {
            repairs.push((
                run_id.clone(),
                SessionEventKind::CodeDispatchFinished {
                    parent_call_id,
                    call_id,
                    name,
                    output: interrupted_tool_output(),
                    retained_output: None,
                },
            ));
        }
        for (call_id, name) in self.tools {
            repairs.push((
                run_id.clone(),
                SessionEventKind::ToolCallFinished {
                    call_id,
                    name,
                    output: interrupted_tool_output(),
                    retained_output: None,
                },
            ));
        }
        for command_id in self.commands {
            repairs.push((
                run_id.clone(),
                SessionEventKind::CommandFinished {
                    command_id,
                    outcome: interrupted_command_outcome(),
                },
            ));
        }
        if self.turn_started {
            repairs.push((
                run_id,
                SessionEventKind::TurnFailed {
                    message: "run interrupted by host restart before a terminal event was committed; inspect any started tool side effects before retrying"
                        .to_owned(),
                },
            ));
        }
        repairs
    }
}

#[must_use]
pub fn recovery_events(events: &[SessionEvent]) -> Vec<(RunId, SessionEventKind)> {
    let mut runs = BTreeMap::<RunId, RunRecoveryState>::new();
    for event in events {
        let state = runs
            .entry(event.run_id.clone())
            .or_insert_with(|| RunRecoveryState {
                first_seq: event.seq,
                ..RunRecoveryState::default()
            });
        state.observe(&event.kind);
    }

    let mut unfinished = runs
        .into_iter()
        .filter(|(_, state)| state.is_unfinished())
        .collect::<Vec<_>>();
    unfinished.sort_by_key(|(_, state)| state.first_seq);
    unfinished
        .into_iter()
        .flat_map(|(run_id, state)| state.repair_events(run_id))
        .collect()
}

fn interrupted_tool_output() -> ToolOutput {
    ToolOutput {
        content: "tool outcome is unknown because the host restarted after dispatch; do not retry blindly, inspect the workspace first"
            .to_owned(),
        is_error: true,
    }
}

fn interrupted_command_outcome() -> SessionCommandOutcome {
    SessionCommandOutcome {
        kind: SessionCommandOutcomeKind::Error,
        code: "interrupted_by_restart".to_owned(),
        parameters: BTreeMap::from([(
            "message".to_owned(),
            "command interrupted by host restart".to_owned(),
        )]),
    }
}

async fn repair_incomplete_history(
    environment: &RunEnvironmentClient,
    telemetry: &SessionTelemetryClient,
    identity: &ternilo_protocol::SessionIdentity,
    events: &mut Vec<SessionEvent>,
) -> Result<(), HarnessError> {
    for (run_id, kind) in recovery_events(events) {
        let seq = u64::try_from(events.len())
            .map_err(|_| HarnessError::execution("session sequence exceeds u64"))?;
        let event = SessionEvent {
            seq,
            occurred_at_ms: event_timestamp_ms()?,
            run_id,
            kind,
        };
        environment.commit_event(event.clone()).await?;
        telemetry.capture(identity.clone(), event.clone()).await;
        events.push(event);
    }
    Ok(())
}

fn event_timestamp_ms() -> Result<u64, HarnessError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| HarnessError::execution(format!("system clock error: {error}")))?
        .as_millis()
        .try_into()
        .map_err(|_| HarnessError::execution("timestamp exceeds u64"))
}

fn validate_history(events: &[SessionEvent]) -> Result<(), HarnessError> {
    for (expected, event) in events.iter().enumerate() {
        let expected: u64 = expected
            .try_into()
            .map_err(|_| HarnessError::execution("session sequence exceeds u64"))?;
        if event.seq != expected {
            return Err(HarnessError::execution(format!(
                "session log sequence mismatch: expected {expected}, found {}",
                event.seq
            )));
        }
    }
    Ok(())
}

struct SessionLog {
    environment: RunEnvironmentClient,
    attachments: AttachmentsClient,
    telemetry: SessionTelemetryClient,
    identity: ternilo_protocol::SessionIdentity,
    state: Arc<tokio::sync::Mutex<SessionLogState>>,
    commits: Arc<SessionCommits>,
    max_tool_result_chars: usize,
}

struct SessionLogState {
    events: Vec<SessionEvent>,
    committing: bool,
    failure: Option<HarnessError>,
}

#[derive(Default)]
struct SessionCommits {
    state: std::sync::Mutex<CommitTasks>,
}

#[derive(Default)]
struct CommitTasks {
    closing: bool,
    tasks: tokio::task::JoinSet<Result<(), HarnessError>>,
}

impl SessionCommits {
    fn start(
        &self,
        commit: impl Future<Output = Result<SessionEvent, HarnessError>> + Send + 'static,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<SessionEvent, HarnessError>>, HarnessError>
    {
        let mut state = self
            .state
            .lock()
            .expect("session commit owner lock poisoned");
        if state.closing {
            return Err(HarnessError::cancelled("session log is shutting down"));
        }
        while let Some(completed) = state.tasks.try_join_next() {
            completed.map_err(|error| commit_join_error(&error))??;
        }
        let (reply, receipt) = tokio::sync::oneshot::channel();
        state.tasks.spawn(async move {
            let result = commit.await;
            let completion = result.as_ref().map(|_| ()).map_err(Clone::clone);
            // Cancelling the caller discards its receipt, not the owned commit.
            let _ = reply.send(result);
            completion
        });
        Ok(receipt)
    }

    async fn shutdown(&self) -> Result<(), HarnessError> {
        let mut tasks = {
            let mut state = self
                .state
                .lock()
                .expect("session commit owner lock poisoned");
            state.closing = true;
            std::mem::take(&mut state.tasks)
        };
        let mut failures = Vec::new();
        while let Some(completed) = tasks.join_next().await {
            if let Err(error) = completed
                .map_err(|error| commit_join_error(&error))
                .and_then(std::convert::identity)
            {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HarnessError::execution(format!(
                "session event commits failed: {}",
                failures.join("; ")
            )))
        }
    }
}

fn commit_join_error(error: &tokio::task::JoinError) -> HarnessError {
    HarnessError::execution(format!("session event commit task failed: {error}"))
}

impl SessionLog {
    async fn retain_large_output(&self, kind: &mut SessionEventKind) -> Result<(), HarnessError> {
        let (SessionEventKind::ToolCallFinished {
            call_id,
            name,
            output,
            retained_output,
        }
        | SessionEventKind::CodeDispatchFinished {
            call_id,
            name,
            output,
            retained_output,
            ..
        }) = kind
        else {
            return Ok(());
        };
        let characters = output.content.chars().count();
        if characters <= self.max_tool_result_chars {
            return Ok(());
        }
        let attachment = self
            .attachments
            .store(Attachment {
                name: format!("tool-{name}-{call_id}.txt"),
                media_type: "text/plain; charset=utf-8".to_owned(),
                content: output.content.clone(),
            })
            .await?;
        output.content = format!(
            "{}\n\n<tool_result_retained reference={:?} original_chars={characters} />",
            prune_tool_result(&output.content, self.max_tool_result_chars),
            attachment.content
        );
        *retained_output = Some(attachment);
        Ok(())
    }
}

impl SessionLog {
    async fn append_checked(
        &self,
        expected_seq: Option<u64>,
        run_id: RunId,
        mut kind: SessionEventKind,
    ) -> Result<Option<SessionEvent>, HarnessError> {
        self.retain_large_output(&mut kind).await?;
        let occurred_at_ms = event_timestamp_ms()?;
        let mut state = Arc::clone(&self.state).lock_owned().await;
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.committing {
            return Err(HarnessError::execution(
                "a previous session event commit ended without publishing its sequence",
            ));
        }
        let seq = state
            .events
            .len()
            .try_into()
            .map_err(|_| HarnessError::execution("session sequence exceeds u64"))?;
        if expected_seq.is_some_and(|expected| expected != seq) {
            return Ok(None);
        }
        let event = SessionEvent {
            seq,
            occurred_at_ms,
            run_id,
            kind,
        };
        let environment = self.environment.clone();
        let telemetry = self.telemetry.clone();
        let identity = self.identity.clone();
        let receipt = self.commits.start(async move {
            state.committing = true;
            let result: Result<SessionEvent, HarnessError> = async {
                environment.commit_event(event.clone()).await?;
                telemetry.capture(identity, event.clone()).await;
                state.events.push(event.clone());
                Ok(event)
            }
            .await;
            state.committing = false;
            if let Err(error) = &result {
                // A failed append may already have written bytes. Do not reuse its sequence.
                state.failure = Some(error.clone());
            }
            result
        })?;
        receipt
            .await
            .map_err(|error| {
                HarnessError::execution(format!(
                    "session event commit ended without a receipt: {error}"
                ))
            })?
            .map(Some)
    }
}

impl SessionsProvider for SessionLog {
    fn append<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
        kind: SessionEventKind,
    ) -> Pin<Box<dyn Future<Output = Result<SessionEvent, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(self
                .append_checked(None, run_id, kind)
                .await?
                .expect("unconditional append produces an event"))
        })
    }

    fn append_if_next_seq<'a>(
        &'a self,
        _: CallContext<()>,
        next_seq: u64,
        run_id: RunId,
        kind: SessionEventKind,
    ) -> Pin<Box<dyn Future<Output = Result<Option<SessionEvent>, HarnessError>> + Send + 'a>> {
        Box::pin(self.append_checked(Some(next_seq), run_id, kind))
    }

    fn events<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<SessionEvent>> + Send + 'a>> {
        Box::pin(async move { self.state.lock().await.events.clone() })
    }

    fn events_after<'a>(
        &'a self,
        _: CallContext<()>,
        after_seq: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = Vec<SessionEvent>> + Send + 'a>> {
        Box::pin(async move {
            let start = after_seq
                .map_or(0, |seq| seq.saturating_add(1))
                .try_into()
                .unwrap_or(usize::MAX);
            self.state
                .lock()
                .await
                .events
                .get(start..)
                .unwrap_or_default()
                .to_vec()
        })
    }

    fn history<'a>(
        &'a self,
        _: CallContext<()>,
        query: ternilo_protocol::SessionHistoryQuery,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ternilo_protocol::SessionEventPage, HarnessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            query.validate()?;
            let state = self.state.lock().await;
            let end = query.before_seq.map_or(state.events.len(), |before| {
                state.events.partition_point(|event| event.seq < before)
            });
            Ok(ternilo_protocol::SessionEventPage::new(
                state.events[end.saturating_sub(query.limit as usize)..end].to_vec(),
            ))
        })
    }

    fn derive_messages<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Vec<ModelMessage>> + Send + 'a>> {
        Box::pin(async move {
            derive_model_messages(&self.state.lock().await.events, self.max_tool_result_chars)
        })
    }
}

fn command_only_runs(events: &[SessionEvent]) -> BTreeSet<RunId> {
    let goal_runs = events
        .iter()
        .filter_map(|event| {
            matches!(event.kind, SessionEventKind::GoalRoundStarted { .. }).then_some(&event.run_id)
        })
        .collect::<BTreeSet<_>>();
    events
        .iter()
        .filter(|event| matches!(&event.kind, SessionEventKind::CommandStarted { .. }))
        .filter(|event| !goal_runs.contains(&event.run_id))
        .map(|event| event.run_id.clone())
        .collect()
}

pub(crate) fn derive_model_messages(
    events: &[SessionEvent],
    max_tool_result_chars: usize,
) -> Vec<ModelMessage> {
    let active = ternilo_protocol::conversation_events(events);
    let events = active.as_ref();
    let command_runs = command_only_runs(events);
    let latest_compaction = events.iter().rev().find_map(|event| match &event.kind {
        SessionEventKind::ContextCompacted { compaction, .. } => Some(compaction),
        _ => None,
    });
    let mut messages = Vec::new();
    let after_seq = if let Some(compaction) = latest_compaction {
        messages.push(ModelMessage {
            role: MessageRole::User,
            content: format!(
                "<earlier_conversation_summary through_seq={}>\n{}\n</earlier_conversation_summary>",
                compaction.through_seq, compaction.summary
            ),
            reasoning_content: None,
            provider_state: None,
            attachments: Vec::new(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
        Some(compaction.through_seq)
    } else {
        None
    };
    for (index, event) in events.iter().enumerate() {
        if after_seq.is_some_and(|seq| event.seq <= seq) {
            continue;
        }
        if command_runs.contains(&event.run_id)
            && !matches!(event.kind, SessionEventKind::GoalUpdated { .. })
        {
            continue;
        }
        let message = match &event.kind {
            SessionEventKind::UserMessage {
                content,
                provenance: _,
                display_content: _,
                source: _,
                references: _,
                attachments,
            } => (!content.trim().is_empty() || !attachments.is_empty()).then(|| ModelMessage {
                role: MessageRole::User,
                content: content.clone(),
                reasoning_content: None,
                provider_state: None,
                attachments: attachments.clone(),
                tool_call_id: None,
                tool_calls: Vec::new(),
            }),
            SessionEventKind::AssistantMessage { response, .. } => Some(ModelMessage {
                role: MessageRole::Assistant,
                content: response.content.clone(),
                reasoning_content: response.reasoning_content.clone(),
                provider_state: response.provider_state.clone(),
                attachments: Vec::new(),
                tool_call_id: None,
                tool_calls: response.tool_calls.clone(),
            }),
            SessionEventKind::GoalUpdated { .. } | SessionEventKind::GoalRoundStarted { .. } => {
                crate::agent_goal::model_message(&event.kind)
            }
            SessionEventKind::HookContextAdded {
                handler_id,
                dialect,
                content,
                ..
            } => Some(ModelMessage {
                role: MessageRole::User,
                content: format!(
                    "<hook_context handler={handler_id:?} dialect={dialect:?}>\n{content}\n</hook_context>"
                ),
                reasoning_content: None,
                provider_state: None,
                attachments: Vec::new(),
                tool_call_id: None,
                tool_calls: Vec::new(),
            }),
            _ => None,
        };
        if let Some(message) = message {
            messages.push(message);
        }
        if let SessionEventKind::AssistantMessage { response, .. } = &event.kind
            && !response.tool_calls.is_empty()
        {
            messages.extend(model_tool_responses(
                &events[index + 1..],
                &event.run_id,
                &response.tool_calls,
                max_tool_result_chars,
            ));
        }
    }
    messages
}

fn model_tool_responses(
    events: &[SessionEvent],
    run_id: &RunId,
    calls: &[ToolCall],
    max_chars: usize,
) -> Vec<ModelMessage> {
    let mut outputs = BTreeMap::new();
    let mut started = BTreeSet::new();
    let mut terminal = None;
    // A call ID may be reused in a later assistant batch. Keep its results
    // adjacent to this batch, including actual results recorded after failure.
    for event in events.iter().filter(|event| &event.run_id == run_id) {
        match &event.kind {
            SessionEventKind::AssistantMessage { .. } => break,
            SessionEventKind::ToolCallStarted { call } => {
                started.insert(call.id.as_str());
            }
            SessionEventKind::ToolCallFinished {
                call_id, output, ..
            } => {
                outputs.insert(call_id.as_str(), output);
            }
            SessionEventKind::TurnFailed { message } => terminal = Some(message.clone()),
            SessionEventKind::TurnCancelled => terminal = Some("turn cancelled".to_owned()),
            SessionEventKind::TurnFinished { .. } => {
                terminal = Some("turn ended without a recorded tool result".to_owned());
            }
            _ => {}
        }
    }
    calls
        .iter()
        .filter_map(|call| {
            let content = if let Some(output) = outputs.get(call.id.as_str()) {
                prune_tool_result(&output.content, max_chars)
            } else {
                // Live batches are not failures. Compaction and inspection may
                // derive their current history while a tool is still running.
                interrupted_model_tool_output(
                    terminal.as_deref()?,
                    started.contains(call.id.as_str()),
                )
                .content
            };
            Some(tool_message(call.id.clone(), content))
        })
        .collect()
}

pub(crate) fn pending_tool_calls(events: &[SessionEvent], run_id: &RunId) -> Vec<(ToolCall, bool)> {
    let Some((index, response)) = events.iter().enumerate().rev().find_map(|(index, event)| {
        if &event.run_id == run_id
            && let SessionEventKind::AssistantMessage { response, .. } = &event.kind
        {
            return Some((index, response));
        }
        None
    }) else {
        return Vec::new();
    };
    let mut pending = response
        .tool_calls
        .iter()
        .cloned()
        .map(|call| (call, false))
        .collect::<Vec<_>>();
    for event in events[index + 1..]
        .iter()
        .filter(|event| &event.run_id == run_id)
    {
        match &event.kind {
            SessionEventKind::ToolCallStarted { call } => {
                if let Some((_, started)) = pending.iter_mut().find(|(item, _)| item.id == call.id)
                {
                    *started = true;
                }
            }
            SessionEventKind::ToolCallFinished { call_id, .. } => {
                pending.retain(|(call, _)| &call.id != call_id);
            }
            _ => {}
        }
    }
    pending
}

pub(crate) fn interrupted_model_tool_output(reason: &str, started: bool) -> ToolOutput {
    let status = if started {
        "tool_call_interrupted: execution started but its outcome was not recorded; inspect any side effects before retrying"
    } else {
        "tool_call_not_executed: the turn stopped before this requested tool was dispatched"
    };
    ToolOutput {
        content: format!("{status}. Reason: {reason}"),
        is_error: true,
    }
}

pub(crate) fn finish_model_tool_batch(messages: &mut Vec<ModelMessage>, reason: &str) {
    let Some(index) = messages
        .iter()
        .rposition(|message| message.role == MessageRole::Assistant)
    else {
        return;
    };
    let completed = messages[index + 1..]
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<BTreeSet<_>>();
    let pending = messages[index]
        .tool_calls
        .iter()
        .filter(|call| !completed.contains(call.id.as_str()))
        .map(|call| call.id.clone())
        .collect::<Vec<_>>();
    for call_id in pending {
        messages.push(tool_message(
            call_id,
            interrupted_model_tool_output(reason, false).content,
        ));
    }
}

fn tool_message(call_id: String, content: String) -> ModelMessage {
    ModelMessage {
        role: MessageRole::Tool,
        content,
        reasoning_content: None,
        provider_state: None,
        attachments: Vec::new(),
        tool_call_id: Some(call_id),
        tool_calls: Vec::new(),
    }
}

fn prune_tool_result(content: &str, max_chars: usize) -> String {
    let count = content.chars().count();
    if count <= max_chars {
        return content.to_owned();
    }
    let tail_chars = max_chars / 5;
    let head_chars = max_chars.saturating_sub(tail_chars);
    let head = content.chars().take(head_chars).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!(
        "{head}\n\n<tool_result_pruned omitted_chars={} />\n\n{tail}",
        count.saturating_sub(max_chars)
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy, SessionEventStore};
    use ternilo_protocol::{
        AgentId, ModelResponse, RunId, RunLimits, SessionCommandOutcome, SessionCommandOutcomeKind,
        SessionId, SessionIdentity, TenantId, ToolCall, ToolOutput, UserId,
    };

    use super::*;

    #[test]
    fn rejects_non_contiguous_history() {
        let events = vec![SessionEvent {
            seq: 1,
            occurred_at_ms: 0,
            run_id: RunId::new("run"),
            kind: SessionEventKind::TurnStarted,
        }];
        assert!(validate_history(&events).is_err());
    }

    #[test]
    fn recovery_closes_unknown_dispatches_and_the_incomplete_turn_once() {
        let run_id = RunId::new("interrupted-run");
        let events = vec![
            SessionEvent {
                seq: 0,
                occurred_at_ms: 1,
                run_id: run_id.clone(),
                kind: SessionEventKind::TurnStarted,
            },
            SessionEvent {
                seq: 1,
                occurred_at_ms: 2,
                run_id: run_id.clone(),
                kind: SessionEventKind::CommandStarted {
                    command_id: "command-1".to_owned(),
                    command_name: "write".to_owned(),
                },
            },
            SessionEvent {
                seq: 2,
                occurred_at_ms: 3,
                run_id: run_id.clone(),
                kind: SessionEventKind::ToolCallStarted {
                    call: ToolCall {
                        id: "tool-1".to_owned(),
                        name: "write_file".to_owned(),
                        arguments: serde_json::json!({ "path": "result.txt" }),
                        presentation: None,
                    },
                },
            },
            SessionEvent {
                seq: 3,
                occurred_at_ms: 4,
                run_id: run_id.clone(),
                kind: SessionEventKind::CodeDispatchStarted {
                    parent_call_id: "tool-1".to_owned(),
                    call: ToolCall {
                        id: "nested-1".to_owned(),
                        name: "replace_file".to_owned(),
                        arguments: serde_json::json!({ "path": "result.txt" }),
                        presentation: None,
                    },
                },
            },
        ];

        let repairs = recovery_events(&events);
        assert_eq!(repairs.len(), 4);
        assert!(matches!(
            &repairs[0].1,
            SessionEventKind::CodeDispatchFinished {
                parent_call_id,
                call_id,
                output,
                ..
            } if parent_call_id == "tool-1"
                && call_id == "nested-1"
                && output.is_error
                && output.content.contains("do not retry blindly")
        ));
        assert!(matches!(
            &repairs[1].1,
            SessionEventKind::ToolCallFinished {
                call_id, output, ..
            } if call_id == "tool-1" && output.is_error
        ));
        assert!(matches!(
            &repairs[2].1,
            SessionEventKind::CommandFinished {
                command_id,
                outcome: SessionCommandOutcome {
                    kind: SessionCommandOutcomeKind::Error,
                    code,
                    ..
                }
            } if command_id == "command-1" && code == "interrupted_by_restart"
        ));
        assert!(matches!(
            &repairs[3].1,
            SessionEventKind::TurnFailed { message }
                if message.contains("interrupted by host restart")
        ));

        let mut repaired = events;
        for (run_id, kind) in repairs {
            repaired.push(SessionEvent {
                seq: u64::try_from(repaired.len()).expect("test sequence"),
                occurred_at_ms: 10,
                run_id,
                kind,
            });
        }
        assert!(recovery_events(&repaired).is_empty());
    }

    #[test]
    fn recovery_closes_an_interrupted_command_without_inventing_a_turn() {
        let repairs = recovery_events(&[SessionEvent {
            seq: 0,
            occurred_at_ms: 1,
            run_id: RunId::new("command-run"),
            kind: SessionEventKind::CommandStarted {
                command_id: "feedback-command".to_owned(),
                command_name: "feedback".to_owned(),
            },
        }]);
        assert_eq!(repairs.len(), 1);
        assert!(matches!(
            repairs[0].1,
            SessionEventKind::CommandFinished { .. }
        ));
    }

    struct SeededStore {
        events: StdMutex<Vec<SessionEvent>>,
    }

    impl SessionEventStore for SeededStore {
        fn load<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<SessionEvent>, HarnessError>> + Send + 'a>>
        {
            Box::pin(async move { Ok(self.events.lock().expect("seeded store lock").clone()) })
        }

        fn append<'a>(
            &'a self,
            event: SessionEvent,
        ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
            Box::pin(async move {
                self.events.lock().expect("seeded store lock").push(event);
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn boot_persists_recovery_facts_before_exposing_the_session() {
        let run_id = RunId::new("boot-recovery");
        let store = Arc::new(SeededStore {
            events: StdMutex::new(vec![
                SessionEvent {
                    seq: 0,
                    occurred_at_ms: 1,
                    run_id: run_id.clone(),
                    kind: SessionEventKind::TurnStarted,
                },
                SessionEvent {
                    seq: 1,
                    occurred_at_ms: 2,
                    run_id,
                    kind: SessionEventKind::ToolCallStarted {
                        call: ToolCall {
                            id: "write-1".to_owned(),
                            name: "write_file".to_owned(),
                            arguments: serde_json::json!({ "path": "output.txt" }),
                            presentation: None,
                        },
                    },
                },
            ]),
        });
        let environment = HostEnvironment::new(
            SessionIdentity {
                tenant_id: TenantId::new("local"),
                user_id: UserId::new("user"),
                agent_id: AgentId::new("agent"),
                session_id: SessionId::new("session"),
            },
            None,
            HostPolicy::local(RunLimits::default()),
            store.clone(),
        );
        let harness = HarnessSession::boot(
            &crate::catalog().expect("catalog"),
            &crate::local_profile(),
            environment,
        )
        .await
        .expect("boot recovered harness");

        let events = harness.events().await;
        assert_eq!(events.len(), 4);
        assert!(matches!(
            events[2].kind,
            SessionEventKind::ToolCallFinished { .. }
        ));
        assert!(matches!(
            events[3].kind,
            SessionEventKind::TurnFailed { .. }
        ));
        assert_eq!(
            store.events.lock().expect("seeded store lock").len(),
            events.len()
        );
        harness.shutdown().await.expect("shutdown harness");
    }

    #[test]
    fn command_feedback_facts_never_enter_model_history() {
        let run_id = RunId::new("command-feedback-1");
        let events = vec![
            SessionEvent {
                seq: 0,
                occurred_at_ms: 1,
                run_id: run_id.clone(),
                kind: SessionEventKind::CommandStarted {
                    command_id: "feedback-1".to_owned(),
                    command_name: "feedback".to_owned(),
                },
            },
            SessionEvent {
                seq: 1,
                occurred_at_ms: 2,
                run_id: run_id.clone(),
                kind: SessionEventKind::FeedbackSubmitted {
                    command_id: "feedback-1".to_owned(),
                    text: "do not send this to the model".to_owned(),
                },
            },
            SessionEvent {
                seq: 2,
                occurred_at_ms: 3,
                run_id,
                kind: SessionEventKind::CommandFinished {
                    command_id: "feedback-1".to_owned(),
                    outcome: SessionCommandOutcome {
                        kind: SessionCommandOutcomeKind::Success,
                        code: "feedback_recorded".to_owned(),
                        parameters: std::collections::BTreeMap::new(),
                    },
                },
            },
        ];
        assert!(derive_model_messages(&events, 1_000).is_empty());
    }

    #[test]
    fn direct_command_user_and_tool_facts_never_become_orphan_model_messages() {
        let run_id = RunId::new("direct-read");
        let events = vec![
            SessionEvent {
                seq: 0,
                occurred_at_ms: 1,
                run_id: run_id.clone(),
                kind: SessionEventKind::UserMessage {
                    provenance: None,
                    content: "/read README.md".to_owned(),
                    display_content: None,
                    source: None,
                    references: Vec::new(),
                    attachments: Vec::new(),
                },
            },
            SessionEvent {
                seq: 1,
                occurred_at_ms: 2,
                run_id: run_id.clone(),
                kind: SessionEventKind::CommandStarted {
                    command_id: "direct-read".to_owned(),
                    command_name: "read".to_owned(),
                },
            },
            SessionEvent {
                seq: 2,
                occurred_at_ms: 3,
                run_id,
                kind: SessionEventKind::ToolCallFinished {
                    call_id: "direct-read-file".to_owned(),
                    name: "read_file".to_owned(),
                    output: ToolOutput {
                        content: "private command result".to_owned(),
                        is_error: false,
                    },
                    retained_output: None,
                },
            },
        ];
        assert!(derive_model_messages(&events, 1_000).is_empty());
    }

    #[test]
    fn reference_only_turn_uses_resolved_context_without_an_empty_provider_message() {
        let run_id = RunId::new("reference-only");
        let messages = derive_model_messages(
            &[
                SessionEvent {
                    seq: 0,
                    occurred_at_ms: 1,
                    run_id: run_id.clone(),
                    kind: SessionEventKind::HookContextAdded {
                        handler_id: "reference:file".to_owned(),
                        dialect: "ternilo.reference.file.v1".to_owned(),
                        content: "<referenced-file path=\"README.md\">contents</referenced-file>"
                            .to_owned(),
                        reference: None,
                        completeness: None,
                    },
                },
                SessionEvent {
                    seq: 1,
                    occurred_at_ms: 2,
                    run_id,
                    kind: SessionEventKind::UserMessage {
                        provenance: None,
                        content: String::new(),
                        display_content: None,
                        source: None,
                        references: vec![ternilo_protocol::SubmissionReference::File {
                            path: "README.md".to_owned(),
                            file_kind: ternilo_protocol::ReferenceFileKind::File,
                        }],
                        attachments: Vec::new(),
                    },
                },
            ],
            1_000,
        );
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.contains("reference:file"));
        assert!(messages[0].content.contains("README.md"));
    }

    #[test]
    fn assistant_reasoning_is_preserved_in_tool_round_history() {
        let run_id = RunId::new("run");
        let events = vec![
            SessionEvent {
                seq: 1,
                occurred_at_ms: 1,
                run_id: run_id.clone(),
                kind: SessionEventKind::AssistantMessage {
                    step: 1,
                    response: ModelResponse {
                        provider: "test-provider".into(),
                        model: "test-model".into(),
                        content: String::new(),
                        reasoning_content: Some("I should inspect the file.".into()),
                        provider_state: None,
                        tool_calls: vec![ToolCall {
                            id: "call-1".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({ "path": "README.md" }),
                            presentation: None,
                        }],
                        usage: None,
                        finish_reason: ternilo_protocol::ModelFinishReason::ToolCalls,
                        provider_request_id: None,
                        attempts: 1,
                        request_digest: None,
                        replayed: false,
                    },
                },
            },
            SessionEvent {
                seq: 2,
                occurred_at_ms: 2,
                run_id,
                kind: SessionEventKind::ToolCallFinished {
                    call_id: "call-1".into(),
                    name: "read".into(),
                    output: ToolOutput {
                        content: "contents".into(),
                        is_error: false,
                    },
                    retained_output: None,
                },
            },
        ];

        let messages = derive_model_messages(&events, 1_000);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("I should inspect the file.")
        );
        assert_eq!(messages[0].tool_calls[0].id, "call-1");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-1"));
    }
}
