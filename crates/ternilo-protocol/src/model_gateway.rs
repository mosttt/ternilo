use crate::{
    HarnessError, InputProvenance, ModelRequest, ModelResponse, ModelRetryFailure, RunId,
    RunModelBinding, SessionId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
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
