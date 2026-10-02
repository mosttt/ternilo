use serde::{Deserialize, Serialize};

use crate::{
    HarnessError, ProviderModelDefaults, ProviderProtocol, ReasoningEffort, ResolvedProviderModel,
    TenantId, UserId,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunModelBinding {
    ComputerProvider {
        tenant_id: TenantId,
        owner_user_id: UserId,
        executor_id: String,
        provider_id: String,
        model: String,
    },
    Platform {
        grant_id: String,
        model_id: String,
        beneficiary_user_id: UserId,
    },
    UserProvider {
        tenant_id: TenantId,
        owner_user_id: UserId,
        provider_id: String,
        model: String,
    },
}

impl RunModelBinding {
    pub fn validate(&self) -> Result<(), HarnessError> {
        match self {
            Self::ComputerProvider {
                tenant_id,
                owner_user_id,
                executor_id,
                provider_id,
                model,
            } => {
                tenant_id.validate()?;
                owner_user_id.validate()?;
                validate_reference(executor_id, "source computer", 128)?;
                if !crate::valid_provider_id(provider_id) {
                    return Err(HarnessError::invalid(
                        "invalid computer Provider identifier",
                    ));
                }
                validate_reference(model, "computer model", 200)
            }
            Self::Platform {
                grant_id,
                model_id,
                beneficiary_user_id,
            } => {
                validate_reference(grant_id, "model grant", 128)?;
                validate_reference(model_id, "public model", 128)?;
                beneficiary_user_id.validate()
            }
            Self::UserProvider {
                tenant_id,
                owner_user_id,
                provider_id,
                model,
            } => {
                tenant_id.validate()?;
                owner_user_id.validate()?;
                if !crate::valid_provider_id(provider_id) {
                    return Err(HarnessError::invalid("invalid user Provider identifier"));
                }
                validate_reference(model, "user Provider model", 200)
            }
        }
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        match self {
            Self::Platform { model_id, .. } => model_id,
            Self::UserProvider { model, .. } | Self::ComputerProvider { model, .. } => model,
        }
    }

    #[must_use]
    pub const fn beneficiary_user_id(&self) -> &UserId {
        match self {
            Self::Platform {
                beneficiary_user_id,
                ..
            } => beneficiary_user_id,
            Self::UserProvider { owner_user_id, .. }
            | Self::ComputerProvider { owner_user_id, .. } => owner_user_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunModelSnapshot {
    pub binding: RunModelBinding,
    pub protocol: ProviderProtocol,
    pub defaults: ProviderModelDefaults,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub display_name: String,
    pub source_name: String,
}

impl RunModelSnapshot {
    pub fn validate(&self) -> Result<(), HarnessError> {
        self.binding.validate()?;
        crate::validate_model_defaults(&self.defaults, "run model snapshot")?;
        validate_reference(&self.display_name, "model display name", 200)?;
        validate_reference(&self.source_name, "model source name", 200)?;
        self.resolved_model()
            .reasoning_value(self.reasoning_effort)
            .map(|_| ())
    }

    #[must_use]
    pub fn selection(&self) -> crate::DefaultModelSelection {
        match &self.binding {
            RunModelBinding::ComputerProvider {
                executor_id,
                provider_id,
                model,
                ..
            } => crate::DefaultModelSelection::ComputerProvider {
                executor_id: executor_id.clone(),
                provider_id: provider_id.clone(),
                model: model.clone(),
                reasoning_effort: self.reasoning_effort,
            },
            RunModelBinding::Platform {
                grant_id, model_id, ..
            } => crate::DefaultModelSelection::PlatformModel {
                grant_id: grant_id.clone(),
                model_id: model_id.clone(),
                reasoning_effort: self.reasoning_effort,
            },
            RunModelBinding::UserProvider {
                provider_id, model, ..
            } => crate::DefaultModelSelection::NamedProvider {
                provider_id: provider_id.clone(),
                model: model.clone(),
                reasoning_effort: self.reasoning_effort,
            },
        }
    }

    #[must_use]
    pub fn resolved_model(&self) -> ResolvedProviderModel {
        ResolvedProviderModel {
            id: self.binding.model_id().to_owned(),
            display_name: Some(self.display_name.clone()),
            context_window: self.defaults.context_window,
            max_output_tokens: self.defaults.max_output_tokens,
            reasoning: self.defaults.reasoning.clone(),
        }
    }
}

pub(crate) fn validate_reference(
    value: &str,
    label: &str,
    maximum: usize,
) -> Result<(), HarnessError> {
    if value.trim().is_empty()
        || value.chars().count() > maximum
        || value.chars().any(char::is_control)
    {
        return Err(HarnessError::invalid(format!(
            "{label} must contain 1 to {maximum} characters without control characters"
        )));
    }
    Ok(())
}
