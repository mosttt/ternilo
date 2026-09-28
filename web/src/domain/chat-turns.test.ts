import { describe, expect, it } from 'vitest'
import type { ConversationItem } from './events'
import { buildChatTurns } from './chat-turns'
import type { SessionEvent, ToolCall } from '@/types'

function event(seq: number, type: string, extra: Partial<SessionEvent> = {}): SessionEvent {
  return { seq, type, run_id: 'run-1', occurred_at_ms: seq * 10, ...extra }
}

function user(seq: number, content: string): ConversationItem {
  const value = event(seq, 'user_message', { content })
  return { kind: 'user', key: `event-${seq}`, event: value, content }
}

function assistant(seq: number, content: string, toolCalls: ToolCall[] = []): ConversationItem {
  const value = event(seq, 'assistant_message', { step: seq, response: {
    provider: 'fixture', model: 'fixture-model',
    finish_reason: toolCalls.length > 0 ? 'tool_calls' : 'stop', content, tool_calls: toolCalls,
  } })
  return { kind: 'assistant', key: `event-${seq}`, event: value, content, streaming: false }
}

function tool(seq: number, name: string): ConversationItem {
  const value = event(seq, 'tool_call_started')
  return { kind: 'tool', key: `tool-${seq}`, event: value, trace: {
    id: `call-${seq}`, name, arguments: {}, children: [], started: value, kind: 'tool',
  } }
}

describe('Chat turn process projection', () => {
  it('folds only the process before a settled final answer', () => {
    const items = [user(0, 'fix it'), assistant(2, 'checking', [{ id: 'call', name: 'read_file', arguments: {} }]), tool(3, 'read_file'), assistant(5, 'done')]
    const turns = buildChatTurns(items, [...items.map(item => item.event), event(6, 'turn_finished')])
    expect(turns).toHaveLength(1)
    expect(turns[0]).toMatchObject({ closed: true, foldable: true, finalAnswerKey: 'event-5' })
    expect([...turns[0]!.processKeys]).toEqual(['event-2', 'tool-3'])
    expect(turns[0]!.counts).toEqual({ messages: 1, tools: 1, subagents: 0 })
  })

  it('keeps live and incomplete-history turns expanded', () => {
    const items = [user(0, 'question'), assistant(2, 'working'), assistant(4, 'answer')]
    expect(buildChatTurns(items, items.map(item => item.event))[0]?.foldable).toBe(false)
    expect(buildChatTurns(items, [...items.map(item => item.event), event(5, 'turn_finished')], true)[0]?.foldable).toBe(false)
  })

  it('folds final-answer reasoning even without external process rows', () => {
    const answer = assistant(2, 'answer')
    if (answer.kind === 'assistant') answer.reasoning = {
      text: 'private reasoning', running: false, startedAt: 1, completedAt: 2,
    }
    const turn = buildChatTurns([user(0, 'question'), answer], [event(0, 'user_message'), answer.event, event(3, 'turn_finished')])[0]!
    expect(turn.foldable).toBe(true)
    expect(turn.inlineReasoning).toBe(true)
    expect(turn.processControlIndex).toBe(1)
  })

  it('counts only delegation tools as subagents', () => {
    const items = [user(0, 'question'), tool(1, 'spawn_agent'), tool(2, 'send_message'), assistant(3, 'answer')]
    const turn = buildChatTurns(items, [...items.map(item => item.event), event(4, 'turn_finished')])[0]!
    expect(turn.counts).toEqual({ messages: 0, tools: 1, subagents: 1 })
  })

  it('counts tool and subagent calls across the complete projected tree', () => {
    const root = tool(1, 'shell')
    if (root.kind === 'tool') root.trace.children = [
      {
        id: 'call-child', name: 'spawn_agent', arguments: {}, children: [],
        started: event(2, 'code_dispatch_started'), kind: 'code', parentCallId: root.trace.id,
      },
      {
        id: 'call-child-2', name: 'read_file', arguments: {}, children: [{
          id: 'call-grandchild', name: 'subagent_worker', arguments: {}, children: [],
          started: event(4, 'tool_call_started'), kind: 'tool', parentCallId: 'call-child-2',
        }],
        started: event(3, 'code_dispatch_started'), kind: 'code', parentCallId: root.trace.id,
      },
    ]
    const items = [user(0, 'question'), root, assistant(5, 'answer')]
    const turn = buildChatTurns(items, [...items.map(item => item.event), event(6, 'turn_finished')])[0]!
    expect(turn.counts).toEqual({ messages: 0, tools: 2, subagents: 2 })
  })

  it('aggregates exact per-turn usage only when every model response reports it', () => {
    const first = assistant(1, 'working', [{ id: 'call', name: 'read', arguments: {} }])
    const final = assistant(3, 'answer')
    if (first.kind === 'assistant' && first.event.response) first.event.response.usage = {
      input_tokens: 100, output_tokens: 20, cached_input_tokens: 60, reasoning_tokens: 5,
    }
    if (final.kind === 'assistant' && final.event.response) final.event.response.usage = {
      input_tokens: 80, output_tokens: 10, cached_input_tokens: 20, reasoning_tokens: 0,
    }
    const values = [user(0, 'question'), first, final]
    const complete = buildChatTurns(values, [...values.map(item => item.event), event(4, 'turn_finished')])[0]!
    expect(complete.usage).toEqual({
      uncachedInputTokens: 100,
      cacheReadTokens: 80,
      outputTokens: 30,
      reasoningTokens: 5,
      totalTokens: 210,
      routes: [{ provider: 'fixture', model: 'fixture-model' }],
    })

    if (final.kind === 'assistant' && final.event.response) final.event.response.usage = null
    expect(buildChatTurns(values, [...values.map(item => item.event), event(4, 'turn_finished')])[0]!.usage).toBeUndefined()
  })

  it('preserves provider/model routes and cache-write evidence without inventing absent buckets', () => {
    const final = assistant(2, 'answer')
    if (final.kind === 'assistant' && final.event.response) Object.assign(final.event.response, {
      provider: 'openai', model: 'gpt-test',
      usage: { input_tokens: 100, output_tokens: 12, cached_input_tokens: 40, cache_write_tokens: 10 },
    })
    const values = [user(1, 'question'), final]
    expect(buildChatTurns(values, [...values.map(item => item.event), event(3, 'turn_finished')])[0]?.usage).toEqual({
      uncachedInputTokens: 50,
      cacheReadTokens: 40,
      cacheWriteTokens: 10,
      outputTokens: 12,
      totalTokens: 112,
      routes: [{ provider: 'openai', model: 'gpt-test' }],
    })
  })

  it('derives completed-turn duration, TTFT, and decode throughput from canonical timestamps', () => {
    const promptEvent = event(1, 'user_message', { occurred_at_ms: 110, content: 'measure it' })
    const answerEvent = event(5, 'assistant_message', {
      occurred_at_ms: 1_500,
      step: 1,
      response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop',
        content: 'measured',
        tool_calls: [],
        usage: { input_tokens: 80, output_tokens: 20, cached_input_tokens: 0, reasoning_tokens: 0 },
      },
    })
    const items: ConversationItem[] = [
      { kind: 'user', key: 'event-1', event: promptEvent, content: 'measure it' },
      { kind: 'assistant', key: 'event-5', event: answerEvent, content: 'measured', streaming: false },
    ]
    const turn = buildChatTurns(items, [
      event(0, 'turn_started', { occurred_at_ms: 100 }),
      promptEvent,
      event(2, 'step_started', { occurred_at_ms: 200, step: 1 }),
      event(3, 'model_request_started', { occurred_at_ms: 350, step: 1, system_prompt: 'exact' }),
      event(4, 'assistant_message_delta', { occurred_at_ms: 500, step: 1, delta: 'm' }),
      answerEvent,
      event(6, 'turn_finished', { occurred_at_ms: 2_100 }),
    ])[0]!
    expect(turn.metrics).toEqual({ durationMs: 2_000, ttftMs: 150, tokensPerSecond: 20 })
  })

  it('hides each metric whose canonical boundary is absent', () => {
    const promptEvent = event(1, 'user_message', { occurred_at_ms: 110, content: 'measure it' })
    const requestEvent = event(2, 'model_request_started', { occurred_at_ms: 350, step: 1, system_prompt: 'exact' })
    const tokenEvent = event(3, 'assistant_message_delta', { occurred_at_ms: 500, step: 1, delta: 'm' })
    const answerEvent = event(4, 'assistant_message', {
      occurred_at_ms: 1_500,
      step: 1,
      response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop',
        content: 'measured',
        tool_calls: [],
        usage: { input_tokens: 80, output_tokens: 20, cached_input_tokens: 0, reasoning_tokens: 0 },
      },
    })
    const items: ConversationItem[] = [
      { kind: 'user', key: 'event-1', event: promptEvent, content: 'measure it' },
      { kind: 'assistant', key: 'event-4', event: answerEvent, content: 'measured', streaming: false },
    ]
    const terminal = event(5, 'turn_finished', { occurred_at_ms: 2_100 })

    expect(buildChatTurns(items, [promptEvent, requestEvent, tokenEvent, answerEvent, terminal])[0]!.metrics)
      .toEqual({ ttftMs: 150, tokensPerSecond: 20 })
    expect(buildChatTurns(items, [event(0, 'turn_started', { occurred_at_ms: 100 }), promptEvent, tokenEvent, answerEvent, terminal])[0]!.metrics)
      .toEqual({ durationMs: 2_000, tokensPerSecond: 20 })
    expect(buildChatTurns(items, [event(0, 'turn_started', { occurred_at_ms: 100 }), promptEvent, requestEvent, tokenEvent, answerEvent])[0]!.metrics)
      .toEqual({ ttftMs: 150, tokensPerSecond: 20 })
  })

  it('collects successful durable deliverables through the final answer and keeps the latest snapshot per path', () => {
    const answer = assistant(7, 'done')
    const items = [user(0, 'write files'), answer]
    const attachment = (name: string, content: string) => ({ name, media_type: 'text/plain', content })
    const turn = buildChatTurns(items, [
      event(0, 'turn_started'),
      items[0]!.event,
      event(2, 'deliverable_produced', { path: 'out/a.txt', operation: 'write', attachment: attachment('a.txt', 'first') }),
      event(3, 'deliverable_produced', { path: 'out/b.txt', operation: 'write', attachment: attachment('b.txt', 'second') }),
      event(4, 'deliverable_produced', { path: 'out/a.txt', operation: 'replace', attachment: attachment('a.txt', 'latest') }),
      answer.event,
      event(8, 'deliverable_produced', { path: 'late.txt', operation: 'write', attachment: attachment('late.txt', 'late') }),
      event(9, 'turn_finished'),
    ])[0]!
    expect(turn.deliverables.map(value => [value.path, value.operation, value.attachment.content])).toEqual([
      ['out/a.txt', 'replace', 'latest'],
      ['out/b.txt', 'write', 'second'],
    ])
  })
})
