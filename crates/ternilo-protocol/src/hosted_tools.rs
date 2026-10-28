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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_content_tokens: Option<u32>,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
}

impl HostedWebTools {
    /// Only explicitly supported model/protocol pairs receive hosted tools.
    #[must_use]
    pub fn applies_to(&self, protocol: ProviderProtocol, model: &str) -> bool {
        match protocol {
            ProviderProtocol::AnthropicMessages => self.web_search || self.web_fetch,
            ProviderProtocol::DeepSeekResponses => self.web_search && model == "deepseek-v4-pro",
            _ => false,
        }
    }

    pub fn validate(&self, protocol: ProviderProtocol) -> Result<(), HarnessError> {
        if protocol == ProviderProtocol::DeepSeekResponses {
            if !self.web_search
                || self.web_fetch
                || self.max_uses.is_some()
                || self.max_content_tokens.is_some()
                || !self.allowed_domains.is_empty()
                || !self.blocked_domains.is_empty()
            {
                return Err(HarnessError::invalid(
                    "DeepSeek Responses hosted tools support only web_search, without fetch, call limits, content limits or domain filters",
                ));
            }
            return Ok(());
        }
        if protocol != ProviderProtocol::AnthropicMessages {
            return Err(HarnessError::invalid(
                "hosted web tools require Claude Messages or DeepSeek Responses",
            ));
        }
        if !self.web_search && !self.web_fetch {
            return Err(HarnessError::invalid("select at least one hosted web tool"));
        }
        if !self.max_uses.is_some_and(|uses| (1..=20).contains(&uses))
            || self.max_content_tokens.is_none_or(|tokens| tokens == 0)
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocols_accept_only_their_actual_controls_and_models() {
        let deepseek: HostedWebTools = serde_json::from_value(json!({"web_search":true})).unwrap();
        deepseek
            .validate(ProviderProtocol::DeepSeekResponses)
            .unwrap();
        assert!(deepseek.applies_to(ProviderProtocol::DeepSeekResponses, "deepseek-v4-pro"));
        assert!(!deepseek.applies_to(ProviderProtocol::DeepSeekResponses, "deepseek-flash"));
        assert!(!deepseek.applies_to(ProviderProtocol::OpenAiResponses, "deepseek-v4-pro"));
        assert!(
            deepseek
                .validate(ProviderProtocol::AnthropicMessages)
                .is_err()
        );
        for extra in [
            json!({"web_fetch":true}),
            json!({"max_uses":1}),
            json!({"max_content_tokens":1}),
            json!({"allowed_domains":["example.com"]}),
        ] {
            let mut value = json!({"web_search":true});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(
                serde_json::from_value::<HostedWebTools>(value)
                    .unwrap()
                    .validate(ProviderProtocol::DeepSeekResponses)
                    .is_err()
            );
        }
        let claude: HostedWebTools = serde_json::from_value(json!({"web_search":true,"web_fetch":true,"max_uses":2,"max_content_tokens":1000,"allowed_domains":["example.com"]})).unwrap();
        claude
            .validate(ProviderProtocol::AnthropicMessages)
            .unwrap();
        assert!(
            claude
                .validate(ProviderProtocol::DeepSeekResponses)
                .is_err()
        );
        assert_eq!(serde_json::to_value(&claude).unwrap()["max_uses"], 2);
    }
}
