import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useLocale, useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { ActionDialog, GroupHeader } from './settings-ui'

interface BrowserSession {
  session_id: string
  created_at_ms: number
  expires_at_ms: number
  is_current: boolean
}

interface BrowserSessions {
  current_login: 'native' | 'oidc'
  sessions: BrowserSession[]
}

interface SessionRevocation {
  revoked_count: number
  current_revoked: boolean
}

export function AccountSessions() {
  const { logout } = useWorkbench()
  const translate = useTranslate('accountSessions')
  const { locale } = useLocale()
  const [data, setData] = React.useState<BrowserSessions | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [revision, setRevision] = React.useState(0)
  const [error, setError] = React.useState('')
  const [notice, setNotice] = React.useState<number | null>(null)
  const [target, setTarget] = React.useState<BrowserSession | 'others' | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [mutationError, setMutationError] = React.useState('')
  const mounted = React.useRef(false)

  React.useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])

  React.useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError('')
    void api.request<BrowserSessions>('/auth/sessions').then(value => {
      if (!cancelled) setData(value)
    }).catch(cause => {
      if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause))
    }).finally(() => {
      if (!cancelled) setLoading(false)
    })
    return () => { cancelled = true }
  }, [revision])

  const revoke = async () => {
    if (!target || busy) return
    setBusy(true)
    setMutationError('')
    setNotice(null)
    try {
      const result = await api.request<SessionRevocation>(
        target === 'others' ? '/auth/sessions/revoke-others' : `/auth/sessions/${encodeURIComponent(target.session_id)}`,
        { method: target === 'others' ? 'POST' : 'DELETE' },
      )
      if (!mounted.current) return
      if (result.current_revoked) {
        logout()
        return
      }
      setData(previous => previous && ({
        ...previous,
        sessions: previous.sessions.filter(session => target === 'others' ? session.is_current : session.session_id !== target.session_id),
      }))
      setTarget(null)
      setNotice(result.revoked_count)
      setRevision(value => value + 1)
    } catch (cause) {
      if (mounted.current) setMutationError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current) setBusy(false)
    }
  }

  const formatTime = (timestamp: number) => new Date(timestamp).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US')
  const oidc = data?.current_login === 'oidc'
  const bulkLabel = translate(oidc ? 'revokeAllNative' : 'revokeOthers')
  const dialogTitle = target === 'others' ? bulkLabel : translate(target?.is_current ? 'revokeCurrent' : 'revoke')
  const dialogDescription = target === 'others'
    ? translate(oidc ? 'confirmAllNative' : 'confirmOthers')
    : target?.is_current ? translate('confirmCurrent')
      : translate('confirmSession', { time: target ? formatTime(target.created_at_ms) : '' })
  const choose = (session: BrowserSession | 'others') => { setMutationError(''); setTarget(session) }

  return <section className="mt-5 min-w-0 border-t pt-5" data-account-sessions="" aria-busy={loading || busy}>
    <GroupHeader title={translate('title')} description={translate('description')} />
    {oidc && <p className="mb-4 rounded-lg bg-muted p-3 text-sm leading-relaxed" data-account-sessions-oidc="">{translate('oidcNotice')}</p>}
    <div className="mb-4 flex flex-wrap gap-2">
      <Button type="button" className="min-h-10" variant="outline" disabled={loading || busy} onClick={() => setRevision(value => value + 1)}>{translate('refresh')}</Button>
      <Button type="button" className="min-h-10" variant="outline" disabled={loading || busy || !data?.sessions.some(session => !session.is_current)} onClick={() => choose('others')}>{bulkLabel}</Button>
    </div>
    {loading && <p className="text-sm text-muted-foreground" role="status">{translate('loading')}</p>}
    {error && <p className="break-words text-sm text-destructive" role="alert">{translate('loadError', { message: error })}</p>}
    {data?.sessions.length === 0 && !loading && <p className="text-sm text-muted-foreground">{translate('empty')}</p>}
    <ul className="space-y-3">
      {data?.sessions.map(session => <li className="flex min-w-0 flex-col gap-3 rounded-lg border p-3 sm:flex-row sm:items-center sm:justify-between" key={session.session_id} data-account-session={session.session_id}>
        <div className="min-w-0 space-y-1">
          <div className="flex flex-wrap items-center gap-2 text-sm font-medium">
            <span>{translate('session', { id: session.session_id.slice(-8) })}</span>
            {session.is_current && <span className="rounded-md bg-muted px-2 py-1 text-xs" data-current-session="">{translate('current')}</span>}
          </div>
          <p className="break-words text-xs text-muted-foreground">{translate('created', { time: formatTime(session.created_at_ms) })}</p>
          <p className="break-words text-xs text-muted-foreground">{translate('expires', { time: formatTime(session.expires_at_ms) })}</p>
        </div>
        <Button type="button" className="min-h-10 shrink-0" variant="outline" disabled={busy || loading} onClick={() => choose(session)}>{translate(session.is_current ? 'revokeCurrent' : 'revoke')}</Button>
      </li>)}
    </ul>
    {notice !== null && <p className="mt-3 text-sm text-muted-foreground" role="status">{translate('revoked', { count: notice })}</p>}
    <ActionDialog open={target !== null} title={dialogTitle} description={dialogDescription} cancelLabel={translate('cancel')} confirmLabel={dialogTitle} busyLabel={translate('revoking')} busy={busy} destructive error={mutationError ? translate('revokeError', { message: mutationError }) : undefined} onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void revoke()} />
  </section>
}
