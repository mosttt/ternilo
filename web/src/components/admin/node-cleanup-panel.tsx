import * as React from 'react'
import { RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useLocale, useTranslate } from '@/i18n/provider'
import { localeTag } from '@/i18n/runtime'
import type { LocaleKey } from '@/i18n/runtime'
import { getAccountNodeCleanup, type AccountNodeCleanup } from './admin-api'
import css from './admin.module.css'

const issueKeys: Record<string, LocaleKey<'admin'>> = {
  process_state_unknown: 'accounts.nodeCleanup.issue.unknown',
  process_exit_pending: 'accounts.nodeCleanup.issue.process',
  session_busy: 'accounts.nodeCleanup.issue.session',
  cleanup_failed: 'accounts.nodeCleanup.issue.failed',
}

export function NodeCleanupPanel({ userId, statusRevision }: { userId: string; statusRevision: number }) {
  const t = useTranslate('admin')
  const { locale } = useLocale()
  const [open, setOpen] = React.useState(false)
  const [records, setRecords] = React.useState<AccountNodeCleanup[]>([])
  const [loading, setLoading] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, refresh] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    if (!open) return
    const controller = new AbortController()
    setLoading(true)
    setError('')
    void getAccountNodeCleanup(userId, controller.signal).then(records => {
      if (!controller.signal.aborted) setRecords(records)
    }).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [userId, statusRevision, open, revision])
  const date = (timestamp: number) => new Intl.DateTimeFormat(localeTag(locale), { dateStyle: 'medium', timeStyle: 'medium' }).format(timestamp)
  return <details className={css.nodeCleanup} onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>{t('accounts.nodeCleanup')}</summary>
    {open && <div>
      <p>{t('accounts.nodeCleanupDescription')}</p>
      <Button variant="outline" disabled={loading} onClick={() => refresh()}><RefreshCw />{t('refresh')}</Button>
      {error ? <p role="alert">{error}</p> : loading ? <p>{t('loading')}</p>
        : !records.length ? <p>{t('accounts.nodeCleanupEmpty')}</p>
          : records.map(({ executor_id, tenant_id, request }) => <div key={request.request_id} data-node-cleanup={request.request_id} data-node-cleanup-state={request.state}>
            <strong>{executor_id} · {t(request.state === 'confirmed' ? 'accounts.nodeCleanup.confirmed' : 'accounts.nodeCleanup.pending')}</strong>
            <small>{t('accounts.nodeCleanupSpace', { space: tenant_id, revision: request.status_revision })}</small>
            <small>{t('accounts.nodeCleanupRequested', { date: date(request.created_at_ms) })}</small>
            {request.confirmed_at_ms !== null && <small>{t('accounts.nodeCleanupConfirmed', { date: date(request.confirmed_at_ms) })}</small>}
            {request.detail && <span>{t(issueKeys[request.detail] ?? 'accounts.nodeCleanup.issue.failed')}</span>}
          </div>)}
    </div>}
  </details>
}
