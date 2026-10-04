import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'

export interface OidcSettings {
  id: string
  name: string
  enabled: boolean
  issuer: string
  audience: string
  client_id: string
  scopes: string
  token_auth_method: 'none' | 'client_secret_basic' | 'client_secret_post'
  has_client_secret: boolean
  available?: boolean
  client_secret?: string
}

export function OidcProviderEditor({ value, onChange, onRemove, index }: { value: OidcSettings; onChange(value: OidcSettings): void; onRemove(): void; index: number }) {
  const t = useTranslate('serverSecurity')
  const prefix = `auth-provider-${index}`
  return <section className="grid min-w-0 gap-4 rounded-lg border p-4" data-login-provider={value.id}>
    <div className="flex items-center justify-between gap-3"><Label htmlFor={`${prefix}-enabled`}>{value.name || t('newProvider')}</Label><Switch id={`${prefix}-enabled`} checked={value.enabled} onCheckedChange={enabled => onChange({ ...value, enabled })} /></div>
    <Field><Label htmlFor={`${prefix}-name`}>{t('providerName')}</Label><Input id={`${prefix}-name`} required maxLength={100} value={value.name} onChange={event => onChange({ ...value, name: event.target.value })} /></Field>
    <div className="grid min-w-0 gap-4 sm:grid-cols-2">
      <Field><Label htmlFor={`${prefix}-issuer`}>{t('issuer')}</Label><Input id={`${prefix}-issuer`} type="url" required={value.enabled} value={value.issuer} onChange={event => onChange({ ...value, issuer: event.target.value })} /></Field>
      <Field><Label htmlFor={`${prefix}-client-id`}>{t('clientId')}</Label><Input id={`${prefix}-client-id`} required={value.enabled} value={value.client_id} onChange={event => onChange({ ...value, client_id: event.target.value })} /></Field>
      <Field><Label htmlFor={`${prefix}-scopes`}>{t('scopes')}</Label><Input id={`${prefix}-scopes`} required={value.enabled} value={value.scopes} onChange={event => onChange({ ...value, scopes: event.target.value })} /></Field>
    </div>
    <Field><Label htmlFor={`${prefix}-method`}>{t('method')}</Label><Select id={`${prefix}-method`} value={value.token_auth_method} onValueChange={method => onChange({ ...value, token_auth_method: method as OidcSettings['token_auth_method'] })}><option value="none">{t('publicClient')}</option><option value="client_secret_basic">{t('basic')}</option><option value="client_secret_post">{t('post')}</option></Select></Field>
    {value.token_auth_method !== 'none' && <Field><Label htmlFor={`${prefix}-secret`}>{t('clientSecret')}</Label><Input id={`${prefix}-secret`} type="password" autoComplete="new-password" value={value.client_secret ?? ''} onChange={event => onChange({ ...value, client_secret: event.target.value })} placeholder={t(value.has_client_secret ? 'secretKept' : 'secretRequired')} /></Field>}
    <details className="min-w-0 rounded-lg border p-3"><summary className="cursor-pointer text-sm">{t('advanced')}</summary><Field className="mt-3"><Label htmlFor={`${prefix}-audience`}>{t('audience')}</Label><Input id={`${prefix}-audience`} value={value.audience} onChange={event => onChange({ ...value, audience: event.target.value })} /><p className="text-xs text-muted-foreground">{t('audienceHint')}</p></Field></details>
    {value.enabled && value.available === false && <p className="text-sm text-destructive" role="status">{t('unavailable')}</p>}
    <div className="flex flex-wrap gap-2"><Button type="button" variant="outline" onClick={() => onChange({ ...value, name: 'LINUX DO', issuer: 'https://connect.linux.do/', audience: '', client_id: '', scopes: 'openid profile email', token_auth_method: 'client_secret_post', client_secret: '' })}>{t('linuxDo')}</Button><Button type="button" variant="ghost" onClick={onRemove}>{t('removeProvider')}</Button></div>
  </section>
}
