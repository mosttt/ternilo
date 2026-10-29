//! Stable lifecycle control, independently versioned from task execution.
use crate::{ClientUpdateCommand, ExecutorId, ExecutorScope};
use serde::{Deserialize, Serialize};
use ternilo_protocol::{ClientUpdateInfo, HarnessError};

pub const MAINTENANCE_PROTOCOL_VERSION: u32 = 1;
pub const MAINTENANCE_POLL_MS: u64 = 3_000;
pub const MAINTENANCE_LEASE_MS: u64 = 30_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceHello {
    pub protocol_version: u32,
    pub executor_id: ExecutorId,
    pub instance_nonce: String,
    pub execution_protocol_version: u32,
    pub info: ClientUpdateInfo,
}
impl MaintenanceHello {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.protocol_version != MAINTENANCE_PROTOCOL_VERSION {
            return Err(HarnessError::invalid(
                "unsupported maintenance protocol; manually update Ternilo and Server",
            ));
        }
        self.executor_id.validate()?;
        if self.instance_nonce.is_empty() || self.instance_nonce.len() > 128 {
            return Err(HarnessError::invalid("invalid maintenance instance"));
        }
        self.info.validate()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MaintenanceRequest {
    Hello {
        hello: MaintenanceHello,
    },
    Poll {
        instance_nonce: String,
        info: ClientUpdateInfo,
        reply: Option<MaintenanceReply>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceReply {
    pub id: String,
    pub info: Option<ClientUpdateInfo>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceCommand {
    pub id: String,
    pub expires_at_ms: u64,
    pub command: ClientUpdateCommand,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaintenanceResponse {
    pub protocol_version: u32,
    pub scope: ExecutorScope,
    pub server_version: String,
    pub execution_protocol_version: u32,
    pub poll_after_ms: u64,
    pub command: Option<MaintenanceCommand>,
}
/// Persist only operation references, never a download credential.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaintenanceOperation {
    Status,
    Start { id: String },
    Cancel { id: String },
}
