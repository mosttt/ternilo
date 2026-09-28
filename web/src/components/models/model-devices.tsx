import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { ModelDeviceIdentity, ModelDeviceUsage } from './model-device-types'
import { deviceLimitsDraft, ModelDeviceLimitsForm, parseDeviceLimits, useDeviceExpired, type ModelDeviceLimitsError } from './model-device-limits-form'
import { errorMessage, ModelDirectory, ModelModal, useModelDate, useModelPage } from './model-service-ui'
import css from './model-service.module.css'

export function ModelDevices() {
  const { serverIdentity, authRequired } = useWorkbench()
  const t = useTranslate('modelService')
  return !serverIdentity || authRequired ? <p role="status">{t('loading')}</p> : <ModelDevicesDirectory key={serverIdentity.user.user_id} />
}

function ModelDevicesDirectory() {
  const t = useTranslate('modelService')
  const directory = useModelPage<ModelDeviceIdentity>('/model-access/devices', 'devices')
  const [target, setTarget] = React.useState<{ device: ModelDeviceIdentity; action: 'edit' | 'revoke' | 'usage' } | null>(null)
  const complete = () => { setTarget(null); directory.reload() }
  return <div className={css.page}>
    <p className={css.hint}>{t('deviceDirectoryDescription')}</p>
    <p className={css.hint}>{t('deviceLimitsShared')}</p>
    <ModelDirectory state={directory} label={t('deviceTab')} empty={t('deviceEmpty')}>
      <div className={css.list}>{directory.items.map(device => <DeviceRow key={device.device_id} device={device} onEdit={() => setTarget({ device, action: 'edit' })} onRevoke={() => setTarget({ device, action: 'revoke' })} onUsage={() => setTarget({ device, action: 'usage' })} />)}</div>
    </ModelDirectory>
    {target?.action === 'edit' && <DeviceLimitsDialog key={target.device.device_id} device={target.device} onClose={() => setTarget(null)} onSaved={complete} />}
    {target?.action === 'revoke' && <DeviceRevokeDialog key={target.device.device_id} device={target.device} onClose={() => setTarget(null)} onRevoked={complete} />}
    {target?.action === 'usage' && <ModelModal title={t('deviceUsage')} description={t('deviceLimitsShared')} onClose={() => setTarget(null)}>
      <p className="text-sm font-medium [overflow-wrap:anywhere]">{target.device.device_name}</p>
      <DeviceUsage key={target.device.device_id} deviceId={target.device.device_id} />
      <div className="flex justify-end"><Button type="button" variant="outline" onClick={() => setTarget(null)}>{t('close')}</Button></div>
    </ModelModal>}
  </div>
}

function DeviceRow({ device, onEdit, onRevoke, onUsage }: { device: ModelDeviceIdentity; onEdit(): void; onRevoke(): void; onUsage(): void }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const expires = device.limits?.expires_at_ms ?? null
  const expired = useDeviceExpired(expires)
  const revoked = device.revoked_at_ms !== null
  const hint = React.useId()
  return <article className={css.row} data-model-device={device.device_id}>
    <div className={css.identity}>
      <strong>{device.device_name}</strong><span className={css.badge}>{t(revoked ? 'revoked' : expired ? 'expired' : 'keyUnrevoked')}</span>
      <code>{device.device_id}</code>
      <p>{device.scope.kind === 'account' ? t(device.scope.include_account_providers ? 'deviceScopeCombined' : 'deviceScopeAccount') : t('deviceScopeCount', { count: device.scope.grants.length })}</p>
      {device.scope.kind === 'selected' && Boolean(device.scope.providers?.length) && <p>{t('deviceAccountCount', { count: device.scope.providers!.length })}</p>}
      <p>{t('deviceMonthlySummary', { limit: device.limits?.monthly_tokens?.toLocaleString() ?? t('deviceNoLimit') })}</p>
      <p>{t('deviceConcurrentSummary', { limit: device.limits?.max_concurrent_requests?.toLocaleString() ?? t('deviceNoLimit') })}</p>
      <p>{t('deviceRateSummary', { limit: device.limits?.requests_per_minute?.toLocaleString() ?? t('deviceNoLimit') })}</p>
      <p>{expires === null ? t('noExpiry') : t('expires', { date: date(expires) })}</p>
      <p>{device.last_used_at_ms === null ? t('neverUsed') : t('keyLastUsed', { date: date(device.last_used_at_ms) })}</p>
      {(expired || revoked) && <p id={hint}>{t('deviceReauthorize')}</p>}
    </div>
    <div className={css.rowActions}>
      <Button type="button" variant="outline" disabled={expired || revoked} aria-describedby={expired || revoked ? hint : undefined} onClick={onEdit}>{t('deviceEditLimits')}</Button>
      {(expired || revoked) && <Button type="button" variant="outline" onClick={onUsage}>{t('deviceViewUsage')}</Button>}
      <Button type="button" variant="outline" disabled={revoked} onClick={onRevoke}>{t('revoke')}</Button>
    </div>
  </article>
}

function DeviceUsage({ deviceId }: { deviceId: string }) {
  const t = useTranslate('modelService')
  const [usage, setUsage] = React.useState<ModelDeviceUsage | null>(null)
  const [error, setError] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setUsage(null); setError(''); setLoading(true)
    void api.request<ModelDeviceUsage>(`/model-access/devices/${encodeURIComponent(deviceId)}/usage`, { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setUsage(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [deviceId, revision])
  return <section className="min-w-0 space-y-3 rounded-xl border p-3 sm:p-4" data-device-usage="" aria-busy={loading}>
    <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-medium">{t('deviceUsage')}</h3><Button type="button" variant="outline" size="sm" disabled={loading} onClick={reload}>{t(error ? 'retry' : 'refresh')}</Button></div>
    {loading ? <p className={css.hint} role="status">{t('loading')}</p> : error ? <p role="alert" className="text-sm text-destructive [overflow-wrap:anywhere]">{t('deviceUsageError', { message: error })}</p> : usage && <>
      <p className={css.hint}>{t('deviceUsageMonth', { month: usage.month })}</p>
      <dl className="grid gap-3 text-sm sm:grid-cols-3">
        <div><dt className={css.hint}>{t('deviceUsedTokens')}</dt><dd className="tabular-nums [overflow-wrap:anywhere]" data-device-used="">{usage.used_tokens.toLocaleString()}</dd></div>
        <div><dt className={css.hint}>{t('deviceReservedTokens')}</dt><dd className="tabular-nums [overflow-wrap:anywhere]" data-device-reserved="">{usage.reserved_tokens.toLocaleString()}</dd></div>
        <div><dt className={css.hint}>{t('deviceActiveRequests')}</dt><dd className="tabular-nums [overflow-wrap:anywhere]" data-device-active="">{usage.active_requests.toLocaleString()}</dd></div>
      </dl>
    </>}
    <p className={css.hint}>{t('deviceUsageReservedHint')}</p>
  </section>
}

function DeviceLimitsDialog({ device, onClose, onSaved }: { device: ModelDeviceIdentity; onClose(): void; onSaved(): void }) {
  const t = useTranslate('modelService')
  const [draft, setDraft] = React.useState(() => deviceLimitsDraft(device.limits))
  const [invalid, setInvalid] = React.useState<ModelDeviceLimitsError>()
  const [error, setError] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const mounted = React.useRef(false)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const expired = useDeviceExpired(device.limits?.expires_at_ms ?? null)
  const inactive = expired || device.revoked_at_ms !== null
  const save = async (event: React.FormEvent) => {
    event.preventDefault()
    if (busy || inactive) return
    const result = parseDeviceLimits(draft, device.limits)
    setInvalid(result.error); setError('')
    if (result.error) return
    setBusy(true)
    try {
      await api.request<ModelDeviceIdentity>(`/model-access/devices/${encodeURIComponent(device.device_id)}`, { method: 'PATCH', body: result.limits })
      if (mounted.current) onSaved()
    } catch (cause) { if (mounted.current) setError(errorMessage(cause)) }
    finally { if (mounted.current) setBusy(false) }
  }
  return <ModelModal title={t('deviceEditLimits')} description={t('deviceEditDescription')} busy={busy} onClose={onClose}>
    <p className="text-sm font-medium [overflow-wrap:anywhere]">{device.device_name}</p>
    <form className="min-w-0 space-y-4" noValidate onSubmit={event => void save(event)}>
      <ModelDeviceLimitsForm value={draft} onChange={setDraft} error={invalid} disabled={busy || inactive} />
      {inactive && <p role="status" className={css.hint}>{t('deviceReauthorize')}</p>}
      {error && <p role="alert" className="text-sm text-destructive [overflow-wrap:anywhere]">{t('deviceSaveError', { message: error })}</p>}
      <div className="flex flex-wrap justify-end gap-2"><Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t('cancel')}</Button><Button type="submit" disabled={busy || inactive}>{t(busy ? 'saving' : 'save')}</Button></div>
    </form>
    <DeviceUsage deviceId={device.device_id} />
  </ModelModal>
}

function DeviceRevokeDialog({ device, onClose, onRevoked }: { device: ModelDeviceIdentity; onClose(): void; onRevoked(): void }) {
  const t = useTranslate('modelService')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const mounted = React.useRef(false)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const revoke = async () => {
    if (busy) return
    setBusy(true); setError('')
    try {
      await api.request(`/model-access/devices/${encodeURIComponent(device.device_id)}`, { method: 'DELETE' })
      if (mounted.current) onRevoked()
    } catch (cause) { if (mounted.current) setError(errorMessage(cause)) }
    finally { if (mounted.current) setBusy(false) }
  }
  return <ActionDialog open title={t('deviceDisconnect')} description={t('deviceDisconnectDescription')} cancelLabel={t('cancel')} confirmLabel={t('revoke')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) onClose() }} onConfirm={() => void revoke()} />
}
