use serde::{Deserialize, Serialize};

use crate::{HarnessError, HostedWebTools, ProviderProfile, ProviderProtocol};

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    #[default]
    Session,
    Disabled,
    Hosted,
    Deepseek,
    Brave,
    Tavily,
    Searxng,
}

#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WebFetchMode {
    #[default]
    Session,
    Disabled,
    Hosted,
    Direct,
    Deepseek,
}

const fn timeout() -> u64 {
    30_000
}
const fn results() -> u32 {
    10
}
const fn bytes() -> u64 {
    2 * 1024 * 1024
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSearch {
    pub mode: WebSearchMode,
    pub base_url: Option<String>,
    pub api_key_ref: Option<String>,
    pub timeout_ms: u64,
    pub max_results: u32,
}
impl Default for ProviderSearch {
    fn default() -> Self {
        Self {
            mode: WebSearchMode::Session,
            base_url: None,
            api_key_ref: None,
            timeout_ms: timeout(),
            max_results: results(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderFetch {
    pub mode: WebFetchMode,
    pub base_url: Option<String>,
    pub api_key_ref: Option<String>,
    pub timeout_ms: u64,
    pub max_bytes: u64,
}
impl Default for ProviderFetch {
    fn default() -> Self {
        Self {
            mode: WebFetchMode::Session,
            base_url: None,
            api_key_ref: None,
            timeout_ms: timeout(),
            max_bytes: bytes(),
        }
    }
}

/// Connection settings only. Secret values never belong in profiles or session snapshots.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderWebAccess {
    pub search: ProviderSearch,
    pub fetch: ProviderFetch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebCapability {
    Search,
    Fetch,
}

impl ProviderWebAccess {
    #[must_use]
    pub fn uses_computer(&self) -> bool {
        matches!(
            self.search.mode,
            WebSearchMode::Deepseek
                | WebSearchMode::Brave
                | WebSearchMode::Tavily
                | WebSearchMode::Searxng
        ) || matches!(
            self.fetch.mode,
            WebFetchMode::Deepseek | WebFetchMode::Direct
        )
    }

    pub fn validate(&self, protocol: ProviderProtocol) -> Result<(), HarnessError> {
        validate_connection(
            self.search.base_url.as_deref(),
            self.search.api_key_ref.as_deref(),
        )?;
        validate_connection(
            self.fetch.base_url.as_deref(),
            self.fetch.api_key_ref.as_deref(),
        )?;
        let maximum = if matches!(
            self.search.mode,
            WebSearchMode::Brave | WebSearchMode::Tavily
        ) {
            20
        } else {
            50
        };
        if self.search.timeout_ms == 0
            || !(1..=maximum).contains(&self.search.max_results)
            || self.fetch.timeout_ms == 0
            || self.fetch.max_bytes == 0
        {
            return Err(HarnessError::invalid(
                "web access requires positive timeout/size limits and a supported result count",
            ));
        }
        if matches!(
            self.search.mode,
            WebSearchMode::Deepseek | WebSearchMode::Brave | WebSearchMode::Tavily
        ) && self.search.api_key_ref.is_none()
            || self.fetch.mode == WebFetchMode::Deepseek && self.fetch.api_key_ref.is_none()
        {
            return Err(HarnessError::invalid(
                "configure a separate web credential for computer-executed services",
            ));
        }
        if self.search.mode == WebSearchMode::Searxng && self.search.base_url.is_none() {
            return Err(HarnessError::invalid("SearXNG requires a service address"));
        }
        if self.search.mode == WebSearchMode::Hosted
            && !matches!(
                protocol,
                ProviderProtocol::AnthropicMessages | ProviderProtocol::DeepSeekResponses
            )
            || self.fetch.mode == WebFetchMode::Hosted
                && protocol != ProviderProtocol::AnthropicMessages
        {
            return Err(HarnessError::invalid(
                "this protocol does not support the selected hosted web tool",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn credential(&self, capability: WebCapability) -> Option<&str> {
        match capability {
            WebCapability::Search
                if matches!(
                    self.search.mode,
                    WebSearchMode::Deepseek
                        | WebSearchMode::Brave
                        | WebSearchMode::Tavily
                        | WebSearchMode::Searxng
                ) =>
            {
                self.search.api_key_ref.as_deref()
            }
            WebCapability::Fetch if self.fetch.mode == WebFetchMode::Deepseek => {
                self.fetch.api_key_ref.as_deref()
            }
            _ => None,
        }
    }

    pub fn validate_model(
        &self,
        protocol: ProviderProtocol,
        model: &str,
    ) -> Result<(), HarnessError> {
        if self.search.mode == WebSearchMode::Hosted
            && protocol == ProviderProtocol::DeepSeekResponses
            && model != "deepseek-v4-pro"
        {
            return Err(HarnessError::invalid(
                "this model does not support the Provider's built-in search; choose a supported model or computer-executed search",
            ));
        }
        Ok(())
    }
}

fn validate_connection(url: Option<&str>, reference: Option<&str>) -> Result<(), HarnessError> {
    if url.is_some_and(|url| {
        !(url.starts_with("https://") || url.starts_with("http://"))
            || url.chars().any(char::is_control)
    }) {
        return Err(HarnessError::invalid(
            "web service address must use HTTP(S)",
        ));
    }
    if reference.is_some_and(|reference| !crate::valid_credential_reference(reference)) {
        return Err(HarnessError::invalid("invalid web credential reference"));
    }
    Ok(())
}

impl ProviderProfile {
    #[must_use]
    pub fn effective_hosted_tools(&self) -> Option<HostedWebTools> {
        let Some(web) = &self.web_access else {
            return self.hosted_tools.clone();
        };
        let search = web.search.mode == WebSearchMode::Hosted;
        let fetch = web.fetch.mode == WebFetchMode::Hosted;
        if !search && !fetch {
            return None;
        }
        let deepseek = self.protocol == ProviderProtocol::DeepSeekResponses;
        let mut tools = self.hosted_tools.clone().unwrap_or(HostedWebTools {
            web_search: search,
            web_fetch: fetch,
            max_uses: if deepseek { None } else { Some(5) },
            max_content_tokens: if deepseek { None } else { Some(20000) },
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
        });
        tools.web_search = search;
        tools.web_fetch = fetch;
        Some(tools)
    }
}

/// Run identity shared by model calls and execution-host web credential requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRunContext {
    pub session_id: crate::SessionId,
    pub origin_session_id: crate::SessionId,
    pub run_id: crate::RunId,
    pub provenance: Option<crate::InputProvenance>,
    #[serde(default)]
    pub schedule_origins: Vec<crate::ScheduleModelOrigin>,
    pub binding: crate::RunModelBinding,
}
impl From<&crate::NodeModelRequest> for NodeRunContext {
    fn from(body: &crate::NodeModelRequest) -> Self {
        Self {
            session_id: body.session_id.clone(),
            origin_session_id: body.origin_session_id.clone(),
            run_id: body.run_id.clone(),
            provenance: body.provenance.clone(),
            schedule_origins: body.schedule_origins.clone(),
            binding: body.binding.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeWebAccessRequest {
    pub session_id: crate::SessionId,
    pub binding: crate::RunModelBinding,
    pub invocation: Option<NodeRunContext>,
    pub credential: Option<WebCapability>,
    pub expected: Option<Box<ProviderWebAccess>>,
}
/// Only the authenticated internal transport may return this value; never store it in history.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderWebAccessReply {
    pub configuration: Option<Box<ProviderWebAccess>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_services_do_not_inherit_model_credentials_or_protocol() {
        let mut web = ProviderWebAccess::default();
        web.search.mode = WebSearchMode::Deepseek;
        assert!(web.validate(ProviderProtocol::OpenAiResponses).is_err());
        web.search.api_key_ref = Some("SEARCH_ONLY".into());
        web.validate(ProviderProtocol::OpenAiResponses).unwrap();
        assert_eq!(web.credential(WebCapability::Search), Some("SEARCH_ONLY"));
        assert_eq!(web.credential(WebCapability::Fetch), None);
        web.search.mode = WebSearchMode::Brave;
        web.search.max_results = 21;
        assert!(web.validate(ProviderProtocol::OpenAiResponses).is_err());
    }
    #[test]
    fn explicit_hosted_choice_requires_support_and_never_falls_back() {
        let mut web = ProviderWebAccess::default();
        web.search.mode = WebSearchMode::Hosted;
        assert!(web.validate(ProviderProtocol::OpenAiResponses).is_err());
        web.validate(ProviderProtocol::DeepSeekResponses).unwrap();
        web.validate_model(ProviderProtocol::DeepSeekResponses, "deepseek-v4-pro")
            .unwrap();
        assert!(
            web.validate_model(ProviderProtocol::DeepSeekResponses, "deepseek-flash")
                .is_err()
        );
        web.fetch.mode = WebFetchMode::Hosted;
        assert!(web.validate(ProviderProtocol::DeepSeekResponses).is_err());
        web.validate(ProviderProtocol::AnthropicMessages).unwrap();
    }
}
