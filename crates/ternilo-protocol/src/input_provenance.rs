use serde::{Deserialize, Serialize};

use crate::{HarnessError, RunId, SubmissionId, UserId};

/// The author established by the trusted input entry point, independent of billing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputAuthor {
    Account { user_id: UserId, username: String },
    Local,
    Automation { source: AutomatedInputSource },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomatedInputSource {
    Schedule,
    Subagent,
}

impl InputAuthor {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if let Self::Account { user_id, username } = self {
            user_id.validate()?;
            if !(3..=64).contains(&username.len())
                || !username.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-' | b'_')
                })
            {
                return Err(HarnessError::invalid(
                    "input author must contain a canonical platform username",
                ));
            }
        }
        Ok(())
    }
}

/// A stable input identity lets the receiver verify authors against accepted inputs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputProvenance {
    pub input_id: SubmissionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub author: InputAuthor,
}

impl InputProvenance {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.input_id.validate()?;
        if let Some(run_id) = &self.run_id {
            run_id.validate()?;
        }
        self.author.validate()
    }
}
