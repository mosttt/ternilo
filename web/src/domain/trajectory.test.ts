import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { buildTrajectory } from './trajectory'
import { trajectoryPreviewText } from './trajectory-preview'

function event(seq: number, type: string, values: Partial<SessionEvent> = {}): SessionEvent {
  return { seq, type, run_id: 'run-1', occurred_at_ms: seq * 100, ...values }
}

describe('trajectory projection', () => {
  it('groups model streaming and tool completion without losing their raw events', () => {
    const turns = buildTrajectory([
      event(1, 'turn_started'),
      event(2, 'user_message', { content: 'hello' }),
      event(3, 'step_started', { step: 1 }),
      event(4, 'assistant_reasoning_delta', { step: 1, delta: 'plan ' }),
      event(5, 'assistant_reasoning_delta', { step: 1, delta: 'now' }),
      event(6, 'assistant_message_delta', { step: 1, delta: 'o' }),
      event(7, 'assistant_message_delta', { step: 1, delta: 'k' }),
      event(8, 'assistant_message', { step: 1, response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'ok',
        reasoning_content: 'plan now',
        usage: { input_tokens: 10, output_tokens: 2, cached_input_tokens: 4, cache_write_tokens: 1, reasoning_tokens: 5 },
      } }),
      event(9, 'tool_call_started', { call: { id: 'call-1', name: 'read', arguments: { path: 'README.md' } } }),
      event(10, 'tool_call_finished', { call_id: 'call-1', name: 'read', output: { content: 'done', is_error: false } }),
      event(11, 'turn_finished'),
    ])
    expect(turns).toHaveLength(1)
    const assistant = turns[0]?.records.find(record => record.kind === 'assistant')
    const tool = turns[0]?.records.find(record => record.kind === 'tool')
    expect(assistant).toMatchObject({
      inputTokens: 10, outputTokens: 2, reasoningTokens: 5, cachedTokens: 4, cacheWriteTokens: 1,
      provider: 'fixture', model: 'fixture-model', finishReason: 'stop',
      reasoningContent: 'plan now',
      timing: {
        firstTokenAt: 400, firstTokenDurationMs: 100,
        reasoningStartedAt: 400, reasoningCompletedAt: 600, reasoningDurationMs: 200,
      },
    })
    expect(assistant?.relatedEvents.map(item => item.seq)).toEqual([3, 4, 5, 6, 7, 8])
    expect(tool?.relatedEvents.map(item => item.seq)).toEqual([9, 10])
  })

  it.each(['turn_cancelled', 'turn_failed', 'turn_finished'])('ends incomplete trajectory timers on %s', type => {
    const records = buildTrajectory([
      event(1, 'step_started', { step: 1 }),
      event(2, 'assistant_reasoning_delta', { step: 1, delta: 'unfinished reasoning' }),
      event(6, type),
    ])[0]?.records
    expect(records?.find(record => record.kind === 'assistant')).toMatchObject({
      running: false,
      timing: { completedAt: 600, durationMs: 500, reasoningCompletedAt: 600, reasoningDurationMs: 400 },
    })
  })

  it('projects a reasoning-only stream as one running model record', () => {
    const turn = buildTrajectory([
      event(1, 'step_started', { step: 1 }),
      event(2, 'assistant_reasoning_delta', { step: 1, delta: 'first\n' }),
      event(3, 'assistant_reasoning_delta', { step: 1, delta: 'latest' }),
    ])[0]
    expect(turn?.records).toMatchObject([{
      kind: 'assistant', running: true, summary: 'first\nlatest', reasoningContent: 'first\nlatest',
      relatedEvents: [{ seq: 1 }, { seq: 2 }, { seq: 3 }],
      timing: { firstTokenAt: 200, reasoningStartedAt: 200 },
    }])
  })

  it('numbers model requests and measures the response from the exact request boundary', () => {
    const turn = buildTrajectory([
      event(1, 'step_started', { step: 1 }),
      event(2, 'model_request_started', { step: 1, system_prompt: 'exact prompt' }),
      event(3, 'assistant_reasoning_delta', { step: 1, delta: 'think' }),
      event(4, 'assistant_message', { step: 1, response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'done',
      } }),
    ])[0]
    const request = turn?.records.find(record => record.event.type === 'model_request_started')
    const assistant = turn?.records.find(record => record.kind === 'assistant')
    expect(request?.requestNumber).toBe(1)
    expect(assistant).toMatchObject({
      requestNumber: 1,
      relatedEvents: [{ seq: 1 }, { seq: 2 }, { seq: 3 }, { seq: 4 }],
      timing: { startedAt: 200, firstTokenAt: 300, firstTokenDurationMs: 100, durationMs: 200 },
    })
  })

  it('keeps unrecognized diagnostic events visible', () => {
    const turn = buildTrajectory([event(1, 'custom_diagnostic', { message: 'detail' })])[0]
    expect(turn?.records[0]).toMatchObject({ tag: 'EVENT', summary: 'detail' })
  })

  it('keeps typed reference provenance visible in the trajectory summary', () => {
    const turn = buildTrajectory([
      event(1, 'hook_context_added', {
        handler_id: 'reference:file',
        dialect: 'ternilo.reference.file.v1',
        content: 'opaque file context',
        reference: { kind: 'file', path: 'docs/guide.md', file_kind: 'file' },
      }),
      event(2, 'hook_context_added', {
        handler_id: 'reference:session',
        dialect: 'ternilo.reference.session.v1',
        content: 'opaque session context',
        reference: { kind: 'session', session_id: 'source', label: 'Reference source' },
      }),
    ])[0]
    expect(turn?.records.map(record => record.summary)).toEqual([
      'reference:file · ternilo.reference.file.v1 · docs/guide.md',
      'reference:session · ternilo.reference.session.v1 · Reference source',
    ])
  })

  it('retains a finished-only tool trace instead of degrading it to a generic event', () => {
    const turn = buildTrajectory([
      event(1, 'tool_call_finished', { call_id: 'late', name: 'fetch', output: { content: 'done', is_error: false } }),
    ])[0]
    expect(turn?.records[0]).toMatchObject({ kind: 'tool', title: 'fetch', depth: 0, trace: { id: 'late' } })
  })

  it('projects nested tool and code dispatch calls with semantic previews', () => {
    const turn = buildTrajectory([
      event(1, 'tool_call_started', { call: { id: 'root', name: 'shell', arguments: { command: 'cargo test' } } }),
      event(2, 'custom_diagnostic', { message: 'between calls' }),
      event(3, 'code_dispatch_started', { parent_call_id: 'root', call: { id: 'child', name: 'read_file', arguments: { path: 'src/lib.rs' } } }),
      event(4, 'tool_call_started', { parent_call_id: 'child', call: { id: 'grandchild', name: 'search', arguments: { pattern: 'unsafe', path: 'src' } } }),
      event(5, 'tool_call_finished', { call_id: 'grandchild', name: 'search', output: { content: '**2 matches**', is_error: false } }),
      event(6, 'code_dispatch_finished', { call_id: 'child', name: 'read_file', output: { content: '# Source\nfn main() {}', is_error: false } }),
      event(7, 'tool_call_finished', { call_id: 'root', name: 'shell', output: { content: 'ok', is_error: false } }),
    ])[0]
    const calls = turn?.records.filter(record => record.trace)
    expect(calls?.map(record => ({ title: record.title, parent: record.parentCallId, depth: record.depth }))).toEqual([
      { title: 'shell', parent: undefined, depth: 0 },
      { title: 'read_file', parent: 'root', depth: 1 },
      { title: 'search', parent: 'child', depth: 2 },
    ])
    expect(turn?.records.map(record => record.title)).toEqual(['shell', 'read_file', 'search', 'custom_diagnostic'])
    expect(calls?.[0]?.summary).toBe('command cargo test → result ok')
    expect(calls?.[1]?.summary).toContain('file src/lib.rs → result Source fn main() {}')
    expect(calls?.[2]?.summary).toBe('search unsafe · scope src → result 2 matches')
    expect(calls?.[1]?.trace?.arguments).toEqual({ path: 'src/lib.rs' })
    expect(calls?.[1]?.trace?.output?.content).toBe('# Source\nfn main() {}')
  })

  it('bounds Markdown previews independently from retained source data', () => {
    const long = `# title\n[link](https://example.com) ${'x'.repeat(2_100)}`
    const preview = trajectoryPreviewText(long)
    expect(preview.startsWith('title link ')).toBe(true)
    expect(preview.length).toBeLessThanOrEqual(513)
    expect(preview.endsWith('…')).toBe(true)
  })
})
