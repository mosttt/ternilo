import type { ProviderDraft } from './provider-editor-card'
import type { ProviderProfile } from '@/types'

export function providerCredentialReference(id: string) {
  const normalized = [...id].map(character => character === '-' ? '_H_' : character === '_' ? '_U_' : character.toUpperCase()).join('')
  return `TERNILO_PROVIDER_${normalized}_API_KEY`
}

export function prepareWebCredentials(draft: ProviderDraft, missingKey: string) {
  if (!draft.webAccess) return { webAccess: draft.webAccess ?? null, writes: [] as Array<{ name: string; value: string }> }
  const web = { search: { ...draft.webAccess.search }, fetch: { ...draft.webAccess.fetch } }
  const writes: Array<{ name: string; value: string }> = []
  const prefix = providerCredentialReference(draft.id.trim())
  const search = ['deepseek','brave','tavily','searxng'].includes(web.search.mode)
  const fetch = web.fetch.mode === 'deepseek'
  const shared = web.search.mode === 'deepseek' && fetch && (draft.sharedWebKey ?? (!web.fetch.api_key_ref || web.fetch.api_key_ref === web.search.api_key_ref))
  for (const kind of ['search', 'fetch'] as const) {
    if (!(kind === 'search' ? search : fetch)) { web[kind].api_key_ref = null; continue }
    if (kind === 'fetch' && shared) { web.fetch.api_key_ref = web.search.api_key_ref; continue }
    const value = draft.webKeys?.[kind]?.trim()
    if (value) {
      const name = web[kind].api_key_ref ?? `${prefix}_WEB_${kind.toUpperCase()}_${web[kind].mode.toUpperCase()}`
      web[kind].api_key_ref = name
      writes.push({ name, value })
    }
    if (!web[kind].api_key_ref && !(kind === 'search' && web.search.mode === 'searxng')) throw new Error(missingKey)
  }
  return { webAccess: web, writes }
}

export function providerCredentialReferences(provider: ProviderProfile) {
  return [provider.api_key_ref, provider.web_access?.search.api_key_ref, provider.web_access?.fetch.api_key_ref].filter((value): value is string => Boolean(value))
}
