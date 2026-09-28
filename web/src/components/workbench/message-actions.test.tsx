import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const workbench = vi.hoisted(() => ({
  notify: vi.fn(),
  forkSession: vi.fn(),
  forkOperation: null as null | { sourceSessionId: string; childSessionId: string | null; phase: 'creating' | 'hydrating' },
}))
vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    notify: workbench.notify,
    currentSessionId: 'session-1',
    forkOperation: workbench.forkOperation,
    forkSession: workbench.forkSession,
  }),
}))

import { AssistantMessageActions, UserMessageActions } from './message-actions'
import type { SessionEvent } from '@/types'

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  workbench.notify.mockReset()
  workbench.forkSession.mockReset()
  workbench.forkOperation = null
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText: vi.fn(async () => { throw new DOMException('denied', 'NotAllowedError') }) },
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('message copy feedback', () => {
  it('resends the original event once while pending and remains usable after rejection', async () => {
    const event: SessionEvent = {
      seq: 1, run_id: 'original', type: 'user_message', occurred_at_ms: 1,
      content: 'original input', display_content: 'displayed input',
      attachments: [{ name: 'note.txt', media_type: 'text/plain', content: 'saved attachment' }],
      references: [{ kind: 'file', path: 'README.md', file_kind: 'file' }],
    }
    let finish!: () => void
    const onRegenerate = vi.fn(() => new Promise<void>(resolve => { finish = resolve }))
    act(() => root.render(<UserMessageActions event={event} content="displayed input" onRegenerate={onRegenerate} />))
    const retry = host.querySelector<HTMLButtonElement>('button[aria-label="重新生成"]')!
    expect(retry.previousElementSibling?.getAttribute('aria-label')).toBe('复制')
    await act(async () => { retry.click(); retry.click() })
    expect(onRegenerate).toHaveBeenCalledTimes(1)
    expect(onRegenerate).toHaveBeenCalledWith(event)
    expect(retry.disabled).toBe(true)
    await act(async () => finish())
    expect(retry.disabled).toBe(false)
    onRegenerate.mockRejectedValueOnce(new Error('submission rejected'))
    await act(async () => retry.click())
    expect(workbench.notify).toHaveBeenCalledWith('submission rejected', 'error')
    expect(retry.disabled).toBe(false)
    expect(event.run_id).toBe('original')
  })

  it('disables regeneration when unavailable and omits it for read-only views', async () => {
    const event: SessionEvent = { seq: 1, run_id: 'original', type: 'user_message', occurred_at_ms: 1 }
    const onRegenerate = vi.fn(async () => undefined)
    act(() => root.render(<UserMessageActions event={event} content="input" onRegenerate={onRegenerate} regenerateDisabled />))
    const retry = host.querySelector<HTMLButtonElement>('button[aria-label="重新生成"]')!
    expect(retry.disabled).toBe(true)
    await act(async () => retry.click())
    expect(onRegenerate).not.toHaveBeenCalled()
    act(() => root.render(<UserMessageActions event={event} content="input" />))
    expect(host.querySelector('button[aria-label="重新生成"]')).toBeNull()
    expect(host.querySelector('button[aria-label="复制"]')).not.toBeNull()
  })

  it('disables duplicate message-tail forks while the shared fork operation is active', () => {
    workbench.forkOperation = {
      sourceSessionId: 'session-1', childSessionId: null, phase: 'creating',
    }
    act(() => root.render(<AssistantMessageActions
      event={{ seq: 2, run_id: 'run-1', type: 'assistant_message', occurred_at_ms: 2 }}
      content="answer"
      projection={null}
      reloadMetadata={async () => undefined}
    />))

    const branch = host.querySelector<HTMLButtonElement>('button[aria-label="正在创建分支对话"]')
    expect(branch?.disabled).toBe(true)
    expect(branch?.querySelector('svg')).not.toBeNull()
  })

  it('reports a localized error when browser clipboard access fails', async () => {
    act(() => root.render(<UserMessageActions
      event={{ seq: 1, run_id: 'run-1', type: 'user_message', occurred_at_ms: 1 }}
      content="keep me"
    />))
    const copy = host.querySelector<HTMLButtonElement>('button[aria-label="复制"]')!
    await act(async () => copy.click())
    expect(workbench.notify).toHaveBeenCalledWith('复制失败，请检查浏览器剪贴板权限。', 'error')
  })

  it('sends the projected revision and refreshes after a concurrent feedback conflict', async () => {
    const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      expect(JSON.parse(String(init?.body))).toMatchObject({
        target_seq: 2,
        expected_revision: 4,
        rating: 'positive',
      })
      return new Response(JSON.stringify({
        error: { code: 'conflict', message: 'feedback revision conflict' },
      }), { status: 409, headers: { 'content-type': 'application/json' } })
    })
    vi.stubGlobal('fetch', fetch)
    const reloadMetadata = vi.fn(async () => undefined)
    act(() => root.render(<AssistantMessageActions
      event={{ seq: 2, run_id: 'run-1', type: 'assistant_message', occurred_at_ms: 2 }}
      content="answer"
      projection={{
        session_id: 'session-1',
        as_of_seq: 9,
        values: { feedback: { 2: { revision: 4, rating: 'negative', note: 'stale' } } },
      }}
      reloadMetadata={reloadMetadata}
    />))

    const positive = host.querySelector<HTMLButtonElement>('button[aria-label="好回答"]')!
    await act(async () => positive.click())

    expect(fetch).toHaveBeenCalledTimes(1)
    expect(reloadMetadata).toHaveBeenCalledTimes(1)
    expect(workbench.notify).toHaveBeenCalledWith(
      '反馈已被其他窗口修改，已刷新为最新状态，请重试。',
      'error',
    )
    expect(positive.getAttribute('aria-pressed')).toBe('false')
  })
})
