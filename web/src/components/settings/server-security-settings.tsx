import * as React from 'react'
import { api, ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { randomUuid } from '@/lib/random-id'
import { ServerMailSettings, emptySmtp, type SmtpSettings } from './server-mail-settings'
import { GroupHeader } from './settings-ui'
import { OidcProviderEditor, type OidcSettings } from './oidc-provider-editor'

export interface ServerSecuritySettings {
  revision: number
  public_url: string
  oidc_unavailable: boolean
  oidc_providers: OidcSettings[]
  turnstile: { site_key: string; has_secret_key: boolean } | null
  smtp: SmtpSettings | null
}

const endpoint = '/admin/instance/authentication'

function validPublicOrigin(value: string) {
  try {
    const url = new URL(value.trim())
    const loopback = url.hostname === 'localhost' || url.hostname === '[::1]' || /^127(?:\.\d+){3}$/.test(url.hostname)
    return (url.protocol === 'https:' || (url.protocol === 'http:' && loopback))
      && url.hostname !== '0.0.0.0' && url.hostname !== '[::]'
      && !url.username && !url.password && !url.search && !url.hash && url.pathname === '/'
  } catch { return false }
}

export function ServerSecuritySettingsPanel() {
  const t = useTranslate('serverSecurity')
  const { notify, retryAuthentication } = useWorkbench()
  const [saved, setSaved] = React.useState<ServerSecuritySettings | null>(null)
  const [origin, setOrigin] = React.useState('')
  const [providers, setProviders] = React.useState<OidcSettings[]>([])
  const oidcEnabled = providers.some(provider => provider.enabled)
  const [turnstileEnabled, setTurnstileEnabled] = React.useState(false)
  const [siteKey, setSiteKey] = React.useState('')
  const [secretKey, setSecretKey] = React.useState('')
  const [smtpEnabled, setSmtpEnabled] = React.useState(false)
  const [smtp, setSmtp] = React.useState(emptySmtp)
  const [smtpPassword, setSmtpPassword] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [reload, setReload] = React.useState(0)
  const [conflict, setConflict] = React.useState(false)
  const invalidOrigin = (oidcEnabled || turnstileEnabled || smtpEnabled) && !validPublicOrigin(origin)

  const accept = React.useCallback((settings: ServerSecuritySettings) => {
    setSaved(settings)
    setOrigin(settings.public_url || location.origin)
    setProviders(settings.oidc_providers.map(provider => ({ ...provider, client_secret: '' })))
    setTurnstileEnabled(Boolean(settings.turnstile))
    setSiteKey(settings.turnstile?.site_key ?? '')
    setSecretKey('')
    setSmtpEnabled(Boolean(settings.smtp))
    setSmtp(settings.smtp ?? emptySmtp)
    setSmtpPassword('')
    setConflict(false)
  }, [])

  React.useEffect(() => {
    const controller = new AbortController()
    setError('')
    void api.request<ServerSecuritySettings>(endpoint, { signal: controller.signal })
      .then(settings => { if (!controller.signal.aborted) accept(settings) })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => controller.abort()
  }, [accept, reload])

  const save = async () => {
    if (!saved || busy) return
    if (invalidOrigin) { setError(t('invalidOrigin')); return }
    setBusy(true)
    setError('')
    try {
      const { has_password: _password, ...smtpInput } = smtp
      const settings = await api.request<ServerSecuritySettings>(endpoint, { method: 'PUT', body: {
        revision: saved.revision,
        public_url: origin.trim(),
        oidc_providers: providers.map(({ has_client_secret: _, available: _available, ...provider }) => ({ ...provider, name: provider.name.trim(), issuer: provider.issuer.trim(), audience: provider.audience.trim(), client_id: provider.client_id.trim(), client_secret: provider.client_secret || null })),
        turnstile: turnstileEnabled ? { site_key: siteKey.trim(), secret_key: secretKey || null } : null,
        smtp: smtpEnabled ? { ...smtpInput, host: smtp.host.trim(), from: smtp.from.trim(), password: smtpPassword || null } : null,
      } })
      accept(settings)
      notify(t('saved'))
      retryAuthentication()
    } catch (cause) {
      const conflicted = cause instanceof ApiError && cause.status === 409
      setConflict(conflicted)
      setError(conflicted ? t('conflict') : cause instanceof Error ? cause.message : String(cause))
    } finally { setBusy(false) }
  }

  return <section className="min-w-0 rounded-xl border bg-card p-5" aria-label={t('title')}>
    <GroupHeader title={t('title')} description={t('description')} />
    {!saved ? <p className="text-sm text-muted-foreground" role="status">{error || t('loading')}</p> : <form className="grid min-w-0 gap-5" onSubmit={event => { event.preventDefault(); void save() }}>
      <fieldset disabled={busy || conflict} className="grid min-w-0 gap-5">
        <Field>
          <Label htmlFor="auth-public-url">{t('origin')}</Label>
          <Input id="auth-public-url" type="url" value={origin} onChange={event => setOrigin(event.target.value)} aria-invalid={invalidOrigin} aria-describedby={invalidOrigin ? 'auth-public-url-hint auth-public-url-error' : 'auth-public-url-hint'} required={oidcEnabled || turnstileEnabled || smtpEnabled} placeholder="https://ternilo.example.com" />
          <p id="auth-public-url-hint" className="text-xs text-muted-foreground">{t('originHint')}</p>
          {invalidOrigin && <p id="auth-public-url-error" className="text-xs text-destructive">{t('invalidOrigin')}</p>}
        </Field>
        <div className="grid min-w-0 gap-4 border-t pt-5">
          <div className="grid gap-1"><Label>{t('oauth')}</Label><p className="text-xs leading-relaxed text-muted-foreground">{t('oauthHint')}</p></div>
          {providers.map((provider, index) => <OidcProviderEditor key={provider.id} value={provider} onChange={value => setProviders(current => current.map(item => item.id === provider.id ? value : item))} onRemove={() => setProviders(current => current.filter(item => item.id !== provider.id))} index={index} />)}
          <Button type="button" variant="outline" className="w-fit" disabled={providers.length >= 16} onClick={() => setProviders(current => [...current, { id: randomUuid(), name: '', enabled: true, issuer: '', audience: '', client_id: '', scopes: 'openid profile email', token_auth_method: 'none', has_client_secret: false, client_secret: '' }])}>{t('addProvider')}</Button>
          {oidcEnabled && !invalidOrigin && <p className="break-all text-xs text-muted-foreground">{t('callback')}：<code>{origin.trim().replace(/\/$/, '')}/auth/callback</code></p>}
          {oidcEnabled && <p className="text-xs text-muted-foreground">{t('callbackHint')}</p>}
        </div>
        <div className="grid min-w-0 gap-4 border-t pt-5">
          <div className="flex items-start justify-between gap-4">
            <div className="grid gap-1"><Label htmlFor="auth-turnstile-enabled">{t('turnstile')}</Label><p className="text-xs leading-relaxed text-muted-foreground">{t('turnstileHint')}</p></div>
            <Switch id="auth-turnstile-enabled" checked={turnstileEnabled} onCheckedChange={setTurnstileEnabled} />
          </div>
          {turnstileEnabled && <>
            <Field><Label htmlFor="auth-site-key">{t('siteKey')}</Label><Input id="auth-site-key" required value={siteKey} onChange={event => setSiteKey(event.target.value)} /></Field>
            <Field><Label htmlFor="auth-secret-key">{t('secretKey')}</Label><Input id="auth-secret-key" type="password" autoComplete="new-password" value={secretKey} onChange={event => setSecretKey(event.target.value)} placeholder={t(saved.turnstile?.has_secret_key ? 'secretKept' : 'secretRequired')} /></Field>
            <p className="text-xs leading-relaxed text-muted-foreground">{t('hostnameHint')}</p>
          </>}
        </div>
        <div className="grid min-w-0 gap-4 border-t pt-5">
          <div className="flex items-start justify-between gap-4"><div className="grid gap-1"><Label htmlFor="auth-smtp-enabled">{t('smtp')}</Label><p className="text-xs text-muted-foreground">{t('smtpHint')}</p></div><Switch id="auth-smtp-enabled" checked={smtpEnabled} onCheckedChange={setSmtpEnabled} /></div>
          {smtpEnabled && <ServerMailSettings value={smtp} password={smtpPassword} onChange={setSmtp} onPassword={setSmtpPassword} />}
        </div>
        <p className="border-t pt-5 text-xs leading-relaxed text-muted-foreground">{t('secretsHint')}</p>
        <Button type="submit" className="w-fit">{t('save')}</Button>
      </fieldset>
      {error && <p className="break-words text-sm text-destructive" role="alert">{error}</p>}
    </form>}
    {(error || conflict) && <Button type="button" variant="outline" className="mt-3" disabled={busy} onClick={() => setReload(value => value + 1)}>{t('reload')}</Button>}
  </section>
}
