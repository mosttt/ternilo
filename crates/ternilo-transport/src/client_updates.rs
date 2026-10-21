use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientUpdateRequest {
    pub id: String,
    pub manifest: String,
    pub signature: String,
    pub server_version: String,
    pub download_token: String,
    pub expires_at_ms: u64,
}
impl std::fmt::Debug for ClientUpdateRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientUpdateRequest")
            .field("id", &self.id)
            .field("server_version", &self.server_version)
            .finish_non_exhaustive()
    }
}
impl ClientUpdateRequest {
    pub fn validate(&self) -> Result<(), HarnessError> {
        validate_update_id(&self.id)?;
        if self.manifest.len() > 128 * 1024
            || self.signature.len() > 128
            || self.server_version.len() > 64
            || self.download_token.is_empty()
            || self.download_token.len() > 256
            || self.expires_at_ms == 0
        {
            return Err(HarnessError::invalid("invalid client update request"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientUpdateCommand {
    Status,
    Start { request: ClientUpdateRequest },
    Cancel { id: String },
}
impl ClientUpdateCommand {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::Status => Ok(()),
            Self::Start { request } => request.validate(),
            Self::Cancel { id } => validate_update_id(id),
        }
    }
}
fn validate_update_id(id: &str) -> Result<(), HarnessError> {
    if id.len() < 16
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(HarnessError::invalid("invalid client update identifier"));
    }
    Ok(())
}
