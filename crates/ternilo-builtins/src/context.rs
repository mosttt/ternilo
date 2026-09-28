use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{
    Activation, CallContext, CleanupError, ComponentContext, ComponentDescriptor, effect,
};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    Contexts, ContextsProvider, DiscardModelOutput, HarnessPlugin, Models, ModelsClient,
    PluginFactory, PluginManifest, RunCancellation, Sessions, SessionsClient, ToolExecutionContext,
    ToolHandler, ToolRegistration, Tools,
};
use ternilo_protocol::{
    ContextCompaction, HarnessError, MessageRole, ModelMessage, ModelRequest, RunId,
    SessionEventKind, ToolOutput, ToolSpec,
};

use crate::{factory as make_factory, parse_config, session::derive_model_messages};

pub const KIND: &str = "ternilo.context.compaction";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-context-compaction@1",
        requires: [Sessions, Models, Tools],
        provides: [Contexts],
    }
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextConfig {
    /// Percentage of the resolved model context window that triggers automatic compaction.
    #[serde(default = "default_threshold_percent")]
    automatic_threshold_percent: u8,
    #[serde(default = "default_keep_recent_turns")]
    keep_recent_turns: usize,
    #[serde(default = "default_tool_result_chars")]
    max_tool_result_chars: usize,
}

const fn default_threshold_percent() -> u8 {
    80
}

const fn default_keep_recent_turns() -> usize {
    4
}

const fn default_tool_result_chars() -> usize {
    12_000
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/sessions@1", "ternilo/models@3", "ternilo/tools@1"],
            provides: &["ternilo/contexts@1"],
        },
        |value| {
            let config: ContextConfig = parse_config(value)?;
            if !(1..=100).contains(&config.automatic_threshold_percent)
                || config.keep_recent_turns == 0
                || config.max_tool_result_chars < 256
            {
                return Err(HarnessError::composition(
                    "context compaction requires automatic_threshold_percent between 1 and 100, keep_recent_turns > 0, and max_tool_result_chars >= 256",
                ));
            }
            Ok(Arc::new(ContextPlugin { config }))
        },
    )
    .with_config_schema::<ContextConfig>()
    .with_projection_unit(crate::projection::latest_unit(
        "context_compaction",
        1,
        compaction_value,
    ))
}

fn compaction_value(kind: &SessionEventKind) -> Option<Value> {
    match kind {
        SessionEventKind::ContextCompacted { compaction, .. } => {
            serde_json::to_value(compaction).ok()
        }
        _ => None,
    }
}

struct ContextPlugin {
    config: ContextConfig,
}

impl HarnessPlugin for ContextPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let sessions = context
            .context()
            .service::<Sessions>()
            .expect("context compaction declares Sessions");
        let models = context
            .context()
            .service::<Models>()
            .expect("context compaction declares Models");
        let tools = context
            .context()
            .service::<Tools>()
            .expect("context compaction declares Tools");
        let route = context.context().clone();
        let scope = context.scope().clone();
        let manager = Arc::new(ContextManager {
            sessions,
            models,
            config: self.config.clone(),
            gate: tokio::sync::Mutex::new(()),
        });
        Activation::Once(Box::pin(async move {
            let provider: Arc<dyn ContextsProvider> = manager.clone();
            scope
                .provide::<Contexts>(&route, provider)
                .await
                .map_err(|error| {
                    linorun_core::ActivationFailure::user(format!(
                        "provide context manager: {error}"
                    ))
                })?;
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "compact_context".to_owned(),
                        description: "Summarize older completed turns into one durable context checkpoint while preserving recent turns.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": {
                                "deterministic": {
                                    "type": "boolean",
                                    "default": false,
                                    "description": "Use an extractive local summary without calling a model"
                                }
                            },
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler: manager,
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

struct ContextManager {
    sessions: SessionsClient,
    models: ModelsClient,
    config: ContextConfig,
    gate: tokio::sync::Mutex<()>,
}

impl ContextManager {
    #[expect(
        clippy::too_many_lines,
        reason = "compaction validation, model handoff, and durable commit share one ordered flow"
    )]
    async fn compact_inner(
        &self,
        run_id: RunId,
        automatic: bool,
        deterministic: bool,
        source_command_id: Option<String>,
    ) -> Result<Option<ContextCompaction>, HarnessError> {
        let _gate = self.gate.lock().await;
        let events = self.sessions.events().await;
        let events = ternilo_protocol::conversation_events(&events);
        let current_start = events
            .iter()
            .position(|event| event.run_id == run_id)
            .ok_or_else(|| HarnessError::execution("current run is absent from the session log"))?;
        let Some(history_end) = current_start.checked_sub(1) else {
            return if automatic {
                Ok(None)
            } else {
                Err(HarnessError::invalid(
                    "there are no earlier turns to compact",
                ))
            };
        };
        let latest_through =
            events[..=history_end]
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
                    SessionEventKind::ContextCompacted { compaction, .. } => {
                        Some(compaction.through_seq)
                    }
                    _ => None,
                });
        if latest_through.is_some_and(|seq| seq >= events[history_end].seq) {
            return if automatic {
                Ok(None)
            } else {
                Err(HarnessError::invalid(
                    "no completed turn has been added since the last compaction",
                ))
            };
        }

        let all_messages = derive_model_messages(&events, self.config.max_tool_result_chars);
        let estimated_tokens_before = estimate_tokens(&all_messages);
        if automatic {
            let Some(context_window) = self.models.context_window().await else {
                return Ok(None);
            };
            if estimated_tokens_before
                < automatic_threshold(context_window, self.config.automatic_threshold_percent)
            {
                return Ok(None);
            }
        }

        let turn_starts = events[..=history_end]
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
            .map(|event| event.seq)
            .filter(|seq| latest_through.is_none_or(|through| *seq > through))
            .collect::<Vec<_>>();
        let through_seq = if turn_starts.len() > self.config.keep_recent_turns {
            turn_starts[turn_starts.len() - self.config.keep_recent_turns]
                .checked_sub(1)
                .unwrap_or(events[history_end].seq)
        } else {
            events[history_end].seq
        };
        let through_index = events
            .iter()
            .position(|event| event.seq == through_seq)
            .ok_or_else(|| HarnessError::execution("compaction cutoff is absent"))?;
        let turn = u32::try_from(
            events[..=current_start]
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::TurnStarted))
                .count(),
        )
        .map_err(|_| HarnessError::execution("session turn number exceeds protocol range"))?;
        let compaction_id = format!("compaction-{}-{through_seq}", run_id.as_str());
        self.sessions
            .append(
                run_id.clone(),
                SessionEventKind::ContextCompactionStarted {
                    compaction_id: compaction_id.clone(),
                    automatic,
                    source_command_id,
                    turn,
                },
            )
            .await?;
        let mut messages =
            derive_model_messages(&events[..=through_index], self.config.max_tool_result_chars);
        messages.push(ModelMessage {
            role: MessageRole::User,
            content: "Write a concise factual continuation summary. Preserve user requirements, decisions, current state, file paths, commands, errors, and unfinished work. Do not add commentary.".to_owned(),
            reasoning_content: None,
            provider_state: None,
            attachments: Vec::new(),
            tool_call_id: None,
            tool_calls: Vec::new(),
        });
        let summary = if deterministic {
            deterministic_summary(&messages, self.config.max_tool_result_chars)
        } else {
            self.summarize(run_id.clone(), messages).await?
        };
        let compaction = ContextCompaction {
            through_seq,
            summary,
            estimated_tokens_before,
            automatic,
        };
        self.sessions
            .append(
                run_id,
                SessionEventKind::ContextCompacted {
                    compaction_id,
                    compaction: compaction.clone(),
                },
            )
            .await?;
        Ok(Some(compaction))
    }

    async fn summarize(
        &self,
        run_id: RunId,
        messages: Vec<ModelMessage>,
    ) -> Result<String, HarnessError> {
        let response = self
            .models
            .complete(
                ModelRequest {
                    run_id,
                    system_prompt:
                        "You compact an agent conversation without losing operational facts."
                            .to_owned(),
                    messages,
                    tools: Vec::new(),
                    step: 0,
                },
                Arc::new(DiscardModelOutput),
                RunCancellation::new(),
            )
            .await?;
        if response.content.trim().is_empty() {
            Err(HarnessError::execution(
                "context compaction model returned an empty summary",
            ))
        } else {
            Ok(response.content)
        }
    }
}

impl ContextsProvider for ContextManager {
    fn prepare<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ContextCompaction>, HarnessError>> + Send + 'a>>
    {
        Box::pin(async move { self.compact_inner(run_id, true, false, None).await })
    }

    fn compact<'a>(
        &'a self,
        _: CallContext<()>,
        run_id: RunId,
    ) -> Pin<Box<dyn Future<Output = Result<ContextCompaction, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.compact_inner(run_id, false, false, None)
                .await?
                .ok_or_else(|| HarnessError::execution("manual compaction produced no checkpoint"))
        })
    }
}

impl ToolHandler for ContextManager {
    fn execute<'a>(
        &'a self,
        context: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: CompactArguments =
                serde_json::from_value(arguments).map_err(|error| {
                    HarnessError::invalid(format!("invalid compact_context arguments: {error}"))
                })?;
            let source_command_id = context
                .call_id
                .starts_with("direct-compact_context-")
                .then(|| format!("direct-{}", context.run_id.as_str()));
            let compaction = self
                .compact_inner(
                    context.run_id,
                    false,
                    arguments.deterministic,
                    source_command_id,
                )
                .await?
                .ok_or_else(|| {
                    HarnessError::execution("manual compaction produced no checkpoint")
                })?;
            Ok(ToolOutput {
                content: serde_json::to_string(&compaction).map_err(|error| {
                    HarnessError::execution(format!("serialize compaction result: {error}"))
                })?,
                is_error: false,
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactArguments {
    #[serde(default)]
    deterministic: bool,
}

fn deterministic_summary(messages: &[ModelMessage], max_chars: usize) -> String {
    if messages.is_empty() {
        return "Earlier completed turns contained only direct commands; their durable tool results remain in the session log.".to_owned();
    }
    let mut summary = String::new();
    for message in messages {
        let role = match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let remaining = max_chars.saturating_sub(summary.chars().count());
        if remaining == 0 {
            break;
        }
        let content = message.content.chars().take(remaining).collect::<String>();
        summary.push_str(role);
        summary.push_str(": ");
        summary.push_str(&content);
        summary.push('\n');
    }
    summary
}

fn estimate_tokens(messages: &[ModelMessage]) -> u64 {
    let chars = messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum::<usize>();
    u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX)
}

fn automatic_threshold(context_window: u64, percent: u8) -> u64 {
    let threshold = u128::from(context_window)
        .saturating_mul(u128::from(percent))
        .div_ceil(100);
    u64::try_from(threshold).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::automatic_threshold;

    #[test]
    fn automatic_threshold_scales_with_the_resolved_model_window() {
        assert_eq!(automatic_threshold(1_000, 80), 800);
        assert_eq!(automatic_threshold(128_000, 80), 102_400);
        assert_eq!(automatic_threshold(1_000_000, 80), 800_000);
        assert_eq!(automatic_threshold(101, 80), 81);
    }
}
