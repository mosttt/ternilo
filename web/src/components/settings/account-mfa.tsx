import * as React from 'react'
import { api } from '@/api/client'
import { copyText } from '@/lib/clipboard'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'

interface MfaStatus { enabled: boolean; enabled_at_ms: number | null; recovery_codes_remaining: number }
interface Enrollment { generation: string; secret: string; qr_code: string; recovery_codes: string[]; expires_at_ms: number }

export function AccountMfa() {
  const t = useTranslate('settings'), id = React.useId()
  const { logout } = useWorkbench()
  const [status, setStatus] = React.useState<MfaStatus | null>(null), [setup, setSetup] = React.useState<Enrollment | null>(null)
  const [password, setPassword] = React.useState(''), [code, setCode] = React.useState('')
  const [saved, setSaved] = React.useState(false), [busy, setBusy] = React.useState(false), [error, setError] = React.useState(''), [revision, setRevision] = React.useState(0)
  const mounted = React.useRef(false)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  React.useEffect(() => {
    const controller = new AbortController()
    void api.request<MfaStatus>('/auth/mfa', { signal: controller.signal }).then(value => { if (!controller.signal.aborted) setStatus(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => controller.abort()
  }, [revision])
  const describeError = (cause: unknown) => {
    const message = cause instanceof Error ? cause.message : String(cause)
    if (message === 'invalid username or password') return t('account.invalidPassword')
    if (message === 'verification code is invalid, already used or temporarily limited') return t(setup && !status?.enabled ? 'mfa.invalidSetupCode' : 'mfa.invalidCode')
    if (message === 'MFA enrollment changed or expired; start again') return t('mfa.expiredSetup')
    return message
  }
  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (busy || !status) return
    setBusy(true); setError('')
    try {
      if (status.enabled) {
        await api.request('/auth/mfa/disable', { method: 'POST', body: { current_password: password, code } })
        if (mounted.current) { setPassword(''); setCode(''); logout() }
      } else if (setup) {
        if (!saved) return
        await api.request('/auth/mfa/enable', { method: 'POST', body: { current_password: password, generation: setup.generation, code } })
        if (mounted.current) { setSetup(null); setPassword(''); setCode(''); logout() }
      } else {
        const value = await api.request<Enrollment>('/auth/mfa/setup', { method: 'POST', body: { current_password: password } })
        if (mounted.current) { setSetup(value); setSaved(false); setCode('') }
      }
    } catch (cause) { if (mounted.current) setError(describeError(cause)) }
    finally { if (mounted.current) setBusy(false) }
  }
  const copy = async (value: string) => {
    try { await copyText(value) } catch { if (mounted.current) setError(t('mfa.copyFailed')) }
  }
  return <details className="mt-6 border-t pt-5" data-account-mfa="">
    <summary className="cursor-pointer text-sm font-medium">{t('mfa.title')}{status?.enabled ? ` · ${t('mfa.enabled')}` : ''}</summary>
    <p className="mt-2 text-xs text-muted-foreground">{t('mfa.description')}</p>
    {!status ? <Button type="button" className="mt-3" variant="outline" onClick={() => { setError(''); setRevision(value => value + 1) }}>{t('account.retry')}</Button>
      : <form className="mt-4 grid max-w-xl gap-4" onSubmit={event => void submit(event)}>
        {status.enabled && <p className="text-sm" data-mfa-recovery-count="">{t('mfa.remaining', { count: status.recovery_codes_remaining })}</p>}
        <Field><Label htmlFor={`${id}-password`}>{t('account.currentPassword')}</Label><Input id={`${id}-password`} type="password" autoComplete="current-password" required maxLength={1024} disabled={busy} value={password} onChange={event => setPassword(event.target.value)} /></Field>
        {setup && <div className="grid min-w-0 gap-4" data-mfa-enrollment="">
          <p className="text-sm">{t('mfa.scan')}</p>
          <img className="size-48 justify-self-center rounded-lg bg-white p-2" src={setup.qr_code} alt={t('mfa.qrAlt')} />
          <details><summary className="cursor-pointer text-sm">{t('mfa.manual')}</summary><code className="my-2 block break-all text-xs select-all" data-mfa-secret="">{setup.secret}</code><Button type="button" variant="outline" onClick={() => void copy(setup.secret)}>{t('mfa.copySecret')}</Button></details>
          <p className="text-sm">{t('mfa.recoveryDescription')}</p>
          <div className="grid gap-2 sm:grid-cols-2">{setup.recovery_codes.map(value => <code className="break-all rounded border p-2 text-xs select-all" data-mfa-recovery-code="" key={value}>{value}</code>)}</div>
          <Button type="button" variant="outline" className="w-fit" onClick={() => void copy(setup.recovery_codes.join('\n'))}>{t('mfa.copyRecovery')}</Button>
          <label className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-0.5 size-4 shrink-0" checked={saved} disabled={busy} onChange={event => setSaved(event.target.checked)} />{t('mfa.savedRecovery')}</label>
        </div>}
        {(setup || status.enabled) && <Field><Label htmlFor={`${id}-code`}>{t(status.enabled ? 'mfa.code' : 'mfa.authenticatorCode')}</Label><Input id={`${id}-code`} type="password" autoComplete="one-time-code" required maxLength={128} disabled={busy} value={code} onChange={event => setCode(event.target.value)} /></Field>}
        <p className="text-xs text-muted-foreground">{t(status.enabled ? 'mfa.disableHint' : 'mfa.enableHint')}</p>
        <div className="flex flex-wrap gap-2">
          <Button disabled={busy || !password || (Boolean(setup) && (!saved || !code.trim())) || (status.enabled && !code.trim())}>{t(busy ? 'mfa.working' : status.enabled ? 'mfa.disable' : setup ? 'mfa.enable' : 'mfa.begin')}</Button>
          {setup && <Button type="button" variant="outline" disabled={busy} onClick={() => { setSetup(null); setSaved(false); setCode(''); setPassword(''); setError('') }}>{t('mfa.cancelSetup')}</Button>}
        </div>
      </form>}
    {error && <p className="mt-3 text-sm text-destructive" role="alert">{error}</p>}
  </details>
}
