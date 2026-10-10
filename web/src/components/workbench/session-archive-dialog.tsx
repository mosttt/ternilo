import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { ownsResource } from '@/domain/resource-access'
import { useLocale, useTranslate } from '@/i18n/provider'
import { localeTag } from '@/i18n/runtime'
import type { LocalSession, Workspace } from '@/types'
import { SessionArchivePreview } from './session-archive-preview'
import css from './session-archive-dialog.module.css'

export function SessionArchiveDialog({ tenantId, platform, readOnly, workspaces, onlineComputersOnly = false, onClose, onRestored }: {
  tenantId: string | null
  platform: boolean
  readOnly: boolean
  workspaces: readonly Workspace[]
  onlineComputersOnly?: boolean
  onClose(): void
  onRestored(): Promise<unknown> | void
}) {
  const translate = useTranslate('sessionArchive')
  const { locale } = useLocale()
  const [sessions, setSessions] = React.useState<LocalSession[] | null>(null)
  const [preview, setPreview] = React.useState<LocalSession | null>(null)
  const [error, setError] = React.useState('')
  const [notice, setNotice] = React.useState('')
  const [pending, setPending] = React.useState<string | null>(null)
  const [loading, setLoading] = React.useState(false)
  const mounted = React.useRef(false)
  const readVersion = React.useRef(0)
  const headers = React.useMemo(() => tenantId ? { 'x-ternilo-tenant': tenantId } : undefined, [tenantId])
  const refresh = React.useCallback(async (signal?: AbortSignal) => {
    const version = ++readVersion.current
    setLoading(true)
    setError('')
    try {
      const result = await api.request<LocalSession[]>(platform && onlineComputersOnly ? '/sessions/archived?online_computers_only=true' : '/sessions/archived', { headers, signal })
      if (mounted.current && !signal?.aborted && version === readVersion.current) {
        setSessions(result.filter(session => session.archived_at_ms != null)
          .sort((left, right) => right.archived_at_ms! - left.archived_at_ms!))
      }
    } catch (cause) {
      if (mounted.current && !signal?.aborted && version === readVersion.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current && !signal?.aborted && version === readVersion.current) setLoading(false)
    }
  }, [headers, onlineComputersOnly, platform])
  React.useEffect(() => {
    mounted.current = true
    const controller = new AbortController()
    void refresh(controller.signal)
    return () => { mounted.current = false; controller.abort() }
  }, [refresh])
  const canRestore = (session: LocalSession) => !readOnly && ownsResource(session.access, !platform)
  const restore = async (session: LocalSession) => {
    if (pending || !canRestore(session)) return
    const sessionId = session.identity.session_id
    setPending(sessionId)
    setError('')
    setNotice('')
    readVersion.current += 1
    try {
      await api.request(`/sessions/${encodeURIComponent(sessionId)}/restore`, { method: 'POST', headers })
      if (!mounted.current) return
      setSessions(current => current?.filter(item => item.identity.session_id !== sessionId) ?? [])
      setPreview(null)
      setNotice(translate('restored', { title: session.title }))
      await onRestored()
    } catch (cause) {
      if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current) setPending(null)
    }
  }
  const time = (timestamp: number) => new Date(timestamp).toLocaleString(localeTag(locale))
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className={css.dialog} data-session-archive="">
      <DialogHeader>
        <DialogTitle>{translate('title')}</DialogTitle>
        <DialogDescription>{translate('description')}</DialogDescription>
      </DialogHeader>
      <div className={css.toolbar}>{preview
        ? <Button variant="outline" size="sm" onClick={() => setPreview(null)}>{translate('backToList')}</Button>
        : <Button variant="ghost" size="sm" disabled={loading || pending !== null} onClick={() => void refresh()}>{translate('refresh')}</Button>}</div>
      {error && <p role="alert" className={css.error}>{error}</p>}
      {notice && <p role="status" className={css.status}>{notice}</p>}
      {loading && <p role="status" className={css.status}>{translate('loading')}</p>}
      {preview ? <SessionArchivePreview key={`${tenantId}:${preview.identity.session_id}`} session={preview} tenantId={tenantId} /> : <>
      {sessions?.length === 0 && <p className={css.status}>{translate('empty')}</p>}
      <ul className={css.list}>{sessions?.map(session => <li key={session.identity.session_id} className={css.session} data-archived-session={session.identity.session_id}>
        <div className={css.heading}><strong>{session.title}</strong><span className={css.hint}>{translate(session.placement ?? 'local')}</span></div>
        <div className={css.metadata}>
          <span>{translate('identifier', { id: session.identity.session_id })}</span>
          <span>{translate('workspace', { name: workspaces.find(workspace => workspace.workspace_id === session.workspace_id)?.title ?? session.workspace_id })}</span>
          <span>{translate('owner', { name: session.identity.user_id })}</span>
          <span>{translate('created', { time: time(session.created_at_ms) })}</span>
          <span>{translate('archived', { time: time(session.archived_at_ms!) })}</span>
        </div>
        <div className={css.actions}>
          {!canRestore(session) && <span className={css.hint}>{translate('ownerOnly')}</span>}
          <Button variant="outline" size="sm" disabled={pending !== null} onClick={() => setPreview(session)}>{translate('preview')}</Button>
          <Button size="sm" disabled={!canRestore(session) || pending !== null || loading} onClick={() => void restore(session)}>{translate(pending === session.identity.session_id ? 'restoring' : 'restore')}</Button>
        </div>
      </li>)}</ul></>}
    </DialogContent>
  </Dialog>
}
