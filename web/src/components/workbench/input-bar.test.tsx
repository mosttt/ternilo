import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import type { InputBarProps } from './input-bar'
import { InputBar } from './input-bar'
import { BUSY_ENTER_STORAGE_KEY } from '@/domain/composer-preference'
import { ApiError } from '@/api/client'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

let host: HTMLDivElement
let root: Root
let fetchMock: ReturnType<typeof vi.spyOn>

const runtimeCommands = {
  session_id: 'session-one',
  commands: [
    { name: 'goal', description: 'Update the Session goal', input: { hint: '<objective>', images: false } },
    { name: 'todo', description: 'Replace the task list', input: { hint: '<step; step>', images: false } },
    { name: 'shell!', description: 'Run with full host access', input: { hint: '<command>', images: false } },
    { name: 'compact', description: 'Compact earlier context' },
    { name: 'skills', description: 'List available Skills' },
  ],
}

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, String(value)) },
  }
}

beforeEach(() => {
  vi.stubGlobal('matchMedia', () => ({ matches: false }))
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  Object.defineProperty(window, 'localStorage', { configurable: true, value: memoryStorage() })
  fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async input => {
    const url = String(input)
    const value = url.includes('/commands')
      ? runtimeCommands
      : url.includes('/skills')
        ? { revision: 0, complete: true, skills: [] }
        : { directory: '', candidates: [] }
    return new Response(JSON.stringify(value), { status: 200, headers: { 'content-type': 'application/json' } })
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  fetchMock.mockRestore()
  vi.unstubAllGlobals()
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function props(overrides: Partial<InputBarProps> = {}): InputBarProps {
  return {
    sessionId: 'session-one',
    busy: false,
    questions: [],
    projection: null,
    events: [],
    model: { provider: 'profile_default' },
    referenceCandidates: [],
    onSubmit: vi.fn(async () => {}),
    onCancel: vi.fn(async () => {}),
    onAnswerQuestion: vi.fn(async () => {}),
    onInspectApproval: vi.fn(),
    onForceTail: vi.fn(),
    onError: vi.fn(),
    sessionControls: <span data-testid="permissions">permissions</span>,
    modelControl: <span data-testid="model">model</span>,
    stats: null,
    inbox: null,
    onEditQueueItem: vi.fn(async () => {}),
    onLoadQueueItem: vi.fn(async () => undefined),
    onRemoveQueueItem: vi.fn(async () => {}),
    onSteerQueueItem: vi.fn(async () => {}),
    t,
    ...overrides,
  }
}

it.each([
  { sessionId: 'session-two', accountScope: 'account-one' },
  { sessionId: 'session-one', accountScope: 'account-two' },
])('isolates a retained failed queue draft when scope changes to $accountScope/$sessionId', async nextScope => {
  const queued = {
    id: 'queued', run_id: 'queued-run', content: { kind: 'prompt' as const, input: 'original' },
    references: [], attachments: [], placement: 'queued' as const, created_at_ms: 1, updated_at_ms: 1,
  }
  const inbox = { session_id: 'session-one', paused: true, items: [queued] }
  const value = props({ accountScope: 'account-one', inbox, onEditQueueItem: vi.fn(async () => {
    throw new ApiError('stale edit', 409, 'conflict')
  }) })
  await act(async () => root.render(<InputBar {...value} />))
  act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.click())
  const editor = host.querySelector<HTMLInputElement>('input[aria-label="编辑排队消息"]')!
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(editor, 'private failed draft')
    editor.dispatchEvent(new Event('input', { bubbles: true }))
  })
  await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.click())
  await act(async () => root.render(<InputBar {...value} inbox={{ ...inbox, items: [] }} />))
  expect(host.querySelector<HTMLInputElement>('input[aria-label="编辑排队消息"]')?.value).toBe('private failed draft')
  expect(host.querySelector('[data-queue-dock]')?.textContent).toContain('草稿已保留')
  await act(async () => root.render(<InputBar {...value} {...nextScope} inbox={{ ...inbox, session_id: nextScope.sessionId, items: [] }} />))
  expect(host.querySelector('[data-queue-dock]')).toBeNull()
  await act(async () => root.render(<InputBar {...value} {...nextScope} inbox={{ ...inbox, session_id: nextScope.sessionId, items: [queued] }} />))
  expect(host.querySelector('input[aria-label="编辑排队消息"]')).toBeNull()
  expect(host.querySelector('[data-queue-dock]')?.textContent).toContain('original')
  expect(host.textContent).not.toContain('private failed draft')
})

it('does not stop a running shared task when only submission is allowed', async () => {
  const value = props({ busy: true, canStop: false })
  await act(async () => root.render(<InputBar {...value} />))
  const stop = host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!
  expect(stop.disabled).toBe(true)
  await act(async () => host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
  expect(value.onCancel).not.toHaveBeenCalled()
  await act(async () => root.render(<InputBar {...value} canStop />))
  await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!.click())
  expect(value.onCancel).toHaveBeenCalledOnce()
})

function write(textarea: HTMLTextAreaElement, value: string) {
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
    setter?.call(textarea, value)
    textarea.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

function select(textarea: HTMLTextAreaElement, start: number, end = start) {
  act(() => {
    textarea.focus()
    textarea.setSelectionRange(start, end)
    textarea.dispatchEvent(new Event('select', { bubbles: true }))
    document.dispatchEvent(new Event('selectionchange', { bubbles: true }))
    textarea.dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }))
  })
}

async function settleCatalog() {
  await act(async () => { await Promise.resolve(); await Promise.resolve() })
}

describe('InputBar interaction behavior', () => {
  it('does not reopen the mobile keyboard after sending or opening commands', async () => {
    vi.stubGlobal('innerWidth', 390)
    let finish!: () => void
    const onSubmit = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
    await act(async () => root.render(<InputBar {...props({ onSubmit })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('textarea')!
    textarea.focus()
    write(textarea, 'Mobile input')
    await act(async () => host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
    expect(onSubmit).toHaveBeenCalledOnce()
    expect(document.activeElement).not.toBe(textarea)
    await act(async () => finish())
    await act(async () => new Promise<void>(resolve => requestAnimationFrame(() => resolve())))
    expect(document.activeElement).not.toBe(textarea)
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="会话指令（/）"]')!.click())
    expect(document.activeElement).not.toBe(textarea)
    expect(host.querySelector('[role="listbox"]')).not.toBeNull()
    await act(async () => textarea.focus())
    expect(document.activeElement).toBe(textarea)
  })
  it('keeps the draft editable but blocks button and keyboard submission until a model is ready', async () => {
    const onSubmit = vi.fn(async () => {})
    const onError = vi.fn()
    act(() => root.render(<InputBar {...props({
      onSubmit,
      onError,
      submissionBlocked: true,
      submissionBlockedMessage: '请先配置模型',
    })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '保留这份草稿')
    const send = host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!
    expect(textarea.disabled).toBe(false)
    expect(send.disabled).toBe(true)
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))
    })
    expect(onSubmit).not.toHaveBeenCalled()
    expect(onError).toHaveBeenCalledWith('请先配置模型')
    expect(textarea.value).toBe('保留这份草稿')
  })

  it('allows real model-independent commands without a Provider but still blocks Skill and unknown slash input', async () => {
    const onSubmit = vi.fn(async () => {})
    const onError = vi.fn()
    act(() => root.render(<InputBar {...props({
      onSubmit,
      onError,
      submissionBlocked: true,
      submissionBlockedMessage: '请先配置模型',
    })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    for (const command of ['/goal edit 交付', '/goal complete 交付', '/todo 实现;验证', '/shell! echo approved', '/compact', '/plan']) {
      write(textarea, command)
      const send = host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!
      expect(send.disabled, command).toBe(false)
      await act(async () => send.click())
      expect(onSubmit).toHaveBeenLastCalledWith(command, [], 'queue', [])
    }

    for (const blocked of ['/goal 交付', '/goal resume 交付', '/skill release-check', '/unknown value']) {
      write(textarea, blocked)
      expect(host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.disabled, blocked).toBe(true)
      await act(async () => {
        textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))
      })
      expect(onError).toHaveBeenLastCalledWith('请先配置模型')
    }
  })

  it('keeps Stop visible for an empty running turn and switches to queue/steer send when a draft exists', async () => {
    const onCancel = vi.fn(async () => {})
    const onSubmit = vi.fn(async () => {})
    const onForceTail = vi.fn()
    const value = props({ busy: true, onCancel, onSubmit, onForceTail })
    act(() => root.render(<InputBar {...value} />))

    const textarea = host.querySelector<HTMLTextAreaElement>('textarea[aria-label="输入任务"]')!
    const stop = host.querySelector<HTMLButtonElement>('button[aria-label="停止运行"]')!
    expect(stop).not.toBeNull()
    expect(stop.title).toBe('停止运行')
    expect(stop.querySelector('.lucide-loader-circle')).not.toBeNull()
    expect(host.querySelectorAll('.lucide-loader-circle')).toHaveLength(1)
    expect(host.textContent).not.toContain('正在运行')
    await act(async () => stop.click())
    expect(onCancel).toHaveBeenCalledOnce()

    write(textarea, '排队消息')
    const send = host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!
    expect(send).not.toBeNull()
    expect(send.querySelector('.lucide-arrow-up')).not.toBeNull()
    expect(host.querySelector('.lucide-loader-circle')).toBeNull()
    await act(async () => send.click())
    expect(onSubmit).toHaveBeenCalledWith('排队消息', [], 'queue', [])
    expect(onForceTail).toHaveBeenCalledOnce()

    write(textarea, '立即调整方向')
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true }))
    })
    expect(onSubmit).toHaveBeenLastCalledWith('立即调整方向', [], 'steer', [])
  })

  it.each(['workspace_execution_waiting', 'waiting_for_subagents', 'waiting_for_capacity'])('keeps stop, queued identity, editing and submission usable during %s', async phase => {
    const value = props({
      busy: true,
      events: [
        { seq: 0, type: 'turn_started', run_id: 'active', occurred_at_ms: 1 },
        { seq: 1, type: 'user_message', run_id: 'active', occurred_at_ms: 2, content: 'Active task' },
        { seq: 2, type: phase === 'workspace_execution_waiting' ? phase : 'execution_activity_changed', phase, run_id: 'active', occurred_at_ms: 3 },
      ],
      inbox: { session_id: 'session-one', active_run_id: 'active', paused: false, items: [{
        id: 'next', run_id: 'next-run', content: { kind: 'prompt', input: 'Queued task' }, references: [], attachments: [], placement: 'queued', created_at_ms: 4, updated_at_ms: 4,
        provenance: { input_id: 'next', author: { kind: 'account', user_id: 'teammate-id', username: 'teammate' } },
      }] },
    })
    await act(async () => root.render(<InputBar {...value} />))
    const stop = host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!
    expect(stop.disabled).toBe(false)
    expect(stop.querySelector('.lucide-square')).not.toBeNull()
    expect(host.querySelector('.lucide-loader-circle')).toBeNull()
    expect(host.querySelector('[data-queued-submission="next"] [data-input-identity-label]')?.textContent).toBe('teammate')
    await act(async () => stop.click())
    expect(value.onCancel).toHaveBeenCalledOnce()
    const queued = host.querySelector('[data-queued-submission="next"]')!
    const edit = queued.querySelector<HTMLButtonElement>(`[aria-label="${t('queue.edit')}"]`)!
    expect(edit.disabled).toBe(false)
    await act(async () => edit.click())
    expect(queued.querySelector('input')?.value).toBe('Queued task')
    await act(async () => queued.querySelector<HTMLButtonElement>(`[aria-label="${t('queue.cancelEdit')}"]`)!.click())
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, 'Another queued task')
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="发送"]')!.click())
    expect(value.onSubmit).toHaveBeenCalledWith('Another queued task', [], 'queue', [])
    write(textarea, 'Change the current task')
    await act(async () => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true })))
    expect(value.onSubmit).toHaveBeenLastCalledWith('Change the current task', [], 'steer', [])
  })

  it.each(['running', 'workspace_execution_waiting', 'waiting_for_subagents', 'waiting_for_capacity'])('preserves the private draft when stopping during %s', async phase => {
    const value = props({
      busy: true,
      canStop: false,
      events: [
        { seq: 0, type: 'turn_started', run_id: 'active', occurred_at_ms: 1 },
        ...(phase !== 'running' ? [{ seq: 1, type: phase === 'workspace_execution_waiting' ? phase : 'execution_activity_changed', phase, run_id: 'active', occurred_at_ms: 2 }] : []),
      ],
    })
    await act(async () => root.render(<InputBar {...value} />))
    expect(host.querySelectorAll('[aria-label="停止运行"]')).toHaveLength(1)
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, 'Keep this draft while stopping the current task')
    expect(host.querySelectorAll('[aria-label="停止运行"]')).toHaveLength(1)
    const stop = host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!
    expect(stop.type).toBe('button')
    expect(stop.disabled).toBe(true)
    await act(async () => stop.click())
    expect(value.onCancel).not.toHaveBeenCalled()
    await act(async () => root.render(<InputBar {...value} canStop />))
    await act(async () => stop.click())
    expect(value.onCancel).toHaveBeenCalledOnce()
    expect(value.onSubmit).not.toHaveBeenCalled()
    expect(textarea.value).toBe('Keep this draft while stopping the current task')
    expect(host.querySelectorAll('.lucide-loader-circle')).toHaveLength(0)
    await act(async () => root.render(<InputBar {...value} busy={false} />))
    expect(host.querySelector('[aria-label="停止运行"]')).toBeNull()
    expect(textarea.value).toBe('Keep this draft while stopping the current task')
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="发送"]')!.click())
    expect(value.onSubmit).toHaveBeenCalledWith('Keep this draft while stopping the current task', [], 'queue', [])
  })

  it('accelerated Enter submits the entire pending batch with one request', async () => {
    const onSteerQueueItem = vi.fn(async (_id: string) => {})
    const value = props({
      busy: true,
      onSteerQueueItem,
      inbox: {
        session_id: 'session-one',
        active_run_id: 'run-one',
        paused: false,
        items: [
          { id: 'one', run_id: 'run-one', content: { kind: 'prompt', input: 'first' }, references: [], attachments: [], placement: 'queued', created_at_ms: 1, updated_at_ms: 1 },
          { id: 'two', run_id: 'run-one', content: { kind: 'prompt', input: 'second' }, references: [], attachments: [], placement: 'queued', created_at_ms: 2, updated_at_ms: 2 },
        ],
      },
    })
    act(() => root.render(<InputBar {...value} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('textarea')!
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', metaKey: true, bubbles: true, cancelable: true }))
    })
    expect(onSteerQueueItem.mock.calls.map(([id]) => id)).toEqual(['one'])
  })

  it('renders a real question takeover and restores the composer after the question clears', async () => {
    const onAnswerQuestion = vi.fn(async () => {})
    const value = props({
      variant: 'hero',
      onAnswerQuestion,
      questions: [{
        session_id: 'session-one',
        question: { id: 'question-one', question: '继续执行吗？', options: [{ label: '继续' }, { label: '停止' }], multi_select: false },
      }],
    })
    act(() => root.render(<InputBar {...value} />))
    expect(host.querySelector('[data-composer-input]')).toBeNull()
    expect(host.querySelector('[data-question-takeover]')?.textContent).toContain('继续执行吗？')
    const answer = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('继续'))!
    act(() => answer.click())
    await act(async () => [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '提交')!.click())
    expect(onAnswerQuestion).toHaveBeenCalledWith('question-one', { selected: ['继续'] })

    act(() => root.render(<InputBar {...value} variant="composer" questions={[]} />))
    expect(host.querySelector('[data-composer-input]')).not.toBeNull()
  })

  it('connects the persisted busy Enter preference and makes Cmd/Ctrl+Enter its inverse', async () => {
    localStorage.setItem(BUSY_ENTER_STORAGE_KEY, 'steer')
    const onSubmit = vi.fn(async () => {})
    act(() => root.render(<InputBar {...props({ busy: true, onSubmit })} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '立即调整')
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))
    })
    expect(onSubmit).toHaveBeenLastCalledWith('立即调整', [], 'steer', [])

    write(textarea, '排到后面')
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true }))
    })
    expect(onSubmit).toHaveBeenLastCalledWith('排到后面', [], 'queue', [])
  })

  it('discovers slash commands with keyboard selection and submits structured real references', async () => {
    const onSubmit = vi.fn(async () => {})
    const value = props({
      onSubmit,
      referenceCandidates: [{
        id: 'file:README.md', kind: 'file', label: 'README.md', detail: 'README.md', fileKind: 'file',
        reference: { kind: 'file', path: 'README.md', file_kind: 'file' },
      }],
    })
    act(() => root.render(<InputBar {...value} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/go')
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('/goal')
    const goal = [...host.querySelectorAll<HTMLButtonElement>('[role="option"]')].find(button => button.textContent?.includes('/goal'))!
    act(() => goal.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })))
    expect(textarea.value).toBe('/goal ')

    write(textarea, '@read')
    expect([...host.querySelectorAll<HTMLButtonElement>('[role="option"]')].some(button => button.textContent?.includes('README.md'))).toBe(true)
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('README.md')
    write(textarea, '检查构建')
    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.click())
    expect(onSubmit).toHaveBeenCalledWith('检查构建', [], 'queue', [
      { kind: 'file', path: 'README.md', file_kind: 'file' },
    ])
  })

  it('moves a single active descendant with arrows and selects a runtime command without sending early', async () => {
    const onSubmit = vi.fn(async () => {})
    act(() => root.render(<InputBar {...props({ onSubmit })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '.')
    const options = [...host.querySelectorAll<HTMLButtonElement>('[role="option"]')]
    expect(options.map(option => option.textContent).join(' ')).not.toContain('/goal')
    const press = (key: string) => act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })))
    const active = () => host.querySelector<HTMLButtonElement>('[aria-selected="true"]')!
    expect(active()).toBe(options[0])
    press('ArrowUp')
    expect(active()).toBe(options.at(-1))
    expect(textarea.getAttribute('aria-activedescendant')).toBe(options.at(-1)!.id)
    press('ArrowDown')
    expect(active()).toBe(options[0])
    press('ArrowDown')
    expect(active()).toBe(options[1])
    press('ArrowDown')
    expect(active()).toBe(options[2])
    press('Tab')
    expect(textarea.value).toBe('.skills')
    expect(onSubmit).not.toHaveBeenCalled()
    expect(host.querySelector('[data-composer-menu]')).toBeNull()
    await act(async () => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(onSubmit).toHaveBeenCalledWith('/skills', [], 'queue', [])
  })

  it('opens session actions from plus without consuming the independent draft or reference chips', async () => {
    const model = vi.fn()
    const onSubmit = vi.fn(async () => {})
    act(() => root.render(<InputBar {...props({ onSubmit, sessionActions: { model }, referenceCandidates: [{
      id: 'file:README.md', kind: 'file', label: 'README.md', detail: 'README.md', fileKind: 'file',
      reference: { kind: 'file', path: 'README.md', file_kind: 'file' },
    }] })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '@read')
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    write(textarea, 'keep this draft')
    const launcher = host.querySelector<HTMLButtonElement>('[aria-label="会话指令（/）"]')!
    act(() => launcher.click())
    expect(textarea.value).toBe('keep this draft')
    const option = [...host.querySelectorAll<HTMLButtonElement>('[role="option"]')].find(item => item.textContent?.includes('/model'))!
    await act(async () => option.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })))
    expect(model).toHaveBeenCalledOnce()
    expect(textarea.value).toBe('keep this draft')
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('README.md')
    expect(onSubmit).not.toHaveBeenCalled()
    act(() => launcher.click())
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    expect(host.querySelector('[data-composer-menu]')).toBeNull()
    expect(textarea.value).toBe('keep this draft')
  })

  it('never sends client actions as model prompts and leaves unsupported dot input unchanged', async () => {
    const model = vi.fn()
    const value = props({ sessionActions: { model } })
    act(() => root.render(<InputBar {...value} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/model unexpected')
    await act(async () => host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
    expect(value.onError).toHaveBeenCalled()
    expect(textarea.value).toBe('/model unexpected')
    expect(model).not.toHaveBeenCalled()
    write(textarea, '/permission')
    await act(async () => host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
    expect(value.onSubmit).not.toHaveBeenCalled()
    expect(textarea.value).toBe('/permission')
    for (const input of ['.env', './folder', '../folder']) {
      write(textarea, input)
      expect(host.querySelector('[data-composer-menu]')).toBeNull()
      await act(async () => host.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
      expect(value.onSubmit).toHaveBeenLastCalledWith(input, [], 'queue', [])
    }
  })

  it('replaces a reference at the caret, preserves its suffix, and undoes or redoes text and chip together', async () => {
    const reference = {
      id: 'file:README.md', kind: 'file' as const, label: 'README.md', detail: 'README.md', fileKind: 'file' as const,
      reference: { kind: 'file' as const, path: 'README.md', file_kind: 'file' as const },
    }
    act(() => root.render(<InputBar {...props({ referenceCandidates: [reference] })} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    const draft = '先看 @read 再继续'
    write(textarea, draft)
    const caret = draft.indexOf(' 再继续')
    select(textarea, caret)
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('README.md')

    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))
      await new Promise(resolve => requestAnimationFrame(resolve))
    })
    expect(textarea.value).toBe('先看  再继续')
    expect(textarea.selectionStart).toBe(3)
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('README.md')

    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, bubbles: true, cancelable: true }))
      await new Promise(resolve => requestAnimationFrame(resolve))
    })
    expect(textarea.value).toBe(draft)
    expect(textarea.selectionStart).toBe(caret)
    expect(host.querySelector('[aria-label="待发送引用"]')).toBeNull()

    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'z', ctrlKey: true, shiftKey: true, bubbles: true, cancelable: true }))
      await new Promise(resolve => requestAnimationFrame(resolve))
    })
    expect(textarea.value).toBe('先看  再继续')
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('README.md')
  })

  it('does not open a completion menu while IME composition owns the caret', async () => {
    act(() => root.render(<InputBar {...props()} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    act(() => textarea.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true })))
    write(textarea, '/go')
    expect(host.querySelector('[data-composer-menu]')).toBeNull()
    act(() => textarea.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true })))
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('/goal')
  })

  it('submits references without synthetic prompt text and restores them after admission failure', async () => {
    const onSubmit = vi.fn().mockRejectedValueOnce(new Error('admission failed')).mockResolvedValueOnce(undefined)
    const reference = {
      id: 'session:s2', kind: 'session' as const, label: 'Build investigation', detail: '',
      reference: { kind: 'session' as const, session_id: 's2', label: 'Build investigation' },
    }
    act(() => root.render(<InputBar {...props({ onSubmit, referenceCandidates: [reference] })} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '@build')
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(textarea.value).toBe('')
    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.click())
    expect(onSubmit).toHaveBeenCalledWith('', [], 'queue', [reference.reference])
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('Build investigation')
    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.click())
    expect(host.querySelector('[aria-label="待发送引用"]')).toBeNull()
  })

  it('keeps commands unambiguous by refusing selected references', async () => {
    const onSubmit = vi.fn(async () => {})
    const onError = vi.fn()
    act(() => root.render(<InputBar {...props({
      onSubmit,
      onError,
      referenceCandidates: [{
        id: 'file:src', kind: 'file', label: 'src', detail: 'src', fileKind: 'directory',
        reference: { kind: 'file', path: 'src', file_kind: 'directory' },
      }],
    })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '@src')
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    write(textarea, '/goal finish')
    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.click())
    expect(onSubmit).not.toHaveBeenCalled()
    expect(onError).toHaveBeenCalled()
    expect(host.querySelector('[aria-label="待发送引用"]')?.textContent).toContain('src')
  })

  it('offers an explicit mouse drill action for directories', () => {
    act(() => root.render(<InputBar {...props({
      referenceCandidates: [{
        id: 'file:src', kind: 'file', label: 'src', detail: 'src', fileKind: 'directory',
        reference: { kind: 'file', path: 'src', file_kind: 'directory' },
      }],
    })} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '@s')
    const drill = host.querySelector<HTMLButtonElement>('button[aria-label="进入文件夹 src"]')!
    act(() => drill.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })))
    expect(textarea.value).toBe('@src/')
    expect(host.querySelector('[aria-label="待发送引用"]')).toBeNull()
  })

  it('reopens a dismissed command menu after the user edits and types the same trigger again', () => {
    act(() => root.render(<InputBar {...props()} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/')
    expect(host.querySelector('[data-composer-menu]')).not.toBeNull()
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    expect(host.querySelector('[data-composer-menu]')).toBeNull()
    write(textarea, '')
    write(textarea, '/')
    expect(host.querySelector('[data-composer-menu]')).not.toBeNull()
  })

  it('automatically retries one transient unavailable command directory after the composition revision changes', async () => {
    let commandAttempts = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      const url = String(input)
      if (url.includes('/commands')) {
        commandAttempts += 1
        if (commandAttempts === 2) {
          return new Response(JSON.stringify({ message: 'catalog offline' }), {
            status: 503,
            headers: { 'content-type': 'application/json' },
          })
        }
        return new Response(JSON.stringify(runtimeCommands), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }
      return new Response(JSON.stringify({ revision: 0, complete: true, skills: [] }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    })
    act(() => root.render(<InputBar {...props({ commandCatalogRevision: 'before-write' })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    act(() => root.render(<InputBar {...props({ commandCatalogRevision: 'after-write' })} />))
    write(textarea, '/')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 120)) })
    await settleCatalog()
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('/goal')
    expect(commandAttempts).toBe(3)
  })

  it('keeps the explicit Retry action when the Session command directory remains unavailable', async () => {
    let commandAttempts = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      const url = String(input)
      if (url.includes('/commands')) {
        commandAttempts += 1
        if (commandAttempts <= 2) {
          return new Response(JSON.stringify({ message: 'catalog offline' }), {
            status: 503,
            headers: { 'content-type': 'application/json' },
          })
        }
        return new Response(JSON.stringify(runtimeCommands), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }
      return new Response(JSON.stringify({ revision: 0, complete: true, skills: [] }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      })
    })
    act(() => root.render(<InputBar {...props()} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 120)) })
    await settleCatalog()
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('HTTP 503')
    await act(async () => host.querySelector<HTMLButtonElement>('[role="alert"] button')!.click())
    await settleCatalog()
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('/goal')
    expect(commandAttempts).toBe(3)
  })

  it('loads the real typed Skill catalog and exposes descriptions and provider badges', async () => {
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => new Response(JSON.stringify(String(input).includes('/commands')
      ? runtimeCommands
      : {
          revision: 3,
          complete: true,
          skills: [{
            name: 'release-check', description: '检查发布前条件', when_to_use: '准备发布时',
            invocation: { model_invocable: true, user_invocable: true }, source: '/work/SKILL.md', provider: 'filesystem',
          }],
        }), { status: 200, headers: { 'content-type': 'application/json' } }))
    act(() => root.render(<InputBar {...props()} />))
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/skill ')
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('检查发布前条件')
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('filesystem')
    act(() => textarea.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })))
    expect(textarea.value).toBe('.skill release-check ')
  })

  it('shows an incomplete Skill catalog without polling and reloads it only when the menu reopens', async () => {
    let available = false
    let skillRequests = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      if (String(input).includes('/commands')) return new Response(JSON.stringify(runtimeCommands), {
        status: 200, headers: { 'content-type': 'application/json' },
      })
      skillRequests += 1
      return new Response(JSON.stringify({ revision: available ? 1 : 0, complete: available, skills: available ? [{
        name: 'release-check', description: 'Ready after the first task',
        invocation: { model_invocable: true, user_invocable: true }, source: 'workspace', provider: 'filesystem',
      }] : [] }), { status: 200, headers: { 'content-type': 'application/json' } })
    })
    act(() => root.render(<InputBar {...props()} />))
    await settleCatalog()
    expect(skillRequests).toBe(1)
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/skill ')
    await settleCatalog()
    expect(skillRequests).toBe(2)
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain(t('composer.skillsNotReady'))
    expect(host.querySelector('[data-composer-menu]')?.textContent).not.toContain(t('composer.noMatches'))
    expect(host.querySelector('[data-composer-menu]')?.textContent).not.toContain(t('composer.catalogLoading'))
    expect(host.querySelector('[role="alert"]')).toBeNull()
    write(textarea, '/skill release')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 150)) })
    expect(skillRequests, 'typing and waiting do not poll an incomplete directory').toBe(2)
    available = true
    write(textarea, '')
    write(textarea, '/skill ')
    await settleCatalog()
    expect(skillRequests).toBe(3)
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('Ready after the first task')
    expect(host.querySelector('[data-composer-menu]')?.textContent).not.toContain(t('composer.skillsNotReady'))
    write(textarea, '')
    write(textarea, '/skill absent')
    await settleCatalog()
    expect(skillRequests, 'a complete cached directory is not fetched again on open').toBe(3)
    expect(host.querySelector('[data-composer-menu] [role="listbox"]')?.textContent).toBe(t('composer.noMatches'))
  })

  it('automatically retries one transient unavailable Skill catalog', async () => {
    let skillAttempts = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      if (String(input).includes('/commands')) {
        return new Response(JSON.stringify(runtimeCommands), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }
      skillAttempts += 1
      if (skillAttempts === 1) {
        return new Response(JSON.stringify({ message: 'skills offline' }), {
          status: 503,
          headers: { 'content-type': 'application/json' },
        })
      }
      return new Response(JSON.stringify({
        revision: 1,
        complete: true,
        skills: [{
          name: 'recovered-skill', description: '暂态后恢复',
          invocation: { model_invocable: true, user_invocable: true },
          source: 'project-agents', provider: 'filesystem',
        }],
      }), { status: 200, headers: { 'content-type': 'application/json' } })
    })
    act(() => root.render(<InputBar {...props()} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/skill ')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 120)) })
    await settleCatalog()
    expect(skillAttempts).toBe(2)
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('recovered-skill')
    expect(host.querySelector('[role="alert"]')).toBeNull()
  })

  it('keeps the Skill catalog error after two unavailable replies and recovers on explicit Retry', async () => {
    let skillAttempts = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      if (String(input).includes('/commands')) {
        return new Response(JSON.stringify(runtimeCommands), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }
      skillAttempts += 1
      if (skillAttempts <= 2) {
        return new Response(JSON.stringify({ message: 'skills offline' }), {
          status: 503,
          headers: { 'content-type': 'application/json' },
        })
      }
      return new Response(JSON.stringify({
        revision: 1,
        complete: true,
        skills: [{
          name: 'manual-recovery', description: '用户重试后恢复',
          invocation: { model_invocable: true, user_invocable: true },
          source: 'project-agents', provider: 'filesystem',
        }],
      }), { status: 200, headers: { 'content-type': 'application/json' } })
    })
    act(() => root.render(<InputBar {...props()} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/skill ')
    await act(async () => { await new Promise(resolve => setTimeout(resolve, 120)) })
    await settleCatalog()
    expect(skillAttempts).toBe(2)
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('HTTP 503')
    await act(async () => host.querySelector<HTMLButtonElement>('[role="alert"] button')!.click())
    await settleCatalog()
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('manual-recovery')
    expect(host.querySelector('[role="alert"]')).toBeNull()
    expect(skillAttempts).toBe(3)
  })

  it('reloads Skills when the Session composition revision changes', async () => {
    let skillRequests = 0
    fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => {
      if (String(input).includes('/commands')) {
        return new Response(JSON.stringify(runtimeCommands), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        })
      }
      skillRequests += 1
      return new Response(JSON.stringify({
        revision: skillRequests,
        complete: true,
        skills: skillRequests === 1 ? [] : [{
          name: 'workspace-added',
          description: '刚写入 Workspace 的 Skill',
          invocation: { model_invocable: true, user_invocable: true },
          source: 'project-agents',
          provider: 'filesystem',
        }],
      }), { status: 200, headers: { 'content-type': 'application/json' } })
    })
    act(() => root.render(<InputBar {...props({ commandCatalogRevision: 'before-write' })} />))
    await settleCatalog()
    const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
    write(textarea, '/skill ')
    expect(host.querySelector('[data-composer-menu]')?.textContent).not.toContain('workspace-added')

    act(() => root.render(<InputBar {...props({ commandCatalogRevision: 'after-write' })} />))
    await settleCatalog()
    expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('workspace-added')
    expect(skillRequests).toBe(2)
  })
})

describe('runtime catalog recovery', () => {
  const respond = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'content-type': 'application/json' } })
  const unavailable = () => respond({ error: { code: 'unavailable', message: 'Worker offline' } }, 503)
  const skills = { revision: 1, complete: true, skills: [{ name: 'recovered-skill', description: 'Recovered', invocation: { model_invocable: true, user_invocable: true }, source: 'workspace', provider: 'filesystem' }] }
  const commandCalls = () => fetchMock.mock.calls.filter(([url]: Parameters<typeof fetch>) => String(url).includes('/commands')).length
  const editor = () => host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
  const send = async () => { await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="发送"]')!.click()) }

  it('recovers an unavailable directory for one explicit Skill submission and remains quiet afterwards', async () => {
    vi.useFakeTimers()
    try {
      let available = false
      fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => String(input).includes('/commands') ? available ? respond(runtimeCommands) : unavailable() : respond(skills))
      const value = props()
      await act(async () => root.render(<InputBar {...value} />))
      await act(async () => vi.advanceTimersByTimeAsync(150))
      expect(commandCalls()).toBe(2)
      available = true
      write(editor(), '/skill release-check audit the change')
      await send()
      await settleCatalog()
      expect(value.onSubmit).toHaveBeenCalledExactlyOnceWith('/skill release-check audit the change', [], 'queue', [])
      expect(commandCalls()).toBe(3)
      expect(value.onError).not.toHaveBeenCalled()
      expect(editor().value).toBe('')
      const count = fetchMock.mock.calls.length
      await act(async () => vi.advanceTimersByTimeAsync(30_000))
      expect(fetchMock.mock.calls).toHaveLength(count)
    } finally { vi.useRealTimers() }
  })

  it('waits for the in-flight directory and coalesces repeated Send clicks', async () => {
    let finish!: (response: Response) => void
    fetchMock.mockImplementation((input: Parameters<typeof fetch>[0]) => String(input).includes('/commands') ? new Promise<Response>(resolve => { finish = resolve }) : Promise.resolve(respond(skills)))
    const value = props()
    await act(async () => root.render(<InputBar {...value} />))
    write(editor(), '/skill release-check audit the change')
    await send()
    await send()
    expect(value.onSubmit).not.toHaveBeenCalled()
    expect(commandCalls()).toBe(1)
    await act(async () => finish(respond(runtimeCommands)))
    await settleCatalog()
    expect(value.onSubmit).toHaveBeenCalledExactlyOnceWith('/skill release-check audit the change', [], 'queue', [])
    expect(value.onError).not.toHaveBeenCalled()
  })

  it.each(['draft edit', 'composition change'])('does not submit the waiting intent after a %s', async change => {
    let finish!: (response: Response) => void
    fetchMock.mockImplementation((input: Parameters<typeof fetch>[0]) => String(input).includes('/commands') ? new Promise<Response>(resolve => { finish = resolve }) : Promise.resolve(respond(skills)))
    const value = props({ commandCatalogRevision: 'before' })
    await act(async () => root.render(<InputBar {...value} />))
    const oldResponse = finish
    write(editor(), '/skill release-check original request')
    await send()
    if (change === 'draft edit') write(editor(), 'Keep this newer draft')
    else await act(async () => root.render(<InputBar {...value} commandCatalogRevision="after" />))
    await act(async () => oldResponse(respond(runtimeCommands)))
    await settleCatalog()
    expect(value.onSubmit).not.toHaveBeenCalled()
    expect(value.onError).not.toHaveBeenCalled()
    if (change === 'draft edit') expect(editor().value).toBe('Keep this newer draft')
  })

  it('reloads failed catalogs on connection recovery without polling healthy ones', async () => {
    vi.useFakeTimers()
    try {
      let available = false
      fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => !available ? unavailable() : respond(String(input).includes('/commands') ? runtimeCommands : skills))
      const value = props({ connectionStatus: 'reconnecting' })
      await act(async () => root.render(<InputBar {...value} />))
      await act(async () => vi.advanceTimersByTimeAsync(150))
      expect(commandCalls()).toBe(2)
      available = true
      await act(async () => root.render(<InputBar {...value} connectionStatus="ready" />))
      await settleCatalog()
      write(editor(), '/skill ')
      expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('recovered-skill')
      expect(commandCalls()).toBe(3)
      const count = fetchMock.mock.calls.length
      await act(async () => root.render(<InputBar {...value} connectionStatus="reconnecting" />))
      await act(async () => root.render(<InputBar {...value} connectionStatus="ready" />))
      await act(async () => vi.advanceTimersByTimeAsync(30_000))
      expect(fetchMock.mock.calls).toHaveLength(count)
    } finally { vi.useRealTimers() }
  })

  it('recovers a failed Skill list on menu reopening without reloading healthy commands', async () => {
    vi.useFakeTimers()
    try {
      let available = false
      fetchMock.mockImplementation(async (input: Parameters<typeof fetch>[0]) => String(input).includes('/commands') ? respond(runtimeCommands) : available ? respond(skills) : unavailable())
      await act(async () => root.render(<InputBar {...props()} />))
      await settleCatalog()
      write(editor(), '/skill ')
      await act(async () => vi.advanceTimersByTimeAsync(150))
      expect(host.querySelector('[role="alert"]')).not.toBeNull()
      available = true
      write(editor(), '')
      write(editor(), '/skill ')
      await settleCatalog()
      expect(host.querySelector('[data-composer-menu]')?.textContent).toContain('recovered-skill')
      expect(commandCalls()).toBe(1)
      const count = fetchMock.mock.calls.length
      await act(async () => vi.advanceTimersByTimeAsync(30_000))
      expect(fetchMock.mock.calls).toHaveLength(count)
    } finally { vi.useRealTimers() }
  })
})

it('keeps a task waiting for its directory cancellable before any model turn exists', async () => {
  const value = props({
    busy: true,
    execution: { run_id: 'queued-run', phase: 'waiting_for_workspace' },
    inbox: { session_id: 'session-one', active_run_id: 'queued-run', paused: false, items: [{
      id: 'queued', run_id: 'queued-run', content: { kind: 'prompt', input: 'Waiting task' },
      references: [], attachments: [], placement: 'running', created_at_ms: 1, updated_at_ms: 1,
    }] },
  })
  await act(async () => root.render(<InputBar {...value} />))
  expect(host.querySelector('[data-queue-dock]')).toBeNull()
  expect(host.textContent).not.toContain('Waiting task')
  const stop = host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!
  expect(stop.disabled).toBe(false)
  expect(stop.querySelector('.lucide-square')).not.toBeNull()
  expect(stop.querySelector('.lucide-loader-circle')).toBeNull()
  const textarea = host.querySelector<HTMLTextAreaElement>('[data-composer-input]')!
  write(textarea, 'Keep this independent draft')
  await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="停止运行"]')!.click())
  expect(value.onCancel).toHaveBeenCalledOnce()
  expect(textarea.value).toBe('Keep this independent draft')
})
