import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SessionSubmission } from '@/types'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import { QueueDock } from './queue-dock'
import { ApiError } from '@/api/client'
import { InputViewerProvider } from '@/state/input-viewer'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

let host: HTMLDivElement
let root: Root

function item(id: string, input: string, placement: SessionSubmission['placement'] = 'queued'): SessionSubmission {
  return {
    id,
    run_id: `run-${id}`,
    content: { kind: 'prompt', input },
    references: [],
    attachments: [],
    placement,
    created_at_ms: 1,
    updated_at_ms: 1,
  }
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('QueueDock', () => {
  it('keeps the draft on live conflicts until the user explicitly loads the latest version', async () => {
    const onEdit = vi.fn(async () => {})
    const render = (items: SessionSubmission[]) => act(() => root.render(<QueueDock
      items={items} running onEdit={onEdit} onLoad={async id => items.find(item => item.id === id)} onRemove={vi.fn()} onSteer={vi.fn()} onError={vi.fn()} t={t}
    />))
    render([item('one', 'original')])
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.click())
    setDraft('my unsaved draft')
    render([{ ...item('one', 'other account edit'), updated_at_ms: 2 }])
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('my unsaved draft')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('已被修改')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.disabled).toBe(true)
    expect(onEdit).not.toHaveBeenCalled()
    await act(async () => [...host.querySelectorAll('button')].find(button => button.textContent === '加载新版本')!.click())
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('other account edit')
    setDraft('resolved edit')
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.click())
    expect(onEdit).toHaveBeenCalledExactlyOnceWith('one', 'resolved edit', 2)
    expect(host.querySelector('input')).toBeNull()
  })

  it('preserves a rejected draft through refresh and never retries a 409 automatically', async () => {
    const onError = vi.fn()
    const onEdit = vi.fn(async () => { throw new ApiError('conflicting edit', 409, 'conflict') })
    const render = (items: SessionSubmission[]) => act(() => root.render(<QueueDock
      items={items} running onEdit={onEdit} onLoad={vi.fn()} onRemove={vi.fn()} onSteer={vi.fn()} onError={onError} t={t}
    />))
    render([item('one', 'original')])
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.click())
    setDraft('rejected draft')
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.click())
    expect(onEdit).toHaveBeenCalledExactlyOnceWith('one', 'rejected draft', 1)
    expect(onError).toHaveBeenCalledWith('conflicting edit')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('已被修改')
    render([{ ...item('one', 'saved by another account'), updated_at_ms: 2 }])
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('rejected draft')
    expect(onEdit).toHaveBeenCalledTimes(1)
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="取消编辑"]')!.click())
    expect(host.querySelector('input')).toBeNull()
    expect(host.textContent).toContain('saved by another account')
  })

  it.each(['running', 'deleted'] as const)('keeps the draft if the submission is %s during a failed save', async placement => {
    let rejectEdit!: (cause: Error) => void
    const onEdit = vi.fn(() => new Promise<void>((_resolve, reject) => { rejectEdit = reject }))
    const onError = vi.fn()
    const render = (items: SessionSubmission[]) => act(() => root.render(<QueueDock
      items={items} running onEdit={onEdit} onLoad={vi.fn()} onRemove={vi.fn()} onSteer={vi.fn()} onError={onError} t={t}
    />))
    render([item('one', 'original')])
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.click())
    setDraft('keep this draft')
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.click())
    render(placement === 'running' ? [item('one', 'original', 'running')] : [])
    await act(async () => rejectEdit(new ApiError('original queue refusal', 400, 'invalid_input')))
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('keep this draft')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('无法保存')
    expect(onError).toHaveBeenCalledWith('original queue refusal')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.disabled).toBe(true)
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="取消编辑"]')!.click())
    expect(host.querySelector('[data-queue-dock]')).toBeNull()
  })

  it('keeps the draft when loading fails and loads the current revision only on explicit retry', async () => {
    const onError = vi.fn()
    const onEdit = vi.fn(async () => { throw new ApiError('conflict', 409, 'conflict') })
    const loaded = { ...item('one', 'fresh from server'), updated_at_ms: 3 }
    const onLoad = vi.fn<() => Promise<SessionSubmission>>()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(loaded)
    act(() => root.render(<QueueDock items={[item('one', 'original')]} running
      onEdit={onEdit} onLoad={onLoad} onRemove={vi.fn()} onSteer={vi.fn()} onError={onError} t={t} />))
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.click())
    setDraft('my draft')
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')!.click())
    const reload = () => [...host.querySelectorAll('button')].find(button => button.textContent === '加载新版本')!.click()
    await act(async () => reload())
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('my draft')
    expect(onError).toHaveBeenCalledWith('offline')
    await act(async () => reload())
    expect(host.querySelector<HTMLInputElement>('input')!.value).toBe('fresh from server')
    expect(onLoad).toHaveBeenCalledTimes(2)
    expect(onEdit).toHaveBeenCalledTimes(1)
  })

  it('keeps editing available but denies interruption without stop permission', async () => {
    const onRemove = vi.fn(async () => {})
    act(() => root.render(<QueueDock
      items={[item('one', 'first')]}
      running canRemove={false}
      onEdit={vi.fn()} onLoad={vi.fn()} onRemove={onRemove} onSteer={vi.fn()} onError={vi.fn()} t={t}
    />))
    const remove = host.querySelector<HTMLButtonElement>('[aria-label="删除排队消息"]')!
    expect(remove.disabled).toBe(true)
    await act(async () => remove.click())
    expect(onRemove).not.toHaveBeenCalled()
    expect(host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')!.disabled).toBe(false)
    expect(host.querySelector<HTMLButtonElement>('[aria-label="停止并发送全部"]')!.disabled).toBe(true)
  })

  it('collapses multiple rows and addresses exact edit, remove and steering mutations', async () => {
    const onEdit = vi.fn(async () => {})
    const onRemove = vi.fn(async () => {})
    const onSteer = vi.fn(async () => {})
    act(() => root.render(<QueueDock
      items={[item('one', 'first'), item('two', 'second')]}
      running
      onEdit={onEdit}
      onLoad={vi.fn()}
      onRemove={onRemove}
      onSteer={onSteer}
      onError={vi.fn()}
      t={t}
    />))

    const header = host.querySelector<HTMLButtonElement>('[aria-controls]')
    expect(header?.textContent).toContain('2 条排队消息')
    expect(header?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelector('ul')?.hidden).toBe(false)

    act(() => header?.click())
    expect(host.querySelector('ul')?.hidden).toBe(true)

    act(() => header?.click())
    expect(header?.getAttribute('aria-expanded')).toBe('true')
    const rows = host.querySelectorAll('li')
    expect(rows).toHaveLength(2)

    act(() => rows[1]?.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')?.click())
    const editor = host.querySelector<HTMLInputElement>('input[aria-label="编辑排队消息"]')
    act(() => {
      if (!editor) return
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
      setter?.call(editor, 'edited second')
      editor.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="保存排队消息"]')?.click())
    expect(onEdit).toHaveBeenCalledWith('two', 'edited second', 1)

    await act(async () => rows[0]?.querySelector<HTMLButtonElement>('[aria-label="删除排队消息"]')?.click())
    expect(onRemove).toHaveBeenCalledWith('one')
    expect(host.querySelectorAll('[aria-label="停止并发送全部"]')).toHaveLength(1)
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="停止并发送全部"]')?.click())
    expect(onSteer).toHaveBeenCalledWith('one')
  })

  it('renders accepted steering separately from mutable queued rows', () => {
    act(() => root.render(<QueueDock
      items={[item('steer', 'change direction', 'steering')]}
      running
      onEdit={vi.fn()}
      onLoad={vi.fn()}
      onRemove={vi.fn()}
      onSteer={vi.fn()}
      onError={vi.fn()}
      t={t}
    />))
    expect(host.querySelector('[data-pending-steering]')?.textContent).toContain('change direction')
    expect(host.querySelector('[data-pending-steering]')?.textContent).toContain('已注入当前轮')
    expect(host.querySelector('[aria-label="编辑排队消息"]')).toBeNull()
  })

  it('clears consumed edits and shows the next pending batch expanded', () => {
    const render = (items: SessionSubmission[]) => act(() => root.render(<QueueDock
      items={items}
      running
      onEdit={vi.fn(async () => {})}
      onLoad={vi.fn()}
      onRemove={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onError={vi.fn()}
      t={t}
    />))
    render([item('one', 'first'), item('two', 'second')])
    act(() => host.querySelector<HTMLButtonElement>('[aria-label="编辑排队消息"]')?.click())
    expect(host.querySelector('input')).not.toBeNull()

    render([])
    expect(host.querySelector('[data-queue-dock]')).toBeNull()
    render([item('three', 'third'), item('four', 'fourth')])
    expect(host.querySelector('[aria-controls]')?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelector('input')).toBeNull()
    expect(host.querySelector('ul')?.hidden).toBe(false)
  })
})

function setDraft(value: string) {
  act(() => {
    const editor = host.querySelector<HTMLInputElement>('input')!
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(editor, value)
    editor.dispatchEvent(new Event('input', { bubbles: true }))
  })
}


it('shows pending submitters without repeating the running input', () => {
  const author = { kind: 'account' as const, user_id: 'sender', username: 'teammate' }
  const tasks = (['running', 'queued', 'steering'] as const).map(placement => ({ ...item(placement, `${placement} task`, placement), provenance: { input_id: placement, author } }))
  act(() => root.render(<InputViewerProvider local={false} user={{ user_id: 'reader', username: 'reader' }}><QueueDock
    items={tasks} running onEdit={vi.fn()} onLoad={vi.fn()} onRemove={vi.fn()} onSteer={vi.fn()} onError={vi.fn()} t={t}
  /></InputViewerProvider>))
  expect([...host.querySelectorAll('[data-input-identity-label]')].map(element => element.textContent)).toEqual(['teammate', 'teammate'])
  expect(host.querySelector('[data-current-task]')).toBeNull()
  expect(host.textContent).not.toContain('running task')
  expect(host.querySelector('[data-current-task] button[aria-label="编辑排队消息"]')).toBeNull()
  expect(host.querySelector<HTMLButtonElement>('[data-queued-submission] button[aria-label="编辑排队消息"]')?.disabled).toBe(false)
  expect(host.querySelector<HTMLButtonElement>('[aria-label="停止并发送全部"]')?.disabled).toBe(false)
})

it('hides the dock when only the current task remains', () => {
  act(() => root.render(<QueueDock
    items={[item('next', 'Next task', 'running')]} running
    onEdit={vi.fn()} onLoad={vi.fn()} onRemove={vi.fn()} onSteer={vi.fn()} onError={vi.fn()} t={t}
  />))
  expect(host.querySelector('[data-queue-dock]')).toBeNull()
  expect(host.textContent).toBe('')
})
