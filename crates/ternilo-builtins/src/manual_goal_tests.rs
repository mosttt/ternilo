use std::{future::Future, pin::Pin, sync::Arc};

use linorun_core::{Activation, CallContext, ComponentContext, ComponentDescriptor};
use linorun_macros::component_descriptor;
use serde_json::json;
use ternilo_kernel::{
    HarnessPlugin, HarnessSession, HostEnvironment, HostPolicy, ModelOutput, Models,
    ModelsProvider, PluginManifest, RunCancellation,
};
use ternilo_protocol::{
    AgentId, GoalStatus, HarnessError, MessageRole, ModelFinishReason, ModelRequest, ModelResponse,
    RunId, RunLimits, SessionEventKind, SessionId, SessionIdentity, TenantId, ToolCall, UserId,
};

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/test-manual-goal-model@1",
        requires: [],
        provides: [Models],
    }
}

struct GoalModel;

impl HarnessPlugin for GoalModel {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let route = context.context().clone();
        let scope = context.scope().clone();
        let model: Arc<dyn ModelsProvider> = Arc::new(Self);
        Activation::Once(Box::pin(async move {
            scope
                .provide::<Models>(&route, model)
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(None)
        }))
    }
}

impl ModelsProvider for GoalModel {
    fn context_window<'a>(
        &'a self,
        _: CallContext<()>,
    ) -> Pin<Box<dyn Future<Output = Option<u64>> + Send + 'a>> {
        Box::pin(async { None })
    }

    fn complete<'a>(
        &'a self,
        _: CallContext<()>,
        request: ModelRequest,
        _: Arc<dyn ModelOutput>,
        cancellation: RunCancellation,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            cancellation.check()?;
            let enabled = request.tools.iter().any(|tool| tool.name == "update_goal");
            let finished = request
                .messages
                .last()
                .is_some_and(|message| message.role == MessageRole::Tool);
            Ok(ModelResponse {
                provider: "fixture".into(),
                model: "goal-attempt".into(),
                content: if finished {
                    "answer".into()
                } else {
                    String::new()
                },
                reasoning_content: None,
                provider_state: None,
                tool_calls: if finished {
                    Vec::new()
                } else {
                    vec![ToolCall {
                        id: "model-goal".into(),
                        name: "update_goal".into(),
                        arguments: json!({"objective":"user goal", "status":if enabled { "complete" } else { "active" }}),
                        presentation: None,
                    }]
                },
                usage: None,
                finish_reason: if finished {
                    ModelFinishReason::Stop
                } else {
                    ModelFinishReason::ToolCalls
                },
                provider_request_id: None,
                attempts: 1,
                request_digest: None,
                replayed: false,
            })
        })
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep ordinary model attempts, explicit user enablement, completion and rejected reactivation in one Harness lifecycle."
)]
async fn model_cannot_start_goals_but_can_finish_a_manually_enabled_goal() {
    let mut catalog = crate::catalog().unwrap();
    catalog
        .register(crate::factory(
            PluginManifest {
                kind: "ternilo.test.goal-model",
                requires: &[],
                provides: &["ternilo/models@3"],
            },
            |_| Ok(Arc::new(GoalModel)),
        ))
        .unwrap();
    let mut profile = crate::local_profile();
    profile
        .plugins
        .iter_mut()
        .find(|plugin| plugin.id == "model")
        .unwrap()
        .kind = "ternilo.test.goal-model".into();
    profile.plugins.push(crate::entry(
        "planning-tools",
        crate::PLAN_TOOL_KIND,
        json!({}),
    ));
    let environment = HostEnvironment::memory(
        SessionIdentity {
            tenant_id: TenantId::new("local"),
            user_id: UserId::new("owner"),
            agent_id: AgentId::new("default"),
            session_id: SessionId::new("manual-goal"),
        },
        None,
        HostPolicy::local(RunLimits::default()),
    );
    let harness = HarnessSession::boot(&catalog, &profile, environment)
        .await
        .unwrap();
    assert!(
        harness
            .command_catalog()
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.descriptor.name == "goal")
    );
    assert!(
        !harness
            .tool_catalog()
            .await
            .unwrap()
            .iter()
            .any(|tool| tool.name == "update_goal")
    );
    let ordinary = harness
        .run(RunId::new("ordinary"), "inspect files")
        .await
        .unwrap();
    assert!(
        !ordinary
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::GoalUpdated { .. }))
    );
    assert!(ordinary.events.iter().any(|event| matches!(&event.kind, SessionEventKind::ToolCallFinished { output, .. } if output.is_error)));
    harness
        .run(RunId::new("manual"), "/goal edit user goal")
        .await
        .unwrap();
    assert!(
        harness
            .tool_catalog()
            .await
            .unwrap()
            .iter()
            .any(|tool| tool.name == "update_goal")
    );
    let completion = harness
        .run(RunId::new("finish"), "finish the goal")
        .await
        .unwrap();
    assert!(completion.events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::GoalUpdated {
            status: GoalStatus::Complete,
            ..
        }
    )));
    assert!(
        !harness
            .tool_catalog()
            .await
            .unwrap()
            .iter()
            .any(|tool| tool.name == "update_goal")
    );
    let next = harness
        .run(RunId::new("next"), "another ordinary question")
        .await
        .unwrap();
    assert!(
        !next
            .events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::GoalUpdated { .. }))
    );
    harness.shutdown().await.unwrap();
}
