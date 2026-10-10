import * as React from 'react'
import { LocaleProvider, useLocale, useTranslate } from '@/i18n/provider'
import { readAccountLink, clearAccountLink } from '@/auth/server'
import { applyThemePreference, type ThemePreference } from '@/domain/theme'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { Field, FieldDescription, Input, Label, Select } from '@/components/ui/field'

export function ServerSetup() {
  return <LocaleProvider><SetupForm /></LocaleProvider>
}

function SetupForm() {
  const t = useTranslate('serverSetup')
  const { locale, setLocale } = useLocale()
  const boot = window.__TERNILO_BOOT__
  const [key, setKey] = React.useState(() => readAccountLink().setupToken)
  const [database, setDatabase] = React.useState(boot?.setup_database_preset ? 'preset' : 'sqlite')
  const [databaseUrl, setDatabaseUrl] = React.useState('')
  const [migrationUrl, setMigrationUrl] = React.useState('')
  const [publicUrl, setPublicUrl] = React.useState(boot?.setup_public_url ?? location.origin)
  const [username, setUsername] = React.useState('')
  const [email, setEmail] = React.useState('')
  const [password, setPassword] = React.useState('')
  const [confirmation, setConfirmation] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  React.useEffect(() => {
    clearAccountLink()
    const stored = localStorage.getItem('ternilo.theme')
    const preference: ThemePreference = stored === 'dark' || stored === 'light' ? stored : 'system'
    const media = matchMedia('(prefers-color-scheme: dark)')
    const apply = () => applyThemePreference(preference, media.matches)
    apply()
    media.addEventListener('change', apply)
    return () => media.removeEventListener('change', apply)
  }, [])

  async function submit(event: React.FormEvent) {
    event.preventDefault()
    if (busy) return
    setError('')
    if (password !== confirmation) { setError(t('passwordMismatch')); return }
    setBusy(true)
    try {
      const response = await fetch('/api/v1/setup', {
        method: 'POST', headers: { 'Content-Type': 'application/json' }, cache: 'no-store',
        body: JSON.stringify({ setup_token: key, database: database === 'postgres'
          ? { kind: 'postgres', url: databaseUrl, migration_url: migrationUrl || null }
          : { kind: database }, public_url: publicUrl.trim() || null, username, email, password }),
      })
      if (!response.ok) {
        const body = await response.json() as { error?: { message?: string } }
        if (response.status === 401) throw new Error(t('invalidKey'))
        if (response.status === 500) throw new Error(t('databaseError'))
        throw new Error(body.error?.message || String(response.status))
      }
      setKey(''); setPassword(''); setConfirmation(''); setDatabaseUrl(''); setMigrationUrl('')
      location.replace('/')
    } catch (cause) { setError(t('requestError', { message: cause instanceof Error ? cause.message : String(cause) })) }
    finally { setBusy(false) }
  }

  return <main className="h-full overflow-y-auto overscroll-contain bg-background p-4 text-foreground sm:p-8" data-server-setup="">
    <div className="mx-auto max-w-xl">
      <header className="mb-8 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3 text-xl font-semibold"><BrandMark className="size-7 text-primary" />Ternilo</div>
        <Select aria-label={t('language')} value={locale} onValueChange={value => setLocale(value as 'zh' | 'en')} className="w-28">
          <option value="zh">zh-CN</option><option value="en">English</option>
        </Select>
      </header>
      <h1 className="text-2xl font-semibold">{t('title')}</h1>
      <p className="mt-3 text-sm leading-relaxed text-muted-foreground">{t('description')}</p>
      <form className="mt-6 space-y-6 rounded-xl border bg-card p-5 sm:p-6" onSubmit={submit}>
        <Field><Label htmlFor="setup-key">{t('key')}</Label><Input id="setup-key" type="password" autoComplete="off" required value={key} onChange={event => setKey(event.target.value)} disabled={busy} /><FieldDescription>{t('keyHint')}</FieldDescription></Field>
        <fieldset disabled={busy} className="grid gap-4 border-t pt-5">
          <Field><Label htmlFor="setup-database">{t('database')}</Label><Select id="setup-database" value={database} onValueChange={setDatabase} disabled={boot?.setup_database_preset}>
            {boot?.setup_database_preset && <option value="preset">{t('preset')}</option>}
            {!boot?.setup_database_preset && <option value="sqlite">{t('sqlite')}</option>}
            {!boot?.setup_database_preset && <option value="postgres">{t('postgres')}</option>}
          </Select><FieldDescription>{database === 'postgres' ? t('postgresHint') : database === 'sqlite' ? t('sqliteHint') : t('preset')}</FieldDescription></Field>
          {database === 'postgres' && <>
            <Field><Label htmlFor="setup-database-url">{t('databaseUrl')}</Label><Input id="setup-database-url" type="password" autoComplete="off" required value={databaseUrl} onChange={event => setDatabaseUrl(event.target.value)} placeholder="postgresql://ternilo_app:password@postgres:5432/ternilo" /></Field>
            <Field><Label htmlFor="setup-migration-url">{t('migrationUrl')}</Label><Input id="setup-migration-url" type="password" autoComplete="off" value={migrationUrl} onChange={event => setMigrationUrl(event.target.value)} /><FieldDescription>{t('migrationHint')}</FieldDescription></Field>
          </>}
          <Field><Label htmlFor="setup-public-url">{t('publicUrl')}</Label><Input id="setup-public-url" type="url" value={publicUrl} onChange={event => setPublicUrl(event.target.value)} disabled={Boolean(boot?.setup_public_url)} /><FieldDescription>{t('publicHint')}</FieldDescription></Field>
        </fieldset>
        <fieldset disabled={busy} className="grid gap-4 border-t pt-5">
          <legend className="px-1 text-sm font-semibold">{t('owner')}</legend>
          <Field><Label htmlFor="setup-username">{t('username')}</Label><Input id="setup-username" autoComplete="username" required minLength={3} maxLength={64} pattern={'[A-Za-z0-9._\\-]+'} value={username} onChange={event => setUsername(event.target.value)} /><FieldDescription>{t('usernameHint')}</FieldDescription></Field>
          <Field><Label htmlFor="setup-email">{t('email')}</Label><Input id="setup-email" type="email" autoComplete="email" required value={email} onChange={event => setEmail(event.target.value)} /></Field>
          <Field><Label htmlFor="setup-password">{t('password')}</Label><Input id="setup-password" type="password" autoComplete="new-password" required minLength={8} value={password} onChange={event => setPassword(event.target.value)} /><FieldDescription>{t('passwordHint')}</FieldDescription></Field>
          <Field><Label htmlFor="setup-confirmation">{t('confirmPassword')}</Label><Input id="setup-confirmation" type="password" autoComplete="new-password" required value={confirmation} onChange={event => setConfirmation(event.target.value)} /></Field>
        </fieldset>
        {error && <div role="alert" className="space-y-2 text-sm text-destructive"><p>{error}</p><Button type="button" variant="outline" onClick={() => location.reload()}>{t('reload')}</Button></div>}
        <Button className="w-full whitespace-normal" type="submit" disabled={busy}>{busy ? t('saving') : t('submit')}</Button>
      </form>
    </div>
  </main>
}
