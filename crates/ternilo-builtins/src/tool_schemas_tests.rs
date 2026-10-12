use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use ternilo_kernel::{
    ActivityBranch, RunCancellation, ToolEffect, ToolExecutionContext, ToolGuard, ToolHandler,
    ToolPresentation, ToolPresenter, ToolRegistration, ToolsClient,
};
use ternilo_protocol::{HarnessError, RunId, ToolCall, ToolOutput, ToolSpec};

use super::source_tests::harness_with_registry_config;

struct CountCalls(Arc<AtomicUsize>);
impl ToolHandler for CountCalls {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        _: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput {
                content: "executed".to_owned(),
                is_error: false,
            })
        })
    }
}
fn registration(index: usize, kind: &str, calls: &Arc<AtomicUsize>) -> ToolRegistration {
    let properties = (0..24).map(|field| (format!("field_{field}"), json!({"type":kind,"description":"A detailed argument definition used to measure the actual model payload."}))).collect::<serde_json::Map<_,_>>();
    ToolRegistration {
        spec: ToolSpec {
            name: format!("catalog_{index:02}"),
            description: "Search project evidence".to_owned(),
            input_schema: json!({"type":"object","properties":properties}),
        },
        effect: ToolEffect::ReadOnly,
        handler: Arc::new(CountCalls(Arc::clone(calls))),
    }
}
async fn call(
    tools: &ToolsClient,
    name: &str,
    arguments: Value,
) -> Result<ToolOutput, HarnessError> {
    tools
        .execute(
            RunId::new("schema-test"),
            ToolCall {
                id: "schema-call".to_owned(),
                name: name.to_owned(),
                arguments,
                presentation: None,
            },
            RunCancellation::new(),
            ActivityBranch::untracked(),
        )
        .await
}
struct DenyCatalog;
impl ToolGuard for DenyCatalog {
    fn deny(&self, _: &ToolExecutionContext, call: &ToolCall) -> Option<String> {
        call.name
            .starts_with("catalog_")
            .then(|| "catalog access revoked".to_owned())
    }
}

#[tokio::test]
async fn discovery_reduces_payload_refreshes_schemas_and_preserves_execution_policy() {
    let (harness, tools, _) =
        harness_with_registry_config(json!({"schema_loading":{"enabled":true,"max_loaded":2}}))
            .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registrations = Vec::new();
    for index in 0..32 {
        registrations.push(
            tools
                .register_tool(registration(index, "string", &calls))
                .await
                .unwrap(),
        );
    }
    let full = tools.list().await;
    let initial = tools.present().await.unwrap();
    let full_bytes = serde_json::to_vec(&full).unwrap().len();
    let sent_bytes = serde_json::to_vec(&initial.tools).unwrap().len()
        + initial.system_prompt.as_deref().unwrap_or_default().len();
    assert!(
        sent_bytes < full_bytes / 2,
        "discovery payload {sent_bytes}, full definitions {full_bytes}"
    );
    assert!(initial.tools.iter().any(|tool| tool.name == "search_tools"));
    assert!(
        !initial
            .tools
            .iter()
            .any(|tool| tool.name.starts_with("catalog_"))
    );
    let loaded = call(
        &tools,
        "search_tools",
        json!({"names":["catalog_00","catalog_01"]}),
    )
    .await
    .unwrap();
    let loaded: Value = serde_json::from_str(&loaded.content).unwrap();
    assert_eq!(loaded["tools"].as_array().unwrap().len(), 2);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "schema discovery never executes the tools"
    );
    call(&tools, "search_tools", json!({"names":["catalog_02"]}))
        .await
        .unwrap();
    let visible = tools.present().await.unwrap();
    assert!(!visible.tools.iter().any(|tool| tool.name == "catalog_00"));
    assert!(visible.tools.iter().any(|tool| tool.name == "catalog_01"));
    tools.unregister_tool(registrations[1]).await.unwrap();
    tools
        .register_tool(registration(1, "integer", &calls))
        .await
        .unwrap();
    let updated = tools.present().await.unwrap();
    let spec = updated
        .tools
        .iter()
        .find(|tool| tool.name == "catalog_01")
        .unwrap();
    assert_eq!(
        spec.input_schema["properties"]["field_0"]["type"],
        "integer"
    );
    let guard = tools.register_guard(Arc::new(DenyCatalog)).await;
    let denied = call(&tools, "catalog_01", json!({})).await.unwrap_err();
    assert_eq!(denied.message, "catalog access revoked");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    tools.unregister_guard(guard).await.unwrap();
    assert_eq!(
        call(&tools, "catalog_01", json!({})).await.unwrap().content,
        "executed"
    );
    assert!(
        call(&tools, "search_tools", json!({"names":["missing_tool"]}))
            .await
            .is_err()
    );
    assert!(
        call(&tools, "search_tools", json!({"names":["update_goal"]}))
            .await
            .is_err()
    );
    harness.shutdown().await.unwrap();
}

struct CodeOnly;
impl ToolPresenter for CodeOnly {
    fn present<'a>(
        &'a self,
        _: Vec<ToolSpec>,
    ) -> Pin<Box<dyn Future<Output = Result<ToolPresentation, HarnessError>> + Send + 'a>> {
        Box::pin(async {
            let mut result = ToolPresentation::native(Vec::new());
            result.code_only = true;
            Ok(result)
        })
    }
}

#[tokio::test]
async fn discovery_preserves_explicit_commands_without_bypassing_code_only_mode() {
    let (harness, tools, _) =
        harness_with_registry_config(json!({"schema_loading":{"enabled":true}})).await;
    assert!(
        tools
            .present()
            .await
            .unwrap()
            .deferred_tools
            .contains("list_agents")
    );
    let outcome = harness
        .run(RunId::new("direct-agents"), "/agents")
        .await
        .unwrap();
    assert!(outcome.answer.contains("[]"), "{}", outcome.answer);
    tools.register_presenter(Arc::new(CodeOnly)).await.unwrap();
    let denied = harness
        .run(RunId::new("code-only-agents"), "/agents")
        .await
        .unwrap();
    assert!(denied.answer.contains("code-only"), "{}", denied.answer);
    harness.shutdown().await.unwrap();
}
