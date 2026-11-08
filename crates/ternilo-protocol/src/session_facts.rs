use crate::{InputProvenance, RunId, SubmissionId, UserMessageSource};

/// Lightweight committed facts; no message bodies or tool output are copied.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLogSummary {
    pub completed_turns: u64,
    pub latest_input_run: Option<RunId>,
    pub consumed_submissions: Vec<SubmissionId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionInputFacts {
    pub provenance: Option<InputProvenance>,
    pub source: Option<UserMessageSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRunFacts {
    pub first_seq: u64,
    pub last_seq: u64,
    pub terminal: bool,
    /// The first input establishes the run's authority; steering cannot replace it.
    pub input: Option<SessionInputFacts>,
}
