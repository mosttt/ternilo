import { describe, expect, it } from 'vitest'
import { buildConversationItems, buildToolTraces, compactTrajectoryEvents, completedAssistantTailSeqs, eventSummary } from './events'
import type { SessionEvent } from '@/types'

function event(seq: number, type: string, values: Partial<SessionEvent> = {}): SessionEvent {
  return { seq, type, run_id: 'run-1', occurred_at_ms: seq * 100, ...values }
}

describe('conversation event projection', () => {
  it.each(['turn_cancelled', 'turn_failed', 'turn_finished'])('freezes reasoning-only streams on %s', type => {
    const history = [event(0, 'turn_started'), event(1, 'step_started', { step: 1 }),
      event(2, 'assistant_reasoning_delta', { step: 1, delta: 'unfinished reasoning' }), event(6, type)]
    expect(buildConversationItems(history).filter(item => item.kind === 'assistant')).toMatchObject([{
      streaming: false,
      reasoning: { text: 'unfinished reasoning', running: false, startedAt: 200, completedAt: 600, durationMs: 400 },
    }])
  })
  it('projects an ordinary unanswered question as one pending lifecycle row', () => {
    const items = buildConversationItems([
      event(1, 'user_question_asked', { question: {
        id: 'question-1', question: 'Which branch?', options: [{ label: 'main' }, { label: 'release' }], multi_select: false,
        presentation: null, tool_approval: null,
      } }),
    ])
    expect(items.filter(item => item.kind === 'question')).toMatchObject([{
      kind: 'question', key: 'question-question-1', event: { seq: 1 },
      lifecycle: {
        id: 'question-1', kind: 'question', state: 'pending',
        question: { question: 'Which branch?', options: [{ label: 'main' }, { label: 'release' }] },
      },
    }])
  })

  it('pairs an answer by question id and does not leave a waiting card behind', () => {
    const items = buildConversationItems([
      event(1, 'user_question_asked', { question: {
        id: 'approval-1', question: 'backend approval copy', options: [{ label: 'Allow once' }, { label: 'Deny' }], multi_select: false,
        presentation: null,
        tool_approval: { tool_name: 'shell', call_id: 'call-1', reason: 'run tests', arguments: {} },
      } }),
      event(2, 'user_question_answered', { answer: { question_id: 'approval-1', selected: ['Allow once'] } }),
      event(3, 'turn_finished'),
    ])
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({
      kind: 'question', event: { seq: 1 },
      lifecycle: {
        id: 'approval-1', kind: 'tool-approval', state: 'answered', answer: { selected: ['Allow once'] },
        answered: { seq: 2, type: 'user_question_answered' },
      },
    })
  })

  it.each([
    ['turn_cancelled', 'run-1'],
    ['turn_failed', 'run-1'],
    ['turn_finished', 'run-1'],
  ])('settles an unanswered question as interrupted on %s from the same run', (terminal, runId) => {
    const items = buildConversationItems([
      event(1, 'user_question_asked', { question: {
        id: 'question-1', question: 'Continue?', options: [{ label: 'yes' }, { label: 'no' }], multi_select: false,
        presentation: null, tool_approval: null,
      } }),
      event(2, terminal, { run_id: runId }),
    ])
    expect(items.filter(item => item.kind === 'question')).toMatchObject([{
      kind: 'question',
      lifecycle: { state: 'interrupted', terminal: { type: terminal, run_id: runId } },
    }])
  })

  it('does not settle a question from another run terminal', () => {
    const items = buildConversationItems([
      event(1, 'user_question_asked', { question: {
        id: 'question-1', question: 'Continue?', options: [], multi_select: false,
        presentation: null, tool_approval: null,
      } }),
      event(2, 'turn_cancelled', { run_id: 'run-2' }),
    ])
    expect(items.filter(item => item.kind === 'question')).toMatchObject([{ kind: 'question', lifecycle: { state: 'pending' } }])
  })

  it('classifies a structured plan review without relying on its backend prompt language', () => {
    const items = buildConversationItems([
      event(1, 'user_question_asked', { question: {
        id: 'plan-1', question: 'opaque backend copy', options: [{ label: 'Approve' }, { label: 'Keep planning' }], multi_select: false,
        presentation: { kind: 'plan_review', title: 'Release', plan: '# Release', approve_label: 'Approve' },
        tool_approval: null,
      } }),
    ])
    expect(items).toMatchObject([{ kind: 'question', lifecycle: { kind: 'plan-review', state: 'pending' } }])
  })

  it('projects optional retry, command, compaction and max-token contracts only from durable evidence', () => {
    const items = buildConversationItems([
      event(1, 'model_retry_scheduled', { retry_id: 'retry-1', retry: 1, max_retries: 3, delay_ms: 1_000, failure: { message: 'busy' } }),
      event(2, 'model_retry_started', { retry_id: 'retry-1', retry: 1 }),
      event(3, 'command_started', { command_id: 'command-1', command_name: 'compact', arguments: { automatic: false } }),
      event(4, 'command_finished', { command_id: 'command-1', outcome: { kind: 'success', text: 'done' } }),
      event(5, 'context_compaction_started', { compaction_id: 'compact-1', automatic: true, turn: 2 }),
      event(6, 'turn_finished', { finish_reason: 'max_tokens' }),
    ])
    expect(items).toMatchObject([
      { kind: 'retry', lifecycle: { id: 'retry-1', state: 'started' } },
      { kind: 'command', lifecycle: { id: 'command-1', name: 'compact', outcome: { kind: 'success', text: 'done' } } },
      { kind: 'compaction', running: true, automatic: true },
      { kind: 'max_tokens' },
    ])
  })

  it('pairs a completed compaction by its stable identity', () => {
    const items = buildConversationItems([
      event(1, 'context_compaction_started', {
        compaction_id: 'compact-1', automatic: false, source_command_id: 'direct-run-1', turn: 3,
      }),
      event(2, 'context_compacted', {
        compaction_id: 'compact-1',
        compaction: { through_seq: 8, summary: 'durable summary', estimated_tokens_before: 4_096, automatic: false },
      }),
    ])
    expect(items).toMatchObject([{
      kind: 'compaction', key: 'compaction-compact-1', running: false, automatic: false,
      summary: 'durable summary', shadowedTokens: 4_096,
    }])
  })

  it('keeps failed streaming output as a stopped partial answer', () => {
    const items = buildConversationItems([
      event(1, 'assistant_message_delta', { step: 1, delta: 'partial' }),
      event(2, 'turn_failed', { message: 'provider disconnected' }),
    ])
    expect(items).toMatchObject([
      { kind: 'assistant', content: 'partial', streaming: false, interrupted: true },
      { kind: 'card', event: { type: 'turn_failed' } },
    ])
  })

  it('wraps hook context exactly as model-visible content', () => {
    expect(buildConversationItems([event(1, 'hook_context_added', {
      handler_id: 'memory', dialect: 'xml', content: 'exact\n  spacing',
    })])).toMatchObject([{ kind: 'context', source: 'memory', dialect: 'xml', content: '<hook_context handler="memory" dialect="xml">\nexact\n  spacing\n</hook_context>' }])
  })

  it('projects typed reference provenance and completeness without parsing injected content', () => {
    const context = event(1, 'hook_context_added', {
      handler_id: 'reference:session',
      dialect: 'ternilo.reference.session.v1',
      content: 'opaque model context',
      reference: { kind: 'session', session_id: 'source', label: 'Release investigation' },
      completeness: { retained_items: 40, omitted_items: 7, truncated: true },
    })
    expect(buildConversationItems([context])).toMatchObject([{
      kind: 'context',
      referenceLabel: 'Release investigation',
      completeness: { retained_items: 40, omitted_items: 7, truncated: true },
    }])
    expect(eventSummary(context)).toBe('reference:session · ternilo.reference.session.v1 · Release investigation')
  })

  it('combines tool start and finish into one selectable row', () => {
    const events = [
      event(1, 'tool_call_started', { call: { id: 'call-1', name: 'read', arguments: { path: 'a.md' } } }),
      event(2, 'tool_call_finished', { call_id: 'call-1', name: 'read', output: { content: 'hello', is_error: false } }),
    ]
    const traces = buildToolTraces(events)
    expect(traces.get('call-1')).toMatchObject({ name: 'read', children: [], durationMs: 100, output: { content: 'hello', is_error: false } })
    expect(buildConversationItems(events).filter(item => item.kind === 'tool')).toHaveLength(1)
  })

  it('projects one root with two code children as one conversation item', () => {
    const events = [
      event(1, 'tool_call_started', { call: { id: 'root', name: 'code', arguments: { source: 'run()' } } }),
      event(2, 'code_dispatch_started', { parent_call_id: 'root', call: { id: 'read', name: 'read_file', arguments: { path: 'src/lib.rs' } } }),
      event(3, 'code_dispatch_finished', {
        call_id: 'read', name: 'read_file',
        output: { content: 'source', is_error: false },
        retained_output: { name: 'source.txt', media_type: 'text/plain', content: 'complete source' },
      }),
      event(4, 'code_dispatch_started', { parent_call_id: 'root', call: { id: 'search', name: 'search', arguments: { pattern: 'unsafe' } } }),
      event(5, 'code_dispatch_finished', { call_id: 'search', name: 'search', output: { content: 'none', is_error: false } }),
      event(6, 'tool_call_finished', { call_id: 'root', name: 'code', output: { content: 'done', is_error: false } }),
    ]
    const root = buildToolTraces(events).get('root')!
    expect(root.children.map(child => child.id)).toEqual(['read', 'search'])
    expect(root.children[0]).toMatchObject({
      parentCallId: 'root', durationMs: 100,
      output: { content: 'source', is_error: false },
      retainedOutput: { name: 'source.txt', media_type: 'text/plain', content: 'complete source' },
    })
    const tools = buildConversationItems(events).filter(item => item.kind === 'tool')
    expect(tools).toHaveLength(1)
    expect(tools[0]).toMatchObject({ key: 'tool-root', trace: { id: 'root', children: [{ id: 'read' }, { id: 'search' }] } })
  })

  it('retains nested descendants without rendering them twice', () => {
    const events = [
      event(1, 'tool_call_started', { call: { id: 'root', name: 'shell', arguments: {} } }),
      event(2, 'code_dispatch_started', { parent_call_id: 'root', call: { id: 'child', name: 'code', arguments: {} } }),
      event(3, 'tool_call_started', { parent_call_id: 'child', call: { id: 'grandchild', name: 'read', arguments: {} } }),
      event(4, 'tool_call_finished', { call_id: 'grandchild', name: 'read', output: { content: 'read', is_error: false } }),
      event(5, 'code_dispatch_finished', { call_id: 'child', name: 'code', output: { content: 'code', is_error: false } }),
      event(6, 'tool_call_finished', { call_id: 'root', name: 'shell', output: { content: 'shell', is_error: false } }),
    ]
    const tools = buildConversationItems(events).filter(item => item.kind === 'tool')
    expect(tools).toHaveLength(1)
    expect(tools[0]).toMatchObject({ trace: {
      id: 'root', children: [{ id: 'child', children: [{ id: 'grandchild', children: [] }] }],
    } })
  })

  it('keeps a child whose parent is absent as an orphan root', () => {
    const events = [
      event(1, 'code_dispatch_started', { parent_call_id: 'missing', call: { id: 'orphan', name: 'read', arguments: {} } }),
      event(2, 'code_dispatch_finished', { call_id: 'orphan', name: 'read', output: { content: 'done', is_error: false } }),
    ]
    expect(buildConversationItems(events).filter(item => item.kind === 'tool')).toMatchObject([{
      key: 'tool-orphan', trace: { id: 'orphan', parentCallId: 'missing', children: [], durationMs: 100 },
    }])
  })

  it('projects a running child and then pairs its completion without losing output', () => {
    const started = [
      event(1, 'tool_call_started', { call: { id: 'root', name: 'code', arguments: {} } }),
      event(2, 'code_dispatch_started', { parent_call_id: 'root', call: { id: 'child', name: 'shell', arguments: { command: 'test' } } }),
    ]
    expect(buildToolTraces(started).get('root')?.children[0]).toMatchObject({ id: 'child', finished: undefined, children: [] })

    const finished = [...started, event(5, 'code_dispatch_finished', {
      call_id: 'child', name: 'shell', output: { content: 'passed', is_error: false },
      retained_output: { name: 'test.log', media_type: 'text/plain', content: 'complete log' },
    })]
    expect(buildToolTraces(finished).get('root')?.children[0]).toMatchObject({
      id: 'child', durationMs: 300, output: { content: 'passed', is_error: false },
      retainedOutput: { name: 'test.log', media_type: 'text/plain', content: 'complete log' },
    })
    expect(buildConversationItems(finished).filter(item => item.kind === 'tool')).toHaveLength(1)
  })

  it('shows only the latest streaming aggregate and removes it after final response', () => {
    const partial = [
      event(1, 'assistant_message_delta', { step: 1, delta: '你' }),
      event(2, 'assistant_message_delta', { step: 1, delta: '好' }),
    ]
    expect(buildConversationItems(partial)).toMatchObject([{ kind: 'assistant', content: '你好', streaming: true }])
    const complete = [...partial, event(3, 'assistant_message', { step: 1, response: {
      provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: '你好',
    } })]
    expect(buildConversationItems(complete).filter(item => item.kind === 'assistant')).toMatchObject([{ content: '你好', streaming: false }])
  })

  it('shows an exact system prompt only initially or when its text changes', () => {
    const prompts = buildConversationItems([
      event(1, 'model_request_started', { step: 1, system_prompt: 'same prompt' }),
      event(2, 'model_request_started', { step: 2, system_prompt: 'same prompt' }),
      event(3, 'model_request_started', { run_id: 'run-2', step: 1, system_prompt: 'same prompt' }),
      event(4, 'model_request_started', { run_id: 'run-2', step: 2, system_prompt: 'changed prompt' }),
      event(5, 'model_request_started', { run_id: 'run-2', step: 3, system_prompt: '' }),
    ]).filter(item => item.kind === 'system_prompt')
    expect(prompts).toMatchObject([
      { content: 'same prompt', event: { seq: 1 } },
      { content: 'changed prompt', event: { seq: 4 } },
    ])
  })

  it('keeps reasoning separate through empty, streaming and final events', () => {
    const streaming = [
      event(1, 'step_started', { step: 1 }),
      event(2, 'assistant_reasoning_delta', { step: 1 }),
      event(3, 'assistant_reasoning_delta', { step: 1, delta: '先分析\n' }),
      event(4, 'assistant_reasoning_delta', { step: 1, delta: '再回答' }),
    ]
    expect(buildConversationItems(streaming).filter(item => item.kind === 'assistant')).toMatchObject([{
      content: '',
      streaming: true,
      reasoning: { text: '先分析\n再回答', running: true, startedAt: 300 },
    }])

    const answering = [...streaming, event(5, 'assistant_message_delta', { step: 1, delta: '答案' })]
    expect(buildConversationItems(answering).filter(item => item.kind === 'assistant')).toMatchObject([{
      content: '答案',
      reasoning: { text: '先分析\n再回答', running: false, completedAt: 500, durationMs: 200 },
    }])

    const complete = [...answering, event(6, 'assistant_message', {
      step: 1,
      response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop',
        content: '答案', reasoning_content: '最终推理', usage: { input_tokens: 10, output_tokens: 2, reasoning_tokens: 4 },
      },
    })]
    expect(buildConversationItems(complete).filter(item => item.kind === 'assistant')).toMatchObject([{
      content: '答案',
      streaming: false,
      reasoning: { text: '最终推理', running: false, durationMs: 200 },
    }])
  })

  it('renders a final reasoning-only response even without stream timing', () => {
    const items = buildConversationItems([
      event(1, 'assistant_message', { response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: '', reasoning_content: 'private summary',
      } }),
    ])
    expect(items).toMatchObject([{
      kind: 'assistant', content: '', streaming: false,
      reasoning: { text: 'private summary', running: false, startedAt: 100, completedAt: 100, durationMs: 0 },
    }])
  })

  it('compacts tool completion and streaming noise in trajectory', () => {
    const events = [
      event(1, 'assistant_reasoning_delta', { step: 1, delta: 'think' }),
      event(2, 'assistant_message_delta', { step: 1, delta: 'a' }),
      event(3, 'assistant_message_delta', { step: 1, delta: 'b' }),
      event(4, 'tool_call_started', { call: { id: 'call-1', name: 'read', arguments: {} } }),
      event(5, 'tool_call_finished', { call_id: 'call-1', name: 'read', output: { content: 'ok', is_error: false } }),
    ]
    expect(compactTrajectoryEvents(events).map(item => item.seq)).toEqual([3, 4])
  })

  it('exposes actions only on the last assistant message of a settled turn', () => {
    const events = [
      event(0, 'turn_started'),
      event(1, 'assistant_message', { response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'tool_calls', content: 'tool preface',
      } }),
      event(2, 'tool_call_started', { call: { id: 'call-1', name: 'read', arguments: {} } }),
      event(3, 'tool_call_finished', { call_id: 'call-1', name: 'read', output: { content: 'ok', is_error: false } }),
      event(4, 'assistant_message', { response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'final',
      } }),
      event(5, 'turn_finished', { answer: 'final' }),
      event(6, 'turn_started', { run_id: 'run-2' }),
      event(7, 'assistant_message', { run_id: 'run-2', response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'still running',
      } }),
    ]
    expect([...completedAssistantTailSeqs(events)]).toEqual([4])
  })
})
