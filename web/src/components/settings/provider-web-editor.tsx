import * as React from 'react'
import { Field, FieldDescription, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { ProviderWebAccess, WebSearchMode, WebFetchMode } from '@/types'
import type { ProviderDraft } from './provider-editor-card'
import { HostedToolsEditor } from './provider-hosted-tools'

export function initialWebAccess(draft: ProviderDraft): ProviderWebAccess {
  return draft.webAccess ?? {
    search: { mode: draft.hostedTools?.web_search ? 'hosted' : 'session', timeout_ms: 30000, max_results: 10 },
    fetch: { mode: draft.hostedTools?.web_fetch ? 'hosted' : 'session', timeout_ms: 30000, max_bytes: 2097152 },
  }
}

export function ProviderWebEditor({ draft, onChange, computerEnabled, credentialConfigured }: { draft: ProviderDraft; onChange(draft: ProviderDraft): void; computerEnabled: boolean; credentialConfigured?: { search: boolean; fetch: boolean } }) {
  const t = useTranslate('settings'), id = React.useId()
  const web = initialWebAccess(draft)
  const searchService = ['deepseek', 'brave', 'tavily', 'searxng'].includes(web.search.mode)
  const fetchService = web.fetch.mode === 'deepseek'
  const bothDeepSeek = web.search.mode === 'deepseek' && fetchService
  const shared = bothDeepSeek && (draft.sharedWebKey ?? (!web.fetch.api_key_ref || web.fetch.api_key_ref === web.search.api_key_ref))
  const supportsHosted = draft.protocol === 'anthropic-messages' || draft.protocol === 'deepseek-responses' && draft.models.some(model => model.id === 'deepseek-v4-pro')
  const hosted = web.search.mode === 'hosted' || web.fetch.mode === 'hosted'
  const update = (next: ProviderWebAccess, resetKey?: 'search' | 'fetch') => {
    const native = next.search.mode === 'hosted' || next.fetch.mode === 'hosted'
    onChange({ ...draft, ...(resetKey ? { webKeys: { search: draft.webKeys?.search ?? '', fetch: draft.webKeys?.fetch ?? '', [resetKey]: '' } } : {}), webAccess: next, hostedTools: native ? {
      ...(draft.hostedTools ?? { max_uses: draft.protocol === 'deepseek-responses' ? undefined : 5, max_content_tokens: draft.protocol === 'deepseek-responses' ? undefined : 20000, allowed_domains: [], blocked_domains: [] }),
      web_search: next.search.mode === 'hosted', web_fetch: next.fetch.mode === 'hosted',
    } : null })
  }
  const keyInput = (kind: 'search' | 'fetch') => <Field>
    <Label htmlFor={`${id}-${kind}-key`}>{t(kind === 'search' ? 'provider.webSearchKey' : 'provider.webFetchKey')}</Label>
    <Input id={`${id}-${kind}-key`} type="password" autoComplete="new-password" spellCheck={false} value={draft.webKeys?.[kind] ?? ''}
      placeholder={t(web[kind].api_key_ref && (credentialConfigured?.[kind] ?? true) ? 'provider.keyConfiguredPlaceholder' : kind === 'search' && web.search.mode === 'searxng' ? 'provider.keyEmptyPlaceholder' : 'provider.webKeyEmpty')}
      onChange={event => onChange({ ...draft, webKeys: { search: draft.webKeys?.search ?? '', fetch: draft.webKeys?.fetch ?? '', [kind]: event.target.value } })} />
  </Field>
  return <section className="mt-5 space-y-4 border-t pt-5" data-provider-web-access="">
    <div><h3 className="text-sm font-medium">{t('provider.webAccess')}</h3><p className="mt-1 text-sm text-muted-foreground">{t('provider.webAccessDescription')}</p></div>
    <div className="grid gap-4 sm:grid-cols-2">
      <Field><Label htmlFor={`${id}-search`}>{t('provider.webSearch')}</Label><Select id={`${id}-search`} value={web.search.mode} onValueChange={mode => update({ ...web, search: { ...web.search, mode: mode as WebSearchMode, base_url: null, api_key_ref: null } }, 'search')}>
        <option value="session">{t('provider.webSession')}</option><option value="disabled">{t('provider.webDisabled')}</option>
        {computerEnabled && [['deepseek','DeepSeek'],['brave','Brave'],['tavily','Tavily'],['searxng','SearXNG']].map(([value, service]) => <option key={value} value={value}>{t('provider.webComputer', { service })}</option>)}
        {(supportsHosted || web.search.mode === 'hosted') && <option value="hosted" disabled={!supportsHosted}>{t('provider.webHosted')}</option>}
      </Select></Field>
      <Field><Label htmlFor={`${id}-fetch`}>{t('provider.webFetch')}</Label><Select id={`${id}-fetch`} value={web.fetch.mode} onValueChange={mode => update({ ...web, fetch: { ...web.fetch, mode: mode as WebFetchMode, base_url: null, api_key_ref: null } }, 'fetch')}>
        <option value="session">{t('provider.webSession')}</option><option value="disabled">{t('provider.webDisabled')}</option>
        {computerEnabled && <option value="direct">{t('provider.webDirect')}</option>}{computerEnabled && <option value="deepseek">{t('provider.webComputer', { service: 'DeepSeek' })}</option>}
        {draft.protocol === 'anthropic-messages' && <option value="hosted">{t('provider.webHosted')}</option>}
      </Select></Field>
    </div>
    {(searchService || fetchService) && <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('provider.webCredentialNotice')}</p>
      {searchService && keyInput('search')}
      {bothDeepSeek && <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={shared} onChange={event => onChange({ ...draft, sharedWebKey: event.target.checked })} />{t('provider.webSharedKey')}</label>}
      {fetchService && !shared && keyInput('fetch')}
      <details className="rounded-lg border p-3"><summary className="cursor-pointer text-sm">{t('provider.webAdvanced')}</summary><div className="mt-4 grid gap-4 sm:grid-cols-2">
        {searchService && <><Field><Label htmlFor={`${id}-search-url`}>{t('provider.webSearchAddress')}</Label><Input id={`${id}-search-url`} value={web.search.base_url ?? ''} placeholder={web.search.mode === 'deepseek' ? 'https://api.deepseek.com' : web.search.mode === 'brave' ? 'https://api.search.brave.com/res/v1' : web.search.mode === 'tavily' ? 'https://api.tavily.com' : 'https://search.example.com'} onChange={event => update({ ...web, search: { ...web.search, base_url: event.target.value.trim() || null } })} /></Field><Field><Label htmlFor={`${id}-count`}>{t('webTools.results')}</Label><Input id={`${id}-count`} type="number" min={1} max={['brave','tavily'].includes(web.search.mode) ? 20 : 50} value={web.search.max_results} onChange={event => update({ ...web, search: { ...web.search, max_results: Number(event.target.value) } })} /></Field></>}
        {fetchService && <Field><Label htmlFor={`${id}-fetch-url`}>{t('provider.webFetchAddress')}</Label><Input id={`${id}-fetch-url`} value={web.fetch.base_url ?? ''} placeholder="https://api.deepseek.com" onChange={event => update({ ...web, fetch: { ...web.fetch, base_url: event.target.value.trim() || null } })} /></Field>}
        {(['search','fetch'] as const).filter(kind => kind === 'search' ? searchService : fetchService).map(kind => <Field key={kind}><Label htmlFor={`${id}-${kind}-timeout`}>{t(kind === 'search' ? 'provider.webSearchTimeout' : 'provider.webFetchTimeout')}</Label><Input id={`${id}-${kind}-timeout`} type="number" min={1} value={web[kind].timeout_ms} onChange={event => update({ ...web, [kind]: { ...web[kind], timeout_ms: Number(event.target.value) } })} /></Field>)}
        {fetchService && <Field><Label htmlFor={`${id}-bytes`}>{t('webTools.bytes')}</Label><Input id={`${id}-bytes`} type="number" min={1} value={web.fetch.max_bytes} onChange={event => update({ ...web, fetch: { ...web.fetch, max_bytes: Number(event.target.value) } })} /></Field>}
      </div></details>
    </div>}
    {hosted && <HostedToolsEditor controlsOnly protocol={draft.protocol} models={draft.models.map(model => model.id)} value={draft.hostedTools ?? { web_search: web.search.mode === 'hosted', web_fetch: web.fetch.mode === 'hosted', allowed_domains: [], blocked_domains: [] }} onChange={hostedTools => onChange({ ...draft, webAccess: web, hostedTools })} />}
    {!computerEnabled && <FieldDescription>{t('provider.webPlatformNotice')}</FieldDescription>}
  </section>
}
