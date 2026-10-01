import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SessionEvent } from '@/types'
import { transcriptViewStorageKey } from '@/domain/transcript-view'
import type { DetailsSelection } from './details-panel'

vi.mock('./message-actions', () => ({
  AssistantMessageActions: () => null,
  PendingUserMessageActions: () => null,
  UserMessageActions: () => null,
}))

import { ChatView } from './chat-view'

let host: HTMLDivElement
let root: Root

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

function event(seq: number, type: string, runId = 'run-1', values: Partial<SessionEvent> = {}): SessionEvent {
  return { seq, type, run_id: runId, occurred_at_ms: seq * 100, ...values }
}

function completedTurn(start: number, runId: string, prompt: string, answer: string) {
  return [
    event(start, 'user_message', runId, { content: prompt }),
    event(start + 1, 'assistant_message', runId, { step: 1, response: {
      provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: answer, tool_calls: [],
    } }),
    event(start + 2, 'turn_finished', runId),
  ]
}

function processTurn(complete: boolean) {
  const events = [
    event(0, 'user_message', 'run-1', { content: 'fix it' }),
    event(1, 'assistant_message', 'run-1', { step: 1, response: {
      provider: 'fixture', model: 'fixture-model', finish_reason: 'tool_calls',
      content: 'checking', tool_calls: [{ id: 'call-1', name: 'read_file', arguments: { path: 'a.md' } }],
      usage: { input_tokens: 100, output_tokens: 20, cached_input_tokens: 60, reasoning_tokens: 5 },
    } }),
    event(2, 'tool_call_started', 'run-1', { call: { id: 'call-1', name: 'read_file', arguments: { path: 'a.md' } } }),
  ]
  if (!complete) return events
  return [
    ...events,
    event(3, 'tool_call_finished', 'run-1', { call_id: 'call-1', name: 'read_file', output: { content: 'ok', is_error: false } }),
    event(4, 'assistant_message', 'run-1', { step: 2, response: {
      provider: 'fixture', model: 'fixture-model', finish_reason: 'stop',
      content: 'done', tool_calls: [],
      usage: { input_tokens: 80, output_tokens: 10, cached_input_tokens: 20, reasoning_tokens: 0 },
    } }),
    event(5, 'turn_finished'),
  ]
}

function render(events: SessionEvent[], selection: DetailsSelection = null, onReaderNavigate: () => void = () => undefined) {
  act(() => root.render(<div className="conversation-column"><div className="conversation-scroll"><ChatView
    sessionId="session-1"
    events={events}
    pendingSubmissions={[]}
    projection={null}
    reloadMetadata={async () => undefined}
    selection={selection}
    onSelect={() => undefined}
    onReaderNavigate={onReaderNavigate}
  /></div></div>))
}

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const storage = memoryStorage()
  Object.defineProperty(window, 'localStorage', { configurable: true, value: storage })
  vi.stubGlobal('localStorage', storage)
  window.localStorage.clear()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  window.localStorage.clear()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('ChatView turn process and navigation', () => {
  it('mounts turn navigation outside the transcript scrollport after parent refs are attached', () => {
    render([
      ...completedTurn(0, 'run-1', 'first', 'answer'),
      ...completedTurn(3, 'run-2', 'second', 'answer'),
    ])
    const navigator = host.querySelector('[data-turn-navigator]')!
    expect(navigator.parentElement?.classList.contains('conversation-column')).toBe(true)
    expect(host.querySelector('.conversation-scroll')!.contains(navigator)).toBe(false)
  })

  it('renders typed reference source and retained/omitted completeness', () => {
    render([event(1, 'hook_context_added', 'run-1', {
      handler_id: 'reference:session',
      dialect: 'ternilo.reference.session.v1',
      content: 'opaque model context',
      reference: { kind: 'session', session_id: 'source', label: 'Release investigation' },
      completeness: { retained_items: 40, omitted_items: 7, truncated: true },
    })])
    expect(host.querySelector('[data-context-reference]')?.textContent).toBe('Release investigation')
    expect(host.querySelector('[data-context-completeness]')?.textContent).toContain('保留 40 · 省略 7 · 已截断')
  })

  it('renders an ordinary pending question with its prompt and options', () => {
    render([event(1, 'user_question_asked', 'run-1', { question: {
      id: 'question-1', question: '选择发布分支', options: [{ label: 'main' }, { label: 'release' }], multi_select: false,
      presentation: null, tool_approval: null,
    } })])
    const row = host.querySelector<HTMLElement>('[data-question-id="question-1"]')
    expect(row?.getAttribute('data-state')).toBe('pending')
    expect(row?.getAttribute('data-question-kind')).toBe('question')
    expect(row?.textContent).toContain('问题')
    expect(row?.textContent).toContain('等待回答')
    expect(row?.textContent).toContain('选择发布分支')
    expect(row?.textContent).toContain('main')
    expect(row?.textContent).toContain('release')
  })

  it('renders an answered tool approval from structured fields and removes the waiting state', () => {
    render([
      event(1, 'user_question_asked', 'run-1', { question: {
        id: 'approval-1', question: 'DO NOT USE BACKEND UI COPY', options: [{ label: 'Allow once' }, { label: 'Deny' }], multi_select: false,
        presentation: null,
        tool_approval: { tool_name: 'shell', call_id: 'call-1', reason: '运行聚焦测试', arguments: {} },
      } }),
      event(2, 'user_question_answered', 'run-1', { answer: { question_id: 'approval-1', selected: ['Allow once'] } }),
      event(3, 'turn_finished', 'run-1'),
    ])
    const row = host.querySelector<HTMLElement>('[data-question-id="approval-1"]')
    expect(row?.getAttribute('data-state')).toBe('answered')
    expect(row?.getAttribute('data-question-kind')).toBe('tool-approval')
    expect(row?.textContent).toContain('工具授权')
    expect(row?.textContent).toContain('是否允许 shell 执行一次？')
    expect(row?.textContent).toContain('原因：运行聚焦测试')
    expect(row?.textContent).toContain('回答允许一次')
    expect(row?.textContent).not.toContain('等待回答')
    expect(row?.textContent).not.toContain('DO NOT USE BACKEND UI COPY')
  })

  it('renders an interrupted plan review with a localized title and terminal summary', () => {
    render([
      event(1, 'user_question_asked', 'run-1', { question: {
        id: 'plan-1', question: 'DO NOT USE PLAN BACKEND COPY', options: [{ label: 'Approve' }, { label: 'Keep planning' }], multi_select: false,
        presentation: { kind: 'plan_review', title: '发布方案', plan: '# 发布方案', approve_label: 'Approve' },
        tool_approval: null,
      } }),
      event(2, 'turn_cancelled', 'run-1'),
    ])
    const row = host.querySelector<HTMLElement>('[data-question-id="plan-1"]')
    expect(row?.getAttribute('data-state')).toBe('interrupted')
    expect(row?.getAttribute('data-question-kind')).toBe('plan-review')
    expect(row?.textContent).toContain('计划评审')
    expect(row?.textContent).toContain('请审阅计划“发布方案”。')
    expect(row?.textContent).toContain('运行已取消，未提交回答。')
    expect(row?.textContent).not.toContain('DO NOT USE PLAN BACKEND COPY')
  })

  it('collapses only completed process rows and expands them from one disclosure', () => {
    render(processTurn(true))
    const control = host.querySelector<HTMLButtonElement>('[data-turn-process]')
    expect(control?.textContent).toContain('1 次工具调用')
    expect(control?.textContent).toContain('1 条消息')
    expect(control?.getAttribute('aria-expanded')).toBe('false')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(2)
    expect(host.querySelector('[data-turn-process-member]')?.getAttribute('hidden')).toBe('')
    expect(host.textContent).toContain('done')

    act(() => control?.click())
    expect(control?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(0)
    expect(host.textContent).toContain('checking')
  })

  it('does not inherit an old generation disclosure', () => {
    const first = processTurn(true)
    render(first)
    act(() => host.querySelector<HTMLButtonElement>('[data-turn-process]')?.click())
    expect(host.querySelector('[data-turn-process]')?.getAttribute('aria-expanded')).toBe('true')

    render([
      ...first,
      event(6, 'assistant_message', 'run-1', { step: 3, response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'tool_calls',
        content: 'checking again', tool_calls: [{ id: 'call-2', name: 'read_file', arguments: { path: 'b.md' } }],
      } }),
      event(7, 'tool_call_started', 'run-1', { call: { id: 'call-2', name: 'read_file', arguments: { path: 'b.md' } } }),
      event(8, 'tool_call_finished', 'run-1', { call_id: 'call-2', name: 'read_file', output: { content: 'ok', is_error: false } }),
      event(9, 'assistant_message', 'run-1', { step: 4, response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'new answer', tool_calls: [],
      } }),
      event(10, 'turn_finished', 'run-1'),
    ])
    const control = host.querySelector('[data-turn-process]')
    expect(control?.getAttribute('aria-expanded')).toBe('false')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).not.toHaveLength(0)
    act(() => (control as HTMLButtonElement).click())
    expect(control?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(0)
  })

  it('reveals a compact process when Trajectory selects one of its tool calls', () => {
    const events = processTurn(true)
    render(events, { kind: 'tool', trace: {
      id: 'call-1', name: 'read_file', arguments: { path: 'a.md' }, children: [], kind: 'tool',
      started: events[2]!, finished: events[3]!, output: { content: 'ok', is_error: false }, durationMs: 100,
    } })
    expect(host.querySelector('[data-turn-process]')?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(0)
    expect(host.querySelector('[data-tool-call-id="call-1"]')?.getAttribute('data-selected')).toBe('true')
  })

  it('keeps a focused live process row open when the turn settles', () => {
    render(processTurn(false))
    const tool = host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!
    act(() => tool.focus())
    expect(document.activeElement).toBe(tool)

    render(processTurn(true))
    const control = host.querySelector<HTMLButtonElement>('[data-turn-process]')
    expect(control?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(0)
    expect(document.activeElement).toBe(tool)
  })

  it('preserves expanded reasoning when background history completes a foldable turn', () => {
    const recent = [
      event(3, 'assistant_reasoning_delta', 'run-1', { step: 1, delta: 'visible thought' }),
      event(4, 'assistant_message_delta', 'run-1', { step: 1, delta: 'answer' }),
      event(5, 'turn_failed', 'run-1', { message: 'interrupted' }),
    ]
    render(recent)
    act(() => host.querySelector<HTMLButtonElement>('[data-reasoning-row] [data-disclosure-row]')!.click())
    expect(host.querySelector('[data-reasoning-body]')?.textContent).toBe('visible thought')
    render([
      event(0, 'turn_started'),
      event(1, 'user_message', 'run-1', { content: 'question' }),
      event(2, 'step_started', 'run-1', { step: 1 }),
      ...recent,
    ])
    expect(host.querySelector('[data-turn-process]')?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelector('[data-reasoning-body]')?.textContent).toBe('visible thought')
    expect(host.querySelector('[data-turn-inline-reasoning]')?.hasAttribute('hidden')).toBe(false)
  })

  it('honors the persisted Normal transcript mode', () => {
    window.localStorage.setItem(transcriptViewStorageKey, 'normal')
    render(processTurn(true))
    expect(host.querySelector('[data-turn-process]')).toBeNull()
    expect(host.querySelectorAll('[data-turn-process-member][hidden]')).toHaveLength(0)
    expect(host.textContent).toContain('checking')
  })

  it('shows exact fail-closed usage as an expandable completed-turn footer', () => {
    render(processTurn(true))
    const disclosure = host.querySelector<HTMLDetailsElement>('[data-turn-usage]')
    expect(disclosure?.textContent).toContain('210 tok')
    expect(disclosure?.textContent).toContain('缓存命中率 44.4%')
    expect(disclosure?.querySelector('[data-turn-usage-details]')?.textContent).toContain('未缓存输入100 tok')
  })

  it('renders one keyboard-addressable mark per loaded turn', () => {
    render([
      ...completedTurn(0, 'run-1', 'first prompt', 'first answer'),
      ...completedTurn(3, 'run-2', 'second prompt', 'second answer'),
    ])
    expect(host.querySelectorAll('[data-turn-navigator-mark]')).toHaveLength(2)
    expect(host.querySelector('[aria-label="跳转到第 1 轮"]')).not.toBeNull()
    expect(host.querySelector('[aria-label="跳转到第 2 轮"]')).not.toBeNull()
  })

  it('reports reader navigation before moving the transcript to the selected turn', () => {
    const positions: number[] = []
    render([
      ...completedTurn(0, 'run-1', 'first prompt', 'first answer'),
      ...completedTurn(3, 'run-2', 'second prompt', 'second answer'),
    ], null, () => positions.push(scrollport.scrollTop))
    const scrollport = host.querySelector<HTMLElement>('.conversation-scroll')!
    scrollport.scrollTop = 900
    scrollport.getBoundingClientRect = () => ({
      top: 0, bottom: 600, left: 0, right: 900, width: 900, height: 600, x: 0, y: 0, toJSON() {},
    })
    const firstTurn = host.querySelector<HTMLElement>('[data-chat-run-id="run-1"]')!
    firstTurn.getBoundingClientRect = () => ({
      top: -300, bottom: -220, left: 0, right: 900, width: 900, height: 80, x: 0, y: -300, toJSON() {},
    })

    act(() => host.querySelector<HTMLButtonElement>('[aria-label="跳转到第 1 轮"]')?.click())

    expect(positions).toEqual([900])
    expect(scrollport.scrollTop).toBe(576)
    expect(host.querySelector('[aria-label="跳转到第 1 轮"]')?.getAttribute('aria-current')).toBe('true')
  })

  it('discloses one adapted context-injection row with exact model-visible prompt text', () => {
    const prompt = 'opaque # heading\n\n<strong>not markup</strong>\n  exact spacing'
    render([
      event(0, 'user_message', 'run-1', { content: 'inspect prompt' }),
      event(1, 'model_request_started', 'run-1', { step: 1, system_prompt: prompt }),
      event(2, 'assistant_message', 'run-1', { step: 1, response: {
        provider: 'fixture', model: 'fixture-model', finish_reason: 'stop', content: 'done', tool_calls: [],
      } }),
      event(3, 'turn_finished', 'run-1'),
    ])
    const trigger = host.querySelector<HTMLButtonElement>('[data-system-prompt-row] [data-disclosure-row]')
    expect(trigger?.textContent).toContain('系统提示词')
    expect(trigger?.textContent).not.toContain('第 1 步')
    expect(trigger?.getAttribute('aria-expanded')).toBe('false')
    expect(host.querySelector('[data-system-prompt-body]')).toBeNull()

    act(() => trigger?.click())
    const body = host.querySelector<HTMLElement>('[data-system-prompt-body]')
    expect(trigger?.getAttribute('aria-expanded')).toBe('true')
    expect(body?.textContent).toBe(prompt)
    expect(body?.querySelector('strong')).toBeNull()
    expect(host.querySelector('[data-turn-process]')).toBeNull()
  })

  it('keeps one neutral turn-level thinking status for an open run', () => {
    render([
      event(0, 'turn_started', 'run-1'),
      event(1, 'user_message', 'run-1', { content: 'hello' }),
      event(2, 'model_request_started', 'run-1', { step: 1, system_prompt: 'prompt' }),
    ])
    expect(host.querySelector('[role="status"]')?.textContent).toContain('正在深入思考…')
    render([
      event(0, 'turn_started', 'run-1'),
      event(1, 'user_message', 'run-1', { content: 'hello' }),
      event(2, 'turn_finished', 'run-1'),
    ])
    expect(host.textContent).not.toContain('正在深入思考…')
  })

  it('shows one static directory wait with the submitted message and resumes after acquisition', () => {
    const waiting = [
      event(0, 'turn_started'),
      event(1, 'user_message', 'run-1', { content: 'Update the shared directory', provenance: { input_id: 'submission-1', author: { kind: 'account', user_id: 'teammate-id', username: 'teammate' } } }),
      event(2, 'workspace_execution_waiting'),
    ]
    render(waiting)
    expect(host.querySelector('[data-workspace-waiting]')?.textContent).toBe('等待目录空闲')
    expect(host.querySelectorAll('[role="status"]')).toHaveLength(1)
    expect(host.querySelector('[data-workspace-waiting] .lucide-hourglass')).not.toBeNull()
    expect(host.textContent).not.toContain('正在深入思考')
    expect(host.querySelector('article[data-role="user"]')?.textContent).toContain('Update the shared directory')
    expect(host.querySelector('[data-input-identity-label]')?.textContent).toBe('teammate')
    render([...waiting, event(3, 'workspace_execution_acquired')])
    expect(host.querySelector('[data-workspace-waiting]')).toBeNull()
    expect(host.querySelector('[role="status"]')?.textContent).toContain('正在深入思考')
  })

  it.each([
    ['waiting_for_subagents', '等待子任务完成'],
    ['waiting_for_capacity', '等待继续执行'],
  ])('shows one static %s status and retains the existing transcript', (phase, label) => {
    const history = [event(0, 'turn_started'), event(1, 'user_message', 'run-1', { content: 'Keep working' }),
      event(2, 'assistant_message', 'run-1', { step: 1, response: { provider: 'fixture', model: 'fixture-model', finish_reason: 'tool_calls', content: 'Existing output', tool_calls: [] } }),
      event(3, 'execution_activity_changed', 'run-1', { phase })]
    render(history)
    expect(host.querySelectorAll('[role="status"]')).toHaveLength(1)
    const status = host.querySelector(`[data-execution-phase="${phase}"]`)!
    expect(status.textContent).toBe(label)
    expect(status.querySelector('.lucide-hourglass')).not.toBeNull()
    expect(host.textContent).not.toContain('正在深入思考')
    expect(host.textContent).toContain('Existing output')
    render([...history, event(4, 'execution_activity_changed', 'run-1', { phase: 'running' })])
    expect(host.querySelector('[data-execution-phase="running"]')?.textContent).toContain('正在深入思考')
    render([...history, event(4, 'turn_cancelled')])
    expect(host.querySelector('[data-execution-phase]')).toBeNull()
  })

  it.each(['turn_cancelled', 'turn_failed', 'turn_finished'])('removes the directory wait after %s', type => {
    render([event(0, 'turn_started'), event(1, 'user_message', 'run-1', { content: 'Waiting task' }), event(2, 'workspace_execution_waiting'), event(3, type)])
    expect(host.querySelector('[data-workspace-waiting]')).toBeNull()
    expect(host.textContent).not.toContain('等待目录空闲')
    expect(host.textContent).not.toContain('正在深入思考')
  })

  it('keeps completed streaming code DOM through canonical delta projection updates', () => {
    const first = [
      event(0, 'turn_started', 'run-1'),
      event(1, 'user_message', 'run-1', { content: 'stream code' }),
      event(2, 'assistant_message_delta', 'run-1', {
        step: 1,
        delta: '```ts\nconst stable = 1\nlet partial',
      }),
    ]
    render(first)
    const stableLine = host.querySelector<HTMLElement>('[data-streaming-line="0"]')!
    expect(stableLine.textContent).toBe('const stable = 1')
    render([...first, event(3, 'assistant_message_delta', 'run-1', {
      step: 1,
      delta: '\n// streamed tail',
    })])
    expect(host.querySelector('[data-streaming-line="0"]')).toBe(stableLine)
  })

  it('pages a bounded long transcript in both directions and preserves the visible semantic anchor', () => {
    // This case exercises windowing and anchor geometry, not Markdown. Keeping
    // assistant bodies empty avoids spending the test budget sanitizing 330
    // unrelated documents while preserving the exact turn/event topology.
    const events = Array.from({ length: 330 }, (_, index) => completedTurn(index * 3, `run-${index + 1}`, `prompt ${index + 1}`, '')).flat()
    render(events)
    const scrollport = host.querySelector<HTMLElement>('.conversation-scroll')!
    scrollport.setAttribute('data-conversation-scroll', '')
    Object.defineProperties(scrollport, {
      clientHeight: { configurable: true, value: 600 },
      scrollHeight: { configurable: true, get: () => host.querySelectorAll('[data-chat-turn]').length * 100 },
    })
    scrollport.getBoundingClientRect = () => ({ top: 0, bottom: 600, left: 0, right: 900, width: 900, height: 600, x: 0, y: 0, toJSON() {} })
    const geometry = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this === scrollport) return { top: 0, bottom: 600, left: 0, right: 900, width: 900, height: 600, x: 0, y: 0, toJSON() {} }
      if (this instanceof HTMLElement && this.hasAttribute('data-chat-turn')) {
        const firstTurn = host.querySelector<HTMLElement>('[data-chat-turn]')
        const row = Number(this.getAttribute('data-chat-turn'))
        const firstRow = Number(firstTurn?.getAttribute('data-chat-turn'))
        const top = (row - firstRow) * 100 - scrollport.scrollTop
        return { top, bottom: top + 80, left: 0, right: 900, width: 900, height: 80, x: 0, y: top, toJSON() {} }
      }
      return { top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0, x: 0, y: 0, toJSON() {} }
    })

    expect(host.querySelectorAll('[data-chat-turn]')).toHaveLength(160)
    expect(host.querySelector('[data-chat-turn]')?.getAttribute('data-chat-turn')).toBe('171')
    const initialMarks = [...host.querySelectorAll<HTMLElement>('[data-turn-navigator-mark]')]
    expect(initialMarks[0]?.dataset.turnPosition).toBe('0')
    expect(initialMarks.at(-1)?.dataset.turnPosition).toBe('100')
    act(() => (host.querySelector('button') as HTMLButtonElement).click())
    expect(host.querySelectorAll('[data-chat-turn]')).toHaveLength(320)
    expect(host.querySelector('[data-chat-turn]')?.getAttribute('data-chat-turn')).toBe('11')
    expect(scrollport.scrollTop).toBe(16_000)

    const older = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '加载更早')!
    act(() => older.click())
    expect(host.querySelectorAll('[data-chat-turn]')).toHaveLength(320)
    expect(host.querySelector('[data-chat-turn]')?.getAttribute('data-chat-turn')).toBe('1')
    expect(host.textContent).toContain('加载更新内容')
    const newer = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '加载更新内容')!
    act(() => newer.click())
    expect(host.querySelectorAll('[data-chat-turn]')).toHaveLength(320)
    expect(host.querySelector('[data-chat-turn]')?.getAttribute('data-chat-turn')).toBe('11')
    geometry.mockRestore()
  }, 20_000)
})


it('renders the recorded author on durable human messages without exposing their ID inline', () => {
  render([event(0, 'user_message', 'shared-run', {
    content: 'shared task', provenance: { input_id: 'accepted-input', author: { kind: 'account', user_id: 'sender-id', username: 'teammate' } },
  })])
  const message = host.querySelector('article[data-role="user"]')!
  expect(message.querySelector('[data-input-identity-label]')?.textContent).toBe('teammate')
  expect(message.textContent).not.toContain('sender-id')
})
