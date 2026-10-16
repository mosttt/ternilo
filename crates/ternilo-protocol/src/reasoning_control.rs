use crate::{ProviderModelReasoning, ReasoningEffort};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Configured, permitted effort values for one model. Empty means keep the existing setting.
pub type AutomaticReasoningEfforts = BTreeMap<ReasoningEffort, Option<String>>;

/// The host's actual per-request selection; it never changes the saved model selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningSelection {
    pub recommended: ReasoningEffort,
    /// Absent when the model has no eligible automatic levels.
    pub selected: Option<ReasoningEffort>,
    /// The provider-specific parameter, or omission when the configured level maps to null.
    pub value: Option<String>,
}

/// Only levels declared in both the accepted snapshot and current host policy are eligible.
/// A manually selected effort caps automatic control; automatic mode never exceeds high.
#[must_use]
pub fn automatic_reasoning_efforts(
    accepted: Option<&ProviderModelReasoning>,
    current: Option<&ProviderModelReasoning>,
    manual_ceiling: Option<ReasoningEffort>,
) -> AutomaticReasoningEfforts {
    let (Some(accepted), Some(current)) = (accepted, current) else {
        return BTreeMap::new();
    };
    let ceiling = manual_ceiling
        .unwrap_or(ReasoningEffort::High)
        .min(ReasoningEffort::High);
    accepted
        .efforts
        .iter()
        .filter(|(effort, value)| {
            **effort <= ceiling && current.efforts.get(effort) == Some(*value)
        })
        .map(|(effort, value)| (*effort, value.clone()))
        .collect()
}

#[must_use]
pub fn select_reasoning_effort(
    recommended: ReasoningEffort,
    available: &AutomaticReasoningEfforts,
    existing_value: Option<&str>,
) -> ReasoningSelection {
    let selected = available
        .range(..=recommended)
        .next_back()
        .or_else(|| available.first_key_value());
    ReasoningSelection {
        recommended,
        selected: selected.map(|(effort, _)| *effort),
        value: selected.map_or_else(
            || existing_value.map(str::to_owned),
            |(_, value)| value.clone(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReasoningEffort::{High, Low, Max, Medium};

    #[test]
    fn automatic_effort_respects_model_mapping_manual_ceiling_and_revocation() {
        let accepted = ProviderModelReasoning {
            default_effort: Medium,
            efforts: BTreeMap::from([
                (Low, None),
                (Medium, Some("balanced".into())),
                (High, Some("deep".into())),
                (Max, Some("expensive".into())),
            ]),
        };
        let available = automatic_reasoning_efforts(Some(&accepted), Some(&accepted), None);
        assert!(!available.contains_key(&Max));
        assert_eq!(
            select_reasoning_effort(Low, &available, Some("balanced")).value,
            None
        );
        assert_eq!(
            select_reasoning_effort(High, &available, None)
                .value
                .as_deref(),
            Some("deep")
        );
        let capped = automatic_reasoning_efforts(Some(&accepted), Some(&accepted), Some(Low));
        assert_eq!(
            select_reasoning_effort(High, &capped, None).selected,
            Some(Low)
        );
        let mut current = accepted.clone();
        current.efforts.insert(High, Some("changed".into()));
        let available = automatic_reasoning_efforts(Some(&accepted), Some(&current), None);
        assert_eq!(
            select_reasoning_effort(High, &available, None).selected,
            Some(Medium)
        );
        let absent = automatic_reasoning_efforts(Some(&accepted), None, None);
        let unchanged = select_reasoning_effort(High, &absent, Some("balanced"));
        assert_eq!(unchanged.selected, None);
        assert_eq!(unchanged.value.as_deref(), Some("balanced"));
    }
}
