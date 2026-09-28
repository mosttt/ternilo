use serde_json::Value;
use ternilo_control::ServiceModelUsage;
use ternilo_protocol::ProviderProtocol;

pub(crate) fn parse_usage(value: &Value, protocol: ProviderProtocol) -> Option<ServiceModelUsage> {
    let raw = value.get("usage").or_else(|| value.get("usageMetadata"))?;
    let usage = ternilo_builtins::normalize_provider_usage(raw, protocol)?;
    Some(ServiceModelUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        raw_usage: Some(raw.clone()),
    })
}
