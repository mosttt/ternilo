import * as React from 'react'
import { LoaderCircle, Plus, RefreshCw, Search, Trash2, UserPlus } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Textarea } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { GroupPage, GroupRecord, MemberPage } from '@/types'
import { ActionDialog, GroupHeader } from './settings-ui'
import { DirectoryPagination } from './directory-pagination'
import { createGroup, getGroup, listGroupMembers, listGroups, listMemberships, removeGroup, setGroupMember, updateGroup } from './platform-admin-api'
import styles from './platform-settings.module.css'

const message = (cause: unknown) => cause instanceof Error ? cause.message : String(cause)

export function PlatformGroupsSettings({ tenantId }: { tenantId: string }) {
  const t = useTranslate('settings')
  const commonT = useTranslate('common')
  const [draftQuery, setDraftQuery] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [page, setPage] = React.useState<GroupPage | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const [editor, setEditor] = React.useState<GroupRecord | 'new' | null>(null)
  const [removeTarget, setRemoveTarget] = React.useState<GroupRecord | null>(null)
  const [removing, setRemoving] = React.useState(false)
  const [removeError, setRemoveError] = React.useState('')
  const cursor = cursors.at(-1) ?? null

  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError(''); setPage(null)
    void listGroups(tenantId, { query, cursor }, controller.signal).then(value => {
      if (!controller.signal.aborted) setPage(value)
    }).catch(cause => { if (!controller.signal.aborted) setError(message(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, query, cursor, revision])

  const remove = async () => {
    if (!removeTarget || removing) return
    setRemoving(true); setRemoveError('')
    try {
      await removeGroup(tenantId, removeTarget.group_id)
      setRemoveTarget(null); reload()
    } catch (cause) { setRemoveError(message(cause)) }
    finally { setRemoving(false) }
  }

  return <div data-platform-groups="">
    <GroupHeader title={t('groups.title')} description={t('groups.description')} />
    <p className={styles.notice}>{t('groups.boundary')}</p>
    <div className="mt-4 flex flex-wrap gap-2">
      <Button onClick={() => setEditor('new')}><Plus />{t('groups.create')}</Button>
      <Button variant="ghost" disabled={loading} onClick={reload}><RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}</Button>
    </div>
    <form className={`${styles.searchForm} mt-5`} onSubmit={event => { event.preventDefault(); setQuery(draftQuery.trim()); setCursors([null]); reload() }}>
      <Field><Label htmlFor="platform-group-search">{t('groups.search')}</Label><Input id="platform-group-search" value={draftQuery} onChange={event => setDraftQuery(event.target.value)} /></Field>
      <Button type="submit" variant="outline"><Search />{t('directory.search')}</Button>
    </form>
    {loading ? <div className={`${styles.statePanel} mt-3`} role="status"><div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div></div>
      : error ? <div className={`${styles.statePanel} mt-3`} role="alert"><div>{error}<Button variant="outline" onClick={reload}>{t('platform.retry')}</Button></div></div>
        : !page?.groups.length ? <div className={`${styles.statePanel} mt-3`}>{t('groups.empty')}</div>
          : <div className={`${styles.list} mt-3`}>{page.groups.map(group => <article className={styles.groupRow} key={group.group_id} data-platform-group={group.group_id}>
            <div><strong>{group.name}</strong>{group.description && <p>{group.description}</p>}<p>{t('groups.memberCount', { count: group.member_count })}</p></div>
            <Button variant="outline" onClick={() => setEditor(group)}>{t('groups.manage')}</Button>
            <Button variant="ghost" size="icon" aria-label={t('groups.removeLabel', { name: group.name })} onClick={() => { setRemoveError(''); setRemoveTarget(group) }}><Trash2 /></Button>
          </article>)}</div>}
    <DirectoryPagination page={cursors.length} count={page?.groups.length ?? 0} loading={loading} nextCursor={page?.next_cursor ?? null}
      onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />
    {editor !== null && <GroupEditor key={editor === 'new' ? 'new' : editor.group_id} tenantId={tenantId} initialGroup={editor === 'new' ? null : editor}
      onClose={() => setEditor(null)} onChanged={reload} />}
    <ActionDialog open={removeTarget !== null} title={t('groups.removeTitle')} description={t('groups.removeDescription', { name: removeTarget?.name ?? '' })}
      cancelLabel={commonT('cancel')} confirmLabel={t('groups.remove')} busyLabel={t('groups.removing')} busy={removing} error={removeError} destructive
      onOpenChange={open => { if (!open) setRemoveTarget(null) }} onConfirm={() => void remove()} />
  </div>
}

function GroupEditor({ tenantId, initialGroup, onClose, onChanged }: {
  tenantId: string
  initialGroup: GroupRecord | null
  onClose(): void
  onChanged(): void
}) {
  const t = useTranslate('settings')
  const [group, setGroup] = React.useState(initialGroup)
  const [name, setName] = React.useState(initialGroup?.name ?? '')
  const [description, setDescription] = React.useState(initialGroup?.description ?? '')
  const [saving, setSaving] = React.useState(false)
  const [memberBusy, setMemberBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [saved, setSaved] = React.useState(false)
  const busy = saving || memberBusy
  const save = async () => {
    if (!name.trim() || busy) return
    setSaving(true); setError(''); setSaved(false)
    try {
      const input = { name: name.trim(), description: description.trim() || null }
      const next = group ? await updateGroup(tenantId, group.group_id, input) : await createGroup(tenantId, input)
      setGroup(next); setName(next.name); setDescription(next.description ?? ''); setSaved(true); onChanged()
    } catch (cause) { setError(message(cause)) }
    finally { setSaving(false) }
  }

  return <Dialog open onOpenChange={open => { if (!open && !busy) onClose() }}>
    <DialogContent className="flex max-h-[90dvh] max-w-2xl flex-col overflow-hidden [&>[data-slot=dialog-close]]:right-2 [&>[data-slot=dialog-close]]:top-2 [&>[data-slot=dialog-close]]:size-10" onEscapeKeyDown={event => { if (busy) event.preventDefault() }}>
      <DialogHeader><DialogTitle>{t(group ? 'groups.manage' : 'groups.create')}</DialogTitle><DialogDescription>{t('groups.editorDescription')}</DialogDescription></DialogHeader>
      <div className="grid min-h-0 gap-6 overflow-y-auto py-1">
        <form className={styles.groupEditor} onSubmit={event => { event.preventDefault(); void save() }}>
          <Field><Label htmlFor="group-name">{t('groups.name')}</Label><Input id="group-name" value={name} maxLength={120} disabled={busy} onChange={event => { setName(event.target.value); setSaved(false) }} /></Field>
          <Field><Label htmlFor="group-description">{t('groups.note')}</Label><Textarea id="group-description" value={description} maxLength={2000} disabled={busy} onChange={event => { setDescription(event.target.value); setSaved(false) }} /></Field>
          <div className="flex flex-wrap items-center gap-3"><Button type="submit" disabled={busy || !name.trim()}>{saving && <LoaderCircle className={styles.spinner} />}{t(group ? 'groups.save' : 'groups.create')}</Button>{saved && <p className="text-sm text-success" role="status">{t('groups.saved')}</p>}</div>
          {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
        </form>
        {group && <GroupMembers key={group.group_id} tenantId={tenantId} group={group} disabled={saving} onBusy={setMemberBusy} onChanged={next => { setGroup(next); onChanged() }} />}
      </div>
    </DialogContent>
  </Dialog>
}

function GroupMembers({ tenantId, group, disabled, onBusy, onChanged }: {
  tenantId: string
  group: GroupRecord
  disabled: boolean
  onBusy(value: boolean): void
  onChanged(group: GroupRecord): void
}) {
  const t = useTranslate('settings')
  const [adding, setAdding] = React.useState(false)
  const [draftQuery, setDraftQuery] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [page, setPage] = React.useState<MemberPage | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [notice, setNotice] = React.useState('')
  const [saving, setSaving] = React.useState('')
  const [added, setAdded] = React.useState<string[]>([])
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const cursor = cursors.at(-1) ?? null

  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError(''); setPage(null)
    const input = { query, cursor }
    void (adding ? listMemberships(tenantId, input, controller.signal) : listGroupMembers(tenantId, group.group_id, input, controller.signal))
      .then(value => { if (!controller.signal.aborted) setPage(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(message(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, group.group_id, adding, query, cursor, revision])

  const save = async (userId: string) => {
    if (saving || disabled) return
    setSaving(userId); onBusy(true); setError(''); setNotice('')
    try {
      await setGroupMember(tenantId, group.group_id, userId, adding)
      setNotice(t(adding ? 'groups.memberAdded' : 'groups.memberRemoved'))
      if (adding) setAdded(current => [...current, userId])
      else reload()
      onChanged(await getGroup(tenantId, group.group_id))
    } catch (cause) { setError(message(cause)) }
    finally { setSaving(''); onBusy(false) }
  }

  return <section data-group-members="">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <h3 className="text-sm font-semibold">{t(adding ? 'groups.addMembers' : 'groups.members', { count: group.member_count })}</h3>
      <Button variant="outline" disabled={disabled || Boolean(saving)} onClick={() => { setAdding(!adding); setQuery(''); setDraftQuery(''); setCursors([null]); setAdded([]); setNotice('') }}>{adding ? t('groups.backMembers') : <><UserPlus />{t('groups.addMembers')}</>}</Button>
    </div>
    {adding && <p className="mt-3 text-sm leading-relaxed text-muted-foreground">{t('groups.addDescription')}</p>}
    <form className={`${styles.searchForm} mt-4`} onSubmit={event => { event.preventDefault(); setQuery(draftQuery.trim()); setCursors([null]); reload() }}>
      <Field><Label htmlFor="group-member-search">{t('directory.searchMembers')}</Label><Input id="group-member-search" value={draftQuery} placeholder={t('directory.memberPlaceholder')} onChange={event => setDraftQuery(event.target.value)} /></Field>
      <Button type="submit" variant="outline" disabled={disabled || Boolean(saving)}><Search />{t('directory.search')}</Button>
    </form>
    {notice && <p className="mt-3 text-sm text-success" role="status">{notice}</p>}
    {error && <p className="mt-3 text-sm text-destructive" role="alert">{error}<Button variant="ghost" disabled={loading} onClick={reload}>{t('platform.retry')}</Button></p>}
    {loading ? <p className="mt-4 flex items-center gap-2 text-sm text-muted-foreground" role="status"><LoaderCircle className={styles.spinner} />{t('platform.loading')}</p>
      : !page?.memberships.length ? <p className="mt-4 text-sm text-muted-foreground">{t('groups.membersEmpty')}</p>
        : <div className={`${styles.list} mt-4`}>{page.memberships.map(member => {
          const label = member.username
          return <article className={styles.groupMember} key={member.user_id} data-group-member={member.user_id}>
            <div><strong>{label}</strong><small>{member.user_id}</small></div>
            <Button variant={adding ? 'outline' : 'ghost'} disabled={disabled || Boolean(saving) || (adding && added.includes(member.user_id))} aria-label={t(adding ? 'groups.addMemberLabel' : 'groups.removeMemberLabel', { name: label })} onClick={() => void save(member.user_id)}>
              {saving === member.user_id ? <LoaderCircle className={styles.spinner} /> : adding ? <Plus /> : <Trash2 />}
              {adding ? t(added.includes(member.user_id) ? 'groups.added' : 'groups.add') : null}
            </Button>
          </article>
        })}</div>}
    <DirectoryPagination page={cursors.length} count={page?.memberships.length ?? 0} loading={loading || disabled || Boolean(saving)} nextCursor={page?.next_cursor ?? null}
      onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />
  </section>
}
