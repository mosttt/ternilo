import * as React from 'react'
import { Field, FieldDescription, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { ProviderWebAccess, WebSearchMode, WebFetchMode } from '@/types'
import type { ProviderDraft } from './provider-editor-card'
import { HostedToolsEditor } from './provider-hosted-tools'

type Capability = 'search' | 'fetch'

export function initialWebAccess(draft: ProviderDraft): ProviderWebAccess {
  return draft.webAccess ?? {
    search: { mode: draft.hostedTools?.web_search ? 'hosted' : 'session', timeout_ms: 30000, max_results: 10 },
    fetch: { mode: draft.hostedTools?.web_fetch ? 'hosted' : 'session', timeout_ms: 30000, max_bytes: 2097152 },
  }
}

export function ProviderWebEditor({ draft, onChange, computerEnabled, configuredCredentials, modelIndex }: {
  draft: ProviderDraft
  onChange(draft: ProviderDraft): void
  computerEnabled: boolean
  configuredCredentials?: ReadonlySet<string>
  modelIndex?: number
}) {
  const t = useTranslate('settings'), id = React.useId()
  const model = modelIndex === undefined ? undefined : draft.models[modelIndex]
  const entry = model ?? draft
  const defaults = initialWebAccess(draft)
  const web = { search: model?.webAccess?.search ?? defaults.search, fetch: model?.webAccess?.fetch ?? defaults.fetch }
  const inherited = (kind: Capability) => Boolean(model && !model.webAccess?.[kind])
  const searchService = !inherited('search') && ['deepseek', 'brave', 'tavily', 'searxng'].includes(web.search.mode)
  const fetchService = !inherited('fetch') && web.fetch.mode === 'deepseek'
  const bothDeepSeek = web.search.mode === 'deepseek' && fetchService
  const shared = bothDeepSeek && (entry.sharedWebKey ?? (!web.fetch.api_key_ref || web.fetch.api_key_ref === web.search.api_key_ref))
  const supportsHosted = draft.protocol === 'anthropic-messages' || draft.protocol === 'deepseek-responses'
  const hosted = web.search.mode === 'hosted' || web.fetch.mode === 'hosted'
    || draft.models.some(item => item.webAccess?.search?.mode === 'hosted' || item.webAccess?.fetch?.mode === 'hosted')
  const updateEntry = (change: Partial<typeof entry>) => {
    if (modelIndex === undefined) onChange({ ...draft, ...change } as ProviderDraft)
    else onChange({ ...draft, models: draft.models.map((item, index) => index === modelIndex ? { ...item, ...change } : item) })
  }
  const update = (kind: Capability, value: ProviderWebAccess[Capability] | undefined, resetKey = false) => {
    const webAccess = { ...(model ? model.webAccess : defaults), [kind]: value }
    if (!value) delete webAccess[kind]
    updateEntry({ webAccess, ...(resetKey ? { webKeys: { search: entry.webKeys?.search ?? '', fetch: entry.webKeys?.fetch ?? '', [kind]: '' }, sharedWebKey: undefined } : {}) })
  }
  const selectMode = (kind: Capability, mode: string) => {
    if (mode === 'inherit') { update(kind, undefined, true); return }
    // Reusing the same service can reuse its credential reference, never its secret value.
    const value = mode === defaults[kind].mode ? { ...defaults[kind] }
      : { ...web[kind], mode: mode as WebSearchMode & WebFetchMode, base_url: null, api_key_ref: null }
    update(kind, value, true)
  }
  const modeLabel = (mode: WebSearchMode | WebFetchMode) => mode === 'hosted' ? t('provider.webHosted')
    : mode === 'disabled' ? t('provider.webDisabled') : mode === 'session' ? t('provider.webSession')
      : mode === 'direct' ? t('provider.webDirect') : t('provider.webComputer', { service: { deepseek: 'DeepSeek', brave: 'Brave', tavily: 'Tavily', searxng: 'SearXNG' }[mode] })
  const usesDefaultCredential = (kind: Capability) => Boolean(model
    && web[kind].mode === defaults[kind].mode && web[kind].base_url === defaults[kind].base_url
    && (!web[kind].api_key_ref || web[kind].api_key_ref === defaults[kind].api_key_ref)
    && (defaults[kind].api_key_ref || draft.webKeys?.[kind]?.trim()))
  const keyInput = (kind: Capability) => <Field>
    <Label htmlFor={`${id}-${kind}-key`}>{t(kind === 'search' ? 'provider.webSearchKey' : 'provider.webFetchKey')}</Label>
    <Input id={`${id}-${kind}-key`} type="password" autoComplete="new-password" spellCheck={false} value={entry.webKeys?.[kind] ?? ''}
      placeholder={t(usesDefaultCredential(kind) ? 'provider.webProviderCredential' : web[kind].api_key_ref && (configuredCredentials?.has(web[kind].api_key_ref) ?? true) ? 'provider.keyConfiguredPlaceholder' : kind === 'search' && web.search.mode === 'searxng' ? 'provider.keyEmptyPlaceholder' : 'provider.webKeyEmpty')}
      onChange={event => updateEntry({ webKeys: { search: entry.webKeys?.search ?? '', fetch: entry.webKeys?.fetch ?? '', [kind]: event.target.value } })} />
  </Field>
  return <section className="mt-5 space-y-4 border-t pt-5" data-provider-web-access={model ? model.id : ''}>
    <div><h3 className="text-sm font-medium">{t('provider.webAccess')}</h3><p className="mt-1 text-sm text-muted-foreground">{t(model ? 'provider.webModelDescription' : 'provider.webAccessDescription')}</p></div>
    <div className="grid gap-4 sm:grid-cols-2">
      {(['search', 'fetch'] as const).map(kind => <Field key={kind}>
        <Label htmlFor={`${id}-${kind}`}>{t(kind === 'search' ? 'provider.webSearch' : 'provider.webFetch')}</Label>
        <Select className="[&>span:first-child]:whitespace-normal [&>span:first-child]:break-words" id={`${id}-${kind}`} value={inherited(kind) ? 'inherit' : web[kind].mode} onValueChange={mode => selectMode(kind, mode)}>
          {model && <option value="inherit">{t('provider.webInherit', { value: modeLabel(defaults[kind].mode) })}</option>}
          <option value="disabled">{t('provider.webDisabled')}</option>
          {(kind === 'search' ? supportsHosted : draft.protocol === 'anthropic-messages') && <option value="hosted">{t('provider.webHosted')}</option>}
          {computerEnabled && (kind === 'search' ? ['deepseek', 'brave', 'tavily', 'searxng'] : ['deepseek', 'direct']).map(mode => <option key={mode} value={mode}>{modeLabel(mode as WebSearchMode | WebFetchMode)}</option>)}
          <option value="session">{t('provider.webSession')}</option>
        </Select>
      </Field>)}
    </div>
    {(searchService || fetchService) && <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('provider.webCredentialNotice')}</p>
      {searchService && keyInput('search')}
      {bothDeepSeek && <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={shared} onChange={event => updateEntry({ sharedWebKey: event.target.checked })} />{t('provider.webSharedKey')}</label>}
      {fetchService && !shared && keyInput('fetch')}
      <details className="rounded-lg border p-3"><summary className="cursor-pointer text-sm">{t('provider.webAdvanced')}</summary><div className="mt-4 grid gap-4 sm:grid-cols-2">
        {searchService && <><Field><Label htmlFor={`${id}-search-url`}>{t('provider.webSearchAddress')}</Label><Input id={`${id}-search-url`} value={web.search.base_url ?? ''} placeholder={web.search.mode === 'deepseek' ? 'https://api.deepseek.com' : web.search.mode === 'brave' ? 'https://api.search.brave.com/res/v1' : web.search.mode === 'tavily' ? 'https://api.tavily.com' : 'https://search.example.com'} onChange={event => update('search', { ...web.search, base_url: event.target.value.trim() || null })} /></Field><Field><Label htmlFor={`${id}-count`}>{t('webTools.results')}</Label><Input id={`${id}-count`} type="number" min={1} max={['brave', 'tavily'].includes(web.search.mode) ? 20 : 50} value={web.search.max_results} onChange={event => update('search', { ...web.search, max_results: Number(event.target.value) })} /></Field></>}
        {fetchService && <Field><Label htmlFor={`${id}-fetch-url`}>{t('provider.webFetchAddress')}</Label><Input id={`${id}-fetch-url`} value={web.fetch.base_url ?? ''} placeholder="https://api.deepseek.com" onChange={event => update('fetch', { ...web.fetch, base_url: event.target.value.trim() || null })} /></Field>}
        {(['search', 'fetch'] as const).filter(kind => kind === 'search' ? searchService : fetchService).map(kind => <Field key={kind}><Label htmlFor={`${id}-${kind}-timeout`}>{t(kind === 'search' ? 'provider.webSearchTimeout' : 'provider.webFetchTimeout')}</Label><Input id={`${id}-${kind}-timeout`} type="number" min={1} value={web[kind].timeout_ms} onChange={event => update(kind, { ...web[kind], timeout_ms: Number(event.target.value) })} /></Field>)}
        {fetchService && <Field><Label htmlFor={`${id}-bytes`}>{t('webTools.bytes')}</Label><Input id={`${id}-bytes`} type="number" min={1} value={web.fetch.max_bytes} onChange={event => update('fetch', { ...web.fetch, max_bytes: Number(event.target.value) })} /></Field>}
      </div></details>
    </div>}
    {!model && hosted && <HostedToolsEditor controlsOnly protocol={draft.protocol} value={draft.hostedTools ?? {
      web_search: web.search.mode === 'hosted' || draft.models.some(item => item.webAccess?.search?.mode === 'hosted'),
      web_fetch: web.fetch.mode === 'hosted' || draft.models.some(item => item.webAccess?.fetch?.mode === 'hosted'),
      max_uses: draft.protocol === 'deepseek-responses' ? undefined : 5, max_content_tokens: draft.protocol === 'deepseek-responses' ? undefined : 20000,
      allowed_domains: [], blocked_domains: [],
    }} onChange={hostedTools => onChange({ ...draft, hostedTools })} />}
    {!computerEnabled && <FieldDescription>{t('provider.webPlatformNotice')}</FieldDescription>}
  </section>
}
