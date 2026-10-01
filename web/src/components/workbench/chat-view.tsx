import * as React from 'react'
import { Hourglass } from 'lucide-react'
import { currentTurnActivity, type ExecutionPhase } from '@/domain/turn-activity'
import { buildChatTurns, type ChatTurn } from '@/domain/chat-turns'
import { buildConversationItems, completedAssistantTailSeqs, type ConversationItem } from '@/domain/events'
import { useTranscriptView } from '@/domain/transcript-view'
import { regenerationTarget } from '@/domain/conversation-events'
import { useTranslate } from '@/i18n/provider'
import type { Translate } from '@/i18n/runtime'
import type { PendingSubmissionEcho, SessionEvent, SessionProjection } from '@/types'
import type { DetailsSelection } from './details-panel'
import { ToolCallTree } from './tool-call-tree'
import { ReasoningRow, type ReasoningDisclosure } from './reasoning-row'
import { ContextInjectionRow, SystemPromptRow } from './chat/context-injection-row'
import { ConversationEventRow } from './chat/event-rows'
import { AssistantMessageItem, PendingSubmissionBubble, UserMessageItem } from './chat/message-item'
import { QuestionRow } from './chat/question-row'
import { TurnNavigator } from './chat/turn-navigator'
import { TurnProcessControl } from './chat/turn-process'
import { TurnTail } from './chat/turn-tail'
import css from './chat/chat-view.module.css'

type ChatTranslate = Translate<'chat'>

const INITIAL_TURN_WINDOW = 160
const TURN_PAGE_SIZE = 160
const MAX_TURN_WINDOW = 320

interface TurnWindow {
  start: number
  end: number
}

interface PagingAnchor {
  runId?: string
  key: string
  top: number
}

function tailWindow(total: number): TurnWindow {
  return { start: Math.max(0, total - INITIAL_TURN_WINDOW), end: total }
}

function scrollportOf(flow: HTMLElement): HTMLElement {
  return flow.closest<HTMLElement>('[data-conversation-scroll]') ?? flow
}

function anchorElement(flow: HTMLElement, key: string): HTMLElement | null {
  for (const row of flow.querySelectorAll<HTMLElement>('[data-chat-anchor-key]:not([hidden])')) {
    if (row.dataset.chatAnchorKey === key) return row
  }
  return null
}

function flowTop(row: HTMLElement, scrollport: HTMLElement): number {
  return row.getBoundingClientRect().top - scrollport.getBoundingClientRect().top
}

/** The first settled semantic row intersecting the readable scrollport. */
function pagingAnchor(flow: HTMLElement, scrollport: HTMLElement): HTMLElement | null {
  const viewport = scrollport.getBoundingClientRect()
  const composer = scrollport.querySelector<HTMLElement>('[data-composer-seat]')
  const visibleBottom = composer?.getBoundingClientRect().top ?? viewport.bottom
  let first: HTMLElement | null = null
  for (const row of flow.querySelectorAll<HTMLElement>('[data-chat-anchor-key]:not([hidden])')) {
    first ??= row
    const rect = row.getBoundingClientRect()
    if (rect.bottom > viewport.top && rect.top < visibleBottom) return row
  }
  return first
}

export function runningTurnStartedAt(events: SessionEvent[]) {
  return currentTurnActivity(events)?.startedAt
}

function TurnStatus({ startedAt, phase, t }: { startedAt: number; phase: ExecutionPhase; t: ChatTranslate }) {
  const waiting = phase !== 'running'
  const [now, setNow] = React.useState(Date.now())
  React.useEffect(() => {
    if (waiting) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 1_000)
    return () => window.clearInterval(timer)
  }, [startedAt, waiting])
  const seconds = Math.floor(Math.max(0, now - startedAt) / 1_000)
  const label = phase === 'waiting_for_workspace' ? 'chat.waitingForWorkspace'
    : phase === 'waiting_for_subagents' ? 'chat.waitingForSubagents'
      : phase === 'waiting_for_capacity' ? 'chat.waitingForCapacity' : 'chat.deepDiving'
  return <div className={css.running} role="status" aria-live="polite" data-execution-phase={phase} data-workspace-waiting={phase === 'waiting_for_workspace' || undefined}>
    {waiting ? <Hourglass size={13} className={css.waitingIcon} aria-hidden /> : <span className={css.runningDot} aria-hidden />}
    <span>{t(label)}</span>
    {!waiting && seconds >= 15 && <span>{t('duration.seconds', { seconds })}</span>}
  </div>
}

function ConversationItemView({ item, events, sessionId, selection, onSelect, onRegenerate, onEdit, regenerateDisabled, omitReasoning, reasoningDisclosure, t }: {
  item: ConversationItem
  events: readonly SessionEvent[]
  sessionId: string
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(event: SessionEvent, input: string): Promise<void>
  regenerateDisabled?: boolean
  omitReasoning?: boolean
  reasoningDisclosure?: ReasoningDisclosure
  t: ChatTranslate
}) {
  if (item.kind === 'user') return <UserMessageItem sessionId={sessionId} event={item.event} content={item.content} onRegenerate={onRegenerate} onEdit={onEdit} regenerateDisabled={regenerateDisabled} />
  if (item.kind === 'system_prompt') return <SystemPromptRow content={item.content} step={item.event.step} t={t} />
  if (item.kind === 'context') return <ContextInjectionRow
    content={item.content}
    source={item.source}
    dialect={item.dialect}
    referenceLabel={item.referenceLabel}
    completeness={item.completeness}
    t={t}
  />
  if (item.kind === 'assistant') return <AssistantMessageItem
    event={item.event}
    content={item.content}
    reasoning={item.reasoning}
    streaming={item.streaming}
    interrupted={item.interrupted}
    omitReasoning={omitReasoning}
    reasoningDisclosure={reasoningDisclosure}
    t={t}
  />
  if (item.kind === 'tool') return <ToolCallTree
    trace={item.trace}
    selectedCallId={selection?.kind === 'tool' ? selection.trace.id : undefined}
    onSelect={trace => onSelect({ kind: 'tool', trace })}
  />
  if (item.kind === 'question') return <QuestionRow
    lifecycle={item.lifecycle}
    onSelect={() => onSelect({ kind: 'event', event: item.event })}
    t={t}
  />
  return <ConversationEventRow item={item} events={events} t={t} onSelect={() => onSelect({ kind: 'event', event: item.event })} />
}

function TurnBlock({ turn, latest, events, sessionId, assistantTails, projection, reloadMetadata, selection, onSelect, onRegenerate, onEdit, regenerateDisabled, compact, t }: {
  turn: ChatTurn
  latest: boolean
  events: readonly SessionEvent[]
  sessionId: string
  assistantTails: ReadonlySet<number>
  projection: SessionProjection | null
  reloadMetadata(): Promise<void>
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(event: SessionEvent, input: string): Promise<void>
  regenerateDisabled?: boolean
  compact: boolean
  t: ChatTranslate
}) {
  const memberRefs = React.useRef(new Map<string, HTMLDivElement>())
  const answerRef = React.useRef<HTMLDivElement>(null)
  const sectionRef = React.useRef<HTMLElement>(null)
  const [disclosure, setDisclosure] = React.useState({ generation: '', open: false })
  const [openedReasoningSteps, setOpenedReasoningSteps] = React.useState(() => new Set<number>())
  const folding = compact && turn.foldable
  const generationChanged = disclosure.generation !== turn.generation
  const active = typeof document === 'undefined' ? null : document.activeElement
  const focusedMember = active instanceof Node && [...memberRefs.current.values()].some(node => node.contains(active))
  const focusedInlineReasoning = active instanceof Node && Boolean(answerRef.current?.querySelector('[data-reasoning-row]')?.contains(active))
  const selectedMember = turn.items.some(item => {
    if (selection?.kind === 'event') return selection.event.seq === item.event.seq
    if (selection?.kind !== 'tool' || item.kind !== 'tool') return false
    const contains = (trace: typeof item.trace): boolean => trace.id === selection.trace.id || trace.children.some(contains)
    return contains(item.trace)
  })
  const initialOpen = focusedMember || focusedInlineReasoning || selectedMember || openedReasoningSteps.size > 0
  const open = !folding || selectedMember || (generationChanged ? initialOpen : disclosure.open)

  React.useLayoutEffect(() => {
    if (!folding || !generationChanged) return
    setDisclosure({ generation: turn.generation, open: initialOpen })
    if (focusedInlineReasoning) requestAnimationFrame(() => sectionRef.current?.querySelector<HTMLButtonElement>('[data-turn-inline-reasoning] [data-disclosure-row]')?.focus())
  }, [focusedInlineReasoning, folding, generationChanged, initialOpen, turn.generation])

  const hidden = folding && !open
  let controlRendered = false
  const showActions = Boolean(turn.finalAnswerKey && turn.items.some(item => item.key === turn.finalAnswerKey
    && item.kind === 'assistant' && assistantTails.has(item.event.seq)))
  return <section
    ref={sectionRef}
    className={css.turn}
    data-chat-turn={turn.number}
    data-actions-reveal={latest ? 'always' : 'hover'}
    data-chat-run-id={turn.runId}
    data-chat-anchor-key={turn.anchorKey}
    data-chat-flow-key={turn.anchorKey}
  >
    {turn.items.map((item, itemIndex) => {
      const rows: React.ReactNode[] = []
      const reasoningStep = item.event.step ?? 0
      const reasoningDisclosure: ReasoningDisclosure = {
        open: openedReasoningSteps.has(reasoningStep),
        onToggle: () => setOpenedReasoningSteps(current => {
          const next = new Set(current)
          if (next.has(reasoningStep)) next.delete(reasoningStep)
          else next.add(reasoningStep)
          return next
        }),
      }
      if (folding && !controlRendered && itemIndex === turn.processControlIndex) {
        controlRendered = true
        rows.push(<TurnProcessControl
          key={`process-${turn.generation}`}
          turn={turn.number}
          counts={turn.counts}
          open={open}
          onToggle={() => setDisclosure({ generation: turn.generation, open: !open })}
          t={t}
        />)
      }
      if (folding && turn.inlineReasoning && item.key === turn.finalAnswerKey && item.kind === 'assistant' && item.reasoning) {
        rows.push(<div
          className={css.processMember}
          data-turn-inline-reasoning=""
          data-turn-process-hidden={hidden || undefined}
          hidden={hidden || undefined}
          key={`inline-reasoning-${item.key}`}
          ref={node => {
            if (node) memberRefs.current.set(`inline-${item.key}`, node)
            else memberRefs.current.delete(`inline-${item.key}`)
          }}
        ><ReasoningRow reasoning={item.reasoning} disclosure={reasoningDisclosure} /></div>)
      }
      const body = <ConversationItemView
        item={item}
        events={events}
        sessionId={sessionId}
        selection={selection}
        onSelect={onSelect}
        onRegenerate={onRegenerate}
        onEdit={onEdit}
        regenerateDisabled={regenerateDisabled}
        omitReasoning={folding && turn.inlineReasoning && item.key === turn.finalAnswerKey}
        reasoningDisclosure={reasoningDisclosure}
        t={t}
      />
      if (turn.processKeys.has(item.key)) {
        rows.push(<div
          className={css.processMember}
          data-turn-process-member=""
          data-turn-process-hidden={hidden || undefined}
          hidden={hidden || undefined}
          key={item.key}
          ref={node => {
            if (node) memberRefs.current.set(item.key, node)
            else memberRefs.current.delete(item.key)
          }}
        >{body}</div>)
      } else if (item.key === turn.finalAnswerKey) {
        rows.push(<div className={css.answer} data-turn-process-answer={folding || undefined} key={item.key} ref={answerRef}>{body}</div>)
      } else rows.push(<React.Fragment key={item.key}>{body}</React.Fragment>)
      return rows
    })}
    <TurnTail turn={turn} sessionId={sessionId} projection={projection} reloadMetadata={reloadMetadata} showActions={showActions} t={t} />
  </section>
}

export function ChatView({ sessionId, events, pendingSubmissions, projection, reloadMetadata, selection, onSelect, onReaderNavigate, onRegenerate, onEdit, regenerateDisabled, history }: {
  sessionId: string
  events: SessionEvent[]
  pendingSubmissions: PendingSubmissionEcho[]
  projection: SessionProjection | null
  reloadMetadata(): Promise<void>
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
  onReaderNavigate(): void
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(event: SessionEvent, input: string): Promise<void>
  regenerateDisabled?: boolean
  history?: import('@/plugins/conversation-registry').ConversationViewContext['history']
}) {
  const t = useTranslate('chat')
  const items = React.useMemo(() => buildConversationItems(events), [events])
  const assistantTails = React.useMemo(() => completedAssistantTailSeqs(events), [events])
  const transcriptView = useTranscriptView()
  const historyIncomplete = React.useMemo(() => {
    const regenerated = new Set(events.filter(event => regenerationTarget(event) !== undefined).map(event => event.run_id))
    return events.some((event, index) => index > 0 && event.seq !== events[index - 1]!.seq + 1 && !regenerated.has(event.run_id))
      || Boolean(events[0] && events[0].seq > 1 && !regenerated.has(events[0].run_id))
  }, [events])
  const turns = React.useMemo(() => buildChatTurns(items, events, historyIncomplete), [events, historyIncomplete, items])
  const rootRef = React.useRef<HTMLDivElement>(null)
  const running = React.useMemo(() => currentTurnActivity(events), [events])
  const [turnWindow, setTurnWindow] = React.useState<TurnWindow>(() => tailWindow(turns.length))
  const previousTotalRef = React.useRef(turns.length)
  const previousFirstRef = React.useRef(turns[0]?.runId)
  const pendingAnchorRef = React.useRef<PagingAnchor | null>(null)

  React.useLayoutEffect(() => {
    const previousTotal = previousTotalRef.current
    const prepended = previousFirstRef.current !== undefined && turns[0]?.runId !== previousFirstRef.current
    previousFirstRef.current = turns[0]?.runId
    if (turns.length === previousTotal) return
    previousTotalRef.current = turns.length
    setTurnWindow(current => {
      if (prepended) return { start: 0, end: Math.min(turns.length, MAX_TURN_WINDOW) }
      if (turns.length < previousTotal) return tailWindow(turns.length)
      if (current.end !== previousTotal) return {
        start: Math.min(current.start, turns.length),
        end: Math.min(current.end, turns.length),
      }
      const flow = rootRef.current
      const scrollport = flow ? scrollportOf(flow) : null
      const pinned = scrollport == null
        || scrollport.scrollHeight - scrollport.scrollTop - scrollport.clientHeight <= 25
      const end = turns.length
      const start = pinned ? Math.max(0, end - MAX_TURN_WINDOW) : current.start
      return { start, end: Math.min(end, start + MAX_TURN_WINDOW) }
    })
  }, [turns])

  React.useLayoutEffect(() => {
    const anchor = pendingAnchorRef.current
    const flow = rootRef.current
    if (!anchor || !flow || history?.loading) return
    pendingAnchorRef.current = null
    const scrollport = scrollportOf(flow)
    const row = anchorElement(flow, anchor.key)
      ?? [...flow.querySelectorAll<HTMLElement>('[data-chat-run-id]')].find(item => item.dataset.chatRunId === anchor.runId)
    if (row) scrollport.scrollTop += flowTop(row, scrollport) - anchor.top
  }, [events, history?.loading, turnWindow.end, turnWindow.start])

  const preserveAnchor = React.useCallback(() => {
    const flow = rootRef.current
    if (!flow) return
    const scrollport = scrollportOf(flow)
    const row = pagingAnchor(flow, scrollport)
    const key = row?.dataset.chatAnchorKey
    if (row && key) pendingAnchorRef.current = { key, runId: row.dataset.chatRunId, top: flowTop(row, scrollport) }
  }, [])

  const loadOlder = () => {
    preserveAnchor()
    if (turnWindow.start === 0 && history?.hasOlder) {
      onReaderNavigate()
      void history.loadOlder()
      return
    }
    setTurnWindow(current => {
      const start = Math.max(0, current.start - TURN_PAGE_SIZE)
      return { start, end: Math.min(turns.length, current.end, start + MAX_TURN_WINDOW) }
    })
  }

  const loadNewer = () => {
    preserveAnchor()
    setTurnWindow(current => {
      const end = Math.min(turns.length, current.end + TURN_PAGE_SIZE)
      return { start: Math.max(current.start, end - MAX_TURN_WINDOW), end }
    })
  }

  const visibleTurns = turns.slice(turnWindow.start, turnWindow.end)
  if (!visibleTurns.length && !pendingSubmissions.length && running == null && !history?.hasOlder) return null
  return <div ref={rootRef} className={css.flow} data-chat-flow="">
    {(turnWindow.start > 0 || history?.hasOlder) && <div className={css.paging}><button type="button" disabled={history?.loading} onClick={loadOlder}>{t('chat.loadOlder')}</button></div>}
    {history?.error && <p role="alert">{t('chat.loadError', { message: history.error })}</p>}
    <TurnNavigator turns={visibleTurns} rootRef={rootRef} onReaderNavigate={onReaderNavigate} t={t} />
    {visibleTurns.map(turn => <TurnBlock
      key={turn.runId}
      turn={turn}
      latest={turn === turns.at(-1)}
      events={events}
      sessionId={sessionId}
      assistantTails={assistantTails}
      projection={projection}
      reloadMetadata={reloadMetadata}
      selection={selection}
      onSelect={onSelect}
      onRegenerate={onRegenerate}
      onEdit={onEdit}
      regenerateDisabled={regenerateDisabled}
      compact={transcriptView === 'compact'}
      t={t}
    />)}
    {turnWindow.end < turns.length && <div className={css.paging}><button type="button" onClick={loadNewer}>{t('chat.loadNewer')}</button></div>}
    {pendingSubmissions.map(submission => <PendingSubmissionBubble key={submission.request_id} sessionId={sessionId} submission={submission} />)}
    {running && <TurnStatus startedAt={running.phaseStartedAt} phase={running.phase} t={t} />}
  </div>
}
