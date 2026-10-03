import * as React from 'react'
import { navigate } from '@/app/navigation'
import { api } from '@/api/client'
import { OidcFlowError } from '@/auth/oidc'
import type { TenantSummary } from '@/types'
import { LoaderCircle } from 'lucide-react'
import { clearAccountLink, readAccountLink, type NativeLoginInput } from '@/auth/server'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { TurnstileChallenge } from './turnstile-challenge'

const pendingTeamKey = 'ternilo.pending-team-invitation'
const emptyLink = { setupToken: '', invitationToken: '', teamInvitationToken: '' }

export function ServerLogin() {
  const { authRequired, serverAuthConfig: config, error: connectionError, authenticate, login, retryAuthentication, accessPaused, oidcRegistrationRequired, registerOidcUsername, logout, selectTenant, notify } = useWorkbench()
  const t = useTranslate('app')
  const settingsT = useTranslate('settings')
  const adminT = useTranslate('admin')
  const securityT = useTranslate('serverSecurity')
  const [turnstileToken, setTurnstileToken] = React.useState('')
  const [turnstileAttempt, setTurnstileAttempt] = React.useState(0)
  const [link, setLink] = React.useState(() => {
    const next = readAccountLink()
    return { ...next, teamInvitationToken: next.teamInvitationToken || sessionStorage.getItem(pendingTeamKey) || '' }
  })
  const [choice, setChoice] = React.useState<'login' | 'register' | 'accept'>(link.invitationToken ? 'accept' : 'login')
  const [token, setToken] = React.useState(link.setupToken || link.invitationToken)
  const [username, setUsername] = React.useState('')
  const [email, setEmail] = React.useState('')
  const [password, setPassword] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [pending, setPending] = React.useState(false)
  const [pendingOidc, setPendingOidc] = React.useState(false)
  const multiUser = config?.mode === 'multi_user'
  const publicSignup = multiUser && config?.registration.mode === 'open'
  const inviteSignup = multiUser && config?.registration.mode === 'invite'
  const action = config && !config.initialized ? 'setup'
    : choice === 'register' && publicSignup ? 'register'
      : choice === 'accept' && inviteSignup ? 'accept' : 'login'
  const creating = action !== 'login'
  const requiresToken = action === 'setup' || action === 'accept'
  const turnstile = config?.turnstile && action !== 'setup' ? config.turnstile : undefined
  const turnstileAction = oidcRegistrationRequired || action === 'register' ? 'register' : action === 'accept' ? 'invitation' : 'login'
  const challenge = turnstile && <TurnstileChallenge siteKey={turnstile.site_key} action={turnstileAction} attempt={turnstileAttempt} onToken={setTurnstileToken} />
  const accountError = (message: string) => {
    switch (message) {
      case 'complete the Turnstile verification':
      case 'Turnstile verification failed; please try again': return securityT('challengeError')
      case 'Turnstile verification is temporarily unavailable': return securityT('challengeUnavailable')
      case 'account registration is pending approval': return t('server.pendingLogin')
      case 'account registration was rejected': return t('server.rejectedLogin')
      case 'registration requires an administrator invitation': return t('server.invitationRequired')
      case 'email is already registered': return t('server.emailTaken')
      case 'email must be a valid address such as name@example.com': return t('server.emailInvalid')
      case 'account is banned': return t('server.bannedLogin')
      case 'account was removed': return t('server.removedLogin')
      case 'username is already registered': return t('server.usernameTaken')
      case 'username must contain 3 to 64 ASCII letters, digits, dots, hyphens, or underscores': return t('server.usernameHint')
      default: return message
    }
  }
  const displayedConnectionError = accountError(connectionError)

  React.useEffect(() => {
    if (link.teamInvitationToken) sessionStorage.setItem(pendingTeamKey, link.teamInvitationToken)
    else sessionStorage.removeItem(pendingTeamKey)
  }, [link.teamInvitationToken])
  React.useEffect(() => {
    clearAccountLink()
    const acceptLink = () => {
      const next = readAccountLink()
      if (!next.setupToken && !next.invitationToken && !next.teamInvitationToken) return
      setLink(next)
      setToken(next.setupToken || next.invitationToken)
      setChoice(next.invitationToken ? 'accept' : 'login')
      setUsername('')
      setEmail('')
      setPassword('')
      setError('')
      setPending(false)
      clearAccountLink()
    }
    window.addEventListener('hashchange', acceptLink)
    return () => window.removeEventListener('hashchange', acceptLink)
  }, [])

  const submit = async (oidc = false) => {
    if (busy) return
    setBusy(true)
    setError('')
    try {
      if (oidcRegistrationRequired) {
        const status = await registerOidcUsername(username.trim(), email.trim(), turnstileToken || undefined)
        setEmail('')
        if (status === 'pending') { setPendingOidc(true); setPending(true) }
      } else if (oidc) await login('')
      else {
        const account = { username: username.trim(), password }
        const input: NativeLoginInput = action === 'login' ? { action, ...account }
          : action === 'register' ? { action, ...account, email: email.trim() }
            : action === 'setup' ? { action, ...account, email: email.trim(), setup_token: token.trim() }
              : { action, ...account, email: email.trim(), token: token.trim() }
        const status = await authenticate({ ...input, ...(turnstile ? { turnstile_token: turnstileToken } : {}) })
        setPassword('')
        setEmail('')
        setChoice('login')
        if (status === 'pending') { setPendingOidc(false); setPending(true) }
        if (action === 'accept' || action === 'setup') {
          setToken('')
          setLink(current => ({ ...current, setupToken: '', invitationToken: '' }))
        }
      }
    } catch (cause) {
      setError(cause instanceof OidcFlowError
        ? t(cause.translationKey)
        : accountError(cause instanceof Error ? cause.message : String(cause)))
    } finally { setBusy(false); setTurnstileToken(''); setTurnstileAttempt(value => value + 1) }
  }

  const join = async () => {
    setBusy(true)
    setError('')
    try {
      const tenant = await api.request<TenantSummary>('/invitations/accept', { method: 'POST', body: { token: link.teamInvitationToken } })
      setLink(emptyLink)
      setToken('')
      setChoice('login')
      await selectTenant(tenant.tenant_id)
      notify(adminT('join.success'))
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  if (!authRequired && link.teamInvitationToken) return <Dialog open>
    <DialogContent showClose={false} className="max-w-md overflow-y-auto">
      <DialogHeader><DialogTitle>{adminT('join.title')}</DialogTitle><DialogDescription>{adminT('join.description')}</DialogDescription></DialogHeader>
      {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
      <Button disabled={busy} onClick={() => void join()}>{busy && <LoaderCircle className="animate-spin" />}{adminT('join.accept')}</Button>
      <Button variant="ghost" disabled={busy} onClick={() => { setLink(emptyLink); setChoice('login'); setToken(''); setError('') }}>{adminT('join.dismiss')}</Button>
    </DialogContent>
  </Dialog>

  return <Dialog open={authRequired}>
    <DialogContent showClose={false} className="max-w-md overflow-y-auto [&_input]:min-h-10 [&_button]:min-h-10"
      onEscapeKeyDown={event => event.preventDefault()} onPointerDownOutside={event => event.preventDefault()}>
      <DialogHeader>
        <BrandMark className="mb-2 size-10" />
        <DialogTitle>{t(accessPaused ? 'server.pausedTitle' : pending ? 'server.pendingTitle' : oidcRegistrationRequired ? 'server.oidcUsernameTitle' : action === 'setup' ? 'server.setupTitle' : action === 'accept' ? 'server.inviteTitle' : action === 'register' ? 'server.registerTitle' : 'auth.loginTitle')}</DialogTitle>
        <DialogDescription>{t(accessPaused ? 'server.pausedDescription' : pending ? pendingOidc ? 'server.oidcPendingDescription' : 'server.pendingDescription' : oidcRegistrationRequired ? 'server.oidcUsernameDescription' : action === 'setup' ? 'server.setupDescription' : action === 'accept' ? 'server.inviteDescription' : action === 'register' ? config?.registration.require_approval ? 'server.registerReviewDescription' : 'server.registerDescription' : 'server.loginDescription')}</DialogDescription>
      </DialogHeader>
      {accessPaused ? <div className="grid gap-3">
        <Button onClick={retryAuthentication}>{t('server.retryAccess')}</Button>
        <Button variant="outline" onClick={logout}>{t('server.switchAccount')}</Button>
      </div> : pending ? <div className="grid gap-3" data-registration-pending="">
        <p className="break-words text-sm text-muted-foreground" role="status">{t('server.pendingAccount', { username })}</p>
        <Button onClick={() => { setPending(false); setError('') }}>{t('server.backToLogin')}</Button>
      </div> : oidcRegistrationRequired ? <form className="grid gap-4" data-oidc-registration=""
        onKeyDown={event => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}
        onSubmit={event => { event.preventDefault(); void submit() }}>
        <Field>
          <Label htmlFor="server-username">{t('server.username')}</Label>
          <Input id="server-username" autoComplete="username" autoCapitalize="none" spellCheck={false} required minLength={3} maxLength={64} disabled={busy} value={username} onChange={event => setUsername(event.target.value)} />
          <p className="text-xs leading-relaxed text-muted-foreground">{t('server.usernameHint')}</p>
        </Field>
        <Field>
          <Label htmlFor="server-email">{t('server.email')}</Label>
          <Input id="server-email" type="email" autoComplete="email" autoCapitalize="none" spellCheck={false} required maxLength={254} disabled={busy} value={email} onChange={event => setEmail(event.target.value)} />
          <p className="text-xs leading-relaxed text-muted-foreground">{t('server.emailDescription')}</p>
        </Field>
        {challenge}
        <Button disabled={busy || !username.trim() || !email.trim() || Boolean(turnstile && !turnstileToken)}>{busy && <LoaderCircle className="animate-spin" />}{t(config?.registration.require_approval ? 'server.submitRegistration' : 'server.oidcUsernameContinue')}</Button>
        <Button type="button" variant="ghost" disabled={busy} onClick={logout}>{t('server.switchAccount')}</Button>
      </form> : config ? <form className="grid gap-4"
        onKeyDown={event => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }}
        onSubmit={event => { event.preventDefault(); void submit() }}>
        {link.teamInvitationToken && <p className="text-sm leading-relaxed text-muted-foreground">{adminT('join.signInFirst')}</p>}
        {link.invitationToken && !inviteSignup && <p className="text-sm leading-relaxed text-muted-foreground">{t('server.invitationUnavailable')}</p>}
        {config.native_enabled && <>
          {creating && <p className="text-xs leading-relaxed text-muted-foreground">{t('server.accountHint')}</p>}
          {requiresToken && <Field>
            <Label htmlFor="server-account-token">{t(action === 'setup' ? 'server.setupToken' : 'server.invitationToken')}</Label>
            <Input id="server-account-token" type="password" autoComplete="off" required disabled={busy} value={token} onChange={event => setToken(event.target.value)} />
          </Field>}
          <Field>
            <Label htmlFor="server-username">{t('server.username')}</Label>
            <Input id="server-username" autoComplete="username" autoCapitalize="none" spellCheck={false} required disabled={busy} value={username} onChange={event => setUsername(event.target.value)} />
          </Field>
          {creating && <Field>
            <Label htmlFor="server-email">{t('server.email')}</Label>
            <Input id="server-email" type="email" autoComplete="email" autoCapitalize="none" spellCheck={false} required maxLength={254} disabled={busy} value={email} onChange={event => setEmail(event.target.value)} />
            <p className="text-xs leading-relaxed text-muted-foreground">{t('server.emailDescription')}</p>
          </Field>}
          <Field>
            <Label htmlFor="server-password">{t('server.password')}</Label>
            <Input id="server-password" type="password" autoComplete={creating ? 'new-password' : 'current-password'} required disabled={busy} value={password} onChange={event => setPassword(event.target.value)} />
          </Field>
          {challenge}
          <Button disabled={busy || !username.trim() || !password || (creating && !email.trim()) || (requiresToken && !token.trim()) || Boolean(turnstile && !turnstileToken)}>
            {busy && <LoaderCircle className="animate-spin" />}
            {t(action === 'setup' ? 'server.setup' : action === 'accept' ? 'server.accept' : action === 'register' ? config.registration.require_approval ? 'server.submitRegistration' : 'server.register' : 'server.login')}
          </Button>
        </>}
        {config.initialized && config.oidc_enabled && action === 'login' && <Button type="button" variant="outline" disabled={busy} onClick={() => void submit(true)}>{t('auth.login')}</Button>}
        {config.initialized && config.native_enabled && (creating || publicSignup || (inviteSignup && !link.teamInvitationToken)) && <Button type="button" variant="ghost" disabled={busy} onClick={() => {
          setChoice(creating ? 'login' : publicSignup ? 'register' : 'accept')
          setError('')
        }}>{creating ? t('server.backToLogin') : publicSignup ? t('server.createAccount') : t('server.useInvitation')}</Button>}
        {!creating && config.email_enabled && <Button type="button" variant="ghost" disabled={busy} onClick={() => navigate('/auth/recover')}>{settingsT('account.forgotPassword')}</Button>}
      </form> : connectionError ? <Button variant="outline" onClick={retryAuthentication}>{t('error.retry')}</Button>
        : <p className="flex items-center gap-2 text-sm text-muted-foreground" role="status"><LoaderCircle className="size-4 animate-spin" />{t('server.loading')}</p>}
      {!accessPaused && (error || displayedConnectionError) && <p className="break-words text-sm text-destructive" role="alert">{error || displayedConnectionError}</p>}
    </DialogContent>
  </Dialog>
}
