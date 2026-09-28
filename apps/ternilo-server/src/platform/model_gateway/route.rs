use ternilo_builtins::ProviderModelRoute;
use ternilo_cloud::StartedRun;
use ternilo_control::ResolvedModelRoute;
use ternilo_protocol::{HarnessError, ModelRequest, RunModelBinding, RunModelSnapshot};

pub(super) struct ResolvedBrokeredRoute {
    pub(super) route: ProviderModelRoute,
    pub(super) api_key: Option<String>,
}

pub(super) fn snapshot(
    run: &StartedRun,
    requested: &RunModelBinding,
) -> Result<RunModelSnapshot, HarnessError> {
    let snapshot = ternilo_cloud::profile_model_snapshot(&run.claim.spec.profile)?
        .ok_or_else(|| HarnessError::policy("the canonical run has no brokered model snapshot"))?;
    if &snapshot.binding != requested {
        return Err(HarnessError::policy(
            "Worker requested a model outside its canonical run binding",
        ));
    }
    Ok(snapshot)
}

pub(super) fn resolve(
    resolved: ResolvedModelRoute,
    snapshot: &RunModelSnapshot,
) -> Result<ResolvedBrokeredRoute, HarnessError> {
    if resolved.model.protocol != snapshot.protocol {
        return Err(HarnessError::policy(
            "the model protocol changed after this run was accepted",
        ));
    }
    let reasoning_effort = snapshot
        .resolved_model()
        .reasoning_value(snapshot.reasoning_effort)?
        .map(str::to_owned);
    if let Some(effort) = &reasoning_effort {
        let allowed = resolved
            .model
            .defaults
            .reasoning
            .as_ref()
            .is_some_and(|reasoning| {
                reasoning
                    .efforts
                    .values()
                    .any(|value| value.as_ref() == Some(effort))
            });
        if !allowed {
            return Err(HarnessError::policy(
                "the run's reasoning effort is no longer allowed by its model",
            ));
        }
    }
    let max_output = snapshot
        .defaults
        .max_output_tokens
        .min(resolved.model.defaults.max_output_tokens);
    let max_output = u32::try_from(max_output)
        .map_err(|_| HarnessError::policy("model output limit exceeds u32"))?;
    Ok(ResolvedBrokeredRoute {
        route: ProviderModelRoute {
            provider: public_provider(&snapshot.binding).to_owned(),
            base_url: resolved.provider.base_url,
            protocol: resolved.provider.protocol,
            model: resolved.upstream_model,
            context_window: Some(
                snapshot
                    .defaults
                    .context_window
                    .min(resolved.model.defaults.context_window),
            ),
            timeout_ms: resolved.provider.timeout_ms,
            max_tokens: Some(max_output),
            temperature: None,
            reasoning_effort,
            max_attempts: resolved.provider.max_attempts,
            retry_base_delay_ms: resolved.provider.retry_base_delay_ms,
        },
        api_key: resolved.api_key.map(|key| key.to_string()),
    })
}

pub(super) fn public_provider(binding: &RunModelBinding) -> &str {
    match binding {
        RunModelBinding::Platform { .. } => "platform",
        RunModelBinding::UserProvider { provider_id, .. } => provider_id,
    }
}

pub(super) fn conservative_model_budget(
    request: &ModelRequest,
    snapshot: &RunModelSnapshot,
) -> Result<u64, HarnessError> {
    let input = if request
        .messages
        .iter()
        .flat_map(|message| &message.attachments)
        .any(|attachment| attachment.media_type.starts_with("image/"))
    {
        snapshot.defaults.context_window
    } else {
        let bytes = serde_json::to_vec(request)
            .map_err(|error| HarnessError::execution(format!("meter model request: {error}")))?;
        u64::try_from(bytes.len())
            .ok()
            .and_then(|bytes| bytes.checked_add(4_096))
            .ok_or_else(|| HarnessError::invalid("model request budget overflow"))?
    };
    input
        .checked_add(snapshot.defaults.max_output_tokens)
        .ok_or_else(|| HarnessError::invalid("model request budget overflow"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternilo_protocol::{
        ProviderModel, ProviderModelDefaults, ProviderModelReasoning, ProviderModelSettings,
        ProviderProfile, ProviderProtocol, ReasoningEffort, UserId,
    };

    fn model_snapshot() -> RunModelSnapshot {
        RunModelSnapshot {
            binding: RunModelBinding::Platform {
                grant_id: "grant".to_owned(),
                model_id: "public-model".to_owned(),
                beneficiary_user_id: UserId::new("owner"),
            },
            protocol: ProviderProtocol::OpenAiResponses,
            defaults: ProviderModelDefaults {
                context_window: 32_000,
                max_output_tokens: 512,
                reasoning: Some(ProviderModelReasoning {
                    default_effort: ReasoningEffort::High,
                    efforts: [(ReasoningEffort::High, Some("ultra".to_owned()))]
                        .into_iter()
                        .collect(),
                }),
            },
            reasoning_effort: Some(ReasoningEffort::High),
            display_name: "Public model".to_owned(),
            source_name: "Platform allowance".to_owned(),
        }
    }

    #[test]
    fn conservative_budget_covers_wire_overhead_and_the_frozen_output_limit() {
        let request = ModelRequest {
            run_id: ternilo_protocol::RunId::new("model-test-run"),
            system_prompt: "System".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            step: 1,
        };
        assert_eq!(
            conservative_model_budget(&request, &model_snapshot()).unwrap(),
            serde_json::to_vec(&request).unwrap().len() as u64 + 4_096 + 512
        );
    }

    #[test]
    fn accepted_model_keeps_its_wire_reasoning_and_respects_reduced_current_limits() {
        let snapshot = model_snapshot();
        let mut limits = snapshot.defaults.clone();
        limits.context_window = 16_000;
        limits.max_output_tokens = 256;
        let resolved = resolve(
            ResolvedModelRoute {
                provider: ProviderProfile {
                    id: "upstream".to_owned(),
                    display_name: "Provider".to_owned(),
                    base_url: "https://models.example/v1".to_owned(),
                    protocol: snapshot.protocol,
                    api_key_ref: None,
                    defaults: limits.clone(),
                    models: vec![ProviderModel {
                        id: "internal-model".to_owned(),
                        display_name: None,
                        settings: ProviderModelSettings::Inherit,
                    }],
                    timeout_ms: 30_000,
                    max_attempts: 3,
                    retry_base_delay_ms: 100,
                },
                model: ternilo_control::PublicModel {
                    model_id: snapshot.binding.model_id().to_owned(),
                    display_name: snapshot.display_name.clone(),
                    protocol: snapshot.protocol,
                    defaults: limits,
                },
                upstream_model: "internal-model".to_owned(),
                api_key: None,
            },
            &snapshot,
        )
        .unwrap();
        assert_eq!(resolved.route.max_tokens, Some(256));
        assert_eq!(resolved.route.context_window, Some(16_000));
        assert_eq!(resolved.route.reasoning_effort.as_deref(), Some("ultra"));
        assert_eq!(resolved.route.model, "internal-model");
        assert_eq!(resolved.route.max_attempts, 3);
    }
}
