import { describe, expect, it } from 'vitest'
import {
  mergeDiscoveredModels,
  modelDraft,
  parseCapacity,
  providerModel,
  providerModelDefaults,
  type ProviderModelDraft,
} from './provider-model-editor'

const validationCopy = {
  invalidCapacity: (raw: string) => `invalid capacity: ${raw}`,
  positiveCapacity: (raw: string) => `positive capacity: ${raw}`,
  missingId: (index: number) => `missing model ${index}`,
  missingEffort: (id: string) => `missing effort ${id}`,
  defaultNotEnabled: (id: string) => `default disabled ${id}`,
}

describe('Provider model editor conversion', () => {
  it('accepts compact decimal capacities', () => {
    expect(parseCapacity('128K', validationCopy)).toBe(128_000)
    expect(parseCapacity('1m', validationCopy)).toBe(1_000_000)
    expect(parseCapacity('', validationCopy)).toBeNull()
    expect(() => parseCapacity('128KiB', validationCopy)).toThrow('invalid capacity: 128KiB')
  })

  it('keeps an inheriting model compact and resolves defaults separately', () => {
    const defaults = providerModelDefaults({
      contextWindow: '128K',
      maxOutputTokens: '16K',
      reasoning: {
        defaultEffort: 'medium',
        efforts: { low: 'low', medium: 'medium', high: 'provider-high' },
      },
    }, 'Provider', validationCopy)
    expect(defaults).toEqual({
      context_window: 128_000,
      max_output_tokens: 16_000,
      reasoning: {
        default_effort: 'medium',
        efforts: { low: 'low', medium: 'medium', high: 'provider-high' },
      },
    })
    expect(providerModel({
      id: 'model-a', displayName: '', upstream: {}, overrides: {},
    }, 0, validationCopy)).toEqual({
      id: 'model-a', display_name: null, settings: { mode: 'automatic', upstream: {}, overrides: {} },
    })
  })

  it('round-trips a complete model override including reasoning mappings', () => {
    const draft = modelDraft({
      id: 'reasoning-model',
      display_name: 'Reasoning Model',
      settings: {
        mode: 'override',
        context_window: 1_000_000,
        max_output_tokens: 64_000,
        reasoning: {
          default_effort: 'high',
          efforts: { high: 'ultra', max: 'max', none: null },
        },
      },
    })
    expect(providerModel(draft, 0, validationCopy)).toEqual({
      id: 'reasoning-model',
      display_name: 'Reasoning Model',
      settings: {
        mode: 'automatic',
        upstream: {},
        overrides: {
          context_window: 1_000_000,
          max_output_tokens: 64_000,
          reasoning: { mode: 'enabled', configuration: {
            default_effort: 'high',
            efforts: { none: null, high: 'ultra', max: 'max' },
          } },
        },
      },
    })
  })

  it('rejects a default effort that is not enabled', () => {
    expect(() => providerModel({
      id: 'model-a', displayName: '', upstream: {},
      overrides: {
        contextWindow: '128K', maxOutputTokens: '16K',
        reasoning: { defaultEffort: 'high', efforts: { medium: 'medium' } },
      },
    }, 0, validationCopy)).toThrow('default disabled model-a')
  })

  it('adopts only selected new discoveries without overwriting tuned models', () => {
    const tuned = {
      id: 'existing', displayName: 'Tuned', upstream: {},
      overrides: { contextWindow: '128K', maxOutputTokens: '32K', reasoning: null },
    }
    const result = mergeDiscoveredModels([tuned], [
      { id: 'existing', display_name: 'Remote value', settings: { mode: 'inherit' } },
      { id: 'new-a', display_name: 'New A', settings: { mode: 'inherit' } },
      {
        id: 'new-b', display_name: 'New B',
        settings: { mode: 'automatic', upstream: { context_window: 256_000, max_output_tokens: 16_000 }, overrides: {} },
      },
    ], new Set(['existing', 'new-b']))
    expect(result).toEqual([
      tuned,
      {
        id: 'new-b', displayName: 'New B',
        upstream: { context_window: 256_000, max_output_tokens: 16_000 }, overrides: {},
      },
    ])
  })

  it('replaces empty placeholder rows with discovered models', () => {
    const placeholder: ProviderModelDraft = {
      id: '', displayName: '', upstream: {}, overrides: {},
    }
    const discovered = { id: 'discovered', display_name: null, settings: { mode: 'inherit' as const } }
    expect(mergeDiscoveredModels([
      placeholder,
      { ...placeholder, id: '  ', displayName: '\t' },
    ], [discovered], new Set(['discovered']))).toEqual([modelDraft(discovered)])
  })

  it('preserves unfinished names, model overrides, and existing IDs when importing', () => {
    const drafts: ProviderModelDraft[] = [
      { id: '', displayName: 'First draft', upstream: {}, overrides: {} },
      { id: '', displayName: 'Second draft', upstream: {}, overrides: {} },
      { id: '', displayName: '', upstream: {}, overrides: { contextWindow: '256K', maxOutputTokens: '32K' } },
      { id: ' existing ', displayName: 'Custom name', upstream: {}, overrides: {} },
    ]
    const discovered = { id: 'existing', display_name: 'Remote name', settings: { mode: 'inherit' as const } }
    expect(mergeDiscoveredModels(drafts, [discovered], new Set(['existing']))).toEqual(drafts)
  })

  it('refreshes only selected upstream values and preserves manual values and names', () => {
    const original: ProviderModelDraft = { id: 'model', displayName: 'My name', upstream: { context_window: 32000 }, overrides: { contextWindow: '128K', reasoning: null } }
    const discovered = { id: 'model', display_name: 'Upstream name', settings: { mode: 'automatic' as const, upstream: { context_window: 256000, max_output_tokens: 16000 }, overrides: {} } }
    expect(mergeDiscoveredModels([original], [discovered], new Set())).toEqual([original])
    const [updated] = mergeDiscoveredModels([original], [discovered], new Set(['model']))
    expect(updated).toEqual({ ...original, upstream: discovered.settings.upstream })
    const stored = providerModel(updated, 0, validationCopy)
    expect(stored.settings).toEqual({ mode: 'automatic', upstream: discovered.settings.upstream, overrides: { context_window: 128000, reasoning: { mode: 'disabled' } } })
    expect(modelDraft(stored)).toEqual(updated)
    expect(original.upstream.context_window).toBe(32000)
  })

  it('distinguishes automatic reasoning from explicitly disabled reasoning', () => {
    const draft = { id: 'model', displayName: '', upstream: { context_window: 1000000 }, overrides: {} }
    expect(providerModel(draft, 0, validationCopy).settings).toEqual({ mode: 'automatic', upstream: draft.upstream, overrides: {} })
    expect(providerModel({ ...draft, overrides: { reasoning: null } }, 0, validationCopy).settings).toEqual({ mode: 'automatic', upstream: draft.upstream, overrides: { reasoning: { mode: 'disabled' } } })
  })
})
