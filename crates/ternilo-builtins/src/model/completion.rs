//! Normalized model output assembled by protocol adapters.

use super::protocols;
use std::collections::BTreeMap;
use ternilo_protocol::{
    HarnessError, ModelFinishReason, ModelProviderState, ModelResponse, ModelUsage,
    ProviderProtocol, ToolCall,
};

pub(super) enum StreamEvent {
    Deltas { reasoning: String, text: String },
    Done,
    Metadata,
}

#[derive(Default)]
pub(super) struct StreamCompletion {
    pub(super) deepseek_reasoning: protocols::deepseek_responses::ReasoningState,
    pub(super) content: String,
    pub(super) reasoning_content: String,
    pub(super) tool_calls: BTreeMap<usize, PendingToolCall>,
    pub(super) usage: Option<ModelUsage>,
    pub(super) finish_reason: Option<ModelFinishReason>,
    pub(super) native_protocol: Option<ProviderProtocol>,
    pub(super) native_blocks: BTreeMap<usize, serde_json::Value>,
}

impl StreamCompletion {
    pub(super) fn finish(self, provider: &str, model: &str) -> Result<ModelResponse, HarnessError> {
        let tool_calls = self
            .tool_calls
            .into_values()
            .map(|call| {
                if call.id.is_empty() || call.name.is_empty() {
                    return Err(HarnessError::execution(
                        "streaming model returned an incomplete tool call",
                    ));
                }
                let arguments = serde_json::from_str(&call.arguments).map_err(|error| {
                    HarnessError::execution(format!(
                        "model tool arguments for {:?} are not JSON: {error}",
                        call.name
                    ))
                })?;
                Ok(ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments,
                    presentation: None,
                })
            })
            .collect::<Result<Vec<_>, HarnessError>>()?;
        let finish_reason = self.finish_reason.unwrap_or(if tool_calls.is_empty() {
            ModelFinishReason::Stop
        } else {
            ModelFinishReason::ToolCalls
        });
        Ok(ModelResponse {
            provider: provider.to_owned(),
            model: model.to_owned(),
            content: self.content,
            reasoning_content: (!self.reasoning_content.is_empty())
                .then_some(self.reasoning_content),
            provider_state: self.native_protocol.map(|protocol| {
                Box::new(ModelProviderState {
                    protocol,
                    model: model.to_owned(),
                    blocks: self.native_blocks.into_values().collect(),
                })
            }),
            tool_calls,
            usage: self.usage,
            finish_reason,
            provider_request_id: None,
            attempts: 1,
            request_digest: None,
            replayed: false,
        })
    }
}

#[derive(Default)]
pub(super) struct PendingToolCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) arguments: String,
}
