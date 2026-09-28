import * as React from 'react'
import { Laptop, ShieldCheck } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { ModelDeviceScopeEditor } from './model-device-scope-editor'
import type { ModelDeviceScope, ModelDeviceProvider } from './model-device-types'
import type { ModelEntitlement } from './model-service-api'
import { deviceLimitsDraft, ModelDeviceLimitsForm, parseDeviceLimits, type ModelDeviceLimitsError } from './model-device-limits-form'
import { errorMessage } from './model-service-ui'

interface Review { device_name: string; user_code: string; expires_at_ms: number; providers: ModelDeviceProvider[] }
export function ModelDeviceApproval() {
  const t = useTranslate('modelService')
  const { authRequired, serverIdentity, platform, logout } = useWorkbench()
  if (!platform || authRequired || !serverIdentity) return <main className="p-4 sm:p-8"><p role="status">{t('loading')}</p></main>
  return <DeviceApprovalForm key={serverIdentity.user.user_id} username={serverIdentity.user.username} logout={logout} />
}

function DeviceApprovalForm({ username, logout }: { username: string; logout(): void }) {
  const t = useTranslate('modelService')
  const [code, setCode] = React.useState(() => new URLSearchParams(location.search).get('code') ?? '')
  const [review, setReview] = React.useState<Review | null>(null)
  const [grants, setGrants] = React.useState<ModelEntitlement[]>([])
  const [nextCursor, setNextCursor] = React.useState<string | null>(null)
  const [scope, setScope] = React.useState<ModelDeviceScope>({ kind: 'account' })
  const [error, setError] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [done, setDone] = React.useState<'approved' | 'denied' | null>(null)
  const [limits, setLimits] = React.useState(() => deviceLimitsDraft())
  const [invalid, setInvalid] = React.useState<ModelDeviceLimitsError>()
  const operation = React.useRef<AbortController | null>(null)
  React.useEffect(() => () => operation.current?.abort(), [])
  const switchAccount = () => {
    operation.current?.abort()
    setReview(null); setGrants([]); setNextCursor(null); setError(''); setDone(null)
    setScope({ kind: 'account' })
    setLimits(deviceLimitsDraft()); setInvalid(undefined); setBusy(false)
    logout()
  }
  const inspect = React.useCallback(async (value: string) => {
    operation.current?.abort()
    const controller = new AbortController()
    operation.current = controller
    setBusy(true); setError(''); setReview(null)
    setGrants([]); setNextCursor(null); setScope({ kind: 'account' })
    setLimits(deviceLimitsDraft()); setInvalid(undefined)
    try {
      const [review, catalog] = await Promise.all([
        api.request<Review>(`/model-access/device-authorization?user_code=${encodeURIComponent(value)}`, { signal: controller.signal }),
        api.request<{ entitlements: ModelEntitlement[]; next_cursor: string | null }>('/model-access/catalog?limit=50', { signal: controller.signal }),
      ])
      if (controller.signal.aborted) return
      setReview(review); setGrants(catalog.entitlements); setNextCursor(catalog.next_cursor); setScope({ kind: 'account' })
    } catch (cause) { if (!controller.signal.aborted) setError(errorMessage(cause)) }
    finally { if (!controller.signal.aborted) setBusy(false) }
  }, [])
  React.useEffect(() => {
    const initial = new URLSearchParams(location.search).get('code')
    if (initial) void inspect(initial)
  }, [inspect])
  const loadMore = async () => {
    if (!nextCursor || busy) return
    const controller = new AbortController()
    operation.current?.abort(); operation.current = controller
    setBusy(true); setError('')
    try {
      const page = await api.request<{ entitlements: ModelEntitlement[]; next_cursor: string | null }>(`/model-access/catalog?limit=50&cursor=${encodeURIComponent(nextCursor)}`, { signal: controller.signal })
      if (controller.signal.aborted) return
      setGrants(current => [...current, ...page.entitlements]); setNextCursor(page.next_cursor)
    } catch (cause) { if (!controller.signal.aborted) setError(errorMessage(cause)) }
    finally { if (!controller.signal.aborted) setBusy(false) }
  }
  const decide = async (allow: boolean) => {
    if (!review || busy) return
    if (allow && scope.kind === 'selected' && !scope.grants.length && !scope.providers?.length) return
    const parsed = allow ? parseDeviceLimits(limits) : null
    setInvalid(parsed?.error); setError('')
    if (parsed?.error) return
    const controller = new AbortController()
    operation.current?.abort(); operation.current = controller
    setBusy(true); setError('')
    try {
      await api.request('/model-access/device-authorization', { method: 'POST', signal: controller.signal, body: { user_code: review.user_code, scope: allow ? scope : null, ...(parsed ? { limits: parsed.limits } : {}) } })
      if (!controller.signal.aborted) setDone(allow ? 'approved' : 'denied')
    } catch (cause) { if (!controller.signal.aborted) setError(errorMessage(cause)) }
    finally { if (!controller.signal.aborted) setBusy(false) }
  }
  return <main className="flex h-full min-h-0 flex-col overflow-y-auto bg-background p-4 sm:p-8" data-model-device-approval=""><section className="mx-auto my-auto w-full max-w-lg shrink-0 space-y-5 rounded-2xl border bg-card p-5 sm:p-8">
    <ShieldCheck className="size-8 text-primary" /><div><h1 className="text-xl font-semibold">{t('deviceApprovalTitle')}</h1><p className="mt-2 text-sm leading-relaxed text-muted-foreground">{t('deviceApprovalDescription')}</p></div>
    {done ? <p role="status">{t(done === 'approved' ? 'deviceApproved' : 'deviceDenied')}</p> : <>
      <div className="flex flex-wrap items-center justify-between gap-2"><p className="min-w-0 text-sm [overflow-wrap:anywhere]">{t('deviceAccount', { name: username })}</p><Button type="button" className="min-h-10" size="sm" variant="outline" onClick={switchAccount}>{t('deviceSwitchAccount')}</Button></div>
      {!review ? <form className="space-y-3" onSubmit={event => { event.preventDefault(); if (!busy && code.trim()) void inspect(code.trim()) }}><Field><Label htmlFor="device-code">{t('deviceCode')}</Label><Input id="device-code" value={code} onChange={event => { operation.current?.abort(); setBusy(false); setError(''); setCode(event.target.value) }} autoComplete="off" spellCheck={false} maxLength={12} /></Field><Button className="min-h-10" disabled={busy || !code.trim()}>{t('deviceInspect')}</Button></form> : <form className="min-w-0 space-y-4" noValidate onSubmit={event => { event.preventDefault(); void decide(true) }}>
        <div className="rounded-xl border p-4"><p className="flex items-start gap-2 [overflow-wrap:anywhere]"><Laptop className="mt-1 size-4 shrink-0" />{review.device_name}</p><code className="mt-3 block text-xl tracking-widest">{review.user_code}</code></div>
        <p className="text-sm leading-relaxed">{t('deviceCodeCheck')}</p>
        <fieldset disabled={busy} className="min-w-0"><ModelDeviceScopeEditor scope={scope} onChange={setScope} grants={grants} providers={review.providers} /></fieldset>
        {nextCursor && <Button type="button" className="min-h-10" size="sm" variant="outline" disabled={busy} onClick={() => void loadMore()}>{t('loadMore')}</Button>}
        {!grants.length && !review.providers.length && <p className="text-sm text-muted-foreground">{t('deviceNoAllowance')}</p>}
        <ModelDeviceLimitsForm value={limits} onChange={setLimits} error={invalid} disabled={busy} />
        <div className="flex flex-wrap justify-end gap-2"><Button type="button" className="min-h-10" variant="outline" disabled={busy} onClick={() => void decide(false)}>{t('deviceReject')}</Button><Button type="submit" className="min-h-10" disabled={busy || (scope.kind === 'selected' && scope.grants.length === 0 && !scope.providers?.length)}>{t('deviceApprove')}</Button></div>
      </form>}
    </>}
    {error && <p role="alert" className="text-sm text-destructive [overflow-wrap:anywhere]">{error}</p>}
  </section></main>
}
