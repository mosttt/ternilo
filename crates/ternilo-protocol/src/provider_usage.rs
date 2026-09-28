use crate::ProviderProtocol;
use serde::{Deserialize, Serialize};

/// Device-observed counters. Missing values remain unknown rather than becoming zero.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsageRoute {
    pub provider: String,
    pub model: String,
    pub protocol: ProviderProtocol,
}
