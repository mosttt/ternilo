import * as React from 'react'
import { ServiceWorkspaces } from './service-workspaces'
import { Copy, LoaderCircle, Plus, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Textarea } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import { copyText } from '@/lib/clipboard'
import { ActionDialog, GroupHeader } from './settings-ui'
import { createServiceAccount, issueServiceCredential, listServiceAccounts, listServiceCredentials, revokeServiceCredential, updateServiceAccount, type ServiceAccount, type ServiceCredential, type ServiceScope } from './service-accounts-api'

const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

export function PlatformServiceAccounts({ tenantId }: { tenantId: string }) {
  const t = useTranslate('serviceAccounts')
  const common = useTranslate('common')
  const [accounts, setAccounts] = React.useState<ServiceAccount[]>([])
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const [editor, setEditor] = React.useState<ServiceAccount | 'new' | null>(null)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError('')
    void listServiceAccounts(tenantId, controller.signal).then(value => { if (!controller.signal.aborted) setAccounts(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(message(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, revision])
  return <div data-service-accounts="">
    <GroupHeader title={t('title')} description={t('description')} />
    <div className="mt-4 flex flex-wrap gap-2"><Button onClick={() => setEditor('new')}><Plus />{t('create')}</Button><Button variant="outline" disabled={loading} onClick={reload}><RefreshCw />{t('refresh')}</Button></div>
    {loading ? <p className="mt-4" role="status"><LoaderCircle className="inline size-4 animate-spin" />{common('loading')}</p>
      : error ? <p className="mt-4 text-destructive" role="alert">{error}</p>
        : !accounts.length ? <p className="mt-4 text-sm text-muted-foreground">{t('empty')}</p>
          : <div className="mt-4 grid gap-3">{accounts.map(account => <article key={account.service_account_id} data-service-account={account.service_account_id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl border p-4">
            <div className="min-w-0"><strong className="break-words">{account.name}</strong><p className="whitespace-pre-wrap break-words text-sm text-muted-foreground">{account.notes}</p><p className="text-xs text-muted-foreground">{t(account.enabled ? 'active' : 'disabled')}</p></div>
            <Button variant="outline" onClick={() => setEditor(account)}>{t('manage')}</Button>
          </article>)}</div>}
    {editor !== null && <ServiceAccountEditor key={editor === 'new' ? 'new' : editor.service_account_id} tenantId={tenantId} initial={editor === 'new' ? null : editor} onClose={() => setEditor(null)} onChanged={reload} />}
  </div>
}

function ServiceAccountEditor({ tenantId, initial, onClose, onChanged }: { tenantId: string; initial: ServiceAccount | null; onClose(): void; onChanged(): void }) {
  const t = useTranslate('serviceAccounts')
  const common = useTranslate('common')
  const [account, setAccount] = React.useState(initial)
  const [name, setName] = React.useState(initial?.name ?? '')
  const [notes, setNotes] = React.useState(initial?.notes ?? '')
  const [enabled, setEnabled] = React.useState(initial?.enabled ?? true)
  const [saving, setSaving] = React.useState(false)
  const [credentialBusy, setCredentialBusy] = React.useState(false)
  const [workspaceBusy, setWorkspaceBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [saved, setSaved] = React.useState(false)
  const id = React.useId()
  const busy = saving || credentialBusy || workspaceBusy
  const save = async () => {
    if (busy || !name.trim()) return
    setSaving(true); setError(''); setSaved(false)
    try {
      const draft = { name: name.trim(), notes }
      const next = account ? await updateServiceAccount(tenantId, account.service_account_id, { ...draft, enabled, expected_revision: account.revision }) : await createServiceAccount(tenantId, draft)
      setAccount(next); setName(next.name); setNotes(next.notes); setEnabled(next.enabled); setSaved(true); onChanged()
    } catch (cause) { setError(message(cause)) }
    finally { setSaving(false) }
  }
  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose() }}>
    <DialogContent data-service-account-editor="" className="flex max-h-[90dvh] max-w-2xl flex-col overflow-hidden" onEscapeKeyDown={event => { if (busy) event.preventDefault() }}>
      <DialogHeader><DialogTitle>{t(account ? 'manage' : 'create')}</DialogTitle><DialogDescription>{t('editorDescription')}</DialogDescription></DialogHeader>
      <div className="grid min-h-0 gap-6 overflow-y-auto py-1">
        <form className="grid gap-4" onSubmit={event => { event.preventDefault(); void save() }}>
          {account && <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs text-muted-foreground"><dt>{t('id')}</dt><dd className="break-all">{account.service_account_id}</dd><dt>{t('space')}</dt><dd className="break-all">{tenantId}</dd></dl>}
          <Field><Label htmlFor={`${id}-name`}>{t('name')}</Label><Input id={`${id}-name`} required value={name} disabled={busy} maxLength={128} onChange={event => { setName(event.target.value); setSaved(false) }} /></Field>
          <Field><Label htmlFor={`${id}-notes`}>{t('notes')}</Label><Textarea id={`${id}-notes`} value={notes} disabled={busy} maxLength={4000} onChange={event => { setNotes(event.target.value); setSaved(false) }} /></Field>
          {account && <Field><label className="flex items-center gap-2 text-sm"><input type="checkbox" className="size-4 shrink-0 accent-primary" checked={enabled} disabled={busy} onChange={event => { setEnabled(event.target.checked); setSaved(false) }} />{t('enabled')}</label><p className="text-xs text-muted-foreground">{t('disableHint')}</p></Field>}
          <div className="flex flex-wrap items-center gap-3"><Button type="submit" disabled={busy || !name.trim()}>{saving && <LoaderCircle className="animate-spin" />}{account ? common('save') : t('create')}</Button>{saved && <p className="text-sm text-success" role="status">{t('saved')}</p>}</div>
          {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
        </form>
        {account && <ServiceWorkspaces tenantId={tenantId} accountId={account.service_account_id} disabled={saving || credentialBusy || workspaceBusy} onBusy={setWorkspaceBusy} />}
        {account && <ServiceCredentials tenantId={tenantId} account={account} disabled={saving || workspaceBusy} onBusy={setCredentialBusy} />}
      </div>
      <div className="flex justify-end"><Button variant="outline" disabled={busy} onClick={onClose}>{common('close')}</Button></div>
    </DialogContent>
  </Dialog>
}

function ServiceCredentials({ tenantId, account, disabled, onBusy }: { tenantId: string; account: ServiceAccount; disabled: boolean; onBusy(value: boolean): void }) {
  const t = useTranslate('serviceAccounts')
  const common = useTranslate('common')
  const { locale } = useLocale()
  const [credentials, setCredentials] = React.useState<ServiceCredential[]>([])
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [name, setName] = React.useState('')
  const [days, setDays] = React.useState('30')
  const [read, setRead] = React.useState(true)
  const [execute, setExecute] = React.useState(false)
  const [token, setToken] = React.useState<string | null>(null)
  const [copied, setCopied] = React.useState(false)
  const [target, setTarget] = React.useState<ServiceCredential | null>(null)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const id = React.useId()
  const time = (value: number) => new Date(value).toLocaleString(locale)
  const duration = Number(days)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError('')
    void listServiceCredentials(tenantId, account.service_account_id, controller.signal).then(value => { if (!controller.signal.aborted) setCredentials(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(message(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, account.service_account_id, revision])
  const issue = async () => {
    if (busy || disabled || !account.enabled || !name.trim() || !Number.isInteger(duration) || duration < 1 || duration > 365 || (!read && !execute)) return
    setBusy(true); onBusy(true); setError(''); setToken(null); setCopied(false)
    try {
      const scopes: ServiceScope[] = [...(read ? ['resource.read' as const] : []), ...(execute ? ['run.execute' as const] : [])]
      const grant = await issueServiceCredential(tenantId, account.service_account_id, { name: name.trim(), scopes, expires_at_ms: Date.now() + duration * 86_400_000 })
      setCredentials(current => [grant.credential, ...current]); setToken(grant.access_token); setName('')
    } catch (cause) { setError(message(cause)) }
    finally { setBusy(false); onBusy(false) }
  }
  const revoke = async () => {
    if (!target || busy || disabled) return
    setBusy(true); onBusy(true); setError('')
    try {
      await revokeServiceCredential(tenantId, account.service_account_id, target.credential_id)
      setToken(null); setCopied(false); setTarget(null); reload()
    } catch (cause) { setError(message(cause)) }
    finally { setBusy(false); onBusy(false) }
  }
  return <section data-service-credentials="" className="grid gap-4">
    <GroupHeader title={t('credentials')} description={t('credentialsDescription')} />
    <form className="grid gap-3" onSubmit={event => { event.preventDefault(); void issue() }}>
      <Field><Label htmlFor={`${id}-name`}>{t('credentialName')}</Label><Input id={`${id}-name`} value={name} maxLength={128} disabled={busy || disabled || !account.enabled} onChange={event => setName(event.target.value)} /></Field>
      <div className="flex flex-wrap gap-4"><label className="flex items-center gap-2 text-sm"><input type="checkbox" className="size-4 shrink-0 accent-primary" checked={read} disabled={busy || disabled || !account.enabled} onChange={event => setRead(event.target.checked)} />{t('read')}</label><label className="flex items-center gap-2 text-sm"><input type="checkbox" className="size-4 shrink-0 accent-primary" checked={execute} disabled={busy || disabled || !account.enabled} onChange={event => setExecute(event.target.checked)} />{t('execute')}</label></div>
      <Field><Label htmlFor={`${id}-days`}>{t('days')}</Label><Input id={`${id}-days`} type="number" min="1" max="365" value={days} disabled={busy || disabled || !account.enabled} onChange={event => setDays(event.target.value)} /></Field>
      <Button type="submit" disabled={busy || disabled || !account.enabled || !name.trim() || !Number.isInteger(duration) || duration < 1 || duration > 365 || (!read && !execute)}>{busy && <LoaderCircle className="animate-spin" />}{t('issue')}</Button>
    </form>
    {token && <div data-service-token="" className="grid gap-3 rounded-lg border p-3"><Field><Label htmlFor={`${id}-token`}>{t('token')}</Label><Textarea id={`${id}-token`} readOnly value={token} className="min-h-16 font-mono text-xs" /><p className="text-xs leading-relaxed text-muted-foreground">{t('tokenHint')}</p></Field><Button variant="outline" onClick={() => { void copyText(token).then(() => setCopied(true)).catch(cause => setError(message(cause))) }}><Copy />{t(copied ? 'copied' : 'copy')}</Button></div>}
    {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
    {loading ? <p className="text-sm" role="status">{common('loading')}</p> : !credentials.length ? <p className="text-sm text-muted-foreground">{t('credentialsEmpty')}</p> : credentials.map(credential => <article key={credential.credential_id} data-service-credential={credential.credential_id} className="grid gap-2 rounded-lg border p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2"><strong className="break-words">{credential.name}</strong><span className="text-xs text-muted-foreground">{t(credential.revoked_at_ms !== null ? 'revoked' : credential.expires_at_ms <= Date.now() ? 'expired' : 'valid')}</span></div>
      <p className="text-xs text-muted-foreground">{credential.scopes.map(scope => t(scope === 'resource.read' ? 'read' : 'execute')).join(' · ')}</p><p className="text-xs text-muted-foreground">{t('expires', { time: time(credential.expires_at_ms) })}</p><p className="text-xs text-muted-foreground">{t('lastUsed', { time: credential.last_used_at_ms === null ? t('notUsed') : time(credential.last_used_at_ms) })}</p>
      {credential.revoked_at_ms === null && <Button variant="outline" disabled={busy || disabled} onClick={() => { setError(''); setTarget(credential) }}>{t('revoke')}</Button>}
    </article>)}
    <ActionDialog open={target !== null} title={t('revoke')} description={t('revokeDescription', { name: target?.name ?? '' })} cancelLabel={common('cancel')} confirmLabel={t('revoke')} destructive busy={busy} error={error} onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void revoke()} />
  </section>
}
