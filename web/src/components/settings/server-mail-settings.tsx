import { Field, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'

export interface SmtpSettings {
  host: string
  port: number
  security: 'tls' | 'starttls' | 'local'
  from: string
  username: string | null
  has_password: boolean
}
export const emptySmtp: SmtpSettings = { host: '', port: 587, security: 'starttls', from: '', username: '', has_password: false }

export function ServerMailSettings({ value, password, onChange, onPassword }: { value: SmtpSettings; password: string; onChange: (value: SmtpSettings) => void; onPassword: (value: string) => void }) {
  const t = useTranslate('serverSecurity')
  return <div className="grid min-w-0 gap-4" data-smtp-settings="">
    <div className="grid gap-4 sm:grid-cols-2">
      <Field><Label htmlFor="smtp-host">{t('smtpHost')}</Label><Input id="smtp-host" required maxLength={253} value={value.host} onChange={event => onChange({ ...value, host: event.target.value })} /></Field>
      <Field><Label htmlFor="smtp-port">{t('smtpPort')}</Label><Input id="smtp-port" type="number" required min={1} max={65535} value={value.port} onChange={event => onChange({ ...value, port: Number(event.target.value) })} /></Field>
    </div>
    <Field><Label htmlFor="smtp-security">{t('smtpSecurity')}</Label><Select id="smtp-security" value={value.security} onValueChange={security => onChange({ ...value, security: security as SmtpSettings['security'], port: security === 'tls' ? 465 : security === 'starttls' ? 587 : 25 })}>
      <option value="starttls">STARTTLS</option><option value="tls">TLS</option><option value="local">{t('smtpLocal')}</option>
    </Select></Field>
    <Field><Label htmlFor="smtp-from">{t('smtpFrom')}</Label><Input id="smtp-from" type="email" autoComplete="off" required value={value.from} onChange={event => onChange({ ...value, from: event.target.value })} /></Field>
    <Field><Label htmlFor="smtp-user">{t('smtpUser')}</Label><Input id="smtp-user" autoComplete="off" maxLength={1024} value={value.username ?? ''} onChange={event => onChange({ ...value, username: event.target.value })} /></Field>
    <Field><Label htmlFor="smtp-password">{t('smtpPassword')}</Label><Input id="smtp-password" type="password" autoComplete="new-password" maxLength={4096} value={password} onChange={event => onPassword(event.target.value)} placeholder={t(value.has_password ? 'secretKept' : 'secretRequired')} disabled={!value.username} /></Field>
    <p className="text-xs text-muted-foreground">{t('smtpCredentialsHint')}</p>
  </div>
}
