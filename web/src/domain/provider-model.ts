import type {
  ProviderModel,
  ProviderModelDefaults,
  ProviderModelSettings,
  ProviderModelValues,
  ProviderProfile,
} from '@/types'

export interface ResolvedProviderModel extends ProviderModelDefaults {
  id: string
  display_name?: string | null
}

export function applyProviderModelValues(defaults: ProviderModelDefaults, values: ProviderModelValues): ProviderModelDefaults {
  return {
    context_window: values.context_window ?? defaults.context_window,
    max_output_tokens: values.max_output_tokens ?? defaults.max_output_tokens,
    reasoning: values.reasoning?.mode === 'disabled' ? null : values.reasoning?.configuration ?? defaults.reasoning,
  }
}

export function resolveProviderModelSettings(defaults: ProviderModelDefaults, settings: ProviderModelSettings): ProviderModelDefaults {
  if (settings.mode === 'inherit') return defaults
  if (settings.mode === 'override') return settings
  return applyProviderModelValues(applyProviderModelValues(defaults, settings.upstream), settings.overrides)
}

export function resolvedProviderModel(
  provider: ProviderProfile | undefined,
  modelId: string,
): ResolvedProviderModel | undefined {
  const model = provider?.models.find(candidate => candidate.id === modelId)
  if (!provider || !model) return undefined
  const settings = resolveProviderModelSettings(provider.defaults, model.settings)
  return {
    id: model.id,
    display_name: model.display_name,
    context_window: settings.context_window,
    max_output_tokens: settings.max_output_tokens,
    reasoning: settings.reasoning,
  }
}
