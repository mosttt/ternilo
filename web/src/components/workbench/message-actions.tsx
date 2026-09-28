import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, GitBranch, LoaderCircle, Pencil, RotateCcw, ThumbsDown, ThumbsUp } from 'lucide-react'
import { Popover } from 'radix-ui'
import { api, ApiError } from '@/api/client'
import { useLocale, useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { SessionEvent, SessionProjection } from '@/types'
import { asRecord, cn } from '@/lib/utils'
import css from './message-actions.module.css'

function MessageTimestamp({ occurredAt }: { occurredAt: number }) {
  const { locale } = useLocale()
  const time = new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', {
    hour: '2-digit', minute: '2-digit', hour12: false,
  }).format(new Date(occurredAt))
  const timestamp = new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', {
    dateStyle: 'medium', timeStyle: 'medium', hour12: false,
  }).format(new Date(occurredAt))
  return <time className={css.time} dateTime={new Date(occurredAt).toISOString()} title={timestamp}>{time}</time>
}

function CopyMessageButton({ content }: { content: string }) {
  const t = useTranslate('chat')
  const { notify } = useWorkbench()
  const [copied, setCopied] = React.useState(false)
  const timer = React.useRef<number | null>(null)
  React.useEffect(() => () => { if (timer.current !== null) window.clearTimeout(timer.current) }, [])
  const copy = async () => {
    try {
      await copyText(content)
      setCopied(true)
      if (timer.current !== null) window.clearTimeout(timer.current)
      timer.current = window.setTimeout(() => { setCopied(false); timer.current = null }, 1_000)
    } catch {
      notify(t('message.copyFailed'), 'error')
    }
  }
  return <button type="button" className={css.action} aria-label={copied ? t('message.copied') : t('message.copy')} onClick={() => void copy()}>{copied ? <Check /> : <Copy />}</button>
}

export function UserMessageActions({ event, content, onRegenerate, onEdit, regenerateDisabled = false }: {
  event: SessionEvent
  content: string
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(): void
  regenerateDisabled?: boolean
}) {
  const t = useTranslate('chat')
  const { notify } = useWorkbench()
  const [pending, setPending] = React.useState(false)
  const inFlight = React.useRef(false)
  const regenerate = async () => {
    if (!onRegenerate || regenerateDisabled || inFlight.current) return
    inFlight.current = true
    setPending(true)
    try {
      await onRegenerate(event)
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    } finally {
      inFlight.current = false
      setPending(false)
    }
  }
  return <div className={`${css.actions} ${css.userActions}`} data-message-actions="user">
    <MessageTimestamp occurredAt={event.occurred_at_ms} />
    <CopyMessageButton content={content} />
    {onEdit && <button type="button" className={css.action} disabled={regenerateDisabled || pending}
      aria-label={t('message.edit')} title={t('message.edit')} onClick={onEdit}><Pencil /></button>}
    {onRegenerate && <button type="button" className={css.action} disabled={regenerateDisabled || pending}
      aria-label={t(pending ? 'message.regenerating' : 'message.regenerate')} title={t('message.regenerateHint')}
      onClick={() => void regenerate()}>
      {pending ? <LoaderCircle className={css.spin} /> : <RotateCcw />}
    </button>}
  </div>
}

export function PendingUserMessageActions({ occurredAt, content }: { occurredAt: number; content: string }) {
  return <div className={`${css.actions} ${css.userActions}`} data-message-actions="user"><MessageTimestamp occurredAt={occurredAt} /><CopyMessageButton content={content} /></div>
}

export function AssistantMessageActions({ event, content, projection, reloadMetadata }: {
  event: SessionEvent
  content: string
  projection: SessionProjection | null
  reloadMetadata(): Promise<void>
}) {
  const t = useTranslate('chat')
  const { currentSessionId, forkOperation, forkSession, notify } = useWorkbench()
  const feedback = (projection?.values?.feedback ?? {}) as Record<string, unknown>
  const item = asRecord(feedback[String(event.seq)])
  const projectedRating = String(item.rating ?? '')
  const projectedNote = String(item.note ?? '')
  const projectedRevision = Number(item.revision ?? 0)
  const [optimisticRating, setOptimisticRating] = React.useState('')
  const [noteOpen, setNoteOpen] = React.useState(false)
  const [note, setNote] = React.useState('')
  const [pending, setPending] = React.useState(false)
  const selected = optimisticRating === '__cleared__' ? '' : optimisticRating || projectedRating
  const forkBusy = forkOperation !== null

  React.useEffect(() => { setOptimisticRating('') }, [projectedRating])

  const submit = async (rating: 'positive' | 'negative' | null, nextNote: string | null = projectedNote || null) => {
    if (!currentSessionId) return
    setPending(true)
    setOptimisticRating(rating ?? '__cleared__')
    try {
      await api.request(`/sessions/${encodeURIComponent(currentSessionId)}/feedback`, {
        method: 'POST', body: {
          target_seq: event.seq,
          expected_revision: projectedRevision,
          rating,
          note: nextNote,
        },
      })
      await reloadMetadata()
      notify(t(rating === null ? 'message.feedbackRemoved' : 'message.feedbackSaved'))
      setNoteOpen(false)
    } catch (cause) {
      setOptimisticRating('')
      if (cause instanceof ApiError && cause.status === 409 && cause.code === 'conflict') {
        await reloadMetadata()
        notify(t('message.feedbackConflict'), 'error')
      } else {
        notify(cause instanceof Error ? cause.message : String(cause), 'error')
      }
    } finally { setPending(false) }
  }

  const branch = async () => {
    if (!currentSessionId || forkBusy) return
    try {
      await forkSession(currentSessionId, event.seq)
      notify(t('message.branchCreated'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }

  return <div className={css.actions} data-message-actions="assistant" data-time-hover-root="">
    <CopyMessageButton content={content} />
    <button type="button" className={cn(css.action, selected === 'positive' && css.selected)} disabled={pending} aria-label={selected === 'positive' ? t('message.feedbackRemove') : t('message.feedbackPositive')} aria-pressed={selected === 'positive'} onClick={() => { if (selected === 'positive') void submit(null, null); else void submit('positive') }}><ThumbsUp /></button>
    <button type="button" className={cn(css.action, selected === 'negative' && css.negative)} disabled={pending} aria-label={selected === 'negative' ? t('message.feedbackRemove') : t('message.feedbackNegative')} aria-pressed={selected === 'negative'} onClick={() => { if (selected === 'negative') void submit(null, null); else void submit('negative') }}><ThumbsDown /></button>
    <button type="button" className={css.action} disabled={forkBusy} aria-label={t(forkBusy ? 'message.branching' : 'message.branch')} onClick={() => void branch()}>
      {forkBusy ? <LoaderCircle className={css.spin} /> : <GitBranch />}
    </button>
    <MessageTimestamp occurredAt={event.occurred_at_ms} />
    {selected && <Popover.Root open={noteOpen} onOpenChange={open => { setNoteOpen(open); if (open) setNote(projectedNote) }}>
      <Popover.Trigger asChild><button type="button" className={css.note} aria-label={t('message.feedbackEdit')}>{projectedNote || t('message.feedbackAddNote')}</button></Popover.Trigger>
      <Popover.Portal><Popover.Content side="bottom" align="start" sideOffset={5} className={css.popover}>
        <textarea autoFocus aria-label={t('message.feedbackNote')} rows={3} value={note} onChange={change => setNote(change.target.value)} placeholder={t('message.feedbackPlaceholder')} />
        <div className={css.popoverActions}><button type="button" onClick={() => setNoteOpen(false)}>{t('message.cancel')}</button><button type="button" disabled={pending} onClick={() => void submit(selected as 'positive' | 'negative', note.trim() || null)}>{t('message.save')}</button></div>
      </Popover.Content></Popover.Portal>
    </Popover.Root>}
  </div>
}
