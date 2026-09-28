import type {
  ReferenceContextCompleteness, SessionEvent, ToolCall, ToolOutput, ToolPresentationDescriptor,
  UserQuestion, UserQuestionAnswer,
} from '@/types'
import { asRecord } from '@/lib/utils'
import {
  commandLifecycle, retryLifecycle, turnReachedMaxTokens,
  type ChatCommandLifecycle, type ChatRetryLifecycle,
} from './chat-event-contract'

export interface ToolTrace {
  id: string
  name: string
  arguments: unknown
  presentation?: ToolPresentationDescriptor
  children: ToolTrace[]
  output?: ToolOutput
  retainedOutput?: unknown
  started: SessionEvent
  finished?: SessionEvent
  parentCallId?: string
  durationMs?: number
  kind: 'tool' | 'code'
}

export interface AssistantReasoning {
  text: string
  running: boolean
  startedAt: number
  completedAt?: number
  durationMs?: number
}

export type QuestionLifecycleState = 'pending' | 'answered' | 'interrupted'
export type QuestionLifecycleKind = 'question' | 'tool-approval' | 'plan-review'

export interface QuestionLifecycle {
  id: string
  kind: QuestionLifecycleKind
  state: QuestionLifecycleState
  question: UserQuestion
  asked: SessionEvent
  answered?: SessionEvent
  terminal?: SessionEvent
  answer?: UserQuestionAnswer
}

export type ConversationItem =
  | { kind: 'user'; key: string; event: SessionEvent; content: string }
  | { kind: 'system_prompt'; key: string; event: SessionEvent; content: string }
  | {
    kind: 'context'
    key: string
    event: SessionEvent
    content: string
    source: string
    dialect?: string
    referenceLabel?: string
    completeness?: ReferenceContextCompleteness
  }
  | {
    kind: 'assistant'
    key: string
    event: SessionEvent
    content: string
    reasoning?: AssistantReasoning
    streaming: boolean
    interrupted?: boolean
  }
  | { kind: 'tool'; key: string; event: SessionEvent; trace: ToolTrace }
  | { kind: 'retry'; key: string; event: SessionEvent; lifecycle: ChatRetryLifecycle }
  | { kind: 'command'; key: string; event: SessionEvent; lifecycle: ChatCommandLifecycle }
  | { kind: 'question'; key: string; event: SessionEvent; lifecycle: QuestionLifecycle }
  | {
    kind: 'compaction'
    key: string
    event: SessionEvent
    running: boolean
    automatic: boolean
    summary: string | null
    shadowedItems: number | null
    shadowedTokens: number | null
  }
  | { kind: 'max_tokens'; key: string; event: SessionEvent }
  | { kind: 'card'; key: string; event: SessionEvent }

const visibleCards = new Set([
  'plan_updated', 'plan_review_completed', 'todo_updated', 'goal_updated', 'goal_round_started',
  'schedule_changed', 'subagent_updated', 'runtime_extension_changed',
  'workflow_run_started', 'workflow_phase_changed', 'workflow_log_emitted',
  'workflow_agent_started', 'workflow_agent_finished', 'workflow_run_finished',
  'turn_failed', 'turn_cancelled', 'hook_result',
])

function contextReferenceLabel(value: unknown) {
  const reference = asRecord(value)
  if (reference.kind === 'file' && typeof reference.path === 'string') return reference.path
  if (reference.kind === 'session' && typeof reference.label === 'string') return reference.label
  return undefined
}

function contextCompleteness(value: unknown): ReferenceContextCompleteness | undefined {
  const completeness = asRecord(value)
  const retained = completeness.retained_items
  const omitted = completeness.omitted_items
  if (!Number.isSafeInteger(retained) || Number(retained) < 0
    || !Number.isSafeInteger(omitted) || Number(omitted) < 0
    || typeof completeness.truncated !== 'boolean') return undefined
  return {
    retained_items: Number(retained),
    omitted_items: Number(omitted),
    truncated: completeness.truncated,
  }
}

function questionKind(question: UserQuestion): QuestionLifecycleKind {
  if (question.tool_approval) return 'tool-approval'
  if (question.presentation?.kind === 'plan_review') return 'plan-review'
  return 'question'
}

/** Pair canonical question events and settle unanswered questions with their own run. */
export function questionLifecycles(events: readonly SessionEvent[]) {
  const values = new Map<string, QuestionLifecycle>()
  const pendingByRun = new Map<string, Set<string>>()
  for (const event of events) {
    if (event.type === 'user_question_asked') {
      const question = event.question as UserQuestion
      if (!question?.id) continue
      values.set(question.id, {
        id: question.id,
        kind: questionKind(question),
        state: 'pending',
        question,
        asked: event,
      })
      const pending = pendingByRun.get(event.run_id) ?? new Set<string>()
      pending.add(question.id)
      pendingByRun.set(event.run_id, pending)
      continue
    }
    if (event.type === 'user_question_answered') {
      const answer = asRecord(event.answer)
      const id = String(answer.question_id ?? '')
      const current = values.get(id)
      if (!current) continue
      values.set(id, {
        ...current,
        state: 'answered',
        answered: event,
        terminal: undefined,
        answer: {
          selected: Array.isArray(answer.selected)
            ? answer.selected.filter((value): value is string => typeof value === 'string')
            : [],
          ...(typeof answer.custom === 'string' ? { custom: answer.custom } : {}),
        },
      })
      pendingByRun.get(current.asked.run_id)?.delete(id)
      continue
    }
    if (event.type !== 'turn_finished' && event.type !== 'turn_failed' && event.type !== 'turn_cancelled') continue
    const pending = pendingByRun.get(event.run_id)
    if (!pending) continue
    for (const id of pending) {
      const current = values.get(id)
      if (current?.state === 'pending') values.set(id, {
        ...current,
        state: 'interrupted',
        terminal: event,
      })
    }
    pending.clear()
  }
  return values
}

export function buildToolTraces(events: SessionEvent[]) {
  const traces = new Map<string, ToolTrace>()
  for (const event of events) {
    if ((event.type === 'tool_call_started' || event.type === 'code_dispatch_started') && event.call) {
      const call = event.call as ToolCall
      const existing = traces.get(call.id)
      traces.set(call.id, {
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        presentation: call.presentation ?? undefined,
        children: [],
        started: event,
        parentCallId: typeof event.parent_call_id === 'string' ? event.parent_call_id : undefined,
        kind: event.type === 'code_dispatch_started' ? 'code' : 'tool',
        finished: existing?.finished,
        output: existing?.output,
        retainedOutput: existing?.retainedOutput,
        durationMs: existing?.finished
          ? Math.max(0, existing.finished.occurred_at_ms - event.occurred_at_ms)
          : undefined,
      })
    }
    if (event.type === 'tool_call_finished' || event.type === 'code_dispatch_finished') {
      const id = String(event.call_id ?? '')
      const existing = traces.get(id)
      const output = event.output as ToolOutput | undefined
      if (existing) {
        existing.finished = event
        existing.output = output
        existing.retainedOutput = event.retained_output
        existing.durationMs = Math.max(0, event.occurred_at_ms - existing.started.occurred_at_ms)
      } else if (id) {
        traces.set(id, {
          id,
          name: String(event.name ?? 'tool'),
          arguments: null,
          children: [],
          output,
          retainedOutput: event.retained_output,
          started: event,
          finished: event,
          parentCallId: typeof event.parent_call_id === 'string' ? event.parent_call_id : undefined,
          durationMs: 0,
          kind: event.type === 'code_dispatch_finished' ? 'code' : 'tool',
        })
      }
    }
  }
  for (const trace of traces.values()) {
    const parent = trace.parentCallId ? traces.get(trace.parentCallId) : undefined
    if (parent && parent !== trace) parent.children.push(trace)
  }
  return traces
}

export function buildConversationItems(events: SessionEvent[]): ConversationItem[] {
  const traces = buildToolTraces(events)
  const retries = retryLifecycle(events)
  const commands = commandLifecycle(events)
  const questions = questionLifecycles(events)
  const latestSubagentSeq = new Map<string, number>()
  for (const event of events) {
    if (event.type !== 'subagent_updated') continue
    const id = String(asRecord(event.subagent)?.subagent_id ?? '')
    if (id) latestSubagentSeq.set(id, event.seq)
  }
  const rootTraceIds = new Set([...traces.values()].flatMap(trace =>
    trace.parentCallId && traces.has(trace.parentCallId) ? [] : [trace.id],
  ))
  const finalizedStreams = new Set<string>()
  const terminalRuns = new Map<string, SessionEvent>()
  const stepStarts = new Map<string, SessionEvent>()
  const streaming = new Map<string, {
    content: string
    reasoning: string
    latest: SessionEvent
    reasoningStartedAt?: number
    reasoningCompletedAt?: number
  }>()
  for (const event of events) {
    const key = `${event.run_id}:${event.step ?? 0}`
    if (event.type === 'step_started') stepStarts.set(key, event)
    if (event.type === 'assistant_message') finalizedStreams.add(key)
    if ((event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') && !terminalRuns.has(event.run_id)) {
      terminalRuns.set(event.run_id, event)
    }
    if (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') {
      const delta = String(event.delta ?? '')
      if (!delta) continue
      const current = streaming.get(key) ?? { content: '', reasoning: '', latest: event }
      if (event.type === 'assistant_reasoning_delta') {
        current.reasoning += delta
        current.reasoningStartedAt ??= event.occurred_at_ms
        current.reasoningCompletedAt = undefined
      } else {
        current.content += delta
        if (current.reasoning && current.reasoningCompletedAt == null) {
          current.reasoningCompletedAt = event.occurred_at_ms
        }
      }
      current.latest = event
      streaming.set(key, current)
    }
  }

  const items: ConversationItem[] = []
  const emittedTools = new Set<string>()
  let previousSystemPrompt: string | undefined
  for (const event of events) {
    if (event.type === 'user_message') {
      items.push({
        kind: 'user', key: `event-${event.seq}`, event,
        content: String(event.display_content ?? event.content ?? ''),
      })
      continue
    }
    if (event.type === 'model_request_started') {
      const content = String(event.system_prompt ?? '')
      // Configuration/tool-only
      // request churn does not repeat an identical prompt in Chat. The exact
      // per-request event remains available in Trajectory and export.
      if (content && content !== previousSystemPrompt) {
        items.push({ kind: 'system_prompt', key: `event-${event.seq}`, event, content })
      }
      previousSystemPrompt = content
      continue
    }
    if (event.type === 'hook_context_added') {
      const source = String(event.handler_id ?? 'hook')
      const dialect = String(event.dialect ?? 'native')
      const content = String(event.content ?? '')
      items.push({
        kind: 'context',
        key: `event-${event.seq}`,
        event,
        content: `<hook_context handler=${JSON.stringify(source)} dialect=${JSON.stringify(dialect)}>\n${content}\n</hook_context>`,
        source,
        dialect,
        referenceLabel: contextReferenceLabel(event.reference),
        completeness: contextCompleteness(event.completeness),
      })
      continue
    }
    if (event.type === 'assistant_message') {
      const response = asRecord(event.response)
      const key = `${event.run_id}:${event.step ?? 0}`
      const stream = streaming.get(key)
      const content = String(response.content ?? '') || stream?.content || ''
      const responseReasoning = String(response.reasoning_content ?? '')
      const reasoningText = responseReasoning.trim() ? responseReasoning : stream?.reasoning ?? ''
      const startedAt = stream?.reasoningStartedAt ?? stepStarts.get(key)?.occurred_at_ms ?? event.occurred_at_ms
      const completedAt = stream?.reasoningCompletedAt ?? (reasoningText ? event.occurred_at_ms : undefined)
      const reasoning = reasoningText ? {
        text: reasoningText,
        running: false,
        startedAt,
        completedAt,
        durationMs: completedAt == null ? undefined : Math.max(0, completedAt - startedAt),
      } : undefined
      if (content || reasoning) {
        items.push({ kind: 'assistant', key: `event-${event.seq}`, event, content, reasoning, streaming: false })
      }
      continue
    }
    if (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') {
      const key = `${event.run_id}:${event.step ?? 0}`
      const stream = streaming.get(key)
      if (!finalizedStreams.has(key) && stream?.latest.seq === event.seq && (stream.content || stream.reasoning)) {
        const startedAt = stream.reasoningStartedAt
        const terminal = terminalRuns.get(event.run_id)
        const completedAt = stream.reasoningCompletedAt ?? terminal?.occurred_at_ms
        const reasoning = stream.reasoning && startedAt != null ? {
          text: stream.reasoning,
          running: completedAt == null,
          startedAt,
          completedAt,
          durationMs: completedAt == null
            ? undefined
            : Math.max(0, completedAt - startedAt),
        } : undefined
        items.push({
          kind: 'assistant', key: `stream-${key}`, event,
          content: stream.content, reasoning,
          streaming: !terminalRuns.has(event.run_id),
          interrupted: Boolean(terminal && terminal.type !== 'turn_finished'),
        })
      }
      continue
    }
    if ((event.type === 'tool_call_started' || event.type === 'code_dispatch_started') && event.call) {
      const id = (event.call as ToolCall).id
      const trace = traces.get(id)
      if (trace && rootTraceIds.has(id) && !emittedTools.has(id)) {
        emittedTools.add(id)
        items.push({ kind: 'tool', key: `tool-${id}`, event, trace })
      }
      continue
    }
    if ((event.type === 'tool_call_finished' || event.type === 'code_dispatch_finished') && event.call_id) {
      const id = String(event.call_id)
      if (rootTraceIds.has(id) && !emittedTools.has(id)) {
        const trace = traces.get(id)
        if (trace) {
          emittedTools.add(id)
          items.push({ kind: 'tool', key: `tool-${id}`, event, trace })
        }
      }
      continue
    }
    if (event.type === 'model_retry_scheduled') {
      const id = String(event.retry_id ?? '')
      const lifecycle = retries.get(id)
      if (lifecycle && lifecycle.anchor.seq === event.seq) {
        items.push({ kind: 'retry', key: `retry-${id}`, event, lifecycle })
      }
      continue
    }
    if (event.type === 'model_retry_started' || event.type === 'model_retry_cancelled') continue
    if (event.type === 'command_started' || event.type === 'command_finished') {
      const id = String(event.command_id ?? '')
      const lifecycle = commands.get(id)
      if (lifecycle && lifecycle.event.seq === event.seq) {
        items.push({ kind: 'command', key: `command-${id}`, event, lifecycle })
      }
      continue
    }
    if (event.type === 'user_question_asked') {
      const id = String(asRecord(event.question).id ?? '')
      const lifecycle = questions.get(id)
      if (lifecycle?.asked.seq === event.seq) {
        items.push({ kind: 'question', key: `question-${id}`, event, lifecycle })
      }
      continue
    }
    if (event.type === 'user_question_answered') continue
    if (event.type === 'context_compaction_started') {
      const completed = events.find(candidate => candidate.type === 'context_compacted'
        && candidate.compaction_id === event.compaction_id)
      if (!completed) {
        items.push({
          kind: 'compaction', key: `compaction-${String(event.compaction_id ?? event.seq)}`, event,
          running: true, automatic: event.automatic !== false,
          summary: null, shadowedItems: null, shadowedTokens: null,
        })
      }
      continue
    }
    if (event.type === 'context_compacted') {
      const compaction = asRecord(event.compaction)
      items.push({
        kind: 'compaction', key: `compaction-${String(event.compaction_id ?? event.seq)}`, event,
        running: false,
        automatic: compaction.automatic !== false,
        summary: typeof compaction.summary === 'string' && compaction.summary.trim()
          ? compaction.summary : null,
        shadowedItems: typeof event.shadowed_item_count === 'number' ? event.shadowed_item_count : null,
        shadowedTokens: typeof compaction.estimated_tokens_before === 'number'
          ? compaction.estimated_tokens_before : null,
      })
      continue
    }
    if (turnReachedMaxTokens(event)) {
      items.push({ kind: 'max_tokens', key: `max-tokens-${event.seq}`, event })
      continue
    }
    if (event.type === 'workflow_phase_changed' || event.type === 'workflow_log_emitted'
      || event.type === 'workflow_agent_started' || event.type === 'workflow_agent_finished'
      || event.type === 'workflow_run_finished') continue
    if (event.type === 'subagent_updated') {
      const id = String(asRecord(event.subagent)?.subagent_id ?? '')
      if (!id || latestSubagentSeq.get(id) !== event.seq) continue
    }
    if (visibleCards.has(event.type)) items.push({ kind: 'card', key: `event-${event.seq}`, event })
  }
  return items
}

export function completedAssistantTailSeqs(events: SessionEvent[]) {
  const latest = new Map<string, number>()
  const completed = new Set<number>()
  for (const event of events) {
    if (event.type === 'assistant_message') latest.set(event.run_id, event.seq)
    if (event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') {
      const seq = latest.get(event.run_id)
      if (seq !== undefined) completed.add(seq)
    }
  }
  return completed
}

export function eventSummary(event: SessionEvent) {
  if (event.type === 'user_message') return String(event.display_content ?? event.content ?? '')
  if (event.type === 'model_request_started') return String(event.system_prompt ?? '')
  if (event.type === 'hook_context_added') {
    const handler = String(event.handler_id ?? 'hook')
    const dialect = String(event.dialect ?? '')
    return [handler, dialect, contextReferenceLabel(event.reference)].filter(Boolean).join(' · ')
  }
  if (event.type === 'assistant_message') {
    const response = asRecord(event.response)
    return String(response.content ?? '') || String(response.reasoning_content ?? '')
  }
  if (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') return String(event.delta ?? '')
  if (event.type === 'job_updated') {
    const job = asRecord(event.job)
    return [job.command, job.status].filter(value => typeof value === 'string' && value).join(' · ')
  }
  if (event.call) return (event.call as ToolCall).name
  if (event.name) return String(event.name)
  if (event.message) return String(event.message)
  const question = asRecord(event.question)
  if (question.question) return String(question.question)
  const meta = asRecord(event.meta)
  if (meta.name) return String(meta.name)
  if (event.title) return String(event.title)
  if (event.objective) return String(event.objective)
  if (event.path) return String(event.path)
  return ''
}

export function compactTrajectoryEvents(events: SessionEvent[]) {
  const startedCalls = new Set(events.flatMap(event => {
    if ((event.type === 'tool_call_started' || event.type === 'code_dispatch_started') && event.call) return [event.call.id]
    return []
  }))
  const finalizedStreams = new Set(events.flatMap(event => event.type === 'assistant_message' ? [`${event.run_id}:${event.step ?? 0}`] : []))
  const latestDeltas = new Map<string, number>()
  for (const event of events) {
    if ((event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') && String(event.delta ?? '')) {
      latestDeltas.set(`${event.run_id}:${event.step ?? 0}`, event.seq)
    }
  }
  return events.filter(event => {
    if ((event.type === 'tool_call_finished' || event.type === 'code_dispatch_finished') && startedCalls.has(String(event.call_id ?? ''))) return false
    if (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') {
      const key = `${event.run_id}:${event.step ?? 0}`
      return !finalizedStreams.has(key) && latestDeltas.get(key) === event.seq
    }
    return true
  })
}
