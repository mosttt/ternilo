use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ternilo_kernel::SessionProjectionUnit;
use ternilo_protocol::{
    FeedbackRating, HarnessError, ModelResponse, SessionEvent, SessionEventKind, SessionStats,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsState {
    stats: SessionStats,
    serialized_event_chars: u64,
    model_started_at: BTreeMap<String, u64>,
    model_first_token_at: BTreeMap<String, u64>,
    tool_started_at: BTreeMap<String, u64>,
}

pub(crate) fn stats_unit() -> Arc<dyn SessionProjectionUnit> {
    Arc::new(StatsUnit)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackEntry {
    revision: u64,
    rating: Option<FeedbackRating>,
    note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
struct FeedbackState(BTreeMap<u64, FeedbackEntry>);

pub(crate) fn feedback_unit() -> Arc<dyn SessionProjectionUnit> {
    Arc::new(FeedbackUnit)
}

struct FeedbackUnit;

impl SessionProjectionUnit for FeedbackUnit {
    fn key(&self) -> &'static str {
        "feedback"
    }

    fn version(&self) -> u32 {
        5
    }

    fn initial(&self) -> Value {
        serde_json::json!({})
    }

    fn valid_state(&self, state: &Value) -> bool {
        serde_json::from_value::<FeedbackState>(state.clone()).is_ok()
    }

    fn apply(&self, state: &mut Value, event: &SessionEvent) -> Result<(), HarnessError> {
        let SessionEventKind::FeedbackRecorded {
            target_seq,
            revision,
            rating,
            note,
        } = &event.kind
        else {
            return Ok(());
        };
        let mut feedback =
            serde_json::from_value::<FeedbackState>(state.clone()).map_err(|error| {
                HarnessError::execution(format!("decode feedback projection: {error}"))
            })?;
        feedback.0.insert(
            *target_seq,
            FeedbackEntry {
                revision: *revision,
                rating: *rating,
                note: note.clone(),
            },
        );
        *state = serde_json::to_value(feedback).map_err(|error| {
            HarnessError::execution(format!("encode feedback projection: {error}"))
        })?;
        Ok(())
    }

    fn view(&self, state: &Value) -> Result<Value, HarnessError> {
        serde_json::from_value::<FeedbackState>(state.clone())
            .and_then(serde_json::to_value)
            .map_err(|error| {
                HarnessError::execution(format!("decode feedback projection view: {error}"))
            })
    }
}

pub fn session_stats(events: &[SessionEvent]) -> Result<SessionStats, HarnessError> {
    let active = ternilo_protocol::conversation_events(events);
    let events = active.as_ref();
    let unit = StatsUnit;
    let mut state = unit.initial();
    for event in events {
        unit.apply(&mut state, event)?;
    }
    serde_json::from_value(unit.view(&state)?).map_err(|error| {
        HarnessError::execution(format!("decode projected session stats: {error}"))
    })
}

struct StatsUnit;

impl SessionProjectionUnit for StatsUnit {
    fn key(&self) -> &'static str {
        "stats"
    }

    fn version(&self) -> u32 {
        2
    }

    fn initial(&self) -> Value {
        serde_json::to_value(StatsState::default()).expect("stats state is JSON serializable")
    }

    fn valid_state(&self, state: &Value) -> bool {
        serde_json::from_value::<StatsState>(state.clone()).is_ok()
    }

    fn apply(&self, state: &mut Value, event: &SessionEvent) -> Result<(), HarnessError> {
        let mut decoded: StatsState = serde_json::from_value(state.clone()).map_err(|error| {
            HarnessError::execution(format!("decode stats projection state: {error}"))
        })?;
        apply_stats_event(&mut decoded, event);
        let event_chars = serde_json::to_string(event)
            .map_err(|error| HarnessError::execution(format!("serialize session event: {error}")))?
            .chars()
            .count();
        decoded.serialized_event_chars = decoded
            .serialized_event_chars
            .saturating_add(u64::try_from(event_chars).unwrap_or(u64::MAX));
        decoded.stats.estimated_logged_tokens = decoded
            .serialized_event_chars
            .saturating_add(decoded.stats.events.saturating_sub(1))
            .saturating_add(2)
            .div_ceil(4);
        *state = serde_json::to_value(decoded).map_err(|error| {
            HarnessError::execution(format!("encode stats projection state: {error}"))
        })?;
        Ok(())
    }

    fn view(&self, state: &Value) -> Result<Value, HarnessError> {
        let state: StatsState = serde_json::from_value(state.clone()).map_err(|error| {
            HarnessError::execution(format!("decode stats projection state: {error}"))
        })?;
        serde_json::to_value(state.stats).map_err(|error| {
            HarnessError::execution(format!("encode stats projection view: {error}"))
        })
    }
}

fn apply_stats_event(state: &mut StatsState, event: &SessionEvent) {
    state.stats.events = state.stats.events.saturating_add(1);
    match &event.kind {
        SessionEventKind::TurnStarted => {
            state.stats.turns = state.stats.turns.saturating_add(1);
        }
        SessionEventKind::TurnFinished { .. } => {
            state.stats.completed_turns = state.stats.completed_turns.saturating_add(1);
            clear_run_timing(state, event.run_id.as_str());
        }
        SessionEventKind::TurnFailed { .. } => {
            state.stats.failed_turns = state.stats.failed_turns.saturating_add(1);
            clear_run_timing(state, event.run_id.as_str());
        }
        SessionEventKind::TurnCancelled => {
            state.stats.cancelled_turns = state.stats.cancelled_turns.saturating_add(1);
            clear_run_timing(state, event.run_id.as_str());
        }
        SessionEventKind::StepStarted { step } => {
            state.stats.steps = state.stats.steps.saturating_add(1);
            state
                .model_started_at
                .insert(step_key(event, *step), event.occurred_at_ms);
        }
        SessionEventKind::AssistantMessageDelta { step, .. }
        | SessionEventKind::AssistantReasoningDelta { step, .. } => {
            state
                .model_first_token_at
                .entry(step_key(event, *step))
                .or_insert(event.occurred_at_ms);
        }
        SessionEventKind::ToolCallStarted { call }
        | SessionEventKind::CodeDispatchStarted { call, .. } => {
            state.stats.tool_calls = state.stats.tool_calls.saturating_add(1);
            state
                .tool_started_at
                .insert(call_key(event, &call.id), event.occurred_at_ms);
        }
        SessionEventKind::ToolCallFinished { call_id, .. }
        | SessionEventKind::CodeDispatchFinished { call_id, .. } => {
            record_tool_duration(state, event, call_id);
        }
        SessionEventKind::UserMessage { .. } => {
            state.stats.user_messages = state.stats.user_messages.saturating_add(1);
        }
        SessionEventKind::AssistantMessage { step, response } => {
            record_assistant_message(state, event, *step, response);
        }
        _ => {}
    }
}

fn record_tool_duration(state: &mut StatsState, event: &SessionEvent, call_id: &str) {
    if let Some(started_at) = state.tool_started_at.remove(&call_key(event, call_id)) {
        state.stats.tool_duration_ms = state
            .stats
            .tool_duration_ms
            .saturating_add(event.occurred_at_ms.saturating_sub(started_at));
    }
}

fn record_assistant_message(
    state: &mut StatsState,
    event: &SessionEvent,
    step: u32,
    response: &ModelResponse,
) {
    state.stats.assistant_messages = state.stats.assistant_messages.saturating_add(1);
    let key = step_key(event, step);
    let started_at = state.model_started_at.remove(&key);
    let first_token_at = state.model_first_token_at.remove(&key);
    if let Some(started_at) = started_at {
        state.stats.model_duration_ms = state
            .stats
            .model_duration_ms
            .saturating_add(event.occurred_at_ms.saturating_sub(started_at));
        if let Some(first_token_at) = first_token_at {
            state.stats.first_token_duration_ms = state
                .stats
                .first_token_duration_ms
                .saturating_add(first_token_at.saturating_sub(started_at));
            state.stats.measured_first_tokens = state.stats.measured_first_tokens.saturating_add(1);
            if let Some(usage) = response.usage {
                state.stats.generation_duration_ms = state
                    .stats
                    .generation_duration_ms
                    .saturating_add(event.occurred_at_ms.saturating_sub(first_token_at));
                state.stats.generation_output_tokens = state
                    .stats
                    .generation_output_tokens
                    .saturating_add(usage.output_tokens);
            }
        }
    }
    state.stats.model_attempts = state
        .stats
        .model_attempts
        .saturating_add(u64::from(response.attempts));
    if let Some(usage) = response.usage {
        state.stats.measured_model_responses =
            state.stats.measured_model_responses.saturating_add(1);
        state.stats.exact_input_tokens = state
            .stats
            .exact_input_tokens
            .saturating_add(usage.input_tokens);
        state.stats.exact_output_tokens = state
            .stats
            .exact_output_tokens
            .saturating_add(usage.output_tokens);
        state.stats.cached_input_tokens = state
            .stats
            .cached_input_tokens
            .saturating_add(usage.cached_input_tokens);
        state.stats.exact_reasoning_tokens = state
            .stats
            .exact_reasoning_tokens
            .saturating_add(usage.reasoning_tokens);
    }
}

fn step_key(event: &SessionEvent, step: u32) -> String {
    format!("{}:{step}", event.run_id.as_str())
}

fn call_key(event: &SessionEvent, call_id: &str) -> String {
    format!("{}:{call_id}", event.run_id.as_str())
}

fn clear_run_timing(state: &mut StatsState, run_id: &str) {
    let prefix = format!("{run_id}:");
    state
        .model_started_at
        .retain(|key, _| !key.starts_with(&prefix));
    state
        .model_first_token_at
        .retain(|key, _| !key.starts_with(&prefix));
    state
        .tool_started_at
        .retain(|key, _| !key.starts_with(&prefix));
}

#[cfg(test)]
mod tests {
    use ternilo_protocol::{
        FeedbackRating, ModelFinishReason, ModelResponse, ModelUsage, RunId, SessionEvent,
        SessionEventKind, ToolCall, ToolOutput, TurnFinishReason,
    };

    use super::{feedback_unit, session_stats};

    fn event(seq: u64, at: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            seq,
            occurred_at_ms: at,
            run_id: RunId::new("run-test"),
            kind,
        }
    }

    #[test]
    fn stats_project_model_latency_throughput_and_tool_time() {
        let response = ModelResponse {
            provider: "test-provider".into(),
            model: "test-model".into(),
            content: "done".into(),
            reasoning_content: Some("consider".into()),
            provider_state: None,
            tool_calls: Vec::new(),
            usage: Some(ModelUsage {
                input_tokens: 100,
                output_tokens: 50,
                cached_input_tokens: 20,
                cache_write_tokens: Some(5),
                reasoning_tokens: 12,
            }),
            finish_reason: ModelFinishReason::Stop,
            provider_request_id: None,
            attempts: 1,
            request_digest: None,
            replayed: false,
        };
        let events = vec![
            event(1, 100, SessionEventKind::TurnStarted),
            event(2, 110, SessionEventKind::StepStarted { step: 1 }),
            event(
                3,
                310,
                SessionEventKind::AssistantReasoningDelta {
                    step: 1,
                    delta: "d".into(),
                },
            ),
            event(
                4,
                810,
                SessionEventKind::AssistantMessage { step: 1, response },
            ),
            event(
                5,
                820,
                SessionEventKind::ToolCallStarted {
                    call: ToolCall {
                        id: "call-1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({ "path": "README.md" }),
                        presentation: None,
                    },
                },
            ),
            event(
                6,
                1_320,
                SessionEventKind::ToolCallFinished {
                    call_id: "call-1".into(),
                    name: "read".into(),
                    output: ToolOutput {
                        content: "ok".into(),
                        is_error: false,
                    },
                    retained_output: None,
                },
            ),
            event(
                7,
                1_330,
                SessionEventKind::TurnFinished {
                    answer: "done".into(),
                    finish_reason: TurnFinishReason::Completed,
                },
            ),
        ];

        let stats = session_stats(&events).expect("stats projection succeeds");
        assert_eq!(stats.turns, 1);
        assert_eq!(stats.steps, 1);
        assert_eq!(stats.model_duration_ms, 700);
        assert_eq!(stats.first_token_duration_ms, 200);
        assert_eq!(stats.measured_first_tokens, 1);
        assert_eq!(stats.generation_duration_ms, 500);
        assert_eq!(stats.generation_output_tokens, 50);
        assert_eq!(stats.tool_duration_ms, 500);
        assert_eq!(stats.exact_input_tokens, 100);
        assert_eq!(stats.exact_output_tokens, 50);
        assert_eq!(stats.cached_input_tokens, 20);
        assert_eq!(stats.exact_reasoning_tokens, 12);
    }

    #[test]
    fn feedback_projection_keeps_latest_rating_and_retracts_it() {
        let unit = feedback_unit();
        let mut state = unit.initial();
        for event in [
            event(
                1,
                100,
                SessionEventKind::FeedbackRecorded {
                    target_seq: 7,
                    revision: 1,
                    rating: Some(FeedbackRating::Negative),
                    note: Some("too terse".into()),
                },
            ),
            event(
                2,
                200,
                SessionEventKind::FeedbackRecorded {
                    target_seq: 9,
                    revision: 1,
                    rating: Some(FeedbackRating::Positive),
                    note: None,
                },
            ),
            event(
                3,
                300,
                SessionEventKind::FeedbackRecorded {
                    target_seq: 7,
                    revision: 2,
                    rating: Some(FeedbackRating::Positive),
                    note: Some("fixed".into()),
                },
            ),
            event(
                4,
                400,
                SessionEventKind::FeedbackRecorded {
                    target_seq: 9,
                    revision: 2,
                    rating: None,
                    note: None,
                },
            ),
        ] {
            unit.apply(&mut state, &event).expect("feedback applies");
        }
        let view = unit.view(&state).expect("feedback view");
        assert_eq!(view["7"]["rating"], "positive");
        assert_eq!(view["7"]["note"], "fixed");
        assert_eq!(view["7"]["revision"], 2);
        assert_eq!(view["9"]["revision"], 2);
        assert!(view["9"]["rating"].is_null());
        assert!(view["9"]["note"].is_null());
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestState {
    value: Option<Value>,
}

pub(crate) fn latest_unit(
    key: &'static str,
    version: u32,
    select: fn(&SessionEventKind) -> Option<Value>,
) -> Arc<dyn SessionProjectionUnit> {
    Arc::new(LatestUnit {
        key,
        version,
        select,
    })
}

struct LatestUnit {
    key: &'static str,
    version: u32,
    select: fn(&SessionEventKind) -> Option<Value>,
}

impl SessionProjectionUnit for LatestUnit {
    fn key(&self) -> &'static str {
        self.key
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn initial(&self) -> Value {
        serde_json::json!({ "value": null })
    }

    fn valid_state(&self, state: &Value) -> bool {
        serde_json::from_value::<LatestState>(state.clone()).is_ok()
    }

    fn apply(&self, state: &mut Value, event: &SessionEvent) -> Result<(), HarnessError> {
        if let Some(value) = (self.select)(&event.kind) {
            *state = serde_json::to_value(LatestState { value: Some(value) }).map_err(|error| {
                HarnessError::execution(format!("encode {:?} projection state: {error}", self.key))
            })?;
        }
        Ok(())
    }

    fn view(&self, state: &Value) -> Result<Value, HarnessError> {
        serde_json::from_value::<LatestState>(state.clone())
            .map(|state| state.value.unwrap_or(Value::Null))
            .map_err(|error| {
                HarnessError::execution(format!("decode {:?} projection state: {error}", self.key))
            })
    }
}
