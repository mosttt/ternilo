import * as React from 'react'
import { api, ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { GroupHeader } from './settings-ui'

interface OidcSettings {
  issuer: string
  audience: string
  client_id: string
  scopes: string
  token_auth_method: 'none' | 'client_secret_basic' | 'client_secret_post'
  has_client_secret: boolean
}

export interface ServerSecuritySettings {
  revision: number
  public_url: string
  oidc_unavailable: boolean
  oidc: OidcSettings | null
  turnstile: { site_key: string; has_secret_key: boolean } | null
}

const emptyOidc: OidcSettings = { issuer: '', audience: '', client_id: '', scopes: 'openid profile email', token_auth_method: 'none', has_client_secret: false }
const endpoint = '/admin/instance/authentication'

export function ServerSecuritySettingsPanel() {
  const t = useTranslate('serverSecurity')
  const { notify } = useWorkbench()
  const [saved, setSaved] = React.useState<ServerSecuritySettings | null>(null)
  const [origin, setOrigin] = React.useState('')
  const [oidcEnabled, setOidcEnabled] = React.useState(false)
  const [oidc, setOidc] = React.useState(emptyOidc)
  const [clientSecret, setClientSecret] = React.useState('')
  const [turnstileEnabled, setTurnstileEnabled] = React.useState(false)
  const [siteKey, setSiteKey] = React.useState('')
  const [secretKey, setSecretKey] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [reload, setReload] = React.useState(0)
  const [conflict, setConflict] = React.useState(false)

  const accept = React.useCallback((settings: ServerSecuritySettings) => {
    setSaved(settings)
    setOrigin(settings.public_url || location.origin)
    setOidcEnabled(Boolean(settings.oidc))
    setOidc(settings.oidc ?? emptyOidc)
    setTurnstileEnabled(Boolean(settings.turnstile))
    setSiteKey(settings.turnstile?.site_key ?? '')
    setClientSecret('')
    setSecretKey('')
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
    setBusy(true)
    setError('')
    try {
      const { has_client_secret: _, ...oidcInput } = oidc
      const settings = await api.request<ServerSecuritySettings>(endpoint, { method: 'PUT', body: {
        revision: saved.revision,
        public_url: origin.trim(),
        oidc: oidcEnabled ? { ...oidcInput, issuer: oidc.issuer.trim(), audience: oidc.audience.trim(), client_id: oidc.client_id.trim(), client_secret: clientSecret || null } : null,
        turnstile: turnstileEnabled ? { site_key: siteKey.trim(), secret_key: secretKey || null } : null,
      } })
      accept(settings)
      notify(t('saved'))
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
          <Input id="auth-public-url" type="url" value={origin} onChange={event => setOrigin(event.target.value)} required={oidcEnabled || turnstileEnabled} placeholder="https://ternilo.example.com" />
          <p className="text-xs text-muted-foreground">{t('originHint')}</p>
        </Field>
        <div className="grid min-w-0 gap-4 border-t pt-5">
          <div className="flex items-start justify-between gap-4">
            <div className="grid gap-1"><Label htmlFor="auth-oidc-enabled">{t('oauth')}</Label><p className="text-xs leading-relaxed text-muted-foreground">{t('oauthHint')}</p></div>
            <Switch id="auth-oidc-enabled" checked={oidcEnabled} onCheckedChange={setOidcEnabled} />
          </div>
          {oidcEnabled && <>
            <Button type="button" variant="outline" className="w-fit" onClick={() => {
              setOidc({ ...emptyOidc, issuer: 'https://connect.linux.do/', token_auth_method: 'client_secret_post' })
              setClientSecret('')
            }}>{t('linuxDo')}</Button>
            <div className="grid min-w-0 gap-4 sm:grid-cols-2">
              <Field><Label htmlFor="auth-issuer">{t('issuer')}</Label><Input id="auth-issuer" type="url" required value={oidc.issuer} onChange={event => setOidc(current => ({ ...current, issuer: event.target.value }))} /></Field>
              <Field><Label htmlFor="auth-client-id">{t('clientId')}</Label><Input id="auth-client-id" required value={oidc.client_id} onChange={event => setOidc(current => ({ ...current, client_id: event.target.value }))} /></Field>
              <Field><Label htmlFor="auth-scopes">{t('scopes')}</Label><Input id="auth-scopes" required value={oidc.scopes} onChange={event => setOidc(current => ({ ...current, scopes: event.target.value }))} /></Field>
            </div>
            <Field><Label htmlFor="auth-token-method">{t('method')}</Label><Select id="auth-token-method" value={oidc.token_auth_method} onValueChange={value => setOidc(current => ({ ...current, token_auth_method: value as OidcSettings['token_auth_method'] }))}>
              <option value="none">{t('publicClient')}</option><option value="client_secret_basic">{t('basic')}</option><option value="client_secret_post">{t('post')}</option>
            </Select></Field>
            {oidc.token_auth_method !== 'none' && <Field><Label htmlFor="auth-client-secret">{t('clientSecret')}</Label><Input id="auth-client-secret" type="password" autoComplete="new-password" value={clientSecret} onChange={event => setClientSecret(event.target.value)} placeholder={t(oidc.has_client_secret ? 'secretKept' : 'secretRequired')} /></Field>}
            <p className="break-all text-xs text-muted-foreground">{t('callback')}：<code>{origin.trim().replace(/\/$/, '')}/auth/callback</code></p>
            <details className="min-w-0 rounded-lg border p-3">
              <summary className="cursor-pointer text-sm">{t('advanced')}</summary>
              <Field className="mt-3"><Label htmlFor="auth-audience">{t('audience')}</Label><Input id="auth-audience" value={oidc.audience} onChange={event => setOidc(current => ({ ...current, audience: event.target.value }))} /><p className="text-xs leading-relaxed text-muted-foreground">{t('audienceHint')}</p></Field>
            </details>
            {saved.oidc_unavailable && <p className="text-sm text-destructive" role="status">{t('unavailable')}</p>}
          </>}
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
        <p className="text-xs leading-relaxed text-muted-foreground">{t('secretsHint')}</p>
        <Button type="submit" className="w-fit">{t('save')}</Button>
      </fieldset>
      {error && <p className="break-words text-sm text-destructive" role="alert">{error}</p>}
    </form>}
    {(error || conflict) && <Button type="button" variant="outline" className="mt-3" disabled={busy} onClick={() => setReload(value => value + 1)}>{t('reload')}</Button>}
  </section>
}
