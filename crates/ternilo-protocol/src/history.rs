use crate::{HarnessError, SessionEvent};
use serde::{Deserialize, Serialize};

/// An exclusive backwards cursor over the immutable canonical event sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionHistoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

const fn default_limit() -> u32 {
    200
}

impl Default for SessionHistoryQuery {
    fn default() -> Self {
        Self {
            before_seq: None,
            limit: default_limit(),
        }
    }
}

impl SessionHistoryQuery {
    pub fn validate(self) -> Result<(), HarnessError> {
        if !(1..=1_000).contains(&self.limit) {
            return Err(HarnessError::invalid(
                "history limit must be between 1 and 1000",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEventPage {
    /// Events are always returned in ascending sequence order.
    pub events: Vec<SessionEvent>,
    pub next_before_seq: Option<u64>,
}

impl SessionEventPage {
    #[must_use]
    pub fn new(events: Vec<SessionEvent>) -> Self {
        let next_before_seq = events.first().map(|event| event.seq).filter(|seq| *seq > 0);
        Self {
            events,
            next_before_seq,
        }
    }
}
