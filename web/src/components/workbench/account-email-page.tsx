import * as React from 'react'
import { api } from '@/api/client'
import { publicRequest } from '@/auth/server'
import { navigate } from '@/app/navigation'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { TurnstileChallenge } from './turnstile-challenge'

const pendingKey = 'ternilo.pending-email-verification'
function readToken(verification: boolean) {
  if (location.hash) return location.hash.slice(1)
  if (verification) {
    try { const saved = JSON.parse(sessionStorage.getItem(pendingKey) ?? 'null'); if (saved?.expires > Date.now() && typeof saved.token === 'string') return saved.token } catch { /* Discard invalid pending links. */ }
    sessionStorage.removeItem(pendingKey)
  }
  return ''
}

export function AccountEmailPage({ mode }: { mode: 'verify' | 'reset' | 'recover' }) {
  const t = useTranslate('settings'), appT = useTranslate('app')
  const { authRequired, serverIdentity, serverAuthConfig: config, logout } = useWorkbench()
  const [token, setToken] = React.useState(() => readToken(mode === 'verify'))
  const [email, setEmail] = React.useState(''), [password, setPassword] = React.useState(''), [confirmation, setConfirmation] = React.useState('')
  const [busy, setBusy] = React.useState(false), [complete, setComplete] = React.useState(false), [error, setError] = React.useState('')
  const [challenge, setChallenge] = React.useState(''), [attempt, setAttempt] = React.useState(0)
  const verification = mode === 'verify', recovery = mode === 'recover'
  React.useEffect(() => {
    if (verification && token.startsWith('ter_ev_') && token.length === 50) sessionStorage.setItem(pendingKey, JSON.stringify({ token, expires: Date.now() + 15 * 60_000 }))
    if (location.hash) history.replaceState(history.state, '', location.pathname)
  }, [token, verification])
  React.useEffect(() => {
    const acceptLink = () => {
      if (!location.hash) return
      setToken(readToken(verification)); setComplete(false); setError(''); setPassword(''); setConfirmation('')
    }
    window.addEventListener('hashchange', acceptLink)
    return () => window.removeEventListener('hashchange', acceptLink)
  }, [verification])
  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (busy) return
    if (mode === 'reset' && password !== confirmation) { setError(t('account.passwordMismatch')); return }
    setBusy(true); setError('')
    try {
      if (verification) { await api.request('/auth/email/verify', { method: 'POST', body: { token } }); sessionStorage.removeItem(pendingKey) }
      else if (recovery) await publicRequest('/api/v1/auth/password-recovery', { email, turnstile_token: challenge || null })
      else { await publicRequest('/api/v1/auth/password-reset', { token, password }); setPassword(''); setConfirmation(''); logout() }
      setComplete(true)
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false); setChallenge(''); setAttempt(value => value + 1) }
  }
  const back = () => { sessionStorage.removeItem(pendingKey); navigate(verification && !authRequired ? '/settings/general' : '/') }
  return <main className="flex min-h-dvh items-center justify-center overflow-y-auto bg-background p-4" data-account-email-page={mode}>
    <section className="grid w-full max-w-md gap-4 rounded-xl border bg-card p-6">
      <BrandMark className="size-10" />
      <h1 className="text-xl font-semibold">{t(verification ? 'account.emailVerifyTitle' : recovery ? 'account.forgotPassword' : 'account.emailResetTitle')}</h1>
      <p className="text-sm text-muted-foreground">{t(verification ? 'account.emailVerifyHint' : recovery ? 'account.emailRecoveryHint' : 'account.emailResetHint')}</p>
      {complete ? <p role="status">{t(verification ? 'account.emailVerified' : recovery ? 'account.emailRecoverySent' : 'account.emailResetDone')}</p>
        : !recovery && !token ? <p role="alert" className="text-destructive">{t('account.emailLinkMissing')}</p>
          : <form className="grid gap-4" onSubmit={event => void submit(event)}>
            {verification && <p className="break-all text-sm">{serverIdentity?.email}</p>}
            {recovery && <Field><Label htmlFor="recovery-email">{t('account.email')}</Label><Input id="recovery-email" type="email" required maxLength={254} autoComplete="email" disabled={busy} value={email} onChange={event => setEmail(event.target.value)} /></Field>}
            {mode === 'reset' && <>
              <Field><Label htmlFor="reset-password">{t('account.newPassword')}</Label><Input id="reset-password" type="password" required minLength={8} maxLength={1024} autoComplete="new-password" disabled={busy} value={password} onChange={event => setPassword(event.target.value)} /></Field>
              <Field><Label htmlFor="reset-confirmation">{t('account.confirmPassword')}</Label><Input id="reset-confirmation" type="password" required minLength={8} maxLength={1024} autoComplete="new-password" disabled={busy} value={confirmation} onChange={event => setConfirmation(event.target.value)} /></Field>
            </>}
            {recovery && config?.turnstile && <TurnstileChallenge siteKey={config.turnstile.site_key} action="password_recovery" attempt={attempt} onToken={setChallenge} />}
            {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
            <Button disabled={busy || (verification && authRequired) || (recovery && Boolean(config?.turnstile && !challenge))}>{t(busy ? 'account.emailWorking' : verification ? 'account.emailVerifyTitle' : recovery ? 'account.emailRecoverSend' : 'account.emailResetSave')}</Button>
          </form>}
      <Button type="button" variant="outline" disabled={busy} onClick={back}>{verification && !authRequired ? t('account.backToSettings') : appT('server.backToLogin')}</Button>
      {verification && !authRequired && !complete && <Button type="button" variant="ghost" disabled={busy} onClick={logout}>{appT('server.switchAccount')}</Button>}
    </section>
  </main>
}
