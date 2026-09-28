import * as React from 'react'
import { ExternalLink, Plus, RefreshCw, Unplug } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { invalidateAllProviderInventories } from '@/domain/provider-inventory'
import type { ConnectionAuthorization, ConnectionPoll, ModelConnection } from '@/components/models/model-device-types'
import { useDeviceExpired } from '@/components/models/model-device-limits-form'
import { useModelDate } from '@/components/models/model-service-ui'
import { ActionDialog } from './settings-ui'

export function ModelConnectionsSettings({ onChange }: { onChange(): Promise<void> }) {
  const t = useTranslate('modelService')
  const common = useTranslate('common')
  const [connections, setConnections] = React.useState<ModelConnection[]>([])
  const [editing, setEditing] = React.useState(false)
  const [url, setUrl] = React.useState('')
  const [name, setName] = React.useState('')
  const [authorization, setAuthorization] = React.useState<ConnectionAuthorization | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [notice, setNotice] = React.useState('')
  const [removing, setRemoving] = React.useState<ModelConnection | null>(null)
  const [removeError, setRemoveError] = React.useState('')
  const load = React.useCallback(async () => setConnections(await api.request<ModelConnection[]>('/model-connections')), [])
  const changed = React.useCallback(async () => {
    invalidateAllProviderInventories()
    await Promise.all([load(), onChange()])
  }, [load, onChange])
  React.useEffect(() => { void load().catch(cause => setError(String(cause))) }, [load])
  React.useEffect(() => {
    if (!authorization) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      try {
        const result = await api.request<ConnectionPoll>(`/model-connections/authorize/${authorization.attempt_id}`, { method: 'POST' })
        if (cancelled) return
        if (result.status === 'pending') { timer = setTimeout(poll, result.interval * 1000); return }
        setAuthorization(null)
        if (result.status === 'connected') {
          setEditing(false)
          setNotice(t('deviceConnected'))
          await changed()
        } else setError(t(result.status === 'denied' ? 'deviceDenied' : 'deviceExpired'))
      } catch (cause) {
        if (!cancelled) { setError(String(cause)); timer = setTimeout(poll, Math.max(5, authorization.interval) * 1000) }
      }
    }
    timer = setTimeout(poll, authorization.interval * 1000)
    return () => { cancelled = true; clearTimeout(timer) }
  }, [authorization, changed, t])
  const begin = async () => {
    setBusy(true); setError(''); setNotice('')
    try { setAuthorization(await api.request<ConnectionAuthorization>('/model-connections/authorize', { method: 'POST', body: { server_url: url.trim(), name: name.trim() } })) }
    catch (cause) { setError(String(cause)) }
    finally { setBusy(false) }
  }
  const cancel = async () => {
    if (authorization) await api.request(`/model-connections/authorize/${authorization.attempt_id}`, { method: 'DELETE' }).catch(() => {})
    setAuthorization(null); setEditing(false)
  }
  const refresh = async (id: string) => {
    setBusy(true); setError(''); setNotice('')
    try { await api.request(`/model-connections/${id}/refresh`, { method: 'POST' }); await changed(); setNotice(t('deviceRefreshed')) }
    catch (cause) { setError(String(cause)) }
    finally { setBusy(false) }
  }
  const remove = async (revoke: boolean) => {
    if (!removing) return
    setBusy(true); setRemoveError('')
    try { await api.request(`/model-connections/${removing.connection_id}?revoke=${revoke}`, { method: 'DELETE' }); await changed(); setRemoving(null) }
    catch (cause) { setRemoveError(String(cause)) }
    finally { setBusy(false) }
  }
  return <section className="space-y-3 rounded-xl border bg-card p-4 sm:p-5" data-model-connections="">
    <div className="flex flex-wrap items-start justify-between gap-3"><div className="min-w-0"><h3 className="text-sm font-semibold">{t('deviceConnections')}</h3><p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t('deviceConnectionsDescription')}</p></div><Button className="min-h-10" size="sm" variant="outline" disabled={editing} onClick={() => { setEditing(true); setError(''); setNotice('') }}><Plus />{t('deviceConnect')}</Button></div>
    {connections.map(connection => <div className="rounded-lg border p-3" key={connection.connection_id} data-model-connection={connection.connection_id}>
      <div className="flex flex-wrap items-start justify-between gap-2"><div className="min-w-0 flex-1"><strong className="break-words text-sm">{connection.name}</strong><p className="mt-1 break-all text-xs text-muted-foreground">{connection.server_url}</p><p className="mt-2 break-words text-sm">{connection.session.identity.username}</p><p className="mt-1 break-words text-xs text-muted-foreground">{[...connection.session.grants.map(grant => grant.grant_name), ...(connection.session.providers ?? []).map(provider => `${t('accountSourceTab')} · ${provider.provider_name}`)].join(' · ')}</p><p className="mt-1 text-xs text-muted-foreground">{t('deviceModelCount', { count: [...connection.session.grants, ...(connection.session.providers ?? [])].reduce((total, source) => total + source.models.length, 0) })}</p></div><div className="flex gap-1"><Button className="min-h-10" size="icon-sm" variant="ghost" disabled={busy} aria-label={t('deviceRefreshName', { name: connection.name })} onClick={() => void refresh(connection.connection_id)}><RefreshCw /></Button><Button className="min-h-10" size="icon-sm" variant="ghost" disabled={busy} aria-label={t('deviceDisconnectName', { name: connection.name })} onClick={() => { setRemoveError(''); setRemoving(connection) }}><Unplug /></Button></div></div>
      <ConnectionLimits connection={connection} />
    </div>)}
    {editing && <form className="space-y-3 border-t pt-4" onSubmit={event => { event.preventDefault(); void begin() }}>
      {authorization ? <div className="space-y-3" role="status"><p className="text-sm">{t('deviceVerifyInstruction')}</p><code className="block text-center text-2xl tracking-widest" data-device-user-code="">{authorization.user_code}</code><a className="flex min-h-10 items-center justify-center gap-2 rounded-lg border text-sm hover:bg-accent" href={authorization.verification_uri} target="_blank" rel="noreferrer"><ExternalLink className="size-4" />{t('deviceOpenVerification')}</a><p className="text-xs text-muted-foreground">{t('deviceWaiting')}</p></div> : <><Field><Label htmlFor="model-server-url">{t('deviceServerUrl')}</Label><Input id="model-server-url" type="url" value={url} onChange={event => setUrl(event.target.value)} placeholder="https://ai.example.com" required /></Field><Field><Label htmlFor="model-connection-name">{t('deviceConnectionName')}</Label><Input id="model-connection-name" value={name} onChange={event => setName(event.target.value)} maxLength={120} required /></Field></>}
      <div className="flex justify-end gap-2"><Button className="min-h-10" type="button" variant="ghost" onClick={() => void cancel()}>{common('cancel')}</Button>{!authorization && <Button className="min-h-10" disabled={busy || !url.trim() || !name.trim()}>{t('deviceConnect')}</Button>}</div>
    </form>}
    {error && <p className="break-words text-xs text-destructive" role="alert">{error}</p>}{notice && <p className="text-xs text-muted-foreground" role="status">{notice}</p>}
    <ActionDialog open={removing !== null} title={t('deviceDisconnect')} description={t('deviceDisconnectDescription')} cancelLabel={common('cancel')} confirmLabel={t('deviceDisconnect')} busyLabel={common('loading')} busy={busy} destructive error={removeError} onOpenChange={open => { if (!open) setRemoving(null) }} onConfirm={() => void remove(true)}>
      {removeError && <Button className="min-h-10" variant="outline" disabled={busy} onClick={() => void remove(false)}>{t('deviceForget')}</Button>}
    </ActionDialog>
  </section>
}

function ConnectionLimits({ connection }: { connection: ModelConnection }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const limits = connection.session.identity.limits
  const expiry = limits?.expires_at_ms ?? null
  const expired = useDeviceExpired(expiry)
  return <div className="mt-3 space-y-1 border-t pt-3 text-xs leading-relaxed text-muted-foreground" data-connection-limits="">
    <p>{t('deviceMonthlySummary', { limit: limits?.monthly_tokens?.toLocaleString() ?? t('deviceNoLimit') })}</p>
    <p>{t('deviceConcurrentSummary', { limit: limits?.max_concurrent_requests?.toLocaleString() ?? t('deviceNoLimit') })}</p>
    <p>{t('deviceRateSummary', { limit: limits?.requests_per_minute?.toLocaleString() ?? t('deviceNoLimit') })}</p>
    <p>{expiry === null ? t('noExpiry') : t('expires', { date: date(expiry) })}</p>
    {expired && <p role="status">{t('deviceConnectionExpiryHint')}</p>}
    <p>{t('deviceConnectionLimitsHint')}</p>
  </div>
}
