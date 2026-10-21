use crate::HarnessError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientUpdatePhase {
    Preparing,
    Downloading,
    WaitingIdle,
    Installing,
    AwaitingReconnect,
    Completed,
    Failed,
    RolledBack,
    Cancelled,
}
impl ClientUpdatePhase {
    #[must_use]
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::RolledBack | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientUpdateProgress {
    pub id: String,
    pub version: String,
    pub phase: ClientUpdatePhase,
    pub revision: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientUpdateInfo {
    pub protocol_version: u32,
    pub version: String,
    pub target: Option<String>,
    pub enabled: bool,
    pub data_epoch: u32,
    pub restart_method: Option<String>,
    pub progress: Option<ClientUpdateProgress>,
}

impl ClientUpdateInfo {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.protocol_version == 0
            || self.version.len() > 64
            || self.target.as_ref().is_some_and(|value| value.len() > 80)
            || self
                .restart_method
                .as_ref()
                .is_some_and(|value| value.len() > 32)
            || self.progress.as_ref().is_some_and(|progress| {
                progress.id.is_empty()
                    || progress.id.len() > 64
                    || !progress
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    || progress.version.len() > 64
                    || progress
                        .error
                        .as_ref()
                        .is_some_and(|value| value.len() > 4096)
            })
        {
            return Err(HarnessError::invalid("invalid client update status"));
        }
        Ok(())
    }
}
