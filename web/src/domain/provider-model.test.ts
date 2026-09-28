import { describe, expect, it } from 'vitest'
import type { ProviderModelValues, ProviderProfile } from '@/types'
import { resolvedProviderModel } from './provider-model'

const provider: ProviderProfile = {
  id: 'fixture',
  display_name: 'Fixture',
  base_url: 'https://example.test/v1',
  protocol: 'openai-responses',
  defaults: {
    context_window: 128_000,
    max_output_tokens: 16_000,
    reasoning: { default_effort: 'medium', efforts: { medium: 'provider-medium' } },
  },
  models: [
    { id: 'model-a', settings: { mode: 'inherit' } },
    {
      id: 'model-b',
      settings: {
        mode: 'override',
        context_window: 1_000_000,
        max_output_tokens: 64_000,
        reasoning: { default_effort: 'high', efforts: { high: 'model-high' } },
      },
    },
  ],
  timeout_ms: 120_000,
  max_attempts: 3,
  retry_base_delay_ms: 250,
}

describe('resolvedProviderModel', () => {
  it('resolves inheritance and full override from one path', () => {
    expect(resolvedProviderModel(provider, 'model-a')).toMatchObject({
      context_window: 128_000,
      max_output_tokens: 16_000,
      reasoning: { default_effort: 'medium' },
    })
    expect(resolvedProviderModel(provider, 'model-b')).toMatchObject({
      context_window: 1_000_000,
      max_output_tokens: 64_000,
      reasoning: { default_effort: 'high' },
    })
  })

  it('uses manual, upstream and Provider defaults per field without disabling absent reasoning', () => {
    const settings = { mode: 'automatic' as const, upstream: { context_window: 1048576, max_output_tokens: 393216 }, overrides: { max_output_tokens: 32000 } }
    expect(resolvedProviderModel({ ...provider, models: [{ id: 'model', settings }] }, 'model')).toMatchObject({ context_window: 1048576, max_output_tokens: 32000, reasoning: provider.defaults.reasoning })
    const upstream: ProviderModelValues = { reasoning: { mode: 'enabled', configuration: { default_effort: 'high', efforts: { high: 'upstream-high' } } } }
    expect(resolvedProviderModel({ ...provider, models: [{ id: 'model', settings: { ...settings, upstream } }] }, 'model')?.reasoning?.default_effort).toBe('high')
    for (const disabled of [{ ...settings, upstream: { reasoning: { mode: 'disabled' as const } } }, { ...settings, overrides: { reasoning: { mode: 'disabled' as const } } }]) {
      expect(resolvedProviderModel({ ...provider, models: [{ id: 'model', settings: disabled }] }, 'model')?.reasoning).toBeNull()
    }
  })
})
