import * as React from 'react'
import { RefreshCw } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { ModelPickerList, errorMessage } from './model-service-ui'
import css from './model-service.module.css'

interface Limits { requests_per_minute: number | null; max_concurrent_requests: number | null }
interface Policy { revision: number; policy: { platform: Limits; account_default: Limits } }
interface AccountPolicy { user_id: string; username: string; revision: number; limits: Limits | null; effective: Limits; platform: Limits; recent_requests: number; active_requests: number }
interface Target { user_id: string; username: string; name: string; kind: string; space_name: string | null }
type Draft = { rate: string; concurrent: string }
const blank = (): Draft => ({ rate: '', concurrent: '' })
const draft = (value: Limits): Draft => ({ rate: value.requests_per_minute?.toString() ?? '', concurrent: value.max_concurrent_requests?.toString() ?? '' })
function limits(value: Draft, invalid: string): Limits {
  const number = (text: string, maximum: number) => {
    if (!text.trim()) return null
    const value = Number(text)
    if (!Number.isInteger(value) || value < 1 || value > maximum) throw new Error(invalid)
    return value
  }
  return { requests_per_minute: number(value.rate, 1_000_000), max_concurrent_requests: number(value.concurrent, 10_000) }
}
function Fields({ title, value, change, disabled }: { title: string; value: Draft; change(value: Draft): void; disabled: boolean }) {
  const t = useTranslate('modelService'), id = React.useId()
  return <fieldset className="grid min-w-0 gap-3 rounded-lg border p-4" disabled={disabled}>
    <legend className="px-1 text-sm font-medium">{title}</legend>
    <Field><Label htmlFor={`${id}-rate`}>{t('traffic.rate')}</Label><Input id={`${id}-rate`} type="number" min={1} max={1_000_000} step={1} placeholder={t('traffic.unlimited')} value={value.rate} onChange={event => change({ ...value, rate: event.target.value })} /></Field>
    <Field><Label htmlFor={`${id}-concurrent`}>{t('traffic.concurrent')}</Label><Input id={`${id}-concurrent`} type="number" min={1} max={10_000} step={1} placeholder={t('traffic.unlimited')} value={value.concurrent} onChange={event => change({ ...value, concurrent: event.target.value })} /></Field>
  </fieldset>
}

export function ModelTrafficAdmin({ editable, readAccounts, manageAccounts }: { editable: boolean; readAccounts: boolean; manageAccounts: boolean }) {
  const t = useTranslate('modelService')
  const [policy, setPolicy] = React.useState<Policy | null>(null), [platform, setPlatform] = React.useState(blank), [account, setAccount] = React.useState(blank)
  const [error, setError] = React.useState(''), [saved, setSaved] = React.useState(false), [busy, setBusy] = React.useState(false)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const [selected, select] = React.useState<Target | null>(null)
  const mounted = React.useRef(false)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  React.useEffect(() => {
    const controller = new AbortController(); setPolicy(null); setError(''); setSaved(false)
    void api.request<Policy>('/admin/models/traffic', { signal: controller.signal }).then(value => {
      if (controller.signal.aborted) return
      setPolicy(value); setPlatform(draft(value.policy.platform)); setAccount(draft(value.policy.account_default))
    }).catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [revision])
  const save = async (event: React.FormEvent) => {
    event.preventDefault(); if (!policy || busy || !editable) return
    setError(''); setSaved(false); setBusy(true)
    try {
      const next = await api.request<Policy>('/admin/models/traffic', { method: 'PUT', body: { revision: policy.revision, policy: { platform: limits(platform, t('traffic.invalid')), account_default: limits(account, t('traffic.invalid')) } } })
      if (mounted.current) { setPolicy(next); setSaved(true) }
    } catch (cause) { if (mounted.current) setError(errorMessage(cause)) }
    finally { if (mounted.current) setBusy(false) }
  }
  return <section className="grid gap-5" data-model-traffic-admin="">
    <p className={css.hint}>{t('traffic.description')}</p>
    <form className="grid gap-4" onSubmit={event => void save(event)}>
      {policy ? <div className="grid min-w-0 gap-4 md:grid-cols-2">
        <Fields title={t('traffic.platform')} value={platform} change={value => { setPlatform(value); setSaved(false) }} disabled={!editable || busy} />
        <Fields title={t('traffic.accountDefault')} value={account} change={value => { setAccount(value); setSaved(false) }} disabled={!editable || busy} />
      </div> : !error && <p role="status">{t('loading')}</p>}
      <p className={css.hint}>{t('traffic.hint')}</p>
      <div className="flex flex-wrap gap-2">
        {editable && <Button disabled={busy || !policy}>{t(busy ? 'saving' : 'save')}</Button>}
        <Button type="button" variant="outline" disabled={busy} onClick={() => { setPolicy(null); setSaved(false); reload() }}><RefreshCw />{t('refresh')}</Button>
      </div>
      {saved && <p role="status">{t('traffic.saved')}</p>}
      {error && <p role="alert" className="text-destructive">{error}</p>}
    </form>
    {readAccounts && <section className="grid gap-4 border-t pt-5">
      <h3 className="text-sm font-medium">{t('traffic.accounts')}</h3>
      <ModelPickerList<Target> path="/admin/models/traffic/accounts" field="accounts" label={t('traffic.selectAccount')} selected={selected ? [selected.user_id] : []} onPick={select} id={value => value.user_id} name={value => <span className="min-w-0 break-words">{value.name}{value.kind === 'service' ? ` · ${t('traffic.service')} · ${value.space_name ?? ''}` : ''}</span>} />
      {selected && <AccountTrafficEditor key={`${selected.user_id}:${policy?.revision ?? 0}`} target={selected} editable={manageAccounts} />}
    </section>}
  </section>
}

function AccountTrafficEditor({ target, editable }: { target: Target; editable: boolean }) {
  const t = useTranslate('modelService')
  const [record, setRecord] = React.useState<AccountPolicy | null>(null), [value, setValue] = React.useState(blank), [inherit, setInherit] = React.useState(true)
  const [busy, setBusy] = React.useState(false), [error, setError] = React.useState(''), [saved, setSaved] = React.useState(false)
  const [revision, reload] = React.useReducer(value => value + 1, 0), mounted = React.useRef(false)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  React.useEffect(() => {
    const controller = new AbortController(); setRecord(null); setError('')
    void api.request<AccountPolicy>(`/admin/models/traffic/accounts/${encodeURIComponent(target.user_id)}`, { signal: controller.signal }).then(record => {
      if (controller.signal.aborted) return
      setRecord(record); setInherit(record.limits === null); setValue(draft(record.limits ?? record.effective))
    }).catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [target.user_id, revision])
  const save = async (event: React.FormEvent) => {
    event.preventDefault(); if (!record || busy || !editable) return
    setBusy(true); setError(''); setSaved(false)
    try {
      await api.request(`/admin/models/traffic/accounts/${encodeURIComponent(target.user_id)}`, { method: 'PUT', body: { revision: record.revision, limits: inherit ? null : limits(value, t('traffic.invalid')) } })
      if (mounted.current) { setSaved(true); reload() }
    } catch (cause) { if (mounted.current) setError(errorMessage(cause)) }
    finally { if (mounted.current) setBusy(false) }
  }
  return <form className="grid gap-4 rounded-xl border p-4" data-account-traffic-editor="" onSubmit={event => void save(event)}>
    <h4 className="break-words font-medium">{target.name}</h4>
    {record ? <>
      <p className={css.hint}>{t('traffic.usage', { recent: record.recent_requests, active: record.active_requests })}</p>
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={inherit} disabled={!editable || busy} onChange={event => { setInherit(event.target.checked); setSaved(false) }} />{t('traffic.inherit')}</label>
      <Fields title={t('traffic.accountOverride')} value={value} change={value => { setValue(value); setSaved(false) }} disabled={!editable || busy || inherit} />
      <p className={css.hint}>{t('traffic.overrideHint')}</p>
    </> : !error && <p role="status">{t('loading')}</p>}
    <div className="flex flex-wrap gap-2">
      {editable && <Button disabled={busy || !record}>{t(busy ? 'saving' : 'save')}</Button>}
      <Button type="button" variant="outline" disabled={busy} onClick={() => { setRecord(null); setSaved(false); reload() }}>{t('refresh')}</Button>
    </div>
    {saved && <p role="status">{t('traffic.saved')}</p>}
    {error && <p role="alert" className="text-destructive">{error}</p>}
  </form>
}

function OwnModelTrafficDetails() {
  const t = useTranslate('modelService'), { serverIdentity } = useWorkbench()
  const [record, setRecord] = React.useState<AccountPolicy | null>(null), [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController(); setRecord(null); setError('')
    void api.request<AccountPolicy>('/model-access/traffic', { signal: controller.signal }).then(value => { if (!controller.signal.aborted) setRecord(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [serverIdentity?.user.user_id, revision])
  return <div>
    <p className="mt-3 text-xs text-muted-foreground">{t('traffic.description')}</p>
    {record && <div className="mt-3 grid gap-3 sm:grid-cols-2">{([['traffic.platform', record.platform], ['traffic.accountOverride', record.effective]] as const).map(([title, value]) => <div key={title}><strong className="text-sm">{t(title)}</strong><p className={css.hint}>{t('traffic.rate')}: {value.requests_per_minute ?? t('traffic.unlimited')} · {t('traffic.concurrent')}: {value.max_concurrent_requests ?? t('traffic.unlimited')}</p></div>)}<p className={css.hint}>{t('traffic.usage', { recent: record.recent_requests, active: record.active_requests })}</p></div>}
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <Button type="button" variant="outline" className="mt-3" onClick={reload}><RefreshCw />{t('refresh')}</Button>
  </div>
}


export function OwnModelTraffic() {
  const t = useTranslate('modelService'), { serverIdentity } = useWorkbench()
  const [open, setOpen] = React.useState(false)
  return <details className="rounded-xl border p-4" data-own-model-traffic="" open={open} onToggle={event => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-sm font-medium">{t('traffic.ownTitle')}</summary>
    {open && <OwnModelTrafficDetails key={serverIdentity?.user.user_id} />}
  </details>
}
