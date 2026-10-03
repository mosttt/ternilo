import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'

export function AccountEmail() {
  const t = useTranslate('settings')
  const [status, setStatus] = React.useState<{ verified_at_ms: number | null } | null>(null)
  const [error, setError] = React.useState(''), [busy, setBusy] = React.useState(false)
  const [sent, setSent] = React.useState(false), [cooldown, setCooldown] = React.useState(false), [revision, setRevision] = React.useState(0)
  React.useEffect(() => {
    const controller = new AbortController()
    void api.request<{ verified_at_ms: number | null }>('/auth/email', { signal: controller.signal }).then(setStatus).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause))
    })
    return () => controller.abort()
  }, [revision])
  React.useEffect(() => {
    if (!cooldown) return
    const timer = setTimeout(() => setCooldown(false), 60_000)
    return () => clearTimeout(timer)
  }, [cooldown])
  const send = async () => {
    if (busy) return
    setBusy(true); setError('')
    try { await api.request('/auth/email/send', { method: 'POST' }); setSent(true); setCooldown(true) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  return <div className="mt-4 grid gap-2 text-sm" data-account-email-verification="">
    <p>{t(status?.verified_at_ms != null ? 'account.emailVerified' : 'account.emailUnverified')}</p>
    {status && status.verified_at_ms == null && <Button type="button" className="w-fit" variant="outline" disabled={busy || cooldown} onClick={() => void send()}>{t(busy ? 'account.emailSending' : 'account.emailSend')}</Button>}
    {sent && <p className="text-muted-foreground" role="status">{t('account.emailSent')}</p>}
    {error && <div role="alert"><p className="text-destructive">{error}</p>{!status && <Button type="button" variant="ghost" onClick={() => { setError(''); setRevision(value => value + 1) }}>{t('account.retry')}</Button>}</div>}
  </div>
}
