import { describe, expect, it } from 'vitest'
import type { ApplicationCatalog, CredentialInventory, Profile, ProviderProfile } from '@/types'
import {
  modelSelectionIsUsable,
  providerIsUsable,
  profileModelIsUsable,
  usableProviderModels,
} from './provider-readiness'

const credentials = (configured: string[] = []): CredentialInventory => ({
  records: [],
  references: configured.map(reference => ({
    reference,
    configured: true,
    source: 'managed',
    writable: true,
  })),
})

const provider = (values: Partial<ProviderProfile> = {}): ProviderProfile => ({
  id: 'fixture',
  source: 'user',
  display_name: 'Fixture',
  base_url: 'https://example.test/v1',
  protocol: 'openai-responses',
  api_key_ref: 'FIXTURE_KEY',
  defaults: { context_window: 128_000, max_output_tokens: 16_000 },
  models: [{ id: 'model-a', settings: { mode: 'inherit' } }],
  timeout_ms: 30_000,
  max_attempts: 2,
  retry_base_delay_ms: 250,
  ...values,
})

describe('provider readiness', () => {
  it('treats operator and no-key providers as usable without inventing a credential', () => {
    expect(providerIsUsable(provider({ source: 'operator', api_key_ref: null }), credentials())).toBe(true)
    expect(providerIsUsable(provider({ api_key_ref: null }), credentials())).toBe(true)
  })

  it('requires an actually configured reference and at least one model', () => {
    expect(providerIsUsable(provider(), credentials())).toBe(false)
    expect(providerIsUsable(provider(), credentials(['FIXTURE_KEY']))).toBe(true)
    expect(providerIsUsable(provider({ models: [] }), credentials(['FIXTURE_KEY']))).toBe(false)
  })

  it('uses shared model metadata without exposing the endpoint or a writable credential', () => {
    const inventory = credentials(['FIXTURE_KEY'])
    inventory.references[0].writable = false
    expect(modelSelectionIsUsable(
      { provider: 'named_provider', provider_id: 'fixture', model: 'model-a' },
      [provider({ base_url: '' })],
      inventory,
    )).toBe(true)
    inventory.references[0].configured = false
    expect(providerIsUsable(provider({ base_url: '' }), inventory)).toBe(false)
  })

  it('checks the selected provider and model instead of any green provider', () => {
    const values = [provider(), provider({ id: 'ready', api_key_ref: null, models: [{ id: 'model-b', settings: { mode: 'inherit' } }] })]
    expect(usableProviderModels(values, credentials())).toBe(1)
    expect(modelSelectionIsUsable({ provider: 'named_provider', provider_id: 'fixture', model: 'model-a' }, values, credentials())).toBe(false)
    expect(modelSelectionIsUsable({ provider: 'named_provider', provider_id: 'ready', model: 'missing' }, values, credentials())).toBe(false)
    expect(modelSelectionIsUsable({ provider: 'named_provider', provider_id: 'ready', model: 'model-b' }, values, credentials())).toBe(true)
  })

  it('requires a usable effective Profile instead of assuming the default model works', () => {
    expect(modelSelectionIsUsable({ provider: 'profile_default' }, [], credentials())).toBe(false)
    expect(modelSelectionIsUsable({ provider: 'profile_default' }, [provider()], credentials())).toBe(false)
    expect(modelSelectionIsUsable({ provider: 'profile_default' }, [], credentials(), true)).toBe(true)
  })

  it('checks credentials for inline model selections while allowing no-key endpoints', () => {
    const selection = {
      provider: 'open_ai_compatible' as const, base_url: 'https://example.test/v1', model: 'fixture',
      api_key_env: 'INLINE_KEY', timeout_ms: 30_000, max_attempts: 2, retry_base_delay_ms: 250,
    }
    expect(modelSelectionIsUsable(selection, [], null)).toBe(false)
    expect(modelSelectionIsUsable(selection, [], credentials())).toBe(false)
    expect(modelSelectionIsUsable(selection, [], credentials(['INLINE_KEY']))).toBe(true)
    expect(modelSelectionIsUsable({ ...selection, api_key_env: null }, [], credentials())).toBe(true)
  })

  it('rejects the default rule model and disabled models while supporting registered model plugins', () => {
    const kinds = ['ternilo.model.rule', 'ternilo.model.openai_compatible', 'fixture.custom-model']
    const catalog: ApplicationCatalog = { revision: 'fixture', plugin_kinds: kinds, plugins: kinds.map(kind => ({
      kind, description: '', requires: [], provides: ['ternilo/models@3'], config_schema: {},
    })) }
    const profile: Profile = { plugins: [{ id: 'model', kind: kinds[0], enabled: true, config: {} }] }
    expect(profileModelIsUsable(profile, catalog, credentials())).toBe(false)
    profile.plugins[0].kind = kinds[2]
    expect(profileModelIsUsable(profile, catalog, credentials())).toBe(true)
    profile.plugins[0].enabled = false
    expect(profileModelIsUsable(profile, catalog, credentials())).toBe(false)
    expect(profileModelIsUsable(null, catalog, credentials())).toBe(false)
    expect(profileModelIsUsable(profile, null, credentials())).toBe(false)

    profile.plugins[0] = { id: 'model', kind: kinds[1], enabled: true, config: {
      base_url: 'https://example.test/v1', model: 'fixture', api_key_env: 'STARTUP_KEY',
    } }
    expect(profileModelIsUsable(profile, catalog, credentials())).toBe(false)
    expect(profileModelIsUsable(profile, catalog, credentials(['STARTUP_KEY']))).toBe(true)
    profile.plugins[0].config = { base_url: 'https://example.test/v1', model: 'fixture' }
    expect(profileModelIsUsable(profile, catalog, credentials())).toBe(true)
  })
})
