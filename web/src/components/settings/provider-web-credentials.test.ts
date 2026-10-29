import { describe, expect, it } from 'vitest'
import { prepareWebCredentials } from './provider-web-credentials'
import { initialWebAccess } from './provider-web-editor'
import { changeProviderProtocol, type ProviderDraft } from './provider-editor-card'

const draft: ProviderDraft = { id: 'my-provider', displayName: 'Provider', baseUrl: 'https://relay.example/v1', protocol: 'openai-responses', defaults: { contextWindow: '32K', maxOutputTokens: '2K' }, models: [], timeoutMs: 30000, maxAttempts: 1, retryBaseDelayMs: 250 }

describe('Provider web credentials', () => {
  it('shares an explicitly entered search credential with DeepSeek fetch without copying the model key', () => {
    const web = initialWebAccess(draft)
    web.search.mode = 'deepseek'; web.fetch.mode = 'deepseek'
    const result = prepareWebCredentials({ ...draft, webAccess: web, webKeys: { search: ' web-secret ', fetch: '' } }, 'missing')
    expect(result.writes).toEqual([{ name: 'TERNILO_PROVIDER_MY_H_PROVIDER_API_KEY_WEB_SEARCH_DEEPSEEK', value: 'web-secret' }])
    expect(result.webAccess?.search.api_key_ref).toBe(result.webAccess?.fetch.api_key_ref)
    expect(JSON.stringify(result.webAccess)).not.toContain('web-secret')
    expect(result.webAccess?.search.base_url).toBeUndefined()
    expect(() => prepareWebCredentials({ ...draft, webAccess: web }, 'missing')).toThrow('missing')
  })
  it('preserves saved references and only writes keys that the user replaces', () => {
    const web = initialWebAccess(draft)
    web.search.mode = 'brave'; web.search.api_key_ref = 'BRAVE_KEY'
    web.fetch.mode = 'deepseek'; web.fetch.api_key_ref = 'FETCH_KEY'
    const result = prepareWebCredentials({ ...draft, webAccess: web, webKeys: { search: '', fetch: 'replacement' } }, 'missing')
    expect(result.writes).toEqual([{ name: 'FETCH_KEY', value: 'replacement' }])
    expect(result.webAccess?.search.api_key_ref).toBe('BRAVE_KEY')
  })
  it('keeps standalone search across model protocol changes and clears only hosted choices', () => {
    const web = initialWebAccess(draft); web.search.mode = 'brave'; web.fetch.mode = 'hosted'
    const changed = changeProviderProtocol({ ...draft, protocol: 'anthropic-messages', webAccess: web }, 'deepseek-responses')
    expect(changed.webAccess?.search.mode).toBe('brave')
    expect(changed.webAccess?.fetch.mode).toBe('session')
    expect(initialWebAccess({ ...draft, hostedTools: { web_search: true, web_fetch: false, allowed_domains: [], blocked_domains: [] } }).search.mode).toBe('hosted')
  })
})
