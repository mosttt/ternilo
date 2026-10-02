import * as React from 'react'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { DirectoryPagination } from './directory-pagination'
import { listServiceWorkspaces, setServiceWorkspaceAccess, type ServiceWorkspace, type ServiceWorkspacePage } from './service-accounts-api'

export function ServiceWorkspaces({ tenantId, accountId, disabled, onBusy }: { tenantId: string; accountId: string; disabled: boolean; onBusy(value: boolean): void }) {
  const t = useTranslate('serviceAccounts')
  const common = useTranslate('common')
  const [open, setOpen] = React.useState(false)
  const [draft, setDraft] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [page, setPage] = React.useState<ServiceWorkspacePage | null>(null)
  const [loading, setLoading] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const cursor = cursors.at(-1) ?? null
  const id = React.useId()
  React.useEffect(() => {
    if (!open) return
    const controller = new AbortController()
    setLoading(true); setError('')
    void listServiceWorkspaces(tenantId, accountId, query, cursor, controller.signal)
      .then(value => { if (!controller.signal.aborted) setPage(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, accountId, open, query, cursor, revision])
  return <details data-service-workspaces="" onToggle={event => setOpen(event.currentTarget.open)}>
    <summary className="cursor-pointer text-sm font-semibold">{t('workspaces')}</summary>
    <div className="mt-4 grid gap-4">
      <p className="text-xs leading-relaxed text-muted-foreground">{t('workspaceDescription')}</p>
      <form className="flex flex-wrap items-end gap-2" onSubmit={event => { event.preventDefault(); setQuery(draft.trim()); setCursors([null]); reload() }}>
        <Field className="min-w-0 flex-1"><Label htmlFor={id}>{t('workspaceSearch')}</Label><Input id={id} value={draft} onChange={event => setDraft(event.target.value)} /></Field><Button type="submit" variant="outline" disabled={disabled || loading}>{common('search')}</Button>
      </form>
      {error && <p className="text-sm text-destructive" role="alert">{error}<Button variant="ghost" disabled={loading || disabled} onClick={reload}>{common('retry')}</Button></p>}
      {loading ? <p role="status" className="text-sm">{common('loading')}</p> : !page?.workspaces.length ? <p className="text-sm text-muted-foreground">{t('workspacesEmpty')}</p> : page.workspaces.map(workspace => <WorkspaceGrant key={`${workspace.workspace_id}:${JSON.stringify(workspace.permissions)}`} tenantId={tenantId} accountId={accountId} workspace={workspace} disabled={disabled} onBusy={onBusy} onSaved={reload} />)}
      <DirectoryPagination page={cursors.length} count={page?.workspaces.length ?? 0} nextCursor={page?.next_cursor ?? null} loading={loading || disabled} onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />
    </div>
  </details>
}

function level(workspace: ServiceWorkspace) {
  const permissions = workspace.permissions
  if (!permissions) return 'none'
  if (permissions.configure || permissions.submit !== permissions.stop) return 'custom'
  return permissions.submit ? 'execute' : 'read'
}

function WorkspaceGrant({ tenantId, accountId, workspace, disabled, onBusy, onSaved }: { tenantId: string; accountId: string; workspace: ServiceWorkspace; disabled: boolean; onBusy(value: boolean): void; onSaved(): void }) {
  const t = useTranslate('serviceAccounts')
  const common = useTranslate('common')
  const [selection, setSelection] = React.useState(() => level(workspace))
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const id = React.useId()
  const save = async () => {
    if (busy || disabled || selection === 'custom') return
    setBusy(true); onBusy(true); setError('')
    try {
      await setServiceWorkspaceAccess(tenantId, accountId, workspace, selection === 'none' ? null : { view: true, submit: selection === 'execute', stop: selection === 'execute', configure: false })
      onSaved()
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false); onBusy(false) }
  }
  return <article data-service-workspace={workspace.workspace_id} className="grid gap-3 rounded-lg border p-3 text-sm">
    <div className="min-w-0"><strong className="break-words">{workspace.name}</strong><p className="text-xs text-muted-foreground">{workspace.computer_name ?? t(workspace.placement === 'cloud' ? 'managedWorkspace' : 'localWorkspace')}</p></div>
    <Field><Label htmlFor={id}>{t('workspaceAccess')}</Label><Select id={id} value={selection} disabled={busy || disabled} onValueChange={setSelection}>
      <option value="none">{t('workspaceNone')}</option><option value="read">{t('workspaceRead')}</option><option value="execute">{t('workspaceExecute')}</option>{level(workspace) === 'custom' && <option value="custom" disabled>{t('workspaceCustom')}</option>}
    </Select></Field>
    <Button variant="outline" disabled={busy || disabled || selection === level(workspace) || selection === 'custom'} onClick={() => void save()}>{common(busy ? 'saving' : 'save')}</Button>
    {error && <p className="text-destructive" role="alert">{error}</p>}
  </article>
}
