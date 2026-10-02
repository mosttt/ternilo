use crate::{
    HarnessError, InputProvenance, ModelRequest, ModelResponse, ModelRetryFailure, RunId,
    RunModelBinding, SessionId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelGatewayFrame {
    Delta {
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    RetryScheduled {
        retry: u32,
        max_retries: u32,
        delay_ms: u64,
        failure: ModelRetryFailure,
    },
    RetryStarted {
        retry: u32,
    },
    RetryCancelled {
        retry: u32,
    },
    Complete {
        response: ModelResponse,
    },
    Error {
        error: HarnessError,
    },
}

impl ModelGatewayFrame {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Error { .. })
    }
}

/// A model invocation on its source computer. Endpoints and credentials are
/// resolved on that computer and never supplied by the requesting computer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputerModelRequest {
    pub provider_id: String,
    pub model: String,
    pub protocol: crate::ProviderProtocol,
    pub defaults: crate::ProviderModelDefaults,
    pub reasoning_effort: Option<crate::ReasoningEffort>,
    pub max_attempts: u32,
    pub request: ModelRequest,
}

impl ComputerModelRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if !crate::valid_provider_id(&self.provider_id) {
            return Err(HarnessError::invalid(
                "invalid computer Provider identifier",
            ));
        }
        crate::model_binding::validate_reference(&self.model, "computer model", 200)?;
        crate::validate_model_defaults(&self.defaults, "computer model request")?;
        self.request.run_id.validate()?;
        if !(1..=8).contains(&self.max_attempts) {
            return Err(HarnessError::invalid(
                "computer model requires 1 to 8 attempts",
            ));
        }
        self.resolved_model()
            .reasoning_value(self.reasoning_effort)?;
        Ok(())
    }

    #[must_use]
    pub fn resolved_model(&self) -> crate::ResolvedProviderModel {
        crate::ResolvedProviderModel {
            id: self.model.clone(),
            display_name: None,
            context_window: self.defaults.context_window,
            max_output_tokens: self.defaults.max_output_tokens,
            reasoning: self.defaults.reasoning.clone(),
        }
    }
}

/// Usage is reported by the source computer, not observed by Server itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComputerModelAttempt {
    Started {
        attempt: u32,
    },
    Finished {
        attempt: u32,
        http_status: Option<u16>,
        usage: Option<crate::ReportedModelUsage>,
        upstream_request_id: Option<String>,
        error_code: Option<crate::ErrorCode>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeModelRequest {
    pub session_id: SessionId,
    pub origin_session_id: SessionId,
    pub run_id: RunId,
    pub provenance: Option<InputProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedule_origins: Vec<ScheduleModelOrigin>,
    pub request_id: String,
    pub binding: RunModelBinding,
    pub request: ModelRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleModelOrigin {
    pub session_id: SessionId,
    pub created_seq: u64,
    pub dispatched_seq: u64,
}

impl ScheduleModelOrigin {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.session_id.validate()?;
        if self.created_seq >= self.dispatched_seq || self.dispatched_seq > i64::MAX as u64 {
            return Err(HarnessError::policy(
                "invalid schedule origin event sequence",
            ));
        }
        Ok(())
    }
}
