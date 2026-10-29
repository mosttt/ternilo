import { describe, expect, it } from 'vitest'
import { changeProviderProtocol, providerModelDiscoveryRequest, type ProviderDraft } from './provider-editor-card'

function draft(): ProviderDraft {
  return {
    id: 'draft-provider',
    displayName: 'Draft Provider',
    baseUrl: ' https://draft.example/v2/ ',
    protocol: 'openai-responses',
    defaults: { contextWindow: '128K', maxOutputTokens: '16K' },
    models: [],
    timeoutMs: 42_000,
    maxAttempts: 2,
    retryBaseDelayMs: 50,
  }
}

describe('providerModelDiscoveryRequest', () => {
  it('clears protocol-specific hosted settings when switching provider protocols', () => {
    const original: ProviderDraft = { ...draft(), protocol: 'anthropic-messages', hostedTools: { web_search: true, web_fetch: false, max_uses: 2, max_content_tokens: 20000, allowed_domains: [], blocked_domains: [] } }
    expect(changeProviderProtocol(original, 'deepseek-responses').hostedTools).toBeNull()
    expect(changeProviderProtocol(original, 'anthropic-messages').hostedTools).toEqual(original.hostedTools)
    expect(changeProviderProtocol({ ...original, protocol: 'deepseek-responses' }, 'anthropic-messages').hostedTools).toBeNull()
  })

  it('selects native default URLs without overwriting custom relay endpoints or credentials', () => {
    const initial = { ...draft(), baseUrl: 'https://api.openai.com/v1' }
    expect(changeProviderProtocol(initial, 'deepseek-responses').baseUrl).toBe('https://api.deepseek.com')
    expect(changeProviderProtocol(draft(), 'deepseek-responses').baseUrl).toBe(draft().baseUrl)
    const gemini = changeProviderProtocol(initial, 'google-gemini')
    expect(gemini.baseUrl).toBe('https://generativelanguage.googleapis.com/v1beta')
    const claude = changeProviderProtocol(gemini, 'anthropic-messages')
    expect(claude.baseUrl).toBe('https://api.anthropic.com/v1')
    expect(providerModelDiscoveryRequest(claude, 'key').protocol).toBe('anthropic-messages')
    expect(changeProviderProtocol(draft(), 'google-gemini').baseUrl).toBe(draft().baseUrl)
    expect(changeProviderProtocol(gemini, 'openai-responses').baseUrl).toBe(initial.baseUrl)
  })
  it('sends an unsaved new Provider draft and its typed API key directly', () => {
    expect(providerModelDiscoveryRequest(draft(), '  draft-secret  ')).toEqual({
      base_url: 'https://draft.example/v2',
      protocol: 'openai-responses',
      timeout_ms: 42_000,
      api_key: 'draft-secret',
    })
  })

  it('identifies an existing Provider and leaves an empty key for stored-key resolution', () => {
    expect(providerModelDiscoveryRequest(draft(), '   ', 'saved-provider')).toEqual({
      provider_id: 'saved-provider',
      base_url: 'https://draft.example/v2',
      protocol: 'openai-responses',
      timeout_ms: 42_000,
      api_key: null,
    })
  })
})
