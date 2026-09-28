import * as React from 'react'
import { Ban, ChevronLeft, ChevronRight, Ellipsis, LoaderCircle, RefreshCw, Search, ShieldCheck, UserRoundX } from 'lucide-react'
import { ApiError } from '@/api/client'
import type { PlatformRole } from '@/auth/server'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { ActionDialog, SectionHeader } from '@/components/settings/settings-ui'
import { useLocale, useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { listAccounts, reviewAccount, setAccountRole, setAccountStatus, type AccountPage, type AccountStatus, type AccountStatusAction, type PlatformAccount } from './admin-api'
import { RegistrationPanel } from './registration-panel'
import css from './admin.module.css'

const statusLabels = { active: 'accounts.status.active', pending: 'accounts.status.pending', rejected: 'accounts.status.rejected', banned: 'accounts.status.banned', removed: 'accounts.status.removed' } as const
const statusActions = {
  ban: { label: 'accounts.ban', title: 'accounts.banTitle', description: 'accounts.banDescription', saved: 'accounts.banned' },
  unban: { label: 'accounts.unban', title: 'accounts.unbanTitle', description: 'accounts.unbanDescription', saved: 'accounts.unbanned' },
  remove: { label: 'accounts.remove', title: 'accounts.removeTitle', description: 'accounts.removeDescription', saved: 'accounts.removed' },
} as const
const roles: PlatformRole[] = ['owner', 'admin', 'operator', 'auditor', 'user']
const assignableRoles = roles.filter((role): role is Exclude<PlatformRole, 'owner'> => role !== 'owner')

export function AccountsPage() {
  const t = useTranslate('admin')
  const { locale } = useLocale()
  const { serverIdentity, notify } = useWorkbench()
  const [draftQuery, setDraftQuery] = React.useState('')
  const [query, setQuery] = React.useState('')
  const [role, setRole] = React.useState<PlatformRole | ''>('')
  const [status, setStatus] = React.useState<AccountStatus | ''>('')
  const [cursors, setCursors] = React.useState<Array<string | null>>([null])
  const [page, setPage] = React.useState<AccountPage | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const [draftRoles, setDraftRoles] = React.useState<Record<string, Exclude<PlatformRole, 'owner'>>>({})
  const [change, setChange] = React.useState<{ account: PlatformAccount; role: Exclude<PlatformRole, 'owner'> } | null>(null)
  const [review, setReview] = React.useState<{ account: PlatformAccount; decision: 'approve' | 'reject' } | null>(null)
  const [statusChange, setStatusChange] = React.useState<{ account: PlatformAccount; action: AccountStatusAction } | null>(null)
  const [saving, setSaving] = React.useState(false)
  const [saveError, setSaveError] = React.useState('')
  const cursor = cursors.at(-1) ?? null
  const canAppoint = serverIdentity?.platform_role === 'owner'
  const canReview = canAppoint || serverIdentity?.platform_role === 'admin'
  const canManageAccount = (account: PlatformAccount) => canReview
    && Boolean(serverIdentity?.user?.user_id)
    && account.user_id !== serverIdentity?.user.user_id
    && account.user_id !== serverIdentity?.instance.owner_user_id
    && account.platform_role !== 'owner'
    && account.status !== 'removed'

  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    setPage(null)
    setError('')
    setDraftRoles({})
    void listAccounts({ query, role, status, cursor }, controller.signal).then(value => {
      if (!controller.signal.aborted) setPage(value)
    }).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [query, role, status, cursor, revision])

  const saveReview = async () => {
    if (!review || !canReview || saving) return
    setSaving(true)
    setSaveError('')
    try {
      await reviewAccount(review.account, review.decision)
      setReview(null)
      notify(t(review.decision === 'approve' ? 'accounts.approved' : 'accounts.rejected'))
      reload()
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        setReview(null)
        notify(t('accounts.reviewConflict'), 'error')
        reload()
      } else setSaveError(cause instanceof Error ? cause.message : String(cause))
    } finally { setSaving(false) }
  }

  const save = async () => {
    if (!change || !canAppoint || saving) return
    setSaving(true)
    setSaveError('')
    try {
      await setAccountRole(change.account, change.role)
      setChange(null)
      notify(t('accounts.saved'))
      reload()
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        setChange(null)
        notify(t('accounts.conflict'), 'error')
        reload()
      } else setSaveError(cause instanceof Error ? cause.message : String(cause))
    } finally { setSaving(false) }
  }

  const saveStatus = async () => {
    if (!statusChange || !canManageAccount(statusChange.account) || saving) return
    setSaving(true)
    setSaveError('')
    try {
      await setAccountStatus(statusChange.account, statusChange.action)
      setStatusChange(null)
      notify(t(statusActions[statusChange.action].saved))
      reload()
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        setStatusChange(null)
        notify(t('accounts.statusConflict'), 'error')
        reload()
      } else setSaveError(cause instanceof Error ? cause.message : String(cause))
    } finally { setSaving(false) }
  }

  return <div className={css.page} data-admin-accounts="">
    <SectionHeader title={t('accounts')} description={t('accounts.description')} />
    <RegistrationPanel />
    <form className={css.filters} onSubmit={event => {
      event.preventDefault()
      setQuery(draftQuery.trim())
      setCursors([null])
      reload()
    }}>
      <Field className={css.search}>
        <Label htmlFor="admin-account-search">{t('accounts.search')}</Label>
        <Input id="admin-account-search" value={draftQuery} placeholder={t('accounts.placeholder')} onChange={event => setDraftQuery(event.target.value)} />
      </Field>
      <Field>
        <Label htmlFor="admin-account-role-filter">{t('accounts.role')}</Label>
        <Select id="admin-account-role-filter" value={role} onValueChange={nextValue => { setRole(nextValue as PlatformRole | ''); setCursors([null]) }}>
          <option value="">{t('accounts.allRoles')}</option>
          {roles.map(value => <option key={value} value={value}>{t(`role.${value}`)}</option>)}
        </Select>
      </Field>
      <Field>
        <Label htmlFor="admin-account-status-filter">{t('accounts.status')}</Label>
        <Select id="admin-account-status-filter" value={status} onValueChange={nextValue => { setStatus(nextValue as AccountStatus | ''); setCursors([null]) }}>
          <option value="">{t('accounts.allStatuses')}</option>
          {(['pending', 'active', 'rejected', 'banned', 'removed'] as const).map(value => <option key={value} value={value}>{t(statusLabels[value])}</option>)}
        </Select>
      </Field>
      <Button type="submit" variant="outline"><Search />{t('accounts.searchButton')}</Button>
      <Button type="button" variant="ghost" size="icon" aria-label={t('refresh')} disabled={loading} onClick={reload}><RefreshCw /></Button>
    </form>
    <p className={css.hint}>{t('accounts.roleDescription')}</p>
    {loading ? <div className={css.state} role="status"><LoaderCircle className="size-4 animate-spin" />{t('loading')}</div>
      : error ? <div className={css.state} role="alert"><span>{error}</span><Button variant="outline" onClick={reload}>{t('retry')}</Button></div>
        : !page?.accounts.length ? <div className={css.state}>{t('accounts.empty')}</div>
          : <div className={css.accounts}>
            {page.accounts.map(account => {
              const label = account.username
              const editable = canAppoint && account.status === 'active' && account.platform_role !== 'owner'
              const draft = draftRoles[account.user_id] ?? account.platform_role
              return <article className={css.account} key={account.user_id} data-admin-account={account.user_id}>
                <div className={css.accountIdentity}>
                  <strong>{label}</strong>
                  <span data-account-email="">{account.email ?? t('accounts.emailMissing')}</span>
                  <code>{account.user_id}</code>
                  <small>{t('accounts.created', { date: new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', { dateStyle: 'medium' }).format(account.created_at_ms) })}</small>
                  <span data-account-status={account.status}>{t(statusLabels[account.status])}</span>
                </div>
                <div className={css.accountRole}>
                  {canReview && (account.status === 'pending' || account.status === 'rejected') ? <>
                    {(account.status === 'pending' ? ['approve', 'reject'] as const : ['approve'] as const).map(decision => <Button key={decision} variant="outline" disabled={saving} onClick={() => {
                      setSaveError(''); setReview({ account, decision })
                    }}>{t(`accounts.${decision}`)}</Button>)}
                  </> : <>
                  {editable ? <>
                    <Select aria-label={`${label} · ${t('accounts.role')}`} value={draft} onValueChange={nextValue => setDraftRoles(current => ({ ...current, [account.user_id]: nextValue as Exclude<PlatformRole, 'owner'> }))}>
                      {assignableRoles.map(value => <option key={value} value={value}>{t(`role.${value}`)}</option>)}
                    </Select>
                    <Button variant="outline" disabled={draft === account.platform_role || saving} onClick={() => {
                      setSaveError('')
                      setChange({ account, role: draft as Exclude<PlatformRole, 'owner'> })
                    }}>{t('accounts.save')}</Button>
                  </> : <span className={css.badge}>{t(`role.${account.platform_role}`)}</span>}
                  </>}
                  {canManageAccount(account) && <DropdownMenu>
                    <DropdownMenuTrigger asChild><Button type="button" variant="ghost" size="icon" disabled={saving} aria-label={t('accounts.actions', { name: label })}><Ellipsis /></Button></DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      {account.status === 'active' && <DropdownMenuItem className={css.accountMenuItem} onSelect={() => { setSaveError(''); setStatusChange({ account, action: 'ban' }) }}><Ban />{t('accounts.ban')}</DropdownMenuItem>}
                      {account.status === 'banned' && <DropdownMenuItem className={css.accountMenuItem} onSelect={() => { setSaveError(''); setStatusChange({ account, action: 'unban' }) }}><ShieldCheck />{t('accounts.unban')}</DropdownMenuItem>}
                      {(account.status === 'active' || account.status === 'banned') && <DropdownMenuSeparator />}
                      <DropdownMenuItem className={`${css.accountMenuItem} text-destructive focus:text-destructive`} onSelect={() => { setSaveError(''); setStatusChange({ account, action: 'remove' }) }}><UserRoundX />{t('accounts.remove')}</DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>}
                </div>
              </article>
            })}
          </div>}
    <div className={css.pagination}>
      <span>{t('accounts.page', { page: cursors.length, count: page?.accounts.length ?? 0 })}</span>
      <div>
        <Button variant="outline" disabled={loading || cursors.length === 1} onClick={() => setCursors(current => current.slice(0, -1))}><ChevronLeft />{t('accounts.previous')}</Button>
        <Button variant="outline" disabled={loading || !page?.next_cursor} onClick={() => { if (page?.next_cursor) setCursors(current => [...current, page.next_cursor]) }}>{t('accounts.next')}<ChevronRight /></Button>
      </div>
    </div>
    <ActionDialog open={change !== null} title={t('accounts.confirmTitle')}
      description={t('accounts.confirmDescription', { name: change?.account.username || '', role: t(`role.${change?.role ?? 'user'}`) })}
      cancelLabel={t('cancel')} confirmLabel={t('accounts.save')} busy={saving} error={saveError}
      onOpenChange={open => { if (!open) setChange(null) }} onConfirm={() => void save()} />
    <ActionDialog open={review !== null} title={t(review?.decision === 'reject' ? 'accounts.rejectTitle' : 'accounts.approveTitle')}
      description={t(review?.decision === 'reject' ? 'accounts.rejectDescription' : 'accounts.approveDescription', { name: review?.account.username || '' })}
      cancelLabel={t('cancel')} confirmLabel={t(review?.decision === 'reject' ? 'accounts.reject' : 'accounts.approve')} busy={saving} error={saveError}
      onOpenChange={open => { if (!open) setReview(null) }} onConfirm={() => void saveReview()} />
    <ActionDialog open={statusChange !== null} title={t(statusActions[statusChange?.action ?? 'ban'].title)}
      description={t(statusActions[statusChange?.action ?? 'ban'].description, { name: statusChange?.account.username ?? '' })}
      cancelLabel={t('cancel')} confirmLabel={t(statusActions[statusChange?.action ?? 'ban'].label)} busy={saving} error={saveError}
      destructive={statusChange?.action !== 'unban'} onOpenChange={open => { if (!open) setStatusChange(null) }} onConfirm={() => void saveStatus()} />
  </div>
}
