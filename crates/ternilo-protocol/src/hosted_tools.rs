use serde::{Deserialize, Serialize};

use crate::{HarnessError, ProviderProtocol};

/// Web tools executed by the model provider, enabled only by its owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostedWebTools {
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub web_fetch: bool,
    pub max_uses: u32,
    pub max_content_tokens: u32,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

impl HostedWebTools {
    pub fn validate(&self, protocol: ProviderProtocol) -> Result<(), HarnessError> {
        if protocol != ProviderProtocol::AnthropicMessages {
            return Err(HarnessError::invalid(
                "hosted web tools require the Claude Messages protocol",
            ));
        }
        if !self.web_search && !self.web_fetch {
            return Err(HarnessError::invalid("select at least one hosted web tool"));
        }
        if !(1..=20).contains(&self.max_uses) || self.max_content_tokens == 0 {
            return Err(HarnessError::invalid(
                "hosted web tools require max_uses between 1 and 20 and positive max_content_tokens",
            ));
        }
        if !self.allowed_domains.is_empty() && !self.blocked_domains.is_empty() {
            return Err(HarnessError::invalid(
                "choose either allowed_domains or blocked_domains for hosted web tools",
            ));
        }
        for domain in self.allowed_domains.iter().chain(&self.blocked_domains) {
            if domain.is_empty()
                || domain.starts_with('/')
                || domain.contains("://")
                || domain
                    .chars()
                    .any(|ch| ch.is_whitespace() || ch.is_control())
            {
                return Err(HarnessError::invalid(
                    "hosted web tool domains require a bare domain and optional path, without a scheme",
                ));
            }
        }
        Ok(())
    }
}
