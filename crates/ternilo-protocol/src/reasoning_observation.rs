use crate::{ReasoningEffort, ReasoningSelection};
use serde::{Deserialize, Serialize};

/// A deterministic recommendation, optionally paired with the host's actual selection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ReasoningSelection>,
    pub recommended: ReasoningEffort,
    pub task_chars: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
}
