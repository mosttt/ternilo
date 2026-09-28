use serde::Deserialize;
use serde_json::Value;
use ternilo_protocol::{
    HarnessError, ProviderModel, ProviderModelReasoning, ProviderModelSettings,
    ProviderModelValues, ProviderProtocol, ProviderReasoningSetting,
};

#[derive(Deserialize)]
struct ModelCatalog {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(alias = "display_name")]
    name: Option<String>,
    protocol: Option<Value>,
    #[serde(alias = "contextWindow", alias = "max_input_tokens")]
    context_window: Option<u64>,
    #[serde(alias = "maxTokens", alias = "max_tokens")]
    max_output_tokens: Option<u64>,
    reasoning: Option<Value>,
    capabilities: Option<Value>,
}

fn parse_catalog_page(
    bytes: &[u8],
    protocol: ProviderProtocol,
) -> Result<Vec<ProviderModel>, HarnessError> {
    if protocol == ProviderProtocol::GoogleGemini {
        let value: Value = serde_json::from_slice(bytes).map_err(|error| {
            HarnessError::execution(format!("decode Gemini model catalog: {error}"))
        })?;
        if value.get("models").is_some() || value.get("data").is_none() {
            return parse_gemini_catalog(&value);
        }
    }
    let catalog: ModelCatalog = serde_json::from_slice(bytes).map_err(|error| {
        HarnessError::execution(format!("decode provider model catalog: {error}"))
    })?;
    let mut models = catalog
        .data
        .into_iter()
        .filter(
            |model| match model.protocol.as_ref().and_then(Value::as_str) {
                Some(
                    advertised @ ("openai-chat-completions"
                    | "openai-responses"
                    | "deepseek-responses"
                    | "google-gemini"
                    | "anthropic-messages"),
                ) => advertised == protocol.as_str(),
                _ => true,
            },
        )
        .map(|model| ProviderModel {
            id: model.id,
            display_name: model.name,
            settings: ProviderModelSettings::Automatic {
                upstream: ProviderModelValues {
                    context_window: model.context_window.filter(|value| *value > 0),
                    max_output_tokens: model.max_output_tokens.filter(|value| *value > 0),
                    reasoning: model
                        .reasoning
                        .and_then(|value| {
                            if value == Value::Bool(false) {
                                Some(ProviderReasoningSetting::Disabled)
                            } else {
                                serde_json::from_value::<ProviderModelReasoning>(value)
                                    .ok()
                                    .map(|configuration| ProviderReasoningSetting::Enabled {
                                        configuration,
                                    })
                            }
                        })
                        .or_else(|| model.capabilities.as_ref().and_then(anthropic_reasoning)),
                },
                overrides: ProviderModelValues::default(),
            },
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    Ok(models)
}

pub fn parse_openai_model_catalog(
    bytes: &[u8],
    protocol: ProviderProtocol,
) -> Result<Vec<ProviderModel>, HarnessError> {
    let models = parse_catalog_page(bytes, protocol)?;
    require_models(&models, protocol)?;
    Ok(models)
}

fn require_models(
    models: &[ProviderModel],
    protocol: ProviderProtocol,
) -> Result<(), HarnessError> {
    if models.is_empty() {
        return Err(HarnessError::execution(format!(
            "provider model discovery returned no models for {}",
            protocol.as_str()
        )));
    }
    Ok(())
}

fn parse_gemini_catalog(value: &Value) -> Result<Vec<ProviderModel>, HarnessError> {
    let entries = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| HarnessError::execution("Gemini model catalog omitted models"))?;
    entries
        .iter()
        .filter(|model| {
            model["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|methods| {
                    methods.iter().any(|method| {
                        method == "generateContent" || method == "streamGenerateContent"
                    })
                })
        })
        .map(|model| {
            let name = model["name"]
                .as_str()
                .ok_or_else(|| HarnessError::execution("Gemini catalog model omitted name"))?;
            Ok(ProviderModel {
                id: name.strip_prefix("models/").unwrap_or(name).to_owned(),
                display_name: model["displayName"].as_str().map(str::to_owned),
                settings: ProviderModelSettings::Automatic {
                    upstream: ProviderModelValues {
                        context_window: model["inputTokenLimit"]
                            .as_u64()
                            .filter(|value| *value > 0),
                        max_output_tokens: model["outputTokenLimit"]
                            .as_u64()
                            .filter(|value| *value > 0),
                        reasoning: (model["thinking"].as_bool() == Some(false))
                            .then_some(ProviderReasoningSetting::Disabled),
                    },
                    overrides: ProviderModelValues::default(),
                },
            })
        })
        .collect()
}

fn anthropic_reasoning(capabilities: &Value) -> Option<ProviderReasoningSetting> {
    if capabilities
        .pointer("/thinking/supported")
        .and_then(Value::as_bool)
        == Some(false)
    {
        return Some(ProviderReasoningSetting::Disabled);
    }
    if capabilities
        .pointer("/thinking/types/adaptive/supported")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let efforts = ["low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .filter(|level| capabilities["effort"][*level]["supported"].as_bool() == Some(true))
        .filter_map(|level| {
            serde_json::from_value(serde_json::json!(level))
                .ok()
                .map(|effort| (effort, Some(level.to_owned())))
        })
        .collect::<std::collections::BTreeMap<ternilo_protocol::ReasoningEffort, Option<String>>>();
    let default_effort = if efforts.contains_key(&ternilo_protocol::ReasoningEffort::High) {
        ternilo_protocol::ReasoningEffort::High
    } else {
        *efforts.keys().next()?
    };
    Some(ProviderReasoningSetting::Enabled {
        configuration: ProviderModelReasoning {
            default_effort,
            efforts,
        },
    })
}

pub async fn discover_provider_models(
    client: &reqwest::Client,
    base_url: &str,
    protocol: ProviderProtocol,
    api_key: Option<&str>,
) -> Result<Vec<ProviderModel>, HarnessError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    let mut models = Vec::new();
    loop {
        let mut request = crate::provider_request(client.get(&url), protocol, api_key);
        if let Some(cursor) = cursor.as_deref() {
            request = request.query(&[(
                if protocol == ProviderProtocol::GoogleGemini {
                    "pageToken"
                } else {
                    "after_id"
                },
                cursor,
            )]);
        }
        let response = request.send().await.map_err(|error| {
            HarnessError::execution(format!("discover provider models: {}", error.without_url()))
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(HarnessError::execution(format!(
                "provider model discovery returned HTTP {status}"
            )));
        }
        let bytes = crate::read_provider_response(response, "model catalog").await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| HarnessError::execution(format!("decode model catalog: {error}")))?;
        models.extend(parse_catalog_page(&bytes, protocol)?);
        let next = if protocol == ProviderProtocol::GoogleGemini {
            value["nextPageToken"].as_str()
        } else if protocol == ProviderProtocol::AnthropicMessages
            && value["has_more"].as_bool() == Some(true)
        {
            Some(
                value["last_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        HarnessError::execution("Claude catalog pagination omitted last_id")
                    })?,
            )
        } else {
            None
        };
        let Some(next) = next.filter(|next| !next.is_empty()) else {
            break;
        };
        if !seen.insert(next.to_owned()) {
            return Err(HarnessError::execution(
                "provider repeated its model catalog cursor",
            ));
        }
        cursor = Some(next.to_owned());
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    require_models(&models, protocol)?;
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use ternilo_protocol::{ProviderModelCatalog, ProviderProfile, ReasoningEffort};

    #[test]
    fn mixed_catalog_preserves_explicit_upstream_values_separately() {
        let bytes = serde_json::to_vec(&json!({"data": [
            {"id": "chat", "protocol": "openai-chat-completions"},
            {"id": "reasoning", "name": "Reasoning model", "protocol": "openai-responses",
                "context_window": 128_000, "max_output_tokens": 4096,
                "reasoning": {"default_effort": "high", "efforts": {"high": "high"}}},
            {"id": "generic"}, {"id": "generic"}
        ]}))
        .unwrap();
        let models = parse_openai_model_catalog(&bytes, ProviderProtocol::OpenAiResponses).unwrap();
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["generic", "reasoning"]
        );
        assert_eq!(models[1].display_name.as_deref(), Some("Reasoning model"));
        let ProviderModelSettings::Automatic {
            upstream,
            overrides,
        } = &models[1].settings
        else {
            panic!("discovery must preserve field-level sources")
        };
        assert_eq!(upstream.context_window, Some(128_000));
        assert_eq!(upstream.max_output_tokens, Some(4096));
        assert!(
            matches!(&upstream.reasoning, Some(ProviderReasoningSetting::Enabled { configuration }) if configuration.default_effort == ReasoningEffort::High)
        );
        assert_eq!(overrides, &ProviderModelValues::default());
    }

    #[test]
    fn discovered_capacities_do_not_disable_inherited_reasoning() {
        let bytes = br#"{"data":[{"id":"model","contextWindow":64000,"maxTokens":2000,"reasoning":true,"protocol":{"vendor":"custom"}}]}"#;
        for protocol in [
            ProviderProtocol::OpenAiChatCompletions,
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::DeepSeekResponses,
        ] {
            let models = parse_openai_model_catalog(bytes, protocol).unwrap();
            let profile: ProviderProfile = serde_json::from_value(json!({
                "id": "provider", "display_name": "Provider", "base_url": "https://example.test/v1",
                "protocol": protocol,
                "timeout_ms": 0, "max_attempts": 1, "retry_base_delay_ms": 25,
                "defaults": {"context_window": 128_000, "max_output_tokens": 16_000,
                    "reasoning": {"default_effort": "max", "efforts": {"low": "low", "max": "provider-max"}}},
                "models": models,
            }))
            .unwrap();
            let resolved = profile.resolved_model("model").unwrap();
            assert_eq!(
                (resolved.context_window, resolved.max_output_tokens),
                (64_000, 2000)
            );
            assert_eq!(resolved.reasoning, profile.defaults.reasoning);
            assert_eq!(
                resolved.reasoning_value(None).unwrap(),
                Some("provider-max")
            );
            assert_eq!(
                resolved
                    .reasoning_value(Some(ReasoningEffort::Low))
                    .unwrap(),
                Some("low")
            );
        }
    }

    #[test]
    fn protocol_mismatch_does_not_import_unusable_models() {
        let error = parse_openai_model_catalog(
            br#"{"data":[{"id":"chat","protocol":"openai-chat-completions"}]}"#,
            ProviderProtocol::OpenAiResponses,
        )
        .unwrap_err();
        assert!(error.message.contains("openai-responses"));
    }

    #[test]
    fn partial_metadata_and_explicit_disabled_reasoning_stay_distinct() {
        let models = parse_openai_model_catalog(
            br#"{"data":[{"id":"absent","context_window":1048576},{"id":"disabled","reasoning":false},{"id":"unknown","reasoning":true}]}"#,
            ProviderProtocol::DeepSeekResponses,
        ).unwrap();
        let upstream = models
            .iter()
            .map(|model| {
                let ProviderModelSettings::Automatic { upstream, .. } = &model.settings else {
                    panic!("discovered settings must retain their sources")
                };
                upstream
            })
            .collect::<Vec<_>>();
        assert_eq!(upstream[0].context_window, Some(1_048_576));
        assert_eq!(upstream[0].max_output_tokens, None);
        assert_eq!(upstream[0].reasoning, None);
        assert_eq!(
            upstream[1].reasoning,
            Some(ProviderReasoningSetting::Disabled)
        );
        assert_eq!(upstream[2].reasoning, None);
    }
    #[test]
    fn responses_catalog_preserves_the_selected_native_adapter() {
        let bytes = br#"{"data":[{"id":"openai","protocol":"openai-responses"},{"id":"deepseek","protocol":"deepseek-responses"}]}"#;
        for (protocol, expected) in [
            (ProviderProtocol::OpenAiResponses, "openai"),
            (ProviderProtocol::DeepSeekResponses, "deepseek"),
        ] {
            let models = parse_openai_model_catalog(bytes, protocol).unwrap();
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, expected);
        }
    }
}
