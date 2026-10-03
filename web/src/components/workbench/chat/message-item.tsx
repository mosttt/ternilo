import * as React from 'react'
import type { PendingSubmissionEcho, SessionEvent } from '@/types'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import type { AssistantReasoning } from '@/domain/events'
import type { ProviderSource } from '@/domain/provider-sources'
import type { Translate } from '@/i18n/runtime'
import { AssistantMarkdown } from './assistant-markdown'
import { MessageAttachments } from '../message-attachments'
import { PendingUserMessageActions, UserMessageActions } from '../message-actions'
import { SubmissionReferenceChips } from '../reference-chips'
import { ReasoningRow, type ReasoningDisclosure } from '../reasoning-row'
import { InputIdentity } from '../input-identity'
import css from './message-item.module.css'

type ChatTranslate = Translate<'chat'>

export function UserMessageItem({ sessionId, event, content, onRegenerate, onEdit, regenerateDisabled }: {
  sessionId: string
  event: SessionEvent
  content: string
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(event: SessionEvent, input: string): Promise<void>
  regenerateDisabled?: boolean
}) {
  const t = useTranslate('chat')
  const [editing, setEditing] = React.useState(false)
  const [draft, setDraft] = React.useState(content)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const inFlight = React.useRef(false)
  const empty = !draft.trim() && !event.attachments?.length && !event.references?.length
  const save = async () => {
    if (!onEdit || regenerateDisabled || inFlight.current || empty) return
    inFlight.current = true
    setSaving(true)
    setError('')
    try {
      await onEdit(event, draft)
      setEditing(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      inFlight.current = false
      setSaving(false)
    }
  }
  return <article className={css.userRow} data-role="user" data-time-hover-root="">
    <div className={css.userStack}>
      <InputIdentity author={event.provenance?.author} />
      <MessageAttachments sessionId={sessionId} attachments={event.attachments ?? []} align="end" />
      <SubmissionReferenceChips references={event.references ?? []} align="end" />
      {editing && onEdit ? <form className={css.editor} onSubmit={submit => { submit.preventDefault(); void save() }}>
        <textarea autoFocus aria-label={t('message.editInput')} value={draft} rows={4} maxLength={100_000}
          disabled={saving || regenerateDisabled} onChange={change => setDraft(change.target.value)}
          onKeyDown={key => {
            if (key.key === 'Escape' && !saving) { key.preventDefault(); setEditing(false) }
            if (key.key === 'Enter' && (key.ctrlKey || key.metaKey) && !key.nativeEvent.isComposing) { key.preventDefault(); key.currentTarget.form?.requestSubmit() }
          }} />
        {error && <p role="alert" className={css.editError}>{error}</p>}
        <div className={css.editorActions}>
          <Button type="button" variant="outline" size="sm" disabled={saving} onClick={() => setEditing(false)}>{t('message.cancel')}</Button>
          <Button type="submit" size="sm" disabled={saving || regenerateDisabled || empty}>{t(saving ? 'message.regenerating' : 'message.saveAndRegenerate')}</Button>
        </div>
      </form> : content && <div className={css.bubble}>{content}</div>}
    </div>
    {!editing && <UserMessageActions event={event} content={content} onRegenerate={onRegenerate} regenerateDisabled={regenerateDisabled}
      onEdit={onEdit ? () => { setDraft(content); setError(''); setEditing(true) } : undefined} />}
  </article>
}

export function PendingSubmissionBubble({ sessionId, submission }: {
  sessionId: string
  submission: PendingSubmissionEcho
}) {
  return <article className={css.userRow} data-role="user" data-submission-echo={submission.request_id} data-time-hover-root="">
    <div className={css.userStack}>
      <InputIdentity author={submission.author} />
      <MessageAttachments sessionId={sessionId} attachments={submission.attachments} align="end" />
      <SubmissionReferenceChips references={submission.references} align="end" />
      {submission.input && <div className={css.bubble}>{submission.input}</div>}
    </div>
    <PendingUserMessageActions occurredAt={submission.created_at_ms} content={submission.input} />
  </article>
}

export function AssistantMessageItem({
  content,
  reasoning,
  sources,
  streaming,
  interrupted,
  omitReasoning,
  reasoningDisclosure,
  t,
}: {
  content: string
  event: SessionEvent
  reasoning?: AssistantReasoning
  sources?: ProviderSource[]
  streaming: boolean
  interrupted?: boolean
  omitReasoning?: boolean
  reasoningDisclosure?: ReasoningDisclosure
  t: ChatTranslate
}) {
  return <article className={css.assistantRow} data-role="assistant" data-streaming={streaming || undefined}>
    {!omitReasoning && reasoning && <ReasoningRow reasoning={reasoning} disclosure={reasoningDisclosure} />}
    <AssistantMarkdown source={content} streaming={streaming} interrupted={interrupted} t={t} />
    {!!sources?.length && <nav data-provider-sources="" aria-label={t('message.webSources')} className="mt-3 flex flex-wrap gap-2 text-xs">
      <span className="text-muted-foreground">{t('message.webSources')}</span>
      {sources.map(source => <a key={source.url} href={source.url} target="_blank" rel="noopener noreferrer" className="max-w-full break-all text-primary underline underline-offset-2">{source.title}</a>)}
    </nav>}
  </article>
}
