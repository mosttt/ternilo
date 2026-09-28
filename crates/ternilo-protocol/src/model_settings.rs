use crate::{HarnessError, ProviderModelDefaults, ProviderModelReasoning, validate_model_defaults};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderReasoningSetting {
    Disabled,
    Enabled {
        configuration: ProviderModelReasoning,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelValues {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ProviderReasoningSetting>,
}

impl ProviderModelValues {
    #[must_use]
    pub fn resolve(&self, defaults: &ProviderModelDefaults) -> ProviderModelDefaults {
        ProviderModelDefaults {
            context_window: self.context_window.unwrap_or(defaults.context_window),
            max_output_tokens: self.max_output_tokens.unwrap_or(defaults.max_output_tokens),
            reasoning: match &self.reasoning {
                None => defaults.reasoning.clone(),
                Some(ProviderReasoningSetting::Disabled) => None,
                Some(ProviderReasoningSetting::Enabled { configuration }) => {
                    Some(configuration.clone())
                }
            },
        }
    }

    pub(crate) fn validate(&self, label: &str) -> Result<(), HarnessError> {
        validate_model_defaults(
            &self.resolve(&ProviderModelDefaults {
                context_window: 1,
                max_output_tokens: 1,
                reasoning: None,
            }),
            label,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{ProviderModelCatalog, ProviderProfile};
    use serde_json::{Value, json};

    fn profile(settings: &Value) -> ProviderProfile {
        serde_json::from_value(json!({
            "id": "fields", "display_name": "Fields", "base_url": "https://example.test/v1",
            "defaults": {"context_window": 128_000, "max_output_tokens": 16_000,
                "reasoning": {"default_effort": "max", "efforts": {"max": "provider-max"}}},
            "models": [{"id": "model", "settings": settings}],
            "timeout_ms": 0, "max_attempts": 1, "retry_base_delay_ms": 25,
        }))
        .unwrap()
    }

    #[test]
    fn manual_upstream_and_default_fields_survive_persistence_independently() {
        let original = profile(&json!({"mode": "automatic",
            "upstream": {"context_window": 1_048_576, "max_output_tokens": 393_216},
            "overrides": {"max_output_tokens": 32_000},
        }));
        original.validate().unwrap();
        let restored: ProviderProfile =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        let model = restored.resolved_model("model").unwrap();
        assert_eq!(model.context_window, 1_048_576);
        assert_eq!(model.max_output_tokens, 32_000);
        assert_eq!(model.reasoning_value(None).unwrap(), Some("provider-max"));
        assert_eq!(restored, original);
    }

    #[test]
    fn missing_reasoning_and_explicit_disable_are_different() {
        let enabled = json!({"mode": "enabled", "configuration": {"default_effort": "low", "efforts": {"low": "upstream-low"}}});
        for (upstream, overrides, expected) in [
            (json!({}), json!({}), Some("provider-max")),
            (
                json!({"reasoning": enabled}),
                json!({}),
                Some("upstream-low"),
            ),
            (json!({"reasoning": {"mode": "disabled"}}), json!({}), None),
            (json!({}), json!({"reasoning": {"mode": "disabled"}}), None),
            (
                json!({"reasoning": {"mode": "disabled"}}),
                json!({"reasoning": enabled}),
                Some("upstream-low"),
            ),
        ] {
            let provider = profile(
                &json!({"mode": "automatic", "upstream": upstream, "overrides": overrides}),
            );
            provider.validate().unwrap();
            assert_eq!(
                provider
                    .resolved_model("model")
                    .unwrap()
                    .reasoning_value(None)
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn explicit_invalid_values_are_not_hidden_by_another_source() {
        for settings in [
            json!({"mode": "automatic", "upstream": {"context_window": 0}, "overrides": {"context_window": 64_000}}),
            json!({"mode": "automatic", "overrides": {"max_output_tokens": 4_294_967_296_u64}}),
            json!({"mode": "automatic", "upstream": {"reasoning": {"mode": "enabled", "configuration": {"default_effort": "high", "efforts": {"low": "low"}}}}}),
        ] {
            assert!(profile(&settings).validate().is_err());
        }
    }
}
