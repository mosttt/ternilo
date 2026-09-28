import * as React from 'react'
import { ChevronRight, CircleAlert, Clock3, X } from 'lucide-react'
import { retainHistoricalImageSession } from '@/domain/historical-images'
import type { ToolTrace } from '@/domain/events'
import { presentRuntimeError } from '@/domain/runtime-error'
import type { TrajectoryTiming } from '@/domain/trajectory'
import { useTranslate } from '@/i18n/provider'
import type { Translate } from '@/i18n/runtime'
import { asRecord, formatDuration, formatTime, humanize } from '@/lib/utils'
import '@/plugins/builtin-tool-presentations'
import { useToolPresentation } from '@/plugins/tool-presentation-registry'
import type { Attachment, SessionEvent, UserQuestion } from '@/types'
import { JsonInspector } from './json-tree'
import { MessageAttachments } from './message-attachments'
import css from './details-panel.module.css'

export type DetailsSelection =
  | { kind: 'tool'; trace: ToolTrace }
  | {
    kind: 'approval'
    event: SessionEvent
    approval: NonNullable<UserQuestion['tool_approval']>
  }
  | {
    kind: 'event'
    event: SessionEvent
    relatedEvents?: SessionEvent[]
    timing?: TrajectoryTiming
    reasoningContent?: string
  }
  | null

function eventLabel(event: SessionEvent, t: Translate<'chat'>) {
  const labels: Record<string, string> = {
    turn_started: t('event.runStarted'), user_message: t('event.userMessage'), step_started: t('event.modelStep'),
    model_request_started: t('event.systemPrompt'), assistant_message: t('event.assistantMessage'),
    tool_call_started: t('event.toolCall'), tool_call_finished: t('event.toolResult'),
    code_dispatch_started: t('event.codeCall'), code_dispatch_finished: t('event.codeResult'),
    turn_finished: t('event.completed'), turn_failed: t('event.failed'), turn_cancelled: t('event.cancelled'),
  }
  return labels[event.type] ?? humanize(event.type)
}

function isAttachment(value: unknown): value is Attachment {
  const item = asRecord(value)
  return typeof item.name === 'string' && typeof item.media_type === 'string' && typeof item.content === 'string'
}

export function detailsAttachments(selection: Exclude<DetailsSelection, null>) {
  const roots: unknown[] = selection.kind === 'tool'
    ? [selection.trace.arguments, selection.trace.output, selection.trace.retainedOutput, selection.trace.started, selection.trace.finished]
    : selection.kind === 'approval'
      ? [selection.approval.arguments, selection.event]
      : [selection.event, ...(selection.relatedEvents ?? [])]
  const result: Attachment[] = []
  const seen = new Set<unknown>()
  const keys = new Set<string>()
  const visit = (value: unknown) => {
    if (value == null || typeof value !== 'object' || seen.has(value)) return
    seen.add(value)
    if (isAttachment(value)) {
      const key = `${value.media_type}\u0000${value.content}`
      if (!keys.has(key)) { keys.add(key); result.push(value) }
      return
    }
    if (Array.isArray(value)) value.forEach(visit)
    else Object.values(value as Record<string, unknown>).forEach(visit)
  }
  roots.forEach(visit)
  return result
}

function Metadata({ event, selection, usage, t }: {
  event: SessionEvent
  selection: Exclude<DetailsSelection, null>
  usage: Record<string, unknown>
  t: Translate<'chat'>
}) {
  const timing = selection.kind === 'event' ? selection.timing : undefined
  const response = asRecord(event.response)
  return <details className={css.disclosure}>
    <summary><ChevronRight /><span>{t('details.metadata')}</span></summary>
    <dl className={css.meta}>
      <dt>{t('details.run')}</dt><dd title={event.run_id}>{event.run_id}</dd>
      <dt>{t('details.sequence')}</dt><dd>{event.seq}</dd>
      <dt>{t('details.time')}</dt><dd>{formatTime(event.occurred_at_ms)}</dd>
      {selection.kind === 'tool' && <><dt>{t('details.callId')}</dt><dd title={selection.trace.id}>{selection.trace.id}</dd><dt>{t('details.duration')}</dt><dd>{formatDuration(selection.trace.durationMs)}</dd></>}
      {selection.kind === 'approval' && <><dt>{t('details.callId')}</dt><dd title={selection.approval.call_id}>{selection.approval.call_id}</dd></>}
      {selection.kind === 'event' && typeof event.step === 'number' && <><dt>{t('details.step')}</dt><dd>{event.step}</dd></>}
      {typeof response.provider === 'string' && <><dt>{t('details.provider')}</dt><dd>{response.provider}</dd></>}
      {typeof response.model === 'string' && <><dt>{t('details.model')}</dt><dd>{response.model}</dd></>}
      {typeof response.finish_reason === 'string' && <><dt>{t('details.finishReason')}</dt><dd>{response.finish_reason}</dd></>}
      {timing && <><dt>{t('details.totalDuration')}</dt><dd>{formatDuration(timing.durationMs)}</dd><dt>{t('details.firstToken')}</dt><dd>{formatDuration(timing.firstTokenDurationMs)}</dd>{timing.reasoningDurationMs != null && <><dt>{t('details.reasoningDuration')}</dt><dd>{formatDuration(timing.reasoningDurationMs)}</dd></>}</>}
      {typeof usage.input_tokens === 'number' && <><dt>{t('details.inputTokens')}</dt><dd>{usage.input_tokens}</dd></>}
      {typeof usage.cached_input_tokens === 'number' && <><dt>{t('details.cachedInput')}</dt><dd>{usage.cached_input_tokens}</dd></>}
      {typeof usage.cache_write_tokens === 'number' && <><dt>{t('details.cacheWrite')}</dt><dd>{usage.cache_write_tokens}</dd></>}
      {typeof usage.output_tokens === 'number' && <><dt>{t('details.outputTokens')}</dt><dd>{usage.output_tokens}</dd></>}
      {typeof usage.reasoning_tokens === 'number' && <><dt>{t('details.reasoningTokens')}</dt><dd>{usage.reasoning_tokens}</dd></>}
    </dl>
  </details>
}

function ToolOutputDetails({ trace, selectionKey, outputLabel, t }: {
  trace: ToolTrace
  selectionKey: string
  outputLabel: string
  t: Translate<'chat'>
}) {
  const presentation = useToolPresentation(trace)
  if (!trace.output) return <p className={css.empty}>{t('details.callRunning')}</p>
  return <>
    {presentation?.result
      ? <div
        data-details-tool-presentation=""
        data-tool-contribution={presentation.result.contribution.id}
      >
        {presentation.result.contribution.render(presentation.result.view, { trace, t })}
      </div>
      : <JsonInspector key={`${selectionKey}:output`} value={trace.output.content} label={outputLabel} t={t} />}
    {trace.retainedOutput != null && <JsonInspector key={`${selectionKey}:retained`} value={trace.retainedOutput} label={outputLabel} t={t} />}
  </>
}

export function DetailsPanel({ selection, sessionId, onClose }: {
  selection: DetailsSelection
  sessionId?: string
  onClose(): void
}) {
  const t = useTranslate('chat')
  const closeRef = React.useRef<HTMLButtonElement>(null)
  const open = selection !== null
  // WorkbenchShell keeps this host mounted for the active Session even while
  // the inspector is closed, so Chat/Trajectory switches share one scope.
  React.useEffect(() => sessionId ? retainHistoricalImageSession(sessionId) : undefined, [sessionId])
  React.useEffect(() => {
    if (!open) return
    const previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null
    const frame = requestAnimationFrame(() => closeRef.current?.focus())
    return () => {
      cancelAnimationFrame(frame)
      requestAnimationFrame(() => {
        if (previouslyFocused?.isConnected) previouslyFocused.focus()
      })
    }
  }, [open])
  const attachments = React.useMemo(() => selection ? detailsAttachments(selection) : [], [selection])
  if (!selection) return null
  const event = selection.kind === 'tool' ? selection.trace.started : selection.event
  const title = selection.kind === 'tool'
    ? selection.trace.presentation?.title ?? selection.trace.name
    : selection.kind === 'approval'
      ? selection.approval.presentation?.title ?? selection.approval.tool_name
      : eventLabel(event, t)
  const status = selection.kind === 'tool'
    ? selection.trace.output ? selection.trace.output.is_error ? t('event.failed') : t('event.completed') : t('event.running')
    : selection.kind === 'approval' ? t('question.pending') : t('details.eventNumber', { seq: event.seq })
  const response = asRecord(event.response)
  const usage = asRecord(response.usage)
  const related = selection.kind === 'event' ? selection.relatedEvents ?? [] : []
  const streamedContent = related.filter(item => item.type === 'assistant_message_delta').map(item => String(item.delta ?? '')).join('')
  const streamedReasoning = related.filter(item => item.type === 'assistant_reasoning_delta').map(item => String(item.delta ?? '')).join('')
  const reasoning = selection.kind === 'event'
    ? (selection.reasoningContent ?? String(response.reasoning_content ?? '')) || streamedReasoning : ''
  const assistantOutput = String(response.content ?? '') || streamedContent
  const result = selection.kind === 'tool' ? selection.trace.output?.content
    : selection.kind === 'approval' ? null : assistantOutput || event.output || event.answer || event.message || null
  const input = selection.kind === 'tool' ? selection.trace.arguments
    : selection.kind === 'approval' ? { reason: selection.approval.reason, arguments: selection.approval.arguments }
      : event.type === 'model_request_started' ? { system_prompt: event.system_prompt, messages: event.messages, tools: event.tools }
      : event.type === 'user_message' ? { content: event.content, attachments: event.attachments } : event
  const raw = selection.kind === 'tool' ? { started: selection.trace.started, finished: selection.trace.finished }
    : selection.kind === 'event' && related.length > 1 ? related : event
  const failure = event.type === 'turn_failed' ? presentRuntimeError(event.message, t) : null
  const selectionKey = selection.kind === 'tool'
    ? selection.trace.id
    : selection.kind === 'approval' ? selection.approval.call_id : `${event.run_id}:${event.seq}`
  const outputLabel = selection.kind === 'event' && (event.type.startsWith('assistant_') || related.some(item => item.type.startsWith('assistant_')))
    ? t('details.modelOutput') : t('details.output')
  const dataState = failure
    ? 'error'
    : selection.kind === 'approval' || (selection.kind === 'tool' && !selection.trace.finished)
      ? 'loading'
      : result == null
        ? 'empty'
        : 'ready'

  return <aside className={`${css.panel} details-panel`} aria-label={t('details.title')} data-details-state={dataState}>
    <header className={css.header}>
      <div><h2>{title}</h2><p>{status}</p></div>
      <button ref={closeRef} type="button" className={css.close} aria-label={t('details.close')} onClick={onClose}><X /></button>
    </header>
    <div className={`${css.scroll} details-scroll`} role="region" aria-label={t('details.content')} tabIndex={0}>
      {failure && <section className={css.failure}><CircleAlert /><div><strong>{failure.title}</strong><p>{failure.message}</p></div></section>}
      {attachments.length > 0 && <section className={css.section}><h3>{t('details.attachments')}</h3><MessageAttachments sessionId={sessionId} attachments={attachments} /></section>}
      <section className={css.section}><h3>{t('details.input')}</h3><JsonInspector key={`${selectionKey}:input`} value={input ?? null} label={t('details.input')} t={t} /></section>
      {reasoning && <section className={css.section}><h3>{t('details.think')}</h3><JsonInspector key={`${selectionKey}:think`} value={reasoning} label={t('details.think')} t={t} /></section>}
      {selection.kind !== 'approval' && <section className={css.section}><h3>{outputLabel}</h3>
        {selection.kind === 'tool'
          ? <ToolOutputDetails trace={selection.trace} selectionKey={selectionKey} outputLabel={outputLabel} t={t} />
          : result == null
            ? <p className={css.empty}>{t('details.noResult')}</p>
            : <JsonInspector key={`${selectionKey}:output`} value={result} label={outputLabel} t={t} />}
      </section>}
      <Metadata event={event} selection={selection} usage={usage} t={t} />
      <details className={css.disclosure}><summary><ChevronRight /><span>{t('details.raw')}</span></summary><JsonInspector key={`${selectionKey}:raw`} value={raw} label={t('details.raw')} t={t} /></details>
      {selection.kind === 'tool' && <div className={css.timing}><Clock3 />{selection.trace.finished ? formatDuration(selection.trace.durationMs) : t('details.running')}</div>}
    </div>
  </aside>
}
