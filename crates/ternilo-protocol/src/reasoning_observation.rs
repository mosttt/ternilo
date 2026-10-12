use crate::ReasoningEffort;
use serde::{Deserialize, Serialize};

/// A deterministic recommendation for evaluation; it never changes provider settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningObservation {
    pub recommended: ReasoningEffort,
    pub task_chars: u64,
    pub tool_calls: u64,
    pub tool_errors: u64,
}
