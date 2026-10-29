import type { ProviderDraft } from './provider-editor-card'
import type { ProviderProfile, ProviderWebAccess } from '@/types'
import { initialWebAccess } from './provider-web-editor'

export function providerCredentialReference(id: string) {
  const normalized = [...id].map(character => character === '-' ? '_H_' : character === '_' ? '_U_' : character.toUpperCase()).join('')
  return `TERNILO_PROVIDER_${normalized}_API_KEY`
}

export function prepareWebCredentials(draft: ProviderDraft, missingKey: string) {
  const writes: Array<{ name: string; value: string }> = []
  const prefix = providerCredentialReference(draft.id.trim())
  const prepare = (entry: Pick<ProviderDraft, 'webKeys' | 'sharedWebKey'>, overrides: Partial<ProviderWebAccess>, defaults: ProviderWebAccess, modelId?: string) => {
    const web = { search: { ...(overrides.search ?? defaults.search) }, fetch: { ...(overrides.fetch ?? defaults.fetch) } }
    const search = ['deepseek', 'brave', 'tavily', 'searxng'].includes(web.search.mode)
    const fetch = web.fetch.mode === 'deepseek'
    const shared = web.search.mode === 'deepseek' && fetch && (entry.sharedWebKey ?? (!web.fetch.api_key_ref || web.fetch.api_key_ref === web.search.api_key_ref))
    const scope = modelId === undefined ? '' : `MODEL_${[...new TextEncoder().encode(modelId.trim())].map(byte => byte.toString(16).padStart(2, '0')).join('').toUpperCase()}_`
    for (const kind of ['search', 'fetch'] as const) {
      if (!overrides[kind]) continue
      if (modelId !== undefined && !web[kind].api_key_ref && web[kind].mode === defaults[kind].mode && web[kind].base_url === defaults[kind].base_url) {
        web[kind].api_key_ref = defaults[kind].api_key_ref
      }
      if (!(kind === 'search' ? search : fetch)) { web[kind].api_key_ref = null; continue }
      if (kind === 'fetch' && shared) { web.fetch.api_key_ref = web.search.api_key_ref; continue }
      const value = entry.webKeys?.[kind]?.trim()
      if (value) {
        // A model override must not overwrite an inherited Provider/sibling credential.
        const name = (modelId === undefined ? web[kind].api_key_ref : null) ?? `${prefix}_WEB_${scope}${kind.toUpperCase()}_${web[kind].mode.toUpperCase()}`
        web[kind].api_key_ref = name
        writes.push({ name, value })
      }
      if (!web[kind].api_key_ref && !(kind === 'search' && web.search.mode === 'searxng')) throw new Error(missingKey)
    }
    return web
  }
  const defaults = initialWebAccess(draft)
  const webAccess = draft.webAccess ? prepare(draft, draft.webAccess, defaults) : null
  const modelWebAccess = draft.models.map(model => {
    if (!model.webAccess || !model.webAccess.search && !model.webAccess.fetch) return undefined
    const web = prepare(model, model.webAccess, webAccess ?? defaults, model.id)
    return {
      ...(model.webAccess.search ? { search: web.search } : {}),
      ...(model.webAccess.fetch ? { fetch: web.fetch } : {}),
    }
  })
  return { webAccess, modelWebAccess, writes }
}

export function providerCredentialReferences(provider: ProviderProfile) {
  return [provider.api_key_ref, provider.web_access?.search.api_key_ref, provider.web_access?.fetch.api_key_ref,
    ...provider.models.flatMap(model => [model.web_access?.search?.api_key_ref, model.web_access?.fetch?.api_key_ref]),
  ].filter((value): value is string => Boolean(value))
}
