use serde_json::{Map, Value, json};
use ternilo_protocol::{HarnessError, ProviderModelDefaults, ProviderProtocol};

pub(super) fn validate(
    object: &Map<String, Value>,
    protocol: ProviderProtocol,
) -> Result<(), HarnessError> {
    let field = if protocol == ProviderProtocol::GoogleGemini {
        "contents"
    } else {
        "messages"
    };
    if object
        .get(field)
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Err(HarnessError::invalid(format!(
            "{field} must be a nonempty array"
        )));
    }
    for field in [
        "cachedContent",
        "container",
        "context_management",
        "mcp_servers",
    ] {
        if object.get(field).is_some_and(|value| !value.is_null()) {
            return Err(HarnessError::invalid(format!(
                "{field} is not supported by this stateless model service"
            )));
        }
    }
    if let Some(tools) = object.get("tools").filter(|value| !value.is_null()) {
        let tools = tools
            .as_array()
            .ok_or_else(|| HarnessError::invalid("tools must be an array"))?;
        for tool in tools {
            let valid = if protocol == ProviderProtocol::GoogleGemini {
                tool.as_object().is_some_and(|fields| fields.len() == 1)
                    && tool["functionDeclarations"]
                        .as_array()
                        .is_some_and(|items| {
                            items.iter().all(|item| {
                                item["name"].as_str().is_some_and(|name| !name.is_empty())
                            })
                        })
            } else {
                tool.get("type").is_none_or(|kind| kind == "custom")
                    && tool["name"].as_str().is_some_and(|name| !name.is_empty())
                    && tool["input_schema"].is_object()
            };
            if !valid {
                return Err(HarnessError::invalid(
                    "only client-executed function tools are supported",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn prepare_upstream(
    body: &mut Value,
    protocol: ProviderProtocol,
    model: &str,
    defaults: &ProviderModelDefaults,
) -> Result<u64, HarnessError> {
    let pointer = if protocol == ProviderProtocol::GoogleGemini {
        "/generationConfig/maxOutputTokens"
    } else {
        "/max_tokens"
    };
    let output = body
        .pointer(pointer)
        .filter(|value| !value.is_null())
        .map(|value| {
            value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                HarnessError::invalid("max output tokens must be a positive integer")
            })
        })
        .transpose()?
        .unwrap_or(defaults.max_output_tokens);
    if output > defaults.max_output_tokens {
        return Err(HarnessError::invalid(
            "max output tokens exceeds this model's published limit",
        ));
    }
    if protocol == ProviderProtocol::GoogleGemini {
        if body["generationConfig"]
            .get("candidateCount")
            .is_some_and(|value| value.as_u64() != Some(1))
        {
            return Err(HarnessError::invalid(
                "Gemini model service supports one candidate per request",
            ));
        }
        if body
            .get("generationConfig")
            .is_some_and(|value| !value.is_null() && !value.is_object())
        {
            return Err(HarnessError::invalid("generationConfig must be an object"));
        }
        body["generationConfig"]["maxOutputTokens"] = json!(output);
    } else {
        body["max_tokens"] = json!(output);
    }
    let requested = requested_reasoning(body, protocol)?;
    let default = defaults
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.efforts.get(&reasoning.default_effort))
        .and_then(Option::as_deref);
    if let Some(requested) = requested.as_deref() {
        protocol.validate_reasoning_value(requested, output)?;
        if defaults.reasoning.as_ref().is_none_or(|reasoning| {
            !reasoning.efforts.values().flatten().any(|effort| {
                effort == requested
                    || matches!((effort.as_str(), requested), ("none", "0") | ("0", "none"))
            })
        }) {
            return Err(HarnessError::invalid(
                "thinking setting is not allowed by this published model",
            ));
        }
    } else {
        ternilo_builtins::apply_native_reasoning(body, protocol, default, output)?;
    }
    if protocol == ProviderProtocol::GoogleGemini {
        body.as_object_mut()
            .expect("request object")
            .remove("model");
        body.as_object_mut()
            .expect("request object")
            .remove("stream");
    } else {
        body["model"] = json!(model);
    }
    Ok(output)
}

fn requested_reasoning(
    body: &Value,
    protocol: ProviderProtocol,
) -> Result<Option<String>, HarnessError> {
    if protocol == ProviderProtocol::GoogleGemini {
        let Some(config) = body
            .pointer("/generationConfig/thinkingConfig")
            .filter(|value| !value.is_null())
        else {
            return Ok(None);
        };
        if !config.is_object() {
            return Err(HarnessError::invalid("thinkingConfig must be an object"));
        }
        if let Some(level) = config.get("thinkingLevel") {
            if config.get("thinkingBudget").is_some() {
                return Err(HarnessError::invalid(
                    "choose thinkingLevel or thinkingBudget, not both",
                ));
            }
            return level
                .as_str()
                .map(|level| Some(level.to_lowercase()))
                .ok_or_else(|| HarnessError::invalid("thinkingLevel must be a string"));
        }
        return config.get("thinkingBudget").map_or(Ok(None), |budget| {
            budget
                .as_i64()
                .map(|budget| Some(budget.to_string()))
                .ok_or_else(|| HarnessError::invalid("thinkingBudget must be an integer"))
        });
    }
    if body
        .get("output_config")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err(HarnessError::invalid("output_config must be an object"));
    }
    let effort = body
        .pointer("/output_config/effort")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| HarnessError::invalid("effort must be a string"))
        })
        .transpose()?;
    let Some(thinking) = body.get("thinking").filter(|value| !value.is_null()) else {
        return Ok(effort.map(str::to_owned));
    };
    match thinking["type"].as_str() {
        Some("disabled") => Ok(Some("none".to_owned())),
        Some("adaptive") => Ok(Some(effort.unwrap_or("adaptive").to_owned())),
        Some("enabled") => thinking["budget_tokens"]
            .as_u64()
            .map(|budget| Some(budget.to_string()))
            .ok_or_else(|| HarnessError::invalid("thinking budget_tokens must be an integer")),
        _ => Err(HarnessError::invalid("unsupported thinking type")),
    }
}
