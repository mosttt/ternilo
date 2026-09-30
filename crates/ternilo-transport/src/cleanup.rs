use serde::{Deserialize, Serialize};
use ternilo_protocol::{HarnessError, TenantId, UserId};

use crate::ExecutorId;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeInputAuthorization {
    pub credential_id: String,
    pub status_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAccountAuthorization {
    pub user_id: UserId,
    pub status_revision: u64,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeCleanupState {
    Pending,
    Confirmed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCleanupRequest {
    pub request_id: String,
    pub user_id: UserId,
    pub status_revision: u64,
    pub created_at_ms: u64,
    pub state: NodeCleanupState,
    pub detail: Option<String>,
    pub confirmed_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCleanupReceipt {
    pub storage_instance_id: String,
    pub request_id: String,
    pub status_revision: u64,
    pub state: NodeCleanupState,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeCleanupSnapshot {
    pub protocol_version: u32,
    pub server_id: String,
    pub tenant_id: TenantId,
    pub executor_id: ExecutorId,
    pub credential_id: String,
    pub authorizations: Vec<NodeAccountAuthorization>,
    pub connection_allowed: bool,
    pub requests: Vec<NodeCleanupRequest>,
}

impl NodeCleanupSnapshot {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.protocol_version != 1 {
            return Err(HarnessError::invalid("unsupported Node cleanup protocol"));
        }
        UserId::new(&self.server_id).validate()?;
        self.tenant_id.validate()?;
        self.executor_id.validate()?;
        crate::require_text(&self.credential_id, "Node credential instance")?;
        for account in &self.authorizations {
            account.user_id.validate()?;
        }
        for request in &self.requests {
            crate::require_text(&request.request_id, "Node cleanup request")?;
            request.user_id.validate()?;
        }
        Ok(())
    }
}
