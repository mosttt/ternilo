use serde_json::{Value, json};
use ternilo_protocol::{HarnessError, ProviderProtocol};

pub fn provider_request(
    mut request: reqwest::RequestBuilder,
    protocol: ProviderProtocol,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    if protocol == ProviderProtocol::AnthropicMessages {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some(api_key) = api_key {
        request = match protocol {
            ProviderProtocol::GoogleGemini => request.header("x-goog-api-key", api_key),
            ProviderProtocol::AnthropicMessages => request.header("x-api-key", api_key),
            _ => request.bearer_auth(api_key),
        };
    }
    request
}

pub fn provider_model_endpoint(
    base_url: &str,
    protocol: ProviderProtocol,
    model: &str,
    stream: bool,
) -> Result<String, HarnessError> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|_| HarnessError::invalid("provider base_url must be an HTTP(S) URL"))?;
    let mut path = url
        .path_segments_mut()
        .map_err(|()| HarnessError::invalid("provider base_url cannot be a base URL"))?;
    path.pop_if_empty();
    match protocol {
        ProviderProtocol::OpenAiChatCompletions => {
            path.push("chat").push("completions");
        }
        ProviderProtocol::OpenAiResponses | ProviderProtocol::DeepSeekResponses => {
            path.push("responses");
        }
        ProviderProtocol::AnthropicMessages => {
            path.push("messages");
        }
        ProviderProtocol::GoogleGemini => {
            let model = model.strip_prefix("models/").unwrap_or(model);
            let method = if stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            };
            path.push("models").push(&format!("{model}:{method}"));
        }
    }
    drop(path);
    if stream && protocol == ProviderProtocol::GoogleGemini {
        url.query_pairs_mut().append_pair("alt", "sse");
    }
    Ok(url.into())
}

pub fn apply_native_reasoning(
    body: &mut Value,
    protocol: ProviderProtocol,
    effort: Option<&str>,
    max_tokens: u64,
) -> Result<(), HarnessError> {
    let Some(effort) = effort else {
        return Ok(());
    };
    protocol.validate_reasoning_value(effort, max_tokens)?;
    match protocol {
        ProviderProtocol::GoogleGemini => {
            let thinking = if matches!(effort, "minimal" | "low" | "medium" | "high") {
                json!({"thinkingLevel": effort, "includeThoughts": true})
            } else if effort == "none" {
                json!({"thinkingBudget": 0})
            } else {
                let budget = effort.parse::<i32>().ok().filter(|value| *value >= -1)
                    .ok_or_else(|| HarnessError::invalid("Gemini reasoning must map to minimal/low/medium/high or a thinking token budget (-1 for automatic, 0 for disabled)"))?;
                json!({"thinkingBudget": budget, "includeThoughts": budget != 0})
            };
            body["generationConfig"]["thinkingConfig"] = thinking;
        }
        ProviderProtocol::AnthropicMessages => {
            if matches!(
                effort,
                "low" | "medium" | "high" | "xhigh" | "max" | "adaptive"
            ) {
                body["thinking"] = json!({"type": "adaptive"});
                if effort != "adaptive" {
                    body["output_config"]["effort"] = json!(effort);
                }
            } else if matches!(effort, "none" | "0") {
                body["thinking"] = json!({"type": "disabled"});
            } else {
                let budget = effort.parse::<u64>().ok().filter(|value| *value >= 1024 && *value < max_tokens)
                    .ok_or_else(|| HarnessError::invalid("Claude reasoning must map to adaptive effort (low/medium/high/xhigh/max), none, or a token budget >= 1024 and below max output tokens"))?;
                body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            }
        }
        _ => {}
    }
    Ok(())
}
