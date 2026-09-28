import * as React from 'react'
import {
  Bot, Check, ChevronRight, CircleAlert, CircleDot, Clock3, Command, ListChecks, PackageCheck,
  RefreshCw, Target, Workflow,
} from 'lucide-react'
import type { ConversationItem } from '@/domain/events'
import { presentRuntimeError } from '@/domain/runtime-error'
import { deriveWorkflowRuns } from '@/domain/observability'
import { useWorkbench } from '@/state/workbench'
import type { Translate } from '@/i18n/runtime'
import type { SessionEvent } from '@/types'
import { asRecord, cn, humanize } from '@/lib/utils'
import { ChatDisclosure, DisclosureSeparator } from './chat-disclosure'
import { ScheduleEventRow, SubagentEventRow } from './observability-event-rows'
import { WorkflowRunPanel } from './workflow-run-panel'
import css from './event-rows.module.css'

type ChatTranslate = Translate<'chat'>
type EventItem = Extract<ConversationItem, { kind: 'retry' | 'command' | 'compaction' | 'max_tokens' | 'card' }>

function JsonBody({ value }: { value: unknown }) {
  return <pre className={css.body}>{typeof value === 'string' ? value : JSON.stringify(value, null, 2)}</pre>
}

function RetryRow({ item, t }: { item: Extract<EventItem, { kind: 'retry' }>; t: ChatTranslate }) {
  const [open, setOpen] = React.useState(false)
  const [now, setNow] = React.useState(Date.now())
  const scheduled = item.lifecycle.event
  React.useEffect(() => {
    if (item.lifecycle.state !== 'scheduled') return
    const timer = window.setInterval(() => setNow(Date.now()), 250)
    return () => window.clearInterval(timer)
  }, [item.lifecycle.state])
  const remaining = Math.max(0, Math.ceil((scheduled.occurred_at_ms + scheduled.delay_ms - now) / 1_000))
  const label = item.lifecycle.state === 'scheduled'
    ? t('message.retry.scheduled')
    : item.lifecycle.state === 'started' ? t('message.retry.started') : t('message.retry.cancelled')
  const maximum = scheduled.max_retries ?? '∞'
  return <div className={css.root} data-chat-event="retry" data-state={item.lifecycle.state}>
    <ChatDisclosure
      icon={item.lifecycle.state === 'scheduled' ? <RefreshCw className={css.spin} /> : <Clock3 />}
      title={label}
      summary={<><DisclosureSeparator /><span className={css.summary}>{t('message.retry.status', { label: '', retry: scheduled.retry, maximum, seconds: remaining }).replace(/^\s+/, '')}</span></>}
      open={open}
      onToggle={() => setOpen(value => !value)}
    >
      <dl className={css.definition}>
        <dt>{t('message.retry.delay')}</dt><dd>{scheduled.delay_ms} ms</dd>
        <dt>{t('message.retry.failure')}</dt><dd>{scheduled.failure?.message || '—'}</dd>
      </dl>
    </ChatDisclosure>
  </div>
}

function CommandRow({ item, t, onSelect }: { item: Extract<EventItem, { kind: 'command' }>; t: ChatTranslate; onSelect(): void }) {
  const [open, setOpen] = React.useState(false)
  const state = item.lifecycle.outcome == null ? 'running' : item.lifecycle.outcome.kind === 'error' ? 'error' : 'complete'
  const outcome = item.lifecycle.outcome
  const feedback = item.lifecycle.name === 'feedback'
  const parameters = outcome?.parameters ?? {}
  const status = state === 'running'
    ? t('command.running')
    : outcome?.code === 'goal_execution_started'
      ? t('command.goalStarted')
    : outcome?.code === 'feedback_text_required'
      ? t('command.feedbackTextRequired', { usage: parameters.usage ?? '/feedback <text>' })
      : outcome?.code === 'feedback_recorded'
        ? t('command.feedbackRecordedShort')
        : state === 'error' ? t('command.failed') : t('command.done')
  const sharing = parameters.sharing === 'full'
    ? t('command.feedbackSharing.full')
    : parameters.sharing === 'feedback_only'
      ? t('command.feedbackSharing.feedback_only')
      : parameters.sharing === 'disabled'
        ? t('command.feedbackSharing.disabled')
        : ''
  const localizedOutcome = outcome?.code === 'feedback_recorded'
    ? `${t('command.feedbackRecorded', { session: parameters.session_id ?? '—' })}${sharing ? ` ${sharing}` : ''}`
    : outcome?.code === 'feedback_text_required'
      ? t('command.feedbackTextRequired', { usage: parameters.usage ?? '/feedback <text>' })
      : parameters.message ?? outcome?.text
  const title = feedback ? t('command.feedbackTitle') : item.lifecycle.name || t('command.title')
  return <div className={css.root} data-chat-event="command" data-state={state}>
    <ChatDisclosure
      icon={<Command />}
      title={title}
      summary={<><DisclosureSeparator /><span className={css.summary}>{status}</span></>}
      open={open}
      onToggle={() => setOpen(value => !value)}
    >
      <div className={css.bodyStack}>
        {item.lifecycle.arguments != null && <JsonBody value={item.lifecycle.arguments} />}
        {localizedOutcome && <JsonBody value={localizedOutcome} />}
        <button type="button" className={css.details} onClick={onSelect}>{t('details.title')}</button>
      </div>
    </ChatDisclosure>
  </div>
}

function CompactionRow({ item, t, onSelect }: { item: Extract<EventItem, { kind: 'compaction' }>; t: ChatTranslate; onSelect(): void }) {
  const [open, setOpen] = React.useState(false)
  const summary = item.running
    ? t('message.compaction.running')
    : item.shadowedItems != null && item.shadowedTokens != null
      ? t('message.compaction.completed', { items: item.shadowedItems, tokens: item.shadowedTokens })
      : t('message.compaction')
  return <div className={css.root} data-chat-event="compaction" data-state={item.running ? 'running' : 'complete'}>
    <ChatDisclosure
      icon={item.running ? <RefreshCw className={css.spin} /> : <PackageCheck />}
      title={t('message.compaction.commandTitle')}
      summary={<><DisclosureSeparator /><span className={css.summary}>{summary}</span></>}
      open={open}
      expandable={Boolean(item.summary)}
      onToggle={() => item.summary && setOpen(value => !value)}
    >
      {item.summary && <div className={css.bodyStack}><JsonBody value={item.summary} /><button type="button" className={css.details} onClick={onSelect}>{t('details.title')}</button></div>}
    </ChatDisclosure>
  </div>
}

function PlanItems({ event }: { event: SessionEvent }) {
  const items = Array.isArray(event.items) ? event.items as Array<{ step?: string; status?: string }> : []
  return <div className={css.plan}>{items.map((item, index) => <div key={`${item.step}-${index}`} data-status={item.status}>
    {item.status === 'completed' ? <Check /> : item.status === 'in_progress' ? <RefreshCw className={css.spin} /> : <CircleDot />}
    <span>{item.step}</span>
  </div>)}</div>
}

function CardRow({ item, t, onSelect }: { item: Extract<EventItem, { kind: 'card' }>; t: ChatTranslate; onSelect(): void }) {
  const event = item.event
  if (event.type === 'goal_round_started') return <button type="button" className={css.goalRound} data-chat-event="goal_round_started" onClick={onSelect}>
    <Target /><span>{t('event.goalRound', { round: Number(event.round) })}</span><ChevronRight />
  </button>
  const configuration: Record<string, { title: string; icon: typeof Target }> = {
    plan_updated: { title: t('event.plan'), icon: ListChecks }, todo_updated: { title: t('event.todo'), icon: ListChecks },
    goal_updated: { title: t('event.goal'), icon: Target },
    subagent_updated: { title: t('event.subagent'), icon: Bot }, workflow_run_started: { title: t('event.workflow'), icon: Workflow },
    workflow_phase_changed: { title: t('event.workflowPhase'), icon: Workflow }, workflow_log_emitted: { title: t('event.workflowProgress'), icon: Workflow },
    workflow_agent_started: { title: t('event.workflowAgent'), icon: Bot }, workflow_agent_finished: { title: t('event.workflowAgent'), icon: Bot },
    workflow_run_finished: { title: t('event.workflowFinished'), icon: Workflow }, runtime_extension_changed: { title: t('event.extension'), icon: PackageCheck },
    turn_failed: { title: t('message.turnError'), icon: CircleAlert }, turn_cancelled: { title: t('event.cancelled'), icon: CircleAlert },
  }
  const failure = event.type === 'turn_failed' ? presentRuntimeError(event.message, t) : null
  const config = failure ? { title: failure.title, icon: CircleAlert } : configuration[event.type] ?? { title: humanize(event.type), icon: CircleDot }
  const Icon = config.icon
  const body = failure?.message ?? String(event.explanation ?? event.objective ?? event.message ?? event.title ?? '')
  const subagent = event.subagent ? asRecord(event.subagent) : null
  return <button type="button" className={cn(css.card, event.type === 'turn_failed' && css.error)} data-chat-event={event.type} onClick={onSelect}>
    <span className={css.cardHeader}><Icon /><strong>{config.title}</strong><ChevronRight /></span>
    {body && <span className={css.cardBody}>{body}</span>}
    {subagent && <span className={css.cardMeta}>{String(subagent.label ?? subagent.subagent_id ?? '')} · {String(subagent.status ?? '')}</span>}
    {(event.type === 'plan_updated' || event.type === 'todo_updated') && <PlanItems event={event} />}
  </button>
}

function ConnectedEventRow({ item, events, onSelect }: {
  item: Extract<EventItem, { kind: 'card' }>
  events: readonly SessionEvent[]
  onSelect(): void
}) {
  const { snapshot, selectSession } = useWorkbench()
  if (item.event.type === 'workflow_run_started') {
    const run = deriveWorkflowRuns(events).find(value => value.anchor.seq === item.event.seq)
    if (run) return <WorkflowRunPanel run={run} sessions={snapshot.sessions} onOpenSession={selectSession} />
  }
  return <SubagentEventRow event={item.event} sessions={snapshot.sessions} onOpenSession={selectSession} onSelect={onSelect} />
}

export function ConversationEventRow({ item, events, onSelect, t }: { item: EventItem; events: readonly SessionEvent[]; onSelect(): void; t: ChatTranslate }) {
  if (item.kind === 'retry') return <RetryRow item={item} t={t} />
  if (item.kind === 'command') return <CommandRow item={item} t={t} onSelect={onSelect} />
  if (item.kind === 'compaction') return <CompactionRow item={item} t={t} onSelect={onSelect} />
  if (item.kind === 'max_tokens') return <button type="button" className={cn(css.card, css.warning)} data-chat-event="max_tokens" onClick={onSelect}>
    <span className={css.cardHeader}><CircleAlert /><strong>{t('message.maxTokens')}</strong><ChevronRight /></span>
    <span className={css.cardBody}>{t('message.maxTokens.hint')}</span>
  </button>
  if (item.event.type === 'schedule_changed') return <ScheduleEventRow event={item.event} onSelect={onSelect} />
  if (item.event.type === 'workflow_run_started' || item.event.type === 'subagent_updated') {
    return <ConnectedEventRow item={item} events={events} onSelect={onSelect} />
  }
  return <CardRow item={item} t={t} onSelect={onSelect} />
}
