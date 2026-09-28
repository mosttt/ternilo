use serde_json::{Value, json};
use ternilo_kernel::{ActivityBranch, HarnessSession, HostEnvironment, HostPolicy};
use ternilo_protocol::{
    AgentId, RunId, RunLimits, SessionEventKind, SessionId, SessionIdentity, SubagentStatus,
    TenantId, UserId,
};

use crate::tool_history_tests::model_fixture_with_calls;

fn calls(count: u32) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "id": format!("call-{index}"), "type": "function",
                "function": { "name": "list_agents", "arguments": "{}" },
            })
        })
        .collect()
}

async fn harness(
    base_url: Option<String>,
    plugin_limit: u32,
    child_limit: u32,
    host_limit: u32,
) -> HarnessSession {
    let mut profile = crate::local_profile();
    for plugin in &mut profile.plugins {
        match plugin.id.as_str() {
            "agent-loop" => plugin.config = json!({ "max_tool_calls": plugin_limit }),
            "subagents" => plugin.config = json!({ "max_tool_calls": child_limit }),
            "model" => {
                if let Some(base_url) = &base_url {
                    plugin.kind = crate::model::KIND.to_owned();
                    plugin.config = json!({ "provider": "fixture", "base_url": base_url, "model": "fixture", "timeout_ms": 5000 });
                }
            }
            _ => {}
        }
    }
    let environment = HostEnvironment::memory(
        SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("test"),
            agent_id: AgentId::new("default"),
            session_id: SessionId::new("run-limits"),
        },
        None,
        HostPolicy::local(RunLimits {
            max_steps: 0,
            max_tool_calls: host_limit,
        }),
    );
    HarnessSession::boot(&crate::catalog().unwrap(), &profile, environment)
        .await
        .unwrap()
}

fn executed_results(request: &Value) -> usize {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| {
            message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| serde_json::from_str::<Vec<Value>>(content).is_ok())
        })
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_plugin_tool_limit_and_host_ceiling_bound_real_execution() {
    for (plugin_limit, host_limit, effective) in [(2, 0, 2), (0, 2, 2), (5, 2, 2), (1, 5, 1)] {
        let (base_url, request) = model_fixture_with_calls(calls(3)).await;
        let harness = harness(Some(base_url), plugin_limit, 0, host_limit).await;
        let error = harness
            .run(RunId::new("limited"), "execute the batch")
            .await
            .unwrap_err();
        assert!(
            error
                .message
                .contains(&format!("max_tool_calls ({effective})"))
        );
        assert_eq!(
            harness
                .events()
                .await
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::ToolCallStarted { .. }))
                .count(),
            effective
        );
        harness
            .run(RunId::new("continued"), "explain the limit")
            .await
            .unwrap();
        assert_eq!(executed_results(&request.await.unwrap()), effective);
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_agent_and_host_tool_limits_complete_more_than_the_default_plugin_budget() {
    let (base_url, request) = model_fixture_with_calls(calls(513)).await;
    let harness = harness(Some(base_url), 0, 0, 0).await;
    let outcome = harness
        .run(RunId::new("unrestricted"), "execute all calls")
        .await
        .unwrap();
    assert_eq!(outcome.tool_calls, 513);
    assert_eq!(executed_results(&request.await.unwrap()), 513);
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn zero_subagent_and_host_tool_limits_complete_more_than_the_default_plugin_budget() {
    let (base_url, request) = model_fixture_with_calls(calls(513)).await;
    let harness = harness(Some(base_url), 0, 0, 0).await;
    let child = harness
        .subagents()
        .unwrap()
        .spawn(
            RunId::new("parent"),
            "execute all calls".to_owned(),
            None,
            false,
            ActivityBranch::untracked(),
        )
        .await
        .unwrap();
    assert_eq!(child.status, SubagentStatus::Idle);
    assert_eq!(child.output.as_deref(), Some("continuation accepted"));
    assert_eq!(executed_results(&request.await.unwrap()), 513);
    harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn fallback_subagent_respects_the_positive_plugin_or_host_ceiling() {
    for (child_limit, host_limit, effective) in [(0, 2, 2), (2, 0, 2), (1, 5, 1)] {
        let (base_url, request) = model_fixture_with_calls(calls(3)).await;
        let harness = harness(Some(base_url), 0, child_limit, host_limit).await;
        let subagents = harness.subagents().unwrap();
        let child = subagents
            .spawn(
                RunId::new("parent"),
                "execute the batch".to_owned(),
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
                .contains(&format!("max_tool_calls ({effective})"))
        );
        subagents
            .followup(
                RunId::new("parent"),
                child.subagent_id.clone(),
                "explain the limit".to_owned(),
                None,
            )
            .await
            .unwrap();
        let continued = subagents
            .wait(child.subagent_id, 5000, ActivityBranch::untracked())
            .await
            .unwrap();
        assert_eq!(continued.status, SubagentStatus::Idle);
        assert_eq!(executed_results(&request.await.unwrap()), effective);
        harness.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_commands_run_when_either_or_both_tool_limits_are_zero() {
    for (plugin_limit, host_limit) in [(0, 0), (0, 1), (1, 0)] {
        let harness = harness(None, plugin_limit, 0, host_limit).await;
        let outcome = harness.run(RunId::new("direct"), "/agents").await.unwrap();
        assert_eq!(outcome.answer, "[]");
        assert_eq!(outcome.tool_calls, 1);
        assert!(
            !outcome
                .events
                .iter()
                .any(|event| matches!(event.kind, SessionEventKind::TurnFailed { .. }))
        );
        harness.shutdown().await.unwrap();
    }
}
