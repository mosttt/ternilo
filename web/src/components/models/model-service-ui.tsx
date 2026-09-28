import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, LoaderCircle, RefreshCw, Search, X } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label } from '@/components/ui/field'
import { DirectoryPagination } from '@/components/settings/directory-pagination'
import { pageQuery } from '@/components/settings/platform-admin-api'
import { useLocale, useTranslate } from '@/i18n/provider'
import type { ModelQuota } from './model-service-api'
import type { ProviderProtocol } from '@/types'
import css from './model-service.module.css'

export const errorMessage = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

function modelPagePath(path: string, query: string, cursor: string | null) {
  const [base, existing] = path.split('?')
  const parameters = new URLSearchParams(existing)
  for (const [key, value] of new URLSearchParams(pageQuery({ query, cursor }))) parameters.set(key, value)
  return `${base}?${parameters}`
}

export function useModelPage<T, K extends string = string>(path: string, field: K, tenantId?: string | null) {
  const [draft, setDraft] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [items, setItems] = React.useState<T[]>([])
  const [nextCursor, setNextCursor] = React.useState<string | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const cursor = cursors.at(-1) ?? null
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError(''); setItems([]); setNextCursor(null)
    void api.request<Record<K, T[]> & { next_cursor: string | null }>(modelPagePath(path, query, cursor), { signal: controller.signal, ...(tenantId ? { headers: { 'x-ternilo-tenant': tenantId } } : {}) })
      .then(page => { if (!controller.signal.aborted) { setItems(page[field]); setNextCursor(page.next_cursor) } })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [path, field, query, cursor, revision, tenantId])
  return { items, draft, setDraft, loading, error, reload, nextCursor, page: cursors.length,
    search: () => { setQuery(draft.trim()); setCursors([null]); reload() },
    previous: () => setCursors(current => current.slice(0, -1)), next: (next: string) => setCursors(current => [...current, next]),
  }
}

export function ModelDirectory<T>({ state, label, empty, actions, onRefresh, children }: {
  state: ReturnType<typeof useModelPage<T>>
  label: string
  empty?: string
  actions?: React.ReactNode
  onRefresh?(): void
  children: React.ReactNode
}) {
  const t = useTranslate('modelService')
  const id = React.useId()
  return <section className={css.directory}>
    <div className={css.search} role="search" aria-label={label} onKeyDown={event => { if (event.key === 'Enter' && event.target instanceof HTMLInputElement) { event.preventDefault(); event.stopPropagation(); state.search() } }}>
      <Field><Label htmlFor={id}>{label}</Label><Input id={id} placeholder={t('searchPlaceholder')} value={state.draft} onChange={event => state.setDraft(event.target.value)} /></Field>
      <Button variant="outline" type="button" onClick={state.search}><Search />{t('search')}</Button>
      <Button type="button" variant="ghost" disabled={state.loading} onClick={onRefresh ?? state.reload} aria-label={t('refresh')}><RefreshCw className={state.loading ? css.spinner : ''} /></Button>
      {actions}
    </div>
    {state.loading ? <p className={css.state} role="status"><LoaderCircle className={css.spinner} />{t('loading')}</p>
      : state.error ? <p className={css.state} role="alert">{state.error}<Button type="button" variant="outline" onClick={state.reload}>{t('retry')}</Button></p>
        : !state.items.length ? <p className={css.state}>{empty ?? t('empty')}</p> : children}
    <DirectoryPagination page={state.page} count={state.items.length} loading={state.loading} nextCursor={state.nextCursor} onPrevious={state.previous} onNext={state.next} />
  </section>
}

export function ModelModal({ title, description, busy = false, onClose, children }: {
  title: string; description: string; busy?: boolean; onClose(): void; children: React.ReactNode
}) {
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose() }}>
    <DialogContent className={css.modal} onEscapeKeyDown={event => { if (busy) event.preventDefault() }}>
      <DialogHeader><DialogTitle>{title}</DialogTitle><DialogDescription>{description}</DialogDescription></DialogHeader>
      <div className={css.modalBody}>{children}</div>
    </DialogContent>
  </Dialog>
}

export function ModelPickerList<T>({ path, field, label, selected, onPick, id, name, disabled = false, tenantId }: {
  path: string; field: string; label: string; selected: string[]; onPick(value: T): void;
  id(value: T): string; name(value: T): React.ReactNode; disabled?: boolean; tenantId?: string | null
}) {
  const state = useModelPage<T>(path, field, tenantId)
  return <div className={css.picker}>
    <ModelDirectory state={state} label={label}>
      <div className={css.pickerRows}>{state.items.map(item => <button type="button" key={id(item)} className={css.pick} aria-pressed={selected.includes(id(item))} disabled={disabled} onClick={() => onPick(item)}>
        <span>{name(item)}</span>{selected.includes(id(item)) && <Check aria-hidden="true" />}
      </button>)}</div>
    </ModelDirectory>
  </div>
}

export function SelectedModels({ ids, onRemove }: { ids: string[]; onRemove(id: string): void }) {
  const t = useTranslate('modelService')
  return <div className={css.selected}>{ids.map(id => <Button type="button" variant="outline" key={id} onClick={() => onRemove(id)} aria-label={`${t('remove')}: ${id}`}><code>{id}</code><X /></Button>)}</div>
}

export function QuotaFacts({ quota }: { quota: ModelQuota }) {
  const t = useTranslate('modelService')
  return <p className={css.hint}>{t('quotaFacts', { used: quota.used_tokens.toLocaleString(), limit: quota.limit_tokens.toLocaleString(), reserved: quota.reserved_tokens.toLocaleString() })}</p>
}

export function useModelDate() {
  const { locale } = useLocale()
  return (value: number) => new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(value))
}

export function useModelProtocol() {
  const t = useTranslate('modelService')
  return (protocol: ProviderProtocol) => t(protocol === 'google-gemini' ? 'protocolGemini' : protocol === 'anthropic-messages' ? 'protocolAnthropic' : protocol === 'deepseek-responses' ? 'protocolDeepSeekResponses' : protocol === 'openai-responses' ? 'protocolResponses' : 'protocolChat')
}

export function localDateInput(value: number | null) {
  if (value === null) return ''
  const date = new Date(value)
  return new Date(value - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16)
}

export function futureTimestamp(value: string, error: string) {
  if (!value) return null
  const parsed = new Date(value).getTime()
  if (!Number.isFinite(parsed) || parsed <= Date.now()) throw new Error(error)
  return parsed
}

export function CopyValue({ label, value, secret = false }: { label: string; value: string; secret?: boolean }) {
  const t = useTranslate('modelService')
  const [copied, setCopied] = React.useState(false)
  const [error, setError] = React.useState(false)
  const copy = async () => {
    setError(false)
    try { await copyText(value); setCopied(true) }
    catch { setError(true) }
  }
  return <div className={css.copyValue}>
    <span>{label}</span><div><code data-model-secret={secret || undefined}>{value}</code><Button type="button" variant="outline" onClick={() => void copy()} aria-label={`${t('copy')} ${label}`}>{copied ? <Check /> : <Copy />}{t(copied ? 'copied' : 'copy')}</Button></div>
    {error && <p role="alert">{t('copyError')}</p>}
  </div>
}
