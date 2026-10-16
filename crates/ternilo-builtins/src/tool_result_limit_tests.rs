use super::*;
use serde_json::json;

#[test]
fn per_tool_limits_override_defaults_before_the_shared_history_budget() {
    let run = RunId::new("limits");
    let calls = ["read_file", "search_files", "list_agents"].map(|name| ToolCall {
        id: name.into(),
        name: name.into(),
        arguments: json!({}),
        presentation: None,
    });
    let events = calls
        .iter()
        .enumerate()
        .map(|(index, call)| SessionEvent {
            seq: index as u64,
            occurred_at_ms: 0,
            run_id: run.clone(),
            kind: SessionEventKind::ToolCallFinished {
                call_id: call.id.clone(),
                name: call.name.clone(),
                output: ToolOutput {
                    content: "前내용끝".repeat(500),
                    is_error: false,
                },
                retained_output: None,
            },
        })
        .collect::<Vec<_>>();
    let before = events.clone();
    let limits = BTreeMap::from([("read_file".into(), 512), ("search_files".into(), 1536)]);
    let mut messages = model_tool_responses(&events, &run, &calls, 1024, &limits);
    assert_eq!(messages[0].content.chars().count(), 512);
    assert_eq!(messages[1].content.chars().count(), 1536);
    assert!(messages[2].content.contains("tool_result_pruned"));
    assert!(messages[0].content.contains("full output remains"));
    assert!(messages[0].content.ends_with('끝'));
    tool_output_budget::apply(&mut messages, 1024);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.content.chars().count())
            .sum::<usize>(),
        1024
    );
    for (message, call) in messages.iter().zip(&calls) {
        assert_eq!(message.tool_call_id.as_deref(), Some(call.id.as_str()));
    }
    assert_eq!(events, before);
}

#[test]
fn per_tool_limits_also_bound_synthesized_missing_results() {
    let run = RunId::new("interrupted");
    let calls = [ToolCall {
        id: "missing".into(),
        name: "read_file".into(),
        arguments: json!({}),
        presentation: None,
    }];
    let events = [SessionEvent {
        seq: 0,
        occurred_at_ms: 0,
        run_id: run.clone(),
        kind: SessionEventKind::TurnFailed {
            message: "failure details ".repeat(500),
        },
    }];
    let legacy = model_tool_responses(&events, &run, &calls, 1024, &BTreeMap::new());
    assert!(legacy[0].content.chars().count() > 512);
    let messages = model_tool_responses(
        &events,
        &run,
        &calls,
        1024,
        &BTreeMap::from([("read_file".into(), 512)]),
    );
    assert_eq!(messages[0].content.chars().count(), 512);
    assert_eq!(messages[0].tool_call_id.as_deref(), Some("missing"));
}
