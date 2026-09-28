import * as React from 'react'
import { LoaderCircle, RefreshCw, Search, Trash2, UserPlus } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { MembershipRecord, TenantRole } from '@/types'
import { ActionDialog, GroupHeader } from './settings-ui'
import { listMemberships, removeMembership, setMembership } from './platform-admin-api'
import { DirectoryPagination } from './directory-pagination'
import styles from './platform-settings.module.css'

const roles: TenantRole[] = ['viewer', 'member', 'admin', 'owner']
const adminRoles: TenantRole[] = ['viewer', 'member', 'admin']

function errorCopy(
  cause: unknown,
  action: 'load' | 'save' | 'remove',
  t: ReturnType<typeof useTranslate<'settings'>>,
) {
  if (cause instanceof ApiError && cause.status === 403) return t('platform.permission')
  if (action === 'save' && cause instanceof ApiError && (cause.status === 400 || cause.status === 404)) {
    return t('members.userUnknown')
  }
  return t(action === 'load' ? 'members.loadError' : action === 'save' ? 'members.saveError' : 'members.removeError')
}

export function PlatformMembersSettings({
  tenantId,
  actorRole = 'owner',
}: {
  tenantId: string
  actorRole?: Extract<TenantRole, 'admin' | 'owner'>
}) {
  const t = useTranslate('settings')
  const commonT = useTranslate('common')
  const [members, setMembers] = React.useState<MembershipRecord[]>([])
  const [draftQuery, setDraftQuery] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [nextCursor, setNextCursor] = React.useState<string | null>(null)
  const request = React.useRef<AbortController | null>(null)
  const cursor = cursors.at(-1) ?? null
  const [rolesByUser, setRolesByUser] = React.useState<Record<string, TenantRole>>({})
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [message, setMessage] = React.useState('')
  const [userId, setUserId] = React.useState('')
  const [newRole, setNewRole] = React.useState<TenantRole>('member')
  const [saving, setSaving] = React.useState('')
  const [removeTarget, setRemoveTarget] = React.useState<MembershipRecord | null>(null)
  const grantableRoles = actorRole === 'owner' ? roles : adminRoles
  const listState = loading && !members.length
    ? 'loading'
    : error && !members.length
      ? 'error'
      : members.length
        ? 'ready'
        : 'empty'

  const load = React.useCallback(async () => {
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    setLoading(true)
    setError('')
    try {
      const next = await listMemberships(tenantId, { query, cursor }, controller.signal)
      if (controller.signal.aborted) return
      setMembers(next.memberships)
      setNextCursor(next.next_cursor)
      setRolesByUser(Object.fromEntries(next.memberships.map(member => [member.user_id, member.role])))
    } catch (cause) {
      if (!controller.signal.aborted) setError(errorCopy(cause, 'load', t))
    } finally {
      if (!controller.signal.aborted) setLoading(false)
    }
  }, [t, tenantId, query, cursor])

  React.useEffect(() => { setMembers([]); setNextCursor(null); void load(); return () => request.current?.abort() }, [load])

  const save = async (targetUserId: string, role: TenantRole, isNew: boolean) => {
    const normalized = targetUserId.trim()
    if (!normalized) return
    setSaving(normalized)
    setError('')
    setMessage('')
    try {
      await setMembership(tenantId, normalized, role)
      if (isNew) setUserId('')
      await load()
      setMessage(t('members.saved'))
    } catch (cause) {
      setError(errorCopy(cause, 'save', t))
    } finally {
      setSaving('')
    }
  }

  const remove = async () => {
    if (!removeTarget) return
    setSaving(removeTarget.user_id)
    setError('')
    setMessage('')
    try {
      await removeMembership(tenantId, removeTarget.user_id)
      setRemoveTarget(null)
      await load()
      setMessage(t('members.removed'))
    } catch (cause) {
      setError(errorCopy(cause, 'remove', t))
    } finally {
      setSaving('')
    }
  }

  return (
    <div data-platform-members="" data-platform-list-state={listState}>
      <GroupHeader title={t('platform.members')} description={t('members.description')} />
      <p className={styles.notice}>{t('members.loginFirst')}</p>

      <form
        className={`${styles.formGrid} mt-4`}
        onSubmit={event => {
          event.preventDefault()
          void save(userId, newRole, true)
        }}
      >
        <Field>
          <Label htmlFor="platform-member-id">{t('members.userId')}</Label>
          <Input
            id="platform-member-id"
            autoComplete="off"
            value={userId}
            placeholder={t('members.userIdPlaceholder')}
            onChange={event => setUserId(event.target.value)}
          />
        </Field>
        <Field>
          <Label htmlFor="platform-member-role">{t('members.role')}</Label>
          <Select id="platform-member-role" value={newRole} onValueChange={nextValue => setNewRole(nextValue as TenantRole)}>
            {grantableRoles.map(role => <option value={role} key={role}>{t(`role.${role}`)}</option>)}
          </Select>
        </Field>
        <Button type="submit" disabled={!userId.trim() || Boolean(saving)}>
          {saving === userId.trim() ? <LoaderCircle className={styles.spinner} /> : <UserPlus />}
          {saving === userId.trim() ? t('members.adding') : t('members.add')}
        </Button>
      </form>

      {error && (loading || members.length) ? <p className="mt-4 text-sm text-destructive" role="alert">{error}</p> : null}
      {loading && members.length ? <p className="mt-4 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p> : null}
      {message ? <p className="mt-4 text-sm text-success" role="status">{message}</p> : null}

      <div className="mt-8 flex items-center justify-between gap-3">
        <h3 className="text-sm font-semibold">{t('members.list')}</h3>
        <Button type="button" variant="ghost" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}
        </Button>
      </div>

      <form className={`${styles.searchForm} mt-3`} onSubmit={event => {
        event.preventDefault()
        if (query === draftQuery.trim() && cursor === null) void load()
        else { setQuery(draftQuery.trim()); setCursors([null]) }
      }}>
        <Field>
          <Label htmlFor="platform-member-search">{t('directory.searchMembers')}</Label>
          <Input id="platform-member-search" value={draftQuery} placeholder={t('directory.memberPlaceholder')} onChange={event => setDraftQuery(event.target.value)} />
        </Field>
        <Button type="submit" variant="outline"><Search />{t('directory.search')}</Button>
      </form>

      {loading && !members.length ? (
        <div className={`${styles.statePanel} mt-3`} role="status">
          <div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div>
        </div>
      ) : error && !members.length ? (
        <div className={`${styles.statePanel} mt-3`} role="alert">
          <div><span>{error}</span><Button type="button" variant="outline" onClick={() => void load()}>{t('platform.retry')}</Button></div>
        </div>
      ) : members.length ? (
        <div className={`${styles.list} mt-3`}>
          {members.map(member => {
            const display = member.username
            const busy = saving === member.user_id
            const ownerLocked = actorRole !== 'owner' && member.role === 'owner'
            const rowRoles = ownerLocked ? (['owner'] as TenantRole[]) : grantableRoles
            return (
              <article className={styles.row} key={member.user_id} data-platform-member={member.user_id}>
                <div className={styles.identity}>
                  <strong>{display}</strong>
                  <code>{member.user_id}</code>
                </div>
                <Select
                  aria-label={`${display} · ${t('members.role')}`}
                  value={rolesByUser[member.user_id] ?? member.role}
                  disabled={busy || ownerLocked}
                  onValueChange={nextValue => setRolesByUser(current => ({
                    ...current,
                    [member.user_id]: nextValue as TenantRole,
                  }))}
                >
                  {rowRoles.map(role => <option value={role} key={role}>{t(`role.${role}`)}</option>)}
                </Select>
                <div className={styles.actions}>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    disabled={busy || ownerLocked || (rolesByUser[member.user_id] ?? member.role) === member.role}
                    onClick={() => void save(member.user_id, rolesByUser[member.user_id] ?? member.role, false)}
                  >
                    {busy ? <LoaderCircle className={styles.spinner} /> : null}
                    {busy ? t('members.saving') : t('members.saveRole')}
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`${t('members.remove')} · ${display}`}
                    disabled={busy || ownerLocked}
                    onClick={() => setRemoveTarget(member)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              </article>
            )
          })}
        </div>
      ) : <div className={`${styles.statePanel} mt-3`}>{t('members.empty')}</div>}

      <DirectoryPagination page={cursors.length} count={members.length} loading={loading || Boolean(saving)} nextCursor={nextCursor}
        onPrevious={() => setCursors(current => current.slice(0, -1))} onNext={value => setCursors(current => [...current, value])} />

      <ActionDialog
        open={Boolean(removeTarget)}
        title={t('members.removeTitle')}
        description={t('members.removeDescription', {
          name: removeTarget?.username || '',
        })}
        cancelLabel={commonT('cancel')}
        confirmLabel={t('members.remove')}
        busyLabel={t('members.removing')}
        busy={Boolean(removeTarget && saving === removeTarget.user_id)}
        destructive
        onOpenChange={next => { if (!next) setRemoveTarget(null) }}
        onConfirm={() => void remove()}
      />
    </div>
  )
}
