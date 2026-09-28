use serde_json::{Value, json};
use ternilo_kernel::{ActivityBranch, HarnessSession, HostEnvironment, HostPolicy};
use ternilo_protocol::{
    AgentId, Attachment, ContextCompaction, MessageRole, ModelFinishReason, ModelResponse, RunId,
    RunLimits, SessionEvent, SessionEventKind, SessionId, SessionIdentity, SubagentStatus,
    TenantId, ToolCall, ToolOutput, UserId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::session::{derive_model_messages, pending_tool_calls};

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "echo".to_owned(),
        arguments: json!({"text": id}),
        presentation: None,
    }
}

fn assistant(ids: &[&str]) -> SessionEventKind {
    SessionEventKind::AssistantMessage {
        step: 1,
        response: ModelResponse {
            provider: "fixture".to_owned(),
            model: "fixture".to_owned(),
            content: "tool batch".to_owned(),
            reasoning_content: Some("retained reasoning".to_owned()),
            provider_state: None,
            tool_calls: ids.iter().map(|id| call(id)).collect(),
            usage: None,
            finish_reason: ModelFinishReason::ToolCalls,
            provider_request_id: None,
            attempts: 1,
            request_digest: None,
            replayed: false,
        },
    }
}

fn finished(id: &str, content: &str) -> SessionEventKind {
    SessionEventKind::ToolCallFinished {
        call_id: id.to_owned(),
        name: "echo".to_owned(),
        output: ToolOutput {
            content: content.to_owned(),
            is_error: false,
        },
        retained_output: None,
    }
}

fn event(seq: u64, run: &str, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        seq,
        occurred_at_ms: seq,
        run_id: RunId::new(run),
        kind,
    }
}

fn user() -> SessionEventKind {
    SessionEventKind::UserMessage {
        provenance: None,
        content: "continue".to_owned(),
        display_content: None,
        source: None,
        references: Vec::new(),
        attachments: vec![Attachment {
            name: "note.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            content: "retained attachment".to_owned(),
        }],
    }
}

#[test]
fn terminal_history_closes_only_missing_calls_and_preserves_late_results() {
    let events = vec![
        event(0, "old", assistant(&["done", "late", "missing", "unknown"])),
        event(1, "old", finished("done", "completed result")),
        event(
            2,
            "old",
            SessionEventKind::ToolCallStarted {
                call: call("unknown"),
            },
        ),
        event(
            3,
            "old",
            SessionEventKind::TurnFailed {
                message: "turn exceeded max_tool_calls (32)".to_owned(),
            },
        ),
        event(4, "next", user()),
        event(5, "old", finished("late", "late real result")),
    ];
    let original = events.clone();
    let messages = derive_model_messages(&events, 1_000);
    assert_eq!(
        events, original,
        "replay repair must not mutate canonical facts"
    );
    assert_eq!(messages.len(), 6);
    assert_eq!(
        messages[0].reasoning_content.as_deref(),
        Some("retained reasoning")
    );
    assert_eq!(messages[1].content, "completed result");
    assert_eq!(messages[2].content, "late real result");
    assert!(messages[3].content.contains("tool_call_not_executed"));
    assert!(messages[3].content.contains("max_tool_calls (32)"));
    assert!(messages[4].content.contains("tool_call_interrupted"));
    assert_eq!(messages[5].role, MessageRole::User);
    assert_eq!(messages[5].attachments[0].content, "retained attachment");
    assert_eq!(
        pending_tool_calls(&events, &RunId::new("old")),
        vec![(call("missing"), false), (call("unknown"), true)]
    );
}

#[test]
fn call_ids_are_scoped_to_assistant_batches_and_hook_context_follows_results() {
    let events = vec![
        event(0, "run", assistant(&["reused"])),
        event(
            1,
            "run",
            SessionEventKind::HookContextAdded {
                handler_id: "hook".to_owned(),
                dialect: "plain".to_owned(),
                content: "retained hook context".to_owned(),
                reference: None,
                completeness: None,
            },
        ),
        event(2, "run", finished("reused", "first result")),
        event(3, "run", assistant(&["reused"])),
        event(4, "run", finished("reused", "second result")),
    ];
    let messages = derive_model_messages(&events, 1_000);
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[1].content, "first result");
    assert!(messages[2].content.contains("retained hook context"));
    assert_eq!(messages[4].content, "second result");
}

#[test]
fn live_batches_are_not_failed_and_compaction_does_not_replay_orphan_results() {
    let mut events = vec![
        event(0, "run", assistant(&["active"])),
        event(
            1,
            "run",
            SessionEventKind::ToolCallStarted {
                call: call("active"),
            },
        ),
    ];
    assert_eq!(derive_model_messages(&events, 1_000).len(), 1);
    events.push(event(2, "run", SessionEventKind::TurnCancelled));
    let cancelled = derive_model_messages(&events, 1_000);
    assert_eq!(cancelled.len(), 2);
    assert!(cancelled[1].content.contains("turn cancelled"));
    events.push(event(3, "run", finished("active", "actual result")));
    events.push(event(
        4,
        "compact",
        SessionEventKind::ContextCompacted {
            compaction_id: "compact".to_owned(),
            compaction: ContextCompaction {
                through_seq: 2,
                summary: "earlier summary".to_owned(),
                estimated_tokens_before: 1_000,
                automatic: false,
            },
        },
    ));
    let compacted = derive_model_messages(&events, 1_000);
    assert_eq!(compacted.len(), 1);
    assert!(compacted[0].content.contains("earlier summary"));
}

pub(super) async fn read_request(stream: &mut tokio::net::TcpStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 8_192];
        let count = stream.read(&mut chunk).await.unwrap();
        assert!(count > 0, "fixture request ended early");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

async fn model_fixture() -> (String, tokio::task::JoinHandle<Value>) {
    model_fixture_with_calls(vec![
        json!({"id":"executed", "type":"function", "function":{"name":"list_agents", "arguments":"{}"}}),
        json!({"id":"skipped-a", "type":"function", "function":{"name":"list_agents", "arguments":"{}"}}),
        json!({"id":"skipped-b", "type":"function", "function":{"name":"list_agents", "arguments":"{}"}}),
    ]).await
}

pub(super) async fn model_fixture_with_calls(
    calls: Vec<Value>,
) -> (String, tokio::task::JoinHandle<Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut next_request = Value::Null;
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let message = if attempt == 0 {
                json!({"content":"", "reasoning_content":"retained reasoning", "tool_calls":calls})
            } else {
                next_request = request;
                json!({"content":"continuation accepted", "tool_calls":[]})
            };
            let body = json!({"choices":[{"message":message}]}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        next_request
    });
    (format!("http://{address}/v1"), task)
}

async fn configured_harness(base_url: String, tool_limit: Option<u32>) -> HarnessSession {
    let mut profile = crate::local_profile();
    let model = profile
        .plugins
        .iter_mut()
        .find(|entry| entry.id == "model")
        .unwrap();
    model.kind = crate::model::KIND.to_owned();
    model.config =
        json!({"provider":"fixture", "base_url":base_url, "model":"fixture", "timeout_ms":5000});
    let mut limits = RunLimits::default();
    if let Some(tool_limit) = tool_limit {
        limits.max_tool_calls = tool_limit;
        profile
            .plugins
            .iter_mut()
            .find(|entry| entry.id == "subagents")
            .unwrap()
            .config = json!({"max_tool_calls":tool_limit});
    }
    let identity = SessionIdentity {
        tenant_id: TenantId::new("local"),
        user_id: UserId::new("test"),
        agent_id: AgentId::new("default"),
        session_id: SessionId::new("interrupted-history"),
    };
    let environment = HostEnvironment::memory(identity, None, HostPolicy::local(limits));
    HarnessSession::boot(&crate::catalog().unwrap(), &profile, environment)
        .await
        .unwrap()
}

fn assert_complete_wire_batch(request: &Value) {
    let messages = request["messages"].as_array().unwrap();
    let index = messages
        .iter()
        .position(|message| message["role"] == "assistant" && message["tool_calls"].is_array())
        .unwrap();
    assert!(
        serde_json::from_str::<Vec<Value>>(messages[index + 1]["content"].as_str().unwrap())
            .is_ok()
    );
    for (offset, id) in [(2, "skipped-a"), (3, "skipped-b")] {
        assert_eq!(messages[index + offset]["tool_call_id"], id);
        let content = messages[index + offset]["content"].as_str().unwrap();
        assert!(content.contains("tool_call_not_executed"));
        assert!(content.contains("max_tool_calls (1)"));
    }
    assert_eq!(messages[index + 4]["role"], "user");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        3
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn default_host_completes_a_turn_beyond_the_old_thirty_two_tool_limit() {
    let calls = (0..33)
        .map(|index| {
            json!({
                "id": format!("tool-{index}"), "type": "function",
                "function": { "name": "list_agents", "arguments": "{}" },
            })
        })
        .collect();
    let (base_url, request) = model_fixture_with_calls(calls).await;
    let harness = configured_harness(base_url, None).await;
    let outcome = harness
        .run(
            RunId::new("default-tool-budget"),
            "Complete every tool call",
        )
        .await
        .unwrap();
    assert_eq!(outcome.answer, "continuation accepted");
    assert_eq!(outcome.tool_calls, 33);
    let request = request.await.unwrap();
    let results = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 33);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result["tool_call_id"], format!("tool-{index}"));
        assert_eq!(result["content"], "[]");
    }
    assert!(
        !outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnFailed { .. }))
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn parent_budget_failure_closes_the_batch_before_a_real_provider_continuation() {
    let (base_url, request) = model_fixture().await;
    let harness = configured_harness(base_url, Some(1)).await;
    let failure = harness
        .run(RunId::new("limited"), "call three tools")
        .await
        .unwrap_err();
    assert!(failure.message.contains("max_tool_calls (1)"));
    let events = harness.events().await;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(events.iter().filter(|event| matches!(&event.kind, SessionEventKind::ToolCallFinished { output, .. } if output.is_error)).count(), 2);
    let resumed = harness
        .run(RunId::new("continued"), "explain the failure")
        .await
        .unwrap();
    assert_eq!(resumed.answer, "continuation accepted");
    assert_complete_wire_batch(&request.await.unwrap());
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn in_process_child_budget_failure_preserves_a_valid_followup_history() {
    let (base_url, request) = model_fixture().await;
    let harness = configured_harness(base_url, Some(1)).await;
    let subagents = harness.subagents().unwrap();
    let child = subagents
        .spawn(
            RunId::new("parent"),
            "call three tools".to_owned(),
            None,
            false,
            ActivityBranch::untracked(),
        )
        .await
        .unwrap();
    assert_eq!(child.status, SubagentStatus::Failed);
    assert!(
        child
            .error
            .as_deref()
            .unwrap()
            .contains("max_tool_calls (1)")
    );
    subagents
        .followup(
            RunId::new("parent"),
            child.subagent_id.clone(),
            "continue".to_owned(),
            None,
        )
        .await
        .unwrap();
    let resumed = subagents
        .wait(child.subagent_id, 5_000, ActivityBranch::untracked())
        .await
        .unwrap();
    assert_eq!(resumed.status, SubagentStatus::Idle);
    assert_eq!(resumed.output.as_deref(), Some("continuation accepted"));
    assert_complete_wire_batch(&request.await.unwrap());
    harness.shutdown().await.unwrap();
}
