import * as React from 'react'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { ModelDeviceLimits } from './model-device-types'

export interface ModelDeviceLimitsDraft {
  monthlyTokens: string
  concurrentRequests: string
  requestsPerMinute: string
  expiresAt: string
}

export type ModelDeviceLimitsError = 'deviceMonthlyInvalid' | 'deviceConcurrentInvalid' | 'deviceRateInvalid' | 'deviceExpiryInvalid'

export function deviceLimitsDraft(limits?: ModelDeviceLimits): ModelDeviceLimitsDraft {
  const expiry = limits?.expires_at_ms
  const date = expiry == null ? null : new Date(expiry)
  return {
    monthlyTokens: limits?.monthly_tokens?.toString() ?? '',
    concurrentRequests: limits?.max_concurrent_requests?.toString() ?? '',
    requestsPerMinute: limits?.requests_per_minute?.toString() ?? '',
    expiresAt: date ? new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, -1) : '',
  }
}

export function parseDeviceLimits(draft: ModelDeviceLimitsDraft, original?: ModelDeviceLimits): { limits: ModelDeviceLimits; error?: never } | { limits?: never; error: ModelDeviceLimitsError } {
  const positiveInteger = (value: string) => /^\d+$/.test(value.trim()) && Number.isSafeInteger(Number(value)) && Number(value) > 0
  if (draft.monthlyTokens.trim() && !positiveInteger(draft.monthlyTokens)) return { error: 'deviceMonthlyInvalid' }
  if (draft.concurrentRequests.trim() && (!positiveInteger(draft.concurrentRequests) || Number(draft.concurrentRequests) > 10_000)) return { error: 'deviceConcurrentInvalid' }
  if (draft.requestsPerMinute.trim() && (!positiveInteger(draft.requestsPerMinute) || Number(draft.requestsPerMinute) > 10_000)) return { error: 'deviceRateInvalid' }
  const expires = original && draft.expiresAt === deviceLimitsDraft(original).expiresAt
    ? original.expires_at_ms : draft.expiresAt ? new Date(draft.expiresAt).getTime() : null
  if (expires !== null && (!Number.isFinite(expires) || expires <= Date.now() || expires > 253_402_300_799_999)) return { error: 'deviceExpiryInvalid' }
  return { limits: {
    monthly_tokens: draft.monthlyTokens.trim() ? Number(draft.monthlyTokens) : null,
    max_concurrent_requests: draft.concurrentRequests.trim() ? Number(draft.concurrentRequests) : null,
    ...(draft.requestsPerMinute.trim() ? { requests_per_minute: Number(draft.requestsPerMinute) } : {}),
    expires_at_ms: expires,
  } }
}

export function useDeviceExpired(expiresAt: number | null) {
  const [now, setNow] = React.useState(Date.now)
  React.useEffect(() => {
    if (expiresAt === null || expiresAt <= now) return
    const timer = window.setTimeout(() => setNow(Date.now()), Math.min(Math.max(0, expiresAt - Date.now()), 2_147_483_647))
    return () => window.clearTimeout(timer)
  }, [expiresAt, now])
  return expiresAt !== null && expiresAt <= Math.max(now, Date.now())
}

export function ModelDeviceLimitsForm({ value, onChange, disabled = false, error }: {
  value: ModelDeviceLimitsDraft
  onChange(value: ModelDeviceLimitsDraft): void
  disabled?: boolean
  error?: ModelDeviceLimitsError
}) {
  const t = useTranslate('modelService')
  const id = React.useId()
  return <fieldset disabled={disabled} className="min-w-0 space-y-4 rounded-xl border p-3 sm:p-4" data-device-limits="">
    <legend className="px-1 text-sm font-medium">{t('deviceLimits')}</legend>
    <p id={`${id}-hint`} className="text-xs leading-relaxed text-muted-foreground">{t('deviceLimitsShared')}</p>
    <p className="text-xs leading-relaxed text-muted-foreground">{t('deviceLimitsBlank')}</p>
    <div className="grid min-w-0 gap-4 sm:grid-cols-2">
      <Field className="min-w-0"><Label htmlFor={`${id}-monthly`}>{t('monthlyTokens')}</Label>
        <Input id={`${id}-monthly`} name="monthly_tokens" className="min-w-0" inputMode="numeric" autoComplete="off" value={value.monthlyTokens} onChange={event => onChange({ ...value, monthlyTokens: event.target.value })} aria-invalid={error === 'deviceMonthlyInvalid'} aria-describedby={error === 'deviceMonthlyInvalid' ? `${id}-error` : `${id}-hint`} />
      </Field>
      <Field className="min-w-0"><Label htmlFor={`${id}-concurrent`}>{t('concurrentRequests')}</Label>
        <Input id={`${id}-concurrent`} name="max_concurrent_requests" className="min-w-0" inputMode="numeric" autoComplete="off" value={value.concurrentRequests} onChange={event => onChange({ ...value, concurrentRequests: event.target.value })} aria-invalid={error === 'deviceConcurrentInvalid'} aria-describedby={error === 'deviceConcurrentInvalid' ? `${id}-error` : `${id}-hint`} />
      </Field>
    </div>
    <Field className="min-w-0"><Label htmlFor={`${id}-rate`}>{t('deviceRequestsPerMinute')}</Label>
      <Input id={`${id}-rate`} name="requests_per_minute" className="min-w-0" inputMode="numeric" autoComplete="off" value={value.requestsPerMinute} onChange={event => onChange({ ...value, requestsPerMinute: event.target.value })} aria-invalid={error === 'deviceRateInvalid'} aria-describedby={error === 'deviceRateInvalid' ? `${id}-error` : `${id}-rate-hint`} />
      <p id={`${id}-rate-hint`} className="text-xs leading-relaxed text-muted-foreground">{t('deviceRateHint')}</p>
    </Field>
    <Field className="min-w-0"><Label htmlFor={`${id}-expiry`}>{t('expiry')}</Label>
      <Input id={`${id}-expiry`} name="expires_at_ms" type="datetime-local" step="any" className="min-w-0 max-w-full" value={value.expiresAt} onChange={event => onChange({ ...value, expiresAt: event.target.value })} aria-invalid={error === 'deviceExpiryInvalid'} aria-describedby={error === 'deviceExpiryInvalid' ? `${id}-error` : `${id}-timezone`} />
      <p id={`${id}-timezone`} className="text-xs leading-relaxed text-muted-foreground">{t('deviceExpiryLocal', { zone: Intl.DateTimeFormat().resolvedOptions().timeZone })}</p>
    </Field>
    {error && <p id={`${id}-error`} role="alert" className="text-sm text-destructive">{t(error)}</p>}
    <p className="text-xs leading-relaxed text-muted-foreground">{t('deviceLimitsAccepted')}</p>
  </fieldset>
}
