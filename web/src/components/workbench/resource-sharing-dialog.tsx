import * as React from 'react'
import { Building2, LoaderCircle, Search, Share2, Trash2, Users } from 'lucide-react'
import { navigate } from '@/app/navigation'
import { useWorkbench } from '@/state/workbench'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { DirectoryPagination } from '@/components/settings/directory-pagination'
import { useTranslate } from '@/i18n/provider'
import type { ResourceAccess, ResourcePermissions, ShareSubject } from '@/types'
import { getSharing, listSharingCandidates, listTransferCandidates, transferOwnership, setProjectInheritance, setSharing, subjectKey, subjectLabel, type SharingCandidates, type SharingSnapshot, type SharingTarget } from './sharing-api'
export type { SharingTarget } from './sharing-api'

const readOnly: ResourcePermissions = { view: true, submit: false, stop: false, configure: false }
const permissionLabels = { view: 'sharing.view', submit: 'sharing.submit', stop: 'sharing.stop', configure: 'sharing.configure' } as const
const sourceLabels = { owner: 'sharing.sourceOwner', direct_user: 'sharing.sourceDirect', group: 'sharing.sourceGroup', fork: 'sharing.sourceFork' } as const
const errorMessage = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

function PermissionText({ permissions }: { permissions: ResourcePermissions }) {
  const t = useTranslate('workspace')
  return <>{(Object.keys(permissionLabels) as (keyof ResourcePermissions)[]).filter(permission => permissions[permission]).map(permission => t(permissionLabels[permission])).join(' · ') || t('sharing.noPermissions')}</>
}

function EffectiveAccess({ access, target, canManage }: { access: ResourceAccess; target: SharingTarget; canManage: boolean }) {
  const t = useTranslate('workspace')
  if (target.kind === 'project') return <section className="grid gap-2 rounded-xl border p-3 text-sm" data-sharing-effective-access="">
    <h3 className="font-semibold">{t('sharing.effective')}</h3>
    <p>{t(canManage ? 'sharing.manageProjectRules' : 'sharing.readProjectRules')}</p>
  </section>
  return <section className="grid gap-2 rounded-xl border p-3 text-sm" data-sharing-effective-access="">
    <h3 className="font-semibold">{t('sharing.effective')}</h3>
    <p className="leading-relaxed"><PermissionText permissions={access.permissions} /></p>
    {access.role_limited && <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.roleLimited')}</p>}
    <ul className="grid gap-2 text-xs leading-relaxed text-muted-foreground">
      {access.sources.map((source, index) => <li className="break-words" key={`${source.kind}:${source.resource_kind}:${source.resource_id}:${source.group_id ?? index}`}>
        <span>{t(sourceLabels[source.kind], { name: source.group_name || source.group_id || '' })}</span>
        {source.kind === 'fork' && source.group_name && <span> · {t('sharing.sourceGroup', { name: source.group_name })}</span>}
        {source.resource_kind === 'workspace' && target.kind === 'session' && <span> · {t('sharing.inheritedWorkspace')}</span>}
        {source.resource_kind === 'project' && target.kind !== 'project' && <span> · {t('sharing.inheritedProject', { name: source.resource_name || source.resource_id })}</span>}
        <span> · <PermissionText permissions={source.permissions} /></span>
      </li>)}
    </ul>
    {target.kind === 'session' && access.sources.some(source => source.resource_kind === 'workspace') && <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.inheritedDescription')}</p>}
  </section>
}

export function ResourceSharingDialog({ target, onClose, onChanged }: {
  target: SharingTarget
  onClose(): void
  onChanged(): Promise<void>
}) {
  const t = useTranslate('workspace')
  const { tenants, currentTenantId, serverIdentity } = useWorkbench()
  const tenantId = target.tenantId ?? currentTenantId ?? undefined
  const personal = tenants.find(tenant => tenant.tenant_id === tenantId)?.kind === 'personal'
  const [snapshot, setSnapshot] = React.useState<SharingSnapshot | null>(null)
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [selected, setSelected] = React.useState<ShareSubject | null>(null)
  const [selectedInherited, setSelectedInherited] = React.useState(false)
  const [permissions, setPermissions] = React.useState(readOnly)
  const [busy, setBusy] = React.useState(false)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [projectRules, setProjectRules] = React.useState(false)
  const [transferOpen, setTransferOpen] = React.useState(false)
  const [recipient, setRecipient] = React.useState<ShareSubject | null>(null)
  const [confirmed, setConfirmed] = React.useState(false)
  const [retainPreviousOwner, setRetainPreviousOwner] = React.useState(true)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const cursor = cursors.at(-1) ?? null
  const canManage = Boolean(snapshot && (snapshot.access.can_manage_sharing ?? (snapshot.access.is_owner && snapshot.access.permissions.configure)))
  const requestTarget = React.useMemo(() => ({ kind: target.kind, id: target.id, title: target.title, tenantId }), [target.kind, target.id, target.title, tenantId])
  const actorId = serverIdentity?.user.user_id

  React.useEffect(() => {
    setRecipient(null); setConfirmed(false); setTransferOpen(false)
  }, [requestTarget, actorId, snapshot?.access.owner_user_id, snapshot?.access.ownership_revision])

  React.useEffect(() => {
    if (personal) return
    const controller = new AbortController()
    setLoading(true); setError(''); setSnapshot(null)
    void getSharing(requestTarget, { cursor }, controller.signal).then(value => {
      if (!controller.signal.aborted) setSnapshot(value)
    }).catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [requestTarget, cursor, personal, revision, actorId])

  const select = (subject: ShareSubject | null) => {
    setSelected(subject)
    const share = subject ? snapshot?.shares.find(share => subjectKey(share.subject) === subjectKey(subject)) : null
    setPermissions(share?.permissions ?? readOnly)
    setSelectedInherited(share?.inherited ?? false)
  }

  const save = async (subject: ShareSubject, value: ResourcePermissions | null) => {
    if (personal || !canManage || busy) return
    setBusy(true); setError('')
    try {
      await setSharing(requestTarget, subject, value)
      setSnapshot(await getSharing(requestTarget, { cursor }))
      setSelectedInherited(false)
      if (!value && selected && subjectKey(subject) === subjectKey(selected)) { setSelected(null); setPermissions(readOnly) }
      await onChanged()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }

  const inherit = async (enabled: boolean) => {
    if (busy || !snapshot?.project_inheritance?.can_change) return
    setBusy(true); setError('')
    try {
      const inheritance = await setProjectInheritance(requestTarget, enabled)
      setSnapshot(current => current ? { ...current, project_inheritance: inheritance } : current)
      await onChanged()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }

  const transfer = async () => {
    if (busy || !confirmed || recipient?.kind !== 'user' || !snapshot?.access.is_owner) return
    setBusy(true); setError('')
    try {
      await transferOwnership(requestTarget, recipient.user.user_id, snapshot.access, retainPreviousOwner)
      await onChanged()
      onClose()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }

  return <><Dialog open onOpenChange={open => { if (!open && !busy) onClose() }}>
    <DialogContent className="flex max-h-[90dvh] max-w-xl flex-col overflow-hidden [&>[data-slot=dialog-close]]:right-2 [&>[data-slot=dialog-close]]:top-2 [&>[data-slot=dialog-close]]:size-10" onEscapeKeyDown={event => { if (busy) event.preventDefault() }}>
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2"><Share2 className="size-4" />{t(target.kind === 'project' ? 'sharing.project' : target.kind === 'workspace' ? 'sharing.workspace' : 'sharing.session')}</DialogTitle>
        <DialogDescription>{t(personal ? 'sharing.personalDescription' : target.kind === 'project' ? 'sharing.projectDescription' : target.kind === 'workspace' ? 'sharing.workspaceDescription' : 'sharing.sessionDescription', { name: target.title })}</DialogDescription>
      </DialogHeader>
      <div className="grid min-h-0 gap-5 overflow-y-auto py-1">
        {personal ? <div className="grid gap-4" data-personal-sharing-guidance="">
          <p className="text-sm leading-relaxed text-muted-foreground">{t('sharing.personalSteps')}</p>
          <p className="text-sm leading-relaxed text-muted-foreground">{t('sharing.personalFiles')}</p>
          <Button asChild variant="outline" className="w-fit">
            <a href="/spaces/current" onClick={event => { event.preventDefault(); onClose(); navigate('/spaces/current') }}><Building2 />{t('sharing.manageSpaces')}</a>
          </Button>
        </div> : <p className="text-sm leading-relaxed text-muted-foreground">{t('sharing.machineOwnership')}</p>}
        {!personal && loading && <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />{t('sharing.loading')}</p>}
        {!personal && snapshot && <EffectiveAccess access={snapshot.access} target={target} canManage={canManage} />}
        {!personal && snapshot?.project_inheritance && <section className="grid gap-3 rounded-xl border p-3 text-sm" data-project-inheritance="" aria-busy={busy}>
          <label className="flex min-h-10 items-center gap-2">
            <input type="checkbox" checked={snapshot.project_inheritance.enabled} disabled={busy || !snapshot.project_inheritance.can_change} onChange={event => void inherit(event.target.checked)} />
            {t('sharing.inheritProject', { name: snapshot.project_inheritance.project_name })}
          </label>
          <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.inheritProjectDescription')}</p>
          <Button type="button" variant="outline" className="w-fit" disabled={busy} onClick={() => setProjectRules(true)}>{t('sharing.viewProjectRules')}</Button>
        </section>}
        {!personal && canManage && <>
          <SharingRecipientPicker target={requestTarget} disabled={busy} selected={selected} onSelect={select} />
          {selected && <div className="grid gap-3 rounded-xl border bg-card p-4" data-sharing-permissions="">
            <p className="break-words text-sm font-medium">{subjectLabel(selected)}</p>
            <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.replaceDescription')}</p>
            {selectedInherited && <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.replaceInherited')}</p>}
            <div className="grid grid-cols-2 gap-3">
              {(Object.keys(permissionLabels) as (keyof ResourcePermissions)[]).map(permission => <label key={permission} className="flex min-h-10 items-center gap-2 text-sm">
                <input type="checkbox" checked={permissions[permission]} disabled={busy || permission === 'view'} onChange={event => setPermissions(current => ({ ...current, [permission]: event.target.checked }))} />
                {t(permissionLabels[permission])}
              </label>)}
            </div>
            <Button disabled={busy} onClick={() => void save(selected, permissions)}>{busy && <LoaderCircle className="animate-spin" />}{t('sharing.save')}</Button>
          </div>}
        </>}
        {!personal && snapshot && (canManage || target.kind === 'project') && <section className="grid gap-3">
            <h3 className="text-sm font-semibold">{t('sharing.people')}</h3>
            {snapshot?.shares.length === 0 && <p className="text-sm text-muted-foreground">{t(target.kind === 'project' ? 'sharing.noProjectRules' : 'sharing.private')}</p>}
            {snapshot?.shares.map(share => <div className="flex flex-wrap items-center gap-2 rounded-xl border p-3" key={subjectKey(share.subject)} data-sharing-grant={subjectKey(share.subject)}>
              <div className="min-w-0 flex-[1_1_160px]">
                <p className="break-words text-sm font-medium">{share.subject.kind === 'group' && <Users className="mr-1 inline size-4" />}{subjectLabel(share.subject)}</p>
                {share.inherited && <p className="mt-1 text-xs text-muted-foreground">{t('sharing.inheritedFork')}</p>}
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground"><PermissionText permissions={share.permissions} /></p>
              </div>
              {canManage && <><Button className="min-h-10" variant="ghost" disabled={busy} onClick={() => { setSelected(share.subject); setPermissions(share.permissions); setSelectedInherited(share.inherited) }}>{t('sharing.edit')}</Button>
              <Button size="icon" variant="ghost" disabled={busy} aria-label={t('sharing.remove', { name: subjectLabel(share.subject) })} onClick={() => void save(share.subject, null)}><Trash2 className="text-destructive" /></Button></>}
            </div>)}
            <DirectoryPagination page={cursors.length} count={snapshot?.shares.length ?? 0} loading={loading || busy} nextCursor={snapshot?.next_cursor ?? null}
              onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />
          </section>}
        {!personal && target.kind !== 'project' && snapshot?.access.is_owner && snapshot.access.permissions.configure &&
          <section className="grid gap-3 rounded-xl border p-3 text-sm" data-ownership-transfer="">
            <Button type="button" variant="outline" className="w-fit" disabled={busy} onClick={() => setTransferOpen(open => !open)}>{t('ownership.action')}</Button>
            {transferOpen && <>
              <p className="leading-relaxed text-muted-foreground">{t(target.kind === 'workspace' ? 'ownership.workspaceDescription' : 'ownership.sessionDescription')}</p>
              <p className="text-xs leading-relaxed text-muted-foreground">{t('ownership.executionDescription')}</p>
              <label className="flex min-h-10 items-center gap-2"><input type="checkbox" checked={retainPreviousOwner} disabled={busy} onChange={event => { setRetainPreviousOwner(event.target.checked); setConfirmed(false) }} />{t('ownership.retain')}</label>
              <p className="text-xs leading-relaxed text-muted-foreground">{t(retainPreviousOwner ? 'ownership.retainDescription' : 'ownership.removeDescription')}</p>
              <SharingRecipientPicker target={requestTarget} disabled={busy} selected={recipient} transfer
                onSelect={value => { setRecipient(value); setConfirmed(false) }} />
              {recipient?.kind === 'user' && <>
                <label className="flex min-h-10 items-center gap-2"><input data-ownership-confirm="" type="checkbox" checked={confirmed} disabled={busy} onChange={event => setConfirmed(event.target.checked)} />{t('ownership.confirm', { name: recipient.user.username })}</label>
                <Button type="button" variant="destructive" disabled={busy || !confirmed} onClick={() => void transfer()}>{busy && <LoaderCircle className="animate-spin" />}{t('ownership.submit')}</Button>
              </>}
            </>}
          </section>}
        {error && <div role="alert" className="text-sm text-destructive">{error}<Button variant="ghost" disabled={busy} onClick={reload}>{t('sharing.retry')}</Button></div>}
      </div>
    </DialogContent>
  </Dialog>
    {projectRules && snapshot?.project_inheritance && <ResourceSharingDialog
      target={{ kind: 'project', id: snapshot.project_inheritance.project_id, title: snapshot.project_inheritance.project_name, tenantId }}
      onClose={() => { setProjectRules(false); reload() }} onChanged={onChanged} />}
  </>
}

function SharingRecipientPicker({ target, disabled, selected, onSelect, transfer = false }: {
  target: SharingTarget
  disabled: boolean
  selected: ShareSubject | null
  onSelect(subject: ShareSubject | null): void
  transfer?: boolean
}) {
  const t = useTranslate('workspace')
  const [kind, setKind] = React.useState<ShareSubject['kind']>('user')
  const [draftQuery, setDraftQuery] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [page, setPage] = React.useState<SharingCandidates | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const cursor = cursors.at(-1) ?? null
  const prefix = transfer ? 'ownership' : 'sharing'

  React.useEffect(() => {
    if (selected && selected.kind !== kind) {
      setKind(selected.kind); setQuery(''); setDraftQuery(''); setCursors([null])
    }
  }, [selected, kind])

  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError(''); setPage(null)
    const request = transfer ? listTransferCandidates(target, { query, cursor }, controller.signal) : listSharingCandidates(target, kind, { query, cursor }, controller.signal)
    void request.then(value => {
      if (!controller.signal.aborted) setPage(value)
    }).catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [target, kind, query, cursor, revision, transfer])

  return <section className="grid gap-3" data-sharing-candidates="">
    {!transfer && <Field><Label htmlFor="sharing-kind">{t('sharing.recipient')}</Label>
      <Select id="sharing-kind" value={kind} disabled={disabled} onValueChange={nextValue => { onSelect(null); setKind(nextValue as ShareSubject['kind']); setQuery(''); setDraftQuery(''); setCursors([null]) }}>
        <option value="user">{t('sharing.person')}</option><option value="group">{t('sharing.group')}</option>
      </Select>
    </Field>}
    <form className="flex flex-wrap items-end gap-2" onSubmit={event => { event.preventDefault(); setQuery(draftQuery.trim()); setCursors([null]); reload() }}>
      <Field className="min-w-0 flex-[1_1_180px]"><Label htmlFor={`${prefix}-search`}>{t(transfer ? 'ownership.search' : kind === 'user' ? 'sharing.searchPerson' : 'sharing.searchGroup')}</Label><Input id={`${prefix}-search`} value={draftQuery} disabled={disabled} onChange={event => setDraftQuery(event.target.value)} /></Field>
      <Button type="submit" variant="outline" disabled={disabled}><Search />{t('sharing.search')}</Button>
    </form>
    {kind === 'group' && <p className="text-xs leading-relaxed text-muted-foreground">{t('sharing.groupDescription')}</p>}
    {loading ? <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />{t('sharing.loading')}</p>
      : error ? <p role="alert" className="text-sm text-destructive">{error}<Button variant="ghost" onClick={reload}>{t('sharing.retry')}</Button></p>
        : !page?.candidates.length ? <p className="text-sm text-muted-foreground">{t('sharing.noCandidates')}</p>
          : <div className="grid gap-2">{page.candidates.map(subject => <Button className="h-auto min-h-10 justify-start whitespace-normal px-3 py-2 text-left" variant={selected && subjectKey(selected) === subjectKey(subject) ? 'secondary' : 'outline'}
            key={subjectKey(subject)} disabled={disabled} onClick={() => onSelect(subject)} data-sharing-candidate={subjectKey(subject)}>
            {subject.kind === 'group' && <Users className="shrink-0" />}<span className="min-w-0 break-words">{subjectLabel(subject)}{subject.kind === 'group' && <span className="ml-2 text-xs text-muted-foreground">{t('sharing.groupCount', { count: subject.group.member_count })}</span>}</span>
          </Button>)}</div>}
    <DirectoryPagination page={cursors.length} count={page?.candidates.length ?? 0} loading={loading || disabled} nextCursor={page?.next_cursor ?? null}
      onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />
  </section>
}
