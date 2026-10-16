use ternilo_protocol::{
    ReasoningEffort, ReasoningObservation, RunId, SessionEvent, SessionEventKind,
};

pub(crate) fn observe(events: &[SessionEvent], run_id: &RunId) -> ReasoningObservation {
    let mut observation = ReasoningObservation {
        selection: None,
        recommended: ReasoningEffort::Low,
        task_chars: 0,
        tool_calls: 0,
        tool_errors: 0,
    };
    for event in events.iter().filter(|event| &event.run_id == run_id) {
        match &event.kind {
            SessionEventKind::UserMessage { content, .. } => {
                observation.task_chars = observation
                    .task_chars
                    .saturating_add(u64::try_from(content.chars().count()).unwrap_or(u64::MAX));
            }
            SessionEventKind::ToolCallStarted { .. }
            | SessionEventKind::CodeDispatchStarted { .. } => observation.tool_calls += 1,
            SessionEventKind::ToolCallFinished { output, .. }
            | SessionEventKind::CodeDispatchFinished { output, .. }
                if output.is_error =>
            {
                observation.tool_errors += 1;
            }
            _ => {}
        }
    }
    observation.recommended = if observation.tool_errors > 0
        || observation.task_chars > 1500
        || observation.tool_calls >= 8
    {
        ReasoningEffort::High
    } else if observation.task_chars > 240 || observation.tool_calls > 0 {
        ReasoningEffort::Medium
    } else {
        ReasoningEffort::Low
    };
    observation
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn observation_uses_current_run_work_and_failures_without_language_specific_keywords() {
        let run = RunId::new("current");
        let event = |seq, run_id: &str, kind: serde_json::Value| -> SessionEvent {
            let mut value = kind;
            value["seq"] = json!(seq);
            value["run_id"] = json!(run_id);
            value["occurred_at_ms"] = json!(1);
            serde_json::from_value(value).unwrap()
        };
        let mut events = vec![
            event(
                0,
                "previous",
                json!({"type":"tool_call_finished","call_id":"old","name":"read","output":{"content":"failed","is_error":true}}),
            ),
            event(
                1,
                "current",
                json!({"type":"user_message","content":"继续 / Continue / 계속"}),
            ),
        ];
        assert_eq!(observe(&events, &run).recommended, ReasoningEffort::Low);
        events.push(event(2, "current", json!({"type":"tool_call_started","call":{"id":"read","name":"read_file","arguments":{}}})));
        assert_eq!(observe(&events, &run).recommended, ReasoningEffort::Medium);
        events.push(event(3, "current", json!({"type":"tool_call_finished","call_id":"read","name":"read_file","output":{"content":"failed","is_error":true}})));
        let observation = observe(&events, &run);
        assert_eq!(observation.recommended, ReasoningEffort::High);
        assert_eq!(observation.tool_errors, 1);
        assert_eq!(observation.tool_calls, 1);
    }
}
