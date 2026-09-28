import type { SessionEvent } from '@/types'
import type { Translate } from '@/i18n/runtime'
import { asRecord } from '@/lib/utils'
import { buildToolTraces, eventSummary, type ToolTrace } from './events'
import { toolTracePreview } from './trajectory-preview'
import { regenerationTarget } from './conversation-events'

export type TrajectoryKind = 'user' | 'assistant' | 'tool' | 'code' | 'event' | 'error'

export interface TrajectoryTiming {
  startedAt: number
  firstTokenAt?: number
  completedAt?: number
  durationMs?: number
  firstTokenDurationMs?: number
  generationDurationMs?: number
  reasoningStartedAt?: number
  reasoningCompletedAt?: number
  reasoningDurationMs?: number
}

export interface TrajectoryRecord {
  key: string
  event: SessionEvent
  relatedEvents: SessionEvent[]
  kind: TrajectoryKind
  tag: string
  title: string
  summary: string
  step: number
  requestNumber?: number
  toolCallCount?: number
  parentCallId?: string
  depth: number
  inputTokens?: number
  outputTokens?: number
  reasoningTokens?: number
  cachedTokens?: number
  cacheWriteTokens?: number
  provider?: string
  model?: string
  finishReason?: string
  reasoningContent?: string
  timing?: TrajectoryTiming
  trace?: ToolTrace
  running: boolean
  error: boolean
}

export interface TrajectoryGroup {
  key: string
  title: string
  records: TrajectoryRecord[]
}

export interface TrajectoryTurn {
  runId: string
  number: number
  startedAt: number
  endedAt: number
  durationMs: number
  status: 'running' | 'complete' | 'cancelled' | 'error'
  groups: TrajectoryGroup[]
  records: TrajectoryRecord[]
  boundaryEvents: SessionEvent[]
}

function recordKind(event: SessionEvent): { kind: TrajectoryKind; tag: string } {
  if (event.type === 'user_message' || event.type === 'user_question_answered') return { kind: 'user', tag: 'USER' }
  if (event.type.startsWith('assistant_')) return { kind: 'assistant', tag: 'ASSISTANT' }
  if (event.type.startsWith('tool_call_') || event.type === 'job_updated') return { kind: 'tool', tag: 'TOOL' }
  if (event.type.startsWith('code_dispatch_')) return { kind: 'code', tag: 'CODE' }
  if (event.type === 'turn_failed') return { kind: 'error', tag: 'ERROR' }
  return { kind: 'event', tag: 'EVENT' }
}

function traceDepth(trace: ToolTrace, traces: Map<string, ToolTrace>, memo: Map<string, number>, visiting = new Set<string>()): number {
  const cached = memo.get(trace.id)
  if (cached !== undefined) return cached
  if (!trace.parentCallId) return 0
  if (visiting.has(trace.id)) return 0
  visiting.add(trace.id)
  const parent = traces.get(trace.parentCallId)
  const depth = parent ? traceDepth(parent, traces, memo, visiting) + 1 : 1
  visiting.delete(trace.id)
  memo.set(trace.id, depth)
  return depth
}

function groupRecords(records: TrajectoryRecord[]): TrajectoryGroup[] {
  const groups: TrajectoryGroup[] = []
  for (const record of records) {
    const key = record.step > 0 ? `step-${record.step}` : 'message'
    let group = groups.find(item => item.key === key)
    if (!group) {
      group = {
        key,
        title: record.step > 0 ? `Step ${record.step}` : 'Message',
        records: [],
      }
      groups.push(group)
    }
    group.records.push(record)
  }
  return groups
}

function orderNestedTools(records: TrajectoryRecord[]) {
  const byCallId = new Map(records.flatMap(record => record.trace ? [[record.trace.id, record] as const] : []))
  const children = new Map<string, TrajectoryRecord[]>()
  for (const record of records) {
    if (!record.parentCallId || !byCallId.has(record.parentCallId)) continue
    const group = children.get(record.parentCallId)
    if (group) group.push(record)
    else children.set(record.parentCallId, [record])
  }
  const nestedIds = new Set([...children.values()].flat().map(record => record.trace?.id).filter((id): id is string => Boolean(id)))
  const ordered: TrajectoryRecord[] = []
  const emitted = new Set<string>()
  const append = (record: TrajectoryRecord) => {
    if (emitted.has(record.key)) return
    emitted.add(record.key)
    ordered.push(record)
    for (const child of children.get(record.trace?.id ?? '') ?? []) append(child)
  }
  for (const record of records) {
    if (record.trace && nestedIds.has(record.trace.id)) continue
    append(record)
  }
  for (const record of records) append(record)
  return ordered
}

function buildRunRecords(events: SessionEvent[], traces: Map<string, ToolTrace>): TrajectoryRecord[] {
  const terminal = events.find(event => event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled')
  const stepStarts = new Map<number, SessionEvent>()
  const modelRequests = new Map<number, SessionEvent>()
  const requestNumbers = new Map<number, number>()
  const deltas = new Map<number, SessionEvent[]>()
  const finalSteps = new Set<number>()
  for (const event of events) {
    if (event.type === 'step_started' && typeof event.step === 'number') stepStarts.set(event.step, event)
    if (event.type === 'model_request_started' && typeof event.step === 'number') {
      modelRequests.set(event.step, event)
      if (!requestNumbers.has(event.step)) requestNumbers.set(event.step, requestNumbers.size + 1)
    }
    if ((event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta')
      && typeof event.step === 'number' && String(event.delta ?? '')) {
      const group = deltas.get(event.step)
      if (group) group.push(event)
      else deltas.set(event.step, [event])
    }
    if (event.type === 'assistant_message' && typeof event.step === 'number') finalSteps.add(event.step)
  }

  const records: TrajectoryRecord[] = []
  const traceDepths = new Map<string, number>()
  let currentStep = 0
  for (const event of events) {
    if (event.type === 'step_started') {
      currentStep = Number(event.step ?? currentStep)
      continue
    }
    if (event.type === 'turn_started' || event.type === 'turn_finished'
      || event.type === 'step_finished' || event.type === 'session_title_generation_started'
      || event.type === 'session_title_generated' || event.type === 'session_title_generation_finished') continue

    if (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') {
      const step = Number(event.step ?? currentStep)
      if (finalSteps.has(step)) continue
      const chunks = deltas.get(step) ?? []
      if (chunks.at(-1)?.seq !== event.seq) continue
      const request = modelRequests.get(step)
      const started = request ?? stepStarts.get(step)
      const first = chunks[0]
      const firstReasoning = chunks.find(item => item.type === 'assistant_reasoning_delta')
      const lastReasoning = chunks.findLast(item => item.type === 'assistant_reasoning_delta')
      const firstContent = chunks.find(item => item.type === 'assistant_message_delta')
      const content = chunks.filter(item => item.type === 'assistant_message_delta').map(item => String(item.delta ?? '')).join('')
      const reasoningContent = chunks.filter(item => item.type === 'assistant_reasoning_delta').map(item => String(item.delta ?? '')).join('')
      const firstToken = firstReasoning && firstContent
        ? (firstReasoning.seq < firstContent.seq ? firstReasoning : firstContent)
        : firstReasoning ?? firstContent
      const reasoningCompletedAt = lastReasoning
        ? chunks.find(item => item.type === 'assistant_message_delta' && item.seq > lastReasoning.seq)?.occurred_at_ms ?? terminal?.occurred_at_ms
        : undefined
      records.push({
        key: `assistant-stream-${event.run_id}-${step}`,
        event,
        relatedEvents: [
          ...(stepStarts.get(step) ? [stepStarts.get(step)!] : []),
          ...(request ? [request] : []),
          ...chunks,
        ],
        kind: 'assistant',
        tag: 'ASSISTANT',
        title: event.type,
        summary: content || reasoningContent,
        step,
        requestNumber: requestNumbers.get(step),
        depth: 0,
        reasoningContent: reasoningContent || undefined,
        timing: {
          startedAt: started?.occurred_at_ms ?? first?.occurred_at_ms ?? event.occurred_at_ms,
          completedAt: terminal?.occurred_at_ms,
          durationMs: terminal ? Math.max(0, terminal.occurred_at_ms - (started?.occurred_at_ms ?? first?.occurred_at_ms ?? event.occurred_at_ms)) : undefined,
          firstTokenAt: firstToken?.occurred_at_ms,
          firstTokenDurationMs: started && firstToken ? Math.max(0, firstToken.occurred_at_ms - started.occurred_at_ms) : undefined,
          reasoningStartedAt: firstReasoning?.occurred_at_ms,
          reasoningCompletedAt,
          reasoningDurationMs: firstReasoning && reasoningCompletedAt != null
            ? Math.max(0, reasoningCompletedAt - firstReasoning.occurred_at_ms)
            : undefined,
        },
        running: !terminal,
        error: terminal?.type === 'turn_failed',
      })
      continue
    }

    if (event.type === 'assistant_message') {
      const step = Number(event.step ?? currentStep)
      const request = modelRequests.get(step)
      const started = request ?? stepStarts.get(step)
      const chunks = deltas.get(step) ?? []
      const firstReasoning = chunks.find(item => item.type === 'assistant_reasoning_delta')
      const lastReasoning = chunks.findLast(item => item.type === 'assistant_reasoning_delta')
      const firstContent = chunks.find(item => item.type === 'assistant_message_delta')
      const first = firstReasoning && firstContent
        ? (firstReasoning.seq < firstContent.seq ? firstReasoning : firstContent)
        : firstReasoning ?? firstContent
      const response = asRecord(event.response)
      const usage = asRecord(response.usage)
      const startedAt = started?.occurred_at_ms ?? first?.occurred_at_ms ?? event.occurred_at_ms
      const streamedReasoning = chunks.filter(item => item.type === 'assistant_reasoning_delta').map(item => String(item.delta ?? '')).join('')
      const reasoningContent = String(response.reasoning_content ?? '') || streamedReasoning
      const reasoningStartedAt = firstReasoning?.occurred_at_ms
        ?? (reasoningContent ? startedAt : undefined)
      const reasoningCompletedAt = reasoningStartedAt == null
        ? undefined
        : lastReasoning
          ? chunks.find(item => item.type === 'assistant_message_delta' && item.seq > lastReasoning.seq)?.occurred_at_ms
            ?? event.occurred_at_ms
          : event.occurred_at_ms
      records.push({
        key: `assistant-${event.seq}`,
        event,
        relatedEvents: [
          ...(stepStarts.get(step) ? [stepStarts.get(step)!] : []),
          ...(request ? [request] : []),
          ...chunks,
          event,
        ],
        kind: 'assistant',
        tag: 'ASSISTANT',
        title: event.type,
        summary: String(response.content ?? '') || reasoningContent,
        step,
        requestNumber: requestNumbers.get(step),
        toolCallCount: Array.isArray(response.tool_calls) ? response.tool_calls.length : undefined,
        depth: 0,
        inputTokens: typeof usage.input_tokens === 'number' ? usage.input_tokens : undefined,
        outputTokens: typeof usage.output_tokens === 'number' ? usage.output_tokens : undefined,
        reasoningTokens: typeof usage.reasoning_tokens === 'number' ? usage.reasoning_tokens : undefined,
        cachedTokens: typeof usage.cached_input_tokens === 'number' ? usage.cached_input_tokens : undefined,
        cacheWriteTokens: typeof usage.cache_write_tokens === 'number' ? usage.cache_write_tokens : undefined,
        provider: typeof response.provider === 'string' ? response.provider : undefined,
        model: typeof response.model === 'string' ? response.model : undefined,
        finishReason: typeof response.finish_reason === 'string' ? response.finish_reason : undefined,
        reasoningContent: reasoningContent || undefined,
        timing: {
          startedAt,
          firstTokenAt: first?.occurred_at_ms,
          completedAt: event.occurred_at_ms,
          durationMs: Math.max(0, event.occurred_at_ms - startedAt),
          firstTokenDurationMs: started && first ? Math.max(0, first.occurred_at_ms - started.occurred_at_ms) : undefined,
          generationDurationMs: first ? Math.max(0, event.occurred_at_ms - first.occurred_at_ms) : undefined,
          reasoningStartedAt,
          reasoningCompletedAt,
          reasoningDurationMs: reasoningStartedAt == null || reasoningCompletedAt == null
            ? undefined
            : Math.max(0, reasoningCompletedAt - reasoningStartedAt),
        },
        running: false,
        error: false,
      })
      continue
    }

    const callStarted = event.type === 'tool_call_started' || event.type === 'code_dispatch_started'
    const callFinished = event.type === 'tool_call_finished' || event.type === 'code_dispatch_finished'
    if ((callStarted && event.call) || (callFinished && event.call_id)) {
      const callId = callStarted ? String(asRecord(event.call).id ?? '') : String(event.call_id ?? '')
      const trace = traces.get(callId)
      if (!trace) continue
      if (callFinished && trace.started.seq !== event.seq) continue
      records.push({
        key: `tool-${event.run_id}-${trace.id}`,
        event,
        relatedEvents: [trace.started, ...(trace.finished ? [trace.finished] : [])],
        kind: trace.kind === 'code' ? 'code' : 'tool',
        tag: trace.kind === 'code' ? 'CODE' : 'TOOL',
        title: trace.presentation?.title ?? trace.name,
        summary: toolTracePreview(trace),
        step: currentStep,
        parentCallId: trace.parentCallId,
        depth: traceDepth(trace, traces, traceDepths),
        timing: {
          startedAt: trace.started.occurred_at_ms,
          completedAt: trace.finished?.occurred_at_ms,
          durationMs: trace.durationMs,
        },
        trace,
        running: !trace.finished,
        error: trace.output?.is_error ?? false,
      })
      continue
    }

    const classification = recordKind(event)
    records.push({
      key: `event-${event.seq}`,
      event,
      relatedEvents: [event],
      ...classification,
      title: event.type,
      summary: eventSummary(event),
      step: typeof event.step === 'number' ? event.step : currentStep,
      requestNumber: event.type === 'model_request_started' && typeof event.step === 'number'
        ? requestNumbers.get(event.step)
        : undefined,
      depth: 0,
      running: false,
      error: event.type === 'turn_failed' || Boolean(asRecord(event.output).is_error),
    })
  }
  return records
}

export function trajectoryRecordTitle(record: TrajectoryRecord, t: Translate<'trajectory'>): string {
  if (record.kind === 'assistant') return t('event.assistant')
  if (record.kind === 'tool' || record.kind === 'code') return record.title
  const labels: Record<string, string> = {
    turn_started: t('event.turnStarted'), user_message: t('event.userMessage'), step_started: t('event.modelStep'),
    model_request_started: t('event.systemPrompt'), assistant_reasoning_delta: t('event.reasoning'), assistant_message_delta: t('event.streaming'),
    assistant_message: t('event.assistant'), tool_call_started: t('event.toolCall'), tool_call_finished: t('event.toolResult'),
    code_dispatch_started: t('event.codeCall'), code_dispatch_finished: t('event.codeResult'), plan_updated: t('event.plan'),
    job_updated: t('event.toolResult'),
    plan_review_completed: t('event.planReview'), todo_updated: t('event.todo'), goal_updated: t('event.goal'), context_compacted: t('event.compacted'),
    goal_round_started: t('event.goalRound'),
    schedule_changed: t('event.schedule'), deliverable_produced: t('event.deliverable'), feedback_recorded: t('event.feedback'), subagent_updated: t('event.subagent'),
    workflow_run_started: t('event.workflow'), workflow_phase_changed: t('event.workflow'), workflow_log_emitted: t('event.workflow'),
    workflow_agent_started: t('event.workflow'), workflow_agent_finished: t('event.workflow'), workflow_run_finished: t('event.workflow'),
    user_question_asked: t('event.question'), user_question_answered: t('event.answer'), hook_result: t('event.hook'), hook_context_added: t('event.hook'),
    step_finished: t('event.stepFinished'), turn_finished: t('event.completed'), turn_failed: t('event.failed'), turn_cancelled: t('event.cancelled'),
    runtime_extension_changed: t('event.extension'),
  }
  return labels[record.event.type] ?? record.event.type
}

export function buildTrajectory(events: SessionEvent[]): TrajectoryTurn[] {
  const traces = buildToolTraces(events)
  const grouped = new Map<string, SessionEvent[]>()
  const replaced = new Set<string>()
  for (const event of events) {
    if (regenerationTarget(event) !== undefined) {
      for (const runId of grouped.keys()) if (runId !== event.run_id) replaced.add(runId)
    }
    const group = grouped.get(event.run_id)
    if (group) group.push(event)
    else grouped.set(event.run_id, [event])
  }
  return [...grouped.entries()].map(([runId, runEvents], index) => {
    const ordered = [...runEvents].sort((a, b) => a.seq - b.seq)
    const startedAt = ordered[0]?.occurred_at_ms ?? 0
    const endedAt = ordered.at(-1)?.occurred_at_ms ?? startedAt
    const terminal = ordered.findLast(event => event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled')
    const status = terminal?.type === 'turn_failed' ? 'error'
      : terminal?.type === 'turn_cancelled' ? 'cancelled'
        : terminal?.type === 'turn_finished' || replaced.has(runId) ? 'complete' : 'running'
    const records = orderNestedTools(buildRunRecords(ordered, traces))
    return {
      runId,
      number: index + 1,
      startedAt,
      endedAt,
      durationMs: Math.max(0, endedAt - startedAt),
      status,
      groups: groupRecords(records),
      records,
      boundaryEvents: ordered.filter(event => event.type === 'turn_started' || event.type === 'turn_finished' || event.type === 'step_finished'),
    }
  })
}
