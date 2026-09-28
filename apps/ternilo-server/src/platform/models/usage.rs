use serde_json::Value;
use ternilo_control::ServiceModelUsage;
use ternilo_protocol::ProviderProtocol;

pub(crate) fn parse_usage(value: &Value, protocol: ProviderProtocol) -> Option<ServiceModelUsage> {
    let usage = value
        .get("usage")
        .or_else(|| value.get("usageMetadata"))
        .filter(|usage| usage.is_object())?;
    if protocol == ProviderProtocol::GoogleGemini {
        return Some(ServiceModelUsage {
            input_tokens: number(usage, "promptTokenCount"),
            output_tokens: number(usage, "candidatesTokenCount").map(|output| {
                output.saturating_add(number(usage, "thoughtsTokenCount").unwrap_or(0))
            }),
            cached_input_tokens: number(usage, "cachedContentTokenCount"),
            cache_write_tokens: None,
            reasoning_tokens: number(usage, "thoughtsTokenCount"),
            raw_usage: Some(usage.clone()),
        });
    }
    if protocol == ProviderProtocol::AnthropicMessages {
        let cached_input_tokens = number(usage, "cache_read_input_tokens");
        let cache_write_tokens = number(usage, "cache_creation_input_tokens");
        return Some(ServiceModelUsage {
            input_tokens: number(usage, "input_tokens").map(|input| {
                input
                    .saturating_add(cached_input_tokens.unwrap_or(0))
                    .saturating_add(cache_write_tokens.unwrap_or(0))
            }),
            output_tokens: number(usage, "output_tokens"),
            cached_input_tokens,
            cache_write_tokens,
            reasoning_tokens: None,
            raw_usage: Some(usage.clone()),
        });
    }
    let (input, output, input_details, output_details) = match protocol {
        ProviderProtocol::OpenAiChatCompletions => (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ),
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => (
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ),
        ProviderProtocol::GoogleGemini | ProviderProtocol::AnthropicMessages => {
            unreachable!("native usage handled above")
        }
    };
    let input_tokens = number(usage, input);
    let output_tokens = number(usage, output);
    let cached_input_tokens = usage
        .get(input_details)
        .and_then(|details| number(details, "cached_tokens"))
        .or_else(|| number(usage, "prompt_cache_hit_tokens"))
        .or_else(|| number(usage, "cache_read_input_tokens"));
    let cache_write_tokens = number(usage, "cache_creation_input_tokens")
        .or_else(|| number(usage, "cache_write_tokens"))
        .or_else(|| {
            usage
                .get(input_details)
                .and_then(|details| number(details, "cache_write_tokens"))
        });
    let reasoning_tokens = usage
        .get(output_details)
        .and_then(|details| number(details, "reasoning_tokens"))
        .or_else(|| number(usage, "reasoning_tokens"));
    Some(ServiceModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        cache_write_tokens,
        reasoning_tokens,
        raw_usage: Some(usage.clone()),
    })
}

fn number(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}
