import type { ConversationItem } from './events'
import type { Attachment, SessionEvent } from '@/types'

export interface ChatTurnProcessCounts {
  messages: number
  tools: number
  subagents: number
}

export interface ChatTurnUsage {
  uncachedInputTokens: number
  cacheReadTokens?: number
  cacheWriteTokens?: number
  outputTokens: number
  reasoningTokens?: number
  totalTokens: number
  routes?: Array<{ provider: string; model: string }>
}

export interface ChatTurnMetrics {
  durationMs?: number
  ttftMs?: number
  tokensPerSecond?: number
}

export interface ChatTurnDeliverable {
  path: string
  operation: string
  attachment: Attachment
  seq: number
}

export interface ChatTurn {
  runId: string
  number: number
  items: ConversationItem[]
  anchorKey: string
  prompt: string
  response: string
  closed: boolean
  finalAnswerKey?: string
  processKeys: ReadonlySet<string>
  processControlIndex?: number
  inlineReasoning: boolean
  foldable: boolean
  generation: string
  counts: ChatTurnProcessCounts
  usage?: ChatTurnUsage
  metrics: ChatTurnMetrics
  deliverables: ChatTurnDeliverable[]
}

const independentCardTypes = new Set(['turn_failed', 'turn_cancelled'])

function isFinalAnswer(item: ConversationItem) {
  if (item.kind !== 'assistant' || !item.content.trim()) return false
  const response = item.event.response
  if (typeof response !== 'object' || response === null) return true
  const toolCalls = (response as { tool_calls?: unknown }).tool_calls
  return !Array.isArray(toolCalls) || toolCalls.length === 0
}

function isProcessItem(item: ConversationItem) {
  return item.kind !== 'user'
    && item.kind !== 'system_prompt'
    && item.kind !== 'max_tokens'
    && !(item.kind === 'card' && independentCardTypes.has(item.event.type))
}

function isSubagentDelegation(name: string) {
  return name === 'spawn_agent' || name === 'subagent' || name.startsWith('subagent_')
}

function countToolTree(
  trace: Extract<ConversationItem, { kind: 'tool' }>['trace'],
  counts: ChatTurnProcessCounts,
) {
  if (isSubagentDelegation(trace.name)) counts.subagents += 1
  else counts.tools += 1
  for (const child of trace.children) countToolTree(child, counts)
}

function preview(value: string) {
  return value.replace(/\s+/g, ' ').trim().slice(0, 160)
}

function numericUsage(event: SessionEvent) {
  if (event.type !== 'assistant_message' || typeof event.response !== 'object' || event.response === null) return null
  const usage = event.response.usage
  if (typeof usage !== 'object' || usage === null) return null
  const values = [usage.input_tokens, usage.output_tokens]
  if (values.some(value => typeof value !== 'number' || !Number.isFinite(value) || value < 0)) return null
  const input = usage.input_tokens
  const output = usage.output_tokens
  const cacheRead = usage.cached_input_tokens
  const cacheWrite = usage.cache_write_tokens
  const reasoning = usage.reasoning_tokens
  if ([cacheRead, cacheWrite, reasoning].some(value => value !== undefined
    && (typeof value !== 'number' || !Number.isFinite(value) || value < 0))) return null
  if ((cacheRead ?? 0) + (cacheWrite ?? 0) > input || (reasoning ?? 0) > output) return null
  const provider = typeof event.response.provider === 'string' ? event.response.provider : undefined
  const model = typeof event.response.model === 'string' ? event.response.model : undefined
  return { input, output, cacheRead, cacheWrite, reasoning, provider, model }
}

function deriveUsage(events: SessionEvent[]): ChatTurnUsage | undefined {
  const responses = events.filter(event => event.type === 'assistant_message')
  if (!responses.length) return undefined
  const readings = responses.map(numericUsage)
  if (readings.some(reading => reading === null)) return undefined
  const usage = readings.reduce<ChatTurnUsage>((total, reading) => {
    const value = reading!
    total.uncachedInputTokens += value.input - (value.cacheRead ?? 0) - (value.cacheWrite ?? 0)
    if (value.cacheRead !== undefined) total.cacheReadTokens = (total.cacheReadTokens ?? 0) + value.cacheRead
    if (value.cacheWrite !== undefined) total.cacheWriteTokens = (total.cacheWriteTokens ?? 0) + value.cacheWrite
    total.outputTokens += value.output
    if (value.reasoning !== undefined) total.reasoningTokens = (total.reasoningTokens ?? 0) + value.reasoning
    total.totalTokens += value.input + value.output
    return total
  }, { uncachedInputTokens: 0, outputTokens: 0, totalTokens: 0 })
  const routes = readings.flatMap(reading => reading?.provider && reading.model
    ? [{ provider: reading.provider, model: reading.model }] : [])
    .filter((route, index, values) => values.findIndex(value => value.provider === route.provider && value.model === route.model) === index)
  if (routes.length) usage.routes = routes
  return usage
}

function deriveMetrics(events: SessionEvent[]): ChatTurnMetrics {
  const terminal = events.findLast(event =>
    event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled')
  const started = events.find(event => event.type === 'turn_started')
  const metrics: ChatTurnMetrics = {}
  if (started && terminal) metrics.durationMs = Math.max(0, terminal.occurred_at_ms - started.occurred_at_ms)
  const assistants = events.filter(event => event.type === 'assistant_message' && typeof event.step === 'number')
  const firstStep = assistants.reduce<number | undefined>((lowest, event) =>
    lowest === undefined ? event.step : Math.min(lowest, event.step!), undefined)
  if (firstStep !== undefined) {
    const requestStart = events.find(event => event.type === 'model_request_started' && event.step === firstStep)
    const firstToken = events.find(event => event.step === firstStep
      && (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta')
      && String(event.delta ?? '') !== '')
    if (requestStart && firstToken) metrics.ttftMs = Math.max(0, firstToken.occurred_at_ms - requestStart.occurred_at_ms)
  }
  let outputTokens = 0
  let decodeMs = 0
  for (const assistant of assistants) {
    const usage = numericUsage(assistant)
    const firstToken = events.find(event => event.step === assistant.step
      && (event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta')
      && String(event.delta ?? '') !== '')
    if (!usage || !firstToken || assistant.occurred_at_ms <= firstToken.occurred_at_ms) continue
    outputTokens += usage.output
    decodeMs += assistant.occurred_at_ms - firstToken.occurred_at_ms
  }
  if (decodeMs > 0) metrics.tokensPerSecond = outputTokens / (decodeMs / 1_000)
  return metrics
}

function deriveDeliverables(events: SessionEvent[], throughSeq?: number): ChatTurnDeliverable[] {
  const values: ChatTurnDeliverable[] = []
  const positions = new Map<string, number>()
  for (const event of events) {
    if (event.type !== 'deliverable_produced' || (throughSeq !== undefined && event.seq > throughSeq)) continue
    if (typeof event.path !== 'string' || !event.path.trim()) continue
    const attachment = event.attachment
    if (typeof attachment !== 'object' || attachment === null) continue
    const value = attachment as Partial<Attachment>
    if (typeof value.name !== 'string' || typeof value.media_type !== 'string' || typeof value.content !== 'string') continue
    const deliverable = {
      path: event.path,
      operation: typeof event.operation === 'string' ? event.operation : 'write',
      attachment: value as Attachment,
      seq: event.seq,
    }
    const position = positions.get(event.path)
    if (position === undefined) {
      positions.set(event.path, values.length)
      values.push(deliverable)
    } else values[position] = deliverable
  }
  return values
}

/**
 * Project the visible Chat flow into stable run-owned turns. The projection is
 * deliberately derived from the durable event log: a live turn never folds,
 * and an incomplete history window never receives process controls.
 */
export function buildChatTurns(
  items: ConversationItem[],
  events: SessionEvent[],
  historyIncomplete = false,
): ChatTurn[] {
  const grouped = new Map<string, ConversationItem[]>()
  for (const item of items) {
    const runId = item.event.run_id || `event-${item.event.seq}`
    const group = grouped.get(runId)
    if (group) group.push(item)
    else grouped.set(runId, [item])
  }
  const terminalRuns = new Set(events.flatMap(event =>
    event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled'
      ? [event.run_id]
      : [],
  ))
  const eventsByRun = new Map<string, SessionEvent[]>()
  for (const event of events) {
    const group = eventsByRun.get(event.run_id)
    if (group) group.push(event)
    else eventsByRun.set(event.run_id, [event])
  }

  return [...grouped.entries()].map(([runId, turnItems], index) => {
    const closed = terminalRuns.has(runId)
    const finalAnswerIndex = closed ? turnItems.findLastIndex(isFinalAnswer) : -1
    const finalAnswer = finalAnswerIndex < 0 ? undefined : turnItems[finalAnswerIndex]
    const processItems = finalAnswerIndex < 0
      ? turnItems.filter(isProcessItem)
      : turnItems.slice(0, finalAnswerIndex).filter(isProcessItem)
    const processKeys = new Set(processItems.map(item => item.key))
    const counts: ChatTurnProcessCounts = { messages: 0, tools: 0, subagents: 0 }
    for (const item of processItems) {
      if (item.kind === 'assistant' && item.content.trim()) counts.messages += 1
      if (item.kind !== 'tool') continue
      countToolTree(item.trace, counts)
    }
    const inlineReasoning = finalAnswer?.kind === 'assistant'
      && Boolean(finalAnswer.reasoning?.text.trim())
    const foldable = !historyIncomplete && closed && finalAnswer !== undefined
      && (processKeys.size > 0 || inlineReasoning)
    const firstProcessIndex = turnItems.findIndex(item => processKeys.has(item.key))
    const processControlIndex = foldable
      ? firstProcessIndex >= 0 ? firstProcessIndex : finalAnswerIndex
      : undefined
    const firstUser = turnItems.find(item => item.kind === 'user')
    const anchor = firstUser ?? turnItems[0]
    const runEvents = eventsByRun.get(runId) ?? []
    return {
      runId,
      number: index + 1,
      items: turnItems,
      anchorKey: anchor?.key ?? runId,
      prompt: firstUser?.kind === 'user' ? preview(firstUser.content) : '',
      response: finalAnswer?.kind === 'assistant' ? preview(finalAnswer.content) : '',
      closed,
      finalAnswerKey: finalAnswer?.key,
      processKeys,
      processControlIndex,
      inlineReasoning,
      foldable,
      generation: `${runId}:${finalAnswer?.event.step ?? ''}:${finalAnswer?.event.seq ?? ''}`,
      counts,
      usage: closed ? deriveUsage(runEvents) : undefined,
      metrics: deriveMetrics(runEvents),
      deliverables: deriveDeliverables(runEvents, finalAnswer?.event.seq),
    }
  })
}
