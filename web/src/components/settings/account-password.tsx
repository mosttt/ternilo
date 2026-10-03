import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'

export function AccountPassword() {
  const t = useTranslate('settings'), id = React.useId()
  const { logout } = useWorkbench()
  const [current, setCurrent] = React.useState('')
  const [password, setPassword] = React.useState('')
  const [confirmation, setConfirmation] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const mounted = React.useRef(true)
  React.useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (busy) return
    if (password !== confirmation) { setError(t('account.passwordMismatch')); return }
    setBusy(true); setError('')
    try {
      await api.request('/auth/password', { method: 'POST', body: { current_password: current, new_password: password } })
      if (mounted.current) { setCurrent(''); setPassword(''); setConfirmation(''); logout() }
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally { if (mounted.current) setBusy(false) }
  }
  return <details className="mt-6 border-t pt-5" data-account-password="">
    <summary className="cursor-pointer text-sm font-medium">{t('account.changePassword')}</summary>
    <p className="mt-2 text-xs text-muted-foreground">{t('account.passwordDescription')}</p>
    <form onSubmit={event => void submit(event)} className="mt-4 grid max-w-lg gap-4">
      <Field><Label htmlFor={`${id}-current`}>{t('account.currentPassword')}</Label><Input id={`${id}-current`} type="password" autoComplete="current-password" required maxLength={1024} disabled={busy} value={current} onChange={event => setCurrent(event.target.value)} /></Field>
      <Field><Label htmlFor={`${id}-new`}>{t('account.newPassword')}</Label><Input id={`${id}-new`} type="password" autoComplete="new-password" required minLength={8} maxLength={1024} disabled={busy} value={password} onChange={event => setPassword(event.target.value)} /></Field>
      <Field><Label htmlFor={`${id}-confirm`}>{t('account.confirmPassword')}</Label><Input id={`${id}-confirm`} type="password" autoComplete="new-password" required minLength={8} maxLength={1024} disabled={busy} value={confirmation} onChange={event => setConfirmation(event.target.value)} /></Field>
      {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
      <Button type="submit" disabled={busy}>{t(busy ? 'account.passwordSaving' : 'account.passwordSave')}</Button>
    </form>
  </details>
}
