use serde_json::{Value, json};
use ternilo_protocol::{HostedWebTools, ProviderProtocol};

/// Apply the provider owner's policy only to a supported protocol and model.
pub fn apply_hosted_web_tools(
    body: &mut Value,
    settings: &HostedWebTools,
    protocol: ProviderProtocol,
    model: &str,
) {
    if !settings.applies_to(protocol, model)
        || body["tool_choice"] == "none"
        || body
            .pointer("/tool_choice/type")
            .is_some_and(|value| value == "none")
    {
        return;
    }
    let mut tools = body
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    tools.retain(|tool| {
        !((settings.web_search && tool["name"] == "web_search")
            || (settings.web_fetch && tool["name"] == "web_fetch"))
    });
    if protocol == ProviderProtocol::DeepSeekResponses {
        tools.push(json!({"type":"web_search"}));
        body["tools"] = json!(tools);
        return;
    }
    for (enabled, name, version) in [
        (settings.web_search, "web_search", "web_search_20250305"),
        (settings.web_fetch, "web_fetch", "web_fetch_20250910"),
    ] {
        if !enabled {
            continue;
        }
        let mut tool = json!({"type":version,"name":name,"max_uses":settings.max_uses});
        if !settings.allowed_domains.is_empty() {
            tool["allowed_domains"] = json!(settings.allowed_domains);
        }
        if !settings.blocked_domains.is_empty() {
            tool["blocked_domains"] = json!(settings.blocked_domains);
        }
        if name == "web_fetch" {
            tool["citations"] = json!({"enabled":true});
            tool["max_content_tokens"] = json!(settings.max_content_tokens);
        }
        tools.push(tool);
    }
    body["tools"] = json!(tools);
}

pub(crate) fn apply_request_hosted_tools(
    body: &mut Value,
    settings: &HostedWebTools,
    protocol: ProviderProtocol,
    model: &str,
    request: &ternilo_protocol::ModelRequest,
) {
    let mut settings = settings.clone();
    if request
        .blocked_web_tools
        .contains(&ternilo_protocol::WebCapability::Search)
    {
        settings.web_search = false;
    }
    if request
        .blocked_web_tools
        .contains(&ternilo_protocol::WebCapability::Fetch)
    {
        settings.web_fetch = false;
    }
    apply_hosted_web_tools(body, &settings, protocol, model);
}
