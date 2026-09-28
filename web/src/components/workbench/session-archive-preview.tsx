import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { conversationEvents } from '@/domain/conversation-events'
import { buildConversationItems, type ConversationItem } from '@/domain/events'
import { useTranslate } from '@/i18n/provider'
import type { LocalSession, SessionEvent, SessionEventPage } from '@/types'
import { AssistantMarkdown } from './chat/assistant-markdown'
import { WorkspaceDisplayContext } from './workspace-display-context'
import css from './session-archive-dialog.module.css'

function SnapshotDetails({ title, value }: { title: string; value: unknown }) {
  const [open, setOpen] = React.useState(false)
  return <details className={css.historyDetails} onToggle={event => setOpen(event.currentTarget.open)}>
    <summary>{title}</summary>
    {open && <pre>{typeof value === 'string' ? value : JSON.stringify(value, null, 2)}</pre>}
  </details>
}

function HistoryItem({ item }: { item: ConversationItem }) {
  const translate = useTranslate('sessionArchive')
  const chat = useTranslate('chat')
  const author = item.event.provenance?.author
  if (item.kind === 'user') return <>
    <strong className={css.historyRole}>{author?.kind === 'account' ? author.username || author.user_id : translate('user')}</strong>
    <p className={css.historyText}>{item.content}</p>
    {item.event.attachments?.map((attachment, index) => <p className={css.hint} key={index}>{translate('attachment', { name: attachment.name })}</p>)}
  </>
  if (item.kind === 'assistant') return <>
    <strong className={css.historyRole}>{translate('assistant')}</strong>
    {item.reasoning?.text && <SnapshotDetails title={translate('reasoning')} value={item.reasoning.text} />}
    <AssistantMarkdown source={item.content} streaming={false} t={chat} />
  </>
  if (item.kind === 'system_prompt' || item.kind === 'context') return <SnapshotDetails title={translate(item.kind === 'system_prompt' ? 'systemPrompt' : 'context')} value={item.content} />
  if (item.kind === 'tool') return <SnapshotDetails title={translate('tool', { name: item.trace.name })} value={item.trace} />
  return <SnapshotDetails title={translate('event', { type: item.event.type })} value={item} />
}

export function SessionArchivePreview({ session, tenantId }: { session: LocalSession; tenantId: string | null }) {
  const translate = useTranslate('sessionArchive')
  const [events, setEvents] = React.useState<SessionEvent[] | null>(null)
  const [nextBeforeSeq, setNextBeforeSeq] = React.useState<number | null>(null)
  const [loadingOlder, setLoadingOlder] = React.useState(false)
  const abortRef = React.useRef<AbortController | null>(null)
  const chat = useTranslate('chat')
  const [error, setError] = React.useState('')
  const [revision, setRevision] = React.useState(0)
  const [raw, setRaw] = React.useState(false)
  const [page, setPage] = React.useState(0)
  const sessionId = session.identity.session_id
  React.useEffect(() => {
    const controller = new AbortController()
    abortRef.current = controller
    setNextBeforeSeq(null); setLoadingOlder(false)
    setEvents(null); setError(''); setPage(0)
    void api.request<SessionEventPage>(`/sessions/${encodeURIComponent(sessionId)}/archive-history?limit=200`, {
      headers: tenantId ? { 'x-ternilo-tenant': tenantId } : undefined,
      signal: controller.signal, cache: 'no-store',
    }).then(result => { if (!controller.signal.aborted) { setEvents(result.events); setNextBeforeSeq(result.next_before_seq) } })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => controller.abort()
  }, [sessionId, tenantId, revision])
  const loadOlder = async () => {
    const signal = abortRef.current?.signal
    if (nextBeforeSeq === null || loadingOlder || signal?.aborted) return
    setLoadingOlder(true); setError('')
    try {
      const result = await api.request<SessionEventPage>(`/sessions/${encodeURIComponent(sessionId)}/archive-history?limit=200&before_seq=${nextBeforeSeq}`, {
        headers: tenantId ? { 'x-ternilo-tenant': tenantId } : undefined, signal, cache: 'no-store',
      })
      if (signal?.aborted) return
      setEvents(current => [...result.events, ...(current ?? [])]); setNextBeforeSeq(result.next_before_seq); setPage(0)
    } catch (cause) {
      if (!signal?.aborted) { setEvents(null); setError(cause instanceof Error ? cause.message : String(cause)) }
    } finally {
      if (!signal?.aborted) setLoadingOlder(false)
    }
  }
  const items = React.useMemo(() => buildConversationItems(conversationEvents(events ?? [])), [events])
  const count = raw ? events?.length ?? 0 : items.length
  const lastPage = Math.max(0, Math.ceil(count / 50) - 1)
  const loading = events === null && !error
  return <section className={css.preview} data-archive-preview={sessionId} aria-busy={loading}>
    <strong className={css.previewTitle}>{session.title}</strong>
    <p className={css.hint}>{translate('previewHint')}</p>
    {session.placement === 'local_node' && <p className={css.hint}>{translate('previewNodeHint')}</p>}
    <div className={css.previewToolbar}>
      <Button variant="outline" size="sm" aria-pressed={!raw} onClick={() => { setRaw(false); setPage(0) }}>{translate('conversation')}</Button>
      <Button variant="outline" size="sm" aria-pressed={raw} onClick={() => { setRaw(true); setPage(0) }}>{translate('rawEvents')}</Button>
      <Button variant="ghost" size="sm" disabled={loading} onClick={() => setRevision(value => value + 1)}>{translate('refresh')}</Button>
    </div>
    {loading && <p role="status" className={css.status}>{translate('historyLoading')}</p>}
    {error && <p role="alert" className={css.error}>{translate('historyError', { message: error })}</p>}
    {events && <>
      {nextBeforeSeq !== null && <Button size="sm" variant="outline" disabled={loadingOlder} onClick={() => void loadOlder()}>{chat('chat.loadOlder')}</Button>}
      <p className={css.hint}>{translate('historyCount', { count: events.length })}</p>
      {count === 0 && <p className={css.status}>{translate('historyEmpty')}</p>}
      <WorkspaceDisplayContext.Provider value={session.workspace_path}>
        <ol className={css.history}>
          {raw ? events.slice(page * 50, (page + 1) * 50).map(event => <li key={event.seq} className={css.historyRow} data-archive-event={event.seq}><SnapshotDetails title={`#${event.seq} · ${event.type}`} value={event} /></li>)
            : items.slice(page * 50, (page + 1) * 50).map(item => <li key={item.key} className={css.historyRow} data-archive-item={item.kind}><HistoryItem item={item} /></li>)}
        </ol>
      </WorkspaceDisplayContext.Provider>
      {lastPage > 0 && <nav className={css.previewToolbar} aria-label={translate('historyPages')}>
        <Button size="sm" variant="outline" disabled={page === 0} onClick={() => setPage(value => value - 1)}>{translate('previous')}</Button>
        <span className={css.hint}>{translate('page', { page: page + 1, total: lastPage + 1 })}</span>
        <Button size="sm" variant="outline" disabled={page === lastPage} onClick={() => setPage(value => value + 1)}>{translate('next')}</Button>
      </nav>}
    </>}
  </section>
}
