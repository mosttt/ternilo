use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use ternilo_kernel::{HarnessSession, HostEnvironment, HostPolicy};
use ternilo_protocol::{
    AgentId, ErrorCode, GoalStatus, RunId, RunLimits, SessionEventKind, SessionId, SessionIdentity,
    TenantId, TurnFinishReason, UserId,
};
use tokio::io::AsyncWriteExt;

const OBJECTIVE: &str = "Verify the complete objective";

struct ModelFixture {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ModelFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ModelFixture {
    async fn start(responses: Vec<Option<Value>>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let responses = Arc::new(responses);
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let captured = Arc::clone(&captured);
                let responses = Arc::clone(&responses);
                handlers.spawn(async move {
                    let request = crate::tool_history_tests::read_request(&mut stream).await;
                    let index = {
                        let mut requests = captured.lock().unwrap();
                        let index = requests.len();
                        requests.push(request);
                        index
                    };
                    let Some(Some(response)) = responses.get(index) else {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    };
                    let body = response.to_string();
                    let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    async fn harness(&self, max_rounds: u32) -> HarnessSession {
        let mut profile = crate::local_profile();
        profile.plugins.push(crate::entry(
            "planning-tools",
            crate::PLAN_TOOL_KIND,
            json!({}),
        ));
        for plugin in &mut profile.plugins {
            match plugin.id.as_str() {
                "model" => {
                    plugin.kind = crate::model::KIND.to_owned();
                    plugin.config = json!({"provider":"fixture", "base_url":self.base_url, "model":"fixture", "timeout_ms":5000, "max_attempts":1});
                }
                "agent-loop" => plugin.config = json!({"max_goal_rounds":max_rounds}),
                _ => {}
            }
        }
        let environment = HostEnvironment::memory(
            SessionIdentity {
                tenant_id: TenantId::new("local"),
                user_id: UserId::new("goal-owner"),
                agent_id: AgentId::new("default"),
                session_id: SessionId::new("goal-execution"),
            },
            None,
            HostPolicy::local(RunLimits::default()),
        );
        HarnessSession::boot(&crate::catalog().unwrap(), &profile, environment)
            .await
            .unwrap()
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

fn answer(content: &str) -> Value {
    json!({"choices":[{"message":{"content":content}, "finish_reason":"stop"}]})
}

fn goal_update(status: &str) -> Value {
    json!({"choices":[{"message":{"tool_calls":[{
        "id":"goal-state", "type":"function", "function":{"name":"update_goal", "arguments":json!({"objective":OBJECTIVE,"status":status}).to_string()},
    }]}, "finish_reason":"tool_calls"}]})
}

#[tokio::test]
async fn goal_executes_continues_and_finishes_with_valid_model_history() {
    let fixture = ModelFixture::start(vec![
        Some(answer("More work is needed")),
        Some(goal_update("complete")),
        Some(answer("Verified result")),
    ])
    .await;
    let harness = fixture.harness(4).await;
    let run_id = RunId::new("goal-start");
    let outcome = harness
        .run(run_id.clone(), format!("/goal {OBJECTIVE}"))
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Verified result");
    assert_eq!(outcome.tool_calls, 2);
    assert!(outcome.events.iter().all(|event| event.run_id == run_id));
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::UserMessage { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
            .count(),
        1
    );
    let rounds = outcome
        .events
        .iter()
        .filter_map(|event| match event.kind {
            SessionEventKind::GoalRoundStarted { round, .. } => Some(round),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rounds, [1, 2]);
    assert_eq!(
        crate::agent_goal::current_goal(&outcome.events).unwrap().1,
        GoalStatus::Complete
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["messages"][0], requests[2]["messages"][0]);
    assert!(outcome.events.iter().any(|event| matches!(&event.kind, SessionEventKind::CommandFinished { outcome, .. } if outcome.code == "goal_execution_started")));
    assert!(
        requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == format!("/goal {OBJECTIVE}"))
    );
    assert!(
        !requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool")
    );
    assert!(
        requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Round: 2")
    );
    let messages = requests[2]["messages"].as_array().unwrap();
    let tool_index = messages
        .iter()
        .position(|message| message["role"] == "tool")
        .unwrap();
    assert_eq!(messages[tool_index]["tool_call_id"], "goal-state");
    assert_eq!(messages[tool_index - 1]["role"], "assistant");
    assert_eq!(
        messages[tool_index - 1]["tool_calls"][0]["id"],
        "goal-state"
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_management_is_model_independent_but_resume_executes() {
    let fixture = ModelFixture::start(vec![
        Some(goal_update("complete")),
        Some(answer("Resumed and verified")),
    ])
    .await;
    let harness = fixture.harness(4).await;
    for action in ["edit", "blocked", "complete"] {
        harness
            .run(RunId::new(action), format!("/goal {action} {OBJECTIVE}"))
            .await
            .unwrap();
    }
    assert!(fixture.requests().is_empty());
    let outcome = harness
        .run(RunId::new("resume"), format!("/goal resume {OBJECTIVE}"))
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Resumed and verified");
    assert_eq!(fixture.requests().len(), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_management_state_reaches_later_requests_without_restarting_execution() {
    let fixture = ModelFixture::start(vec![Some(answer("The saved goal is complete"))]).await;
    let harness = fixture.harness(4).await;
    for action in ["edit", "complete"] {
        harness
            .run(RunId::new(action), format!("/goal {action} {OBJECTIVE}"))
            .await
            .unwrap();
    }
    let outcome = harness
        .run(RunId::new("ordinary"), "Report the saved goal state")
        .await
        .unwrap();
    assert_eq!(outcome.answer, "The saved goal is complete");
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let state = requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find_map(|message| {
            message["content"]
                .as_str()
                .filter(|content| content.starts_with("<session_goal_state>"))
        })
        .unwrap();
    assert!(state.contains("\"status\":\"complete\""));
    assert!(
        !outcome
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::GoalRoundStarted { .. }))
    );
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_blocker_stops_automatic_continuation() {
    let fixture = ModelFixture::start(vec![
        Some(goal_update("blocked")),
        Some(answer("Credentials are required")),
    ])
    .await;
    let harness = fixture.harness(4).await;
    let outcome = harness
        .run(RunId::new("blocked"), format!("/goal {OBJECTIVE}"))
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Credentials are required");
    assert_eq!(
        crate::agent_goal::current_goal(&outcome.events).unwrap().1,
        GoalStatus::Blocked
    );
    assert_eq!(fixture.requests().len(), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_round_limit_blocks_without_claiming_completion() {
    let fixture = ModelFixture::start(vec![
        Some(answer("Continue inspecting")),
        Some(answer("Not finished")),
    ])
    .await;
    let harness = fixture.harness(2).await;
    let error = harness
        .run(RunId::new("limited"), format!("/goal {OBJECTIVE}"))
        .await
        .unwrap_err();
    assert!(error.message.contains("limit of 2 rounds"));
    let events = harness.events().await;
    assert_eq!(
        crate::agent_goal::current_goal(&events).unwrap().1,
        GoalStatus::Blocked
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::TurnFinished { .. }))
    );
    assert_eq!(fixture.requests().len(), 2);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_cancellation_stays_stopped_until_explicit_resume() {
    let fixture = ModelFixture::start(vec![
        None,
        Some(goal_update("complete")),
        Some(answer("Resumed after cancellation")),
    ])
    .await;
    let harness = Arc::new(fixture.harness(4).await);
    let running = Arc::clone(&harness);
    let task = tokio::spawn(async move {
        running
            .run(RunId::new("cancelled"), format!("/goal {OBJECTIVE}"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    harness.cancel(RunId::new("cancelled")).await.unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::Cancelled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fixture.requests().len(), 1);
    assert_eq!(
        crate::agent_goal::current_goal(&harness.events().await)
            .unwrap()
            .1,
        GoalStatus::Active
    );
    let outcome = harness
        .run(RunId::new("resumed"), format!("/goal resume {OBJECTIVE}"))
        .await
        .unwrap();
    assert_eq!(outcome.answer, "Resumed after cancellation");
    assert_eq!(fixture.requests().len(), 3);
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_model_truncation_does_not_start_another_round() {
    let fixture = ModelFixture::start(vec![Some(
        json!({"choices":[{"message":{"content":"Truncated response"},"finish_reason":"length"}]}),
    )])
    .await;
    let harness = fixture.harness(4).await;
    let outcome = harness
        .run(RunId::new("truncated"), format!("/goal {OBJECTIVE}"))
        .await
        .unwrap();
    assert!(outcome.events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnFinished {
            finish_reason: TurnFinishReason::MaxTokens,
            ..
        }
    )));
    assert_eq!(
        crate::agent_goal::current_goal(&outcome.events).unwrap().1,
        GoalStatus::Blocked
    );
    assert_eq!(fixture.requests().len(), 1);
    harness.shutdown().await.unwrap();
}
