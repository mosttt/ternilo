#![forbid(unsafe_code)]

use rhai::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ternilo_protocol::HarnessError;

/// Resource ceilings applied to a restricted Rhai engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RhaiSandboxLimits {
    pub max_operations: u64,
    pub max_string_bytes: usize,
    pub max_collection_items: usize,
    pub max_call_levels: usize,
    pub max_expr_depth: usize,
    pub max_variables: usize,
    pub max_functions: usize,
}

impl Default for RhaiSandboxLimits {
    fn default() -> Self {
        Self {
            max_operations: 1_000_000,
            max_string_bytes: 1024 * 1024,
            max_collection_items: 100_000,
            max_call_levels: 64,
            max_expr_depth: 64,
            max_variables: 4_096,
            max_functions: 256,
        }
    }
}

impl RhaiSandboxLimits {
    /// Validates that every restricted-engine limit is active.
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.max_operations == 0
            || self.max_string_bytes == 0
            || self.max_collection_items == 0
            || self.max_call_levels == 0
            || self.max_expr_depth == 0
            || self.max_variables == 0
            || self.max_functions == 0
        {
            return Err(HarnessError::composition(
                "Rhai sandbox limits must be positive",
            ));
        }
        Ok(())
    }

    /// Validates these limits against a host-provided ceiling.
    pub fn validate_within(&self, maximum: &Self) -> Result<(), HarnessError> {
        self.validate()?;
        maximum.validate()?;
        let exceeded = [
            (
                self.max_operations > maximum.max_operations,
                "max_operations",
            ),
            (
                self.max_string_bytes > maximum.max_string_bytes,
                "max_string_bytes",
            ),
            (
                self.max_collection_items > maximum.max_collection_items,
                "max_collection_items",
            ),
            (
                self.max_call_levels > maximum.max_call_levels,
                "max_call_levels",
            ),
            (
                self.max_expr_depth > maximum.max_expr_depth,
                "max_expr_depth",
            ),
            (self.max_variables > maximum.max_variables, "max_variables"),
            (self.max_functions > maximum.max_functions, "max_functions"),
        ]
        .into_iter()
        .find_map(|(exceeded, field)| exceeded.then_some(field));
        if let Some(field) = exceeded {
            return Err(HarnessError::composition(format!(
                "Rhai sandbox limit {field} exceeds the host maximum"
            )));
        }
        Ok(())
    }
}

/// Builds the common restricted Rhai engine baseline.
pub fn restricted_engine(limits: &RhaiSandboxLimits) -> Result<Engine, HarnessError> {
    limits.validate()?;
    let mut engine = Engine::new();
    engine
        .set_max_operations(limits.max_operations)
        .set_max_string_size(limits.max_string_bytes)
        .set_max_array_size(limits.max_collection_items)
        .set_max_map_size(limits.max_collection_items)
        .set_max_call_levels(limits.max_call_levels)
        .set_max_expr_depths(limits.max_expr_depth, limits.max_expr_depth)
        .set_max_variables(limits.max_variables)
        .set_max_functions(limits.max_functions)
        .disable_symbol("eval")
        .disable_symbol("import");
    Ok(engine)
}

#[cfg(test)]
mod tests {
    use rhai::EvalAltResult;

    use super::*;

    #[test]
    fn defaults_match_the_code_mode_runtime_baseline() {
        assert_eq!(
            RhaiSandboxLimits::default(),
            RhaiSandboxLimits {
                max_operations: 1_000_000,
                max_string_bytes: 1024 * 1024,
                max_collection_items: 100_000,
                max_call_levels: 64,
                max_expr_depth: 64,
                max_variables: 4_096,
                max_functions: 256,
            }
        );
    }

    #[test]
    fn limits_must_be_positive_and_within_the_host_ceiling() {
        let maximum = RhaiSandboxLimits::default();
        let mut requested = maximum.clone();
        requested.max_operations = 0;
        assert!(requested.validate().is_err());

        requested.max_operations = maximum.max_operations + 1;
        let error = requested.validate_within(&maximum).unwrap_err();
        assert!(error.message.contains("max_operations"), "{error:?}");

        requested.max_operations = maximum.max_operations;
        assert_eq!(requested.validate_within(&maximum), Ok(()));
    }

    #[test]
    fn restricted_engine_disables_dynamic_code_loading() {
        let engine = restricted_engine(&RhaiSandboxLimits::default()).unwrap();
        assert!(engine.compile(r#"import "host" as host;"#).is_err());
        assert!(engine.compile(r#"eval("40 + 2")"#).is_err());
    }

    #[test]
    fn restricted_engine_enforces_the_operation_budget() {
        let limits = RhaiSandboxLimits {
            max_operations: 100,
            ..RhaiSandboxLimits::default()
        };
        let engine = restricted_engine(&limits).unwrap();
        let error = engine.eval::<()>("loop { }").unwrap_err();
        assert!(matches!(*error, EvalAltResult::ErrorTooManyOperations(_)));
    }
}
