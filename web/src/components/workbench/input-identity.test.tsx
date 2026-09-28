import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { InputAuthor } from '@/types'
import { InputViewerProvider, type InputViewer } from '@/state/input-viewer'
import { InputIdentity } from './input-identity'

let root: Root
let host: HTMLDivElement
const alice = { kind: 'account', user_id: 'usr-alice-private-id', username: 'alice' } satisfies InputAuthor
const bob = { kind: 'account', user_id: 'usr-bob-private-id', username: 'bob' } satisfies InputAuthor
const viewer: InputViewer = { local: false, user: alice }

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } })
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
})
async function settle(action?: () => void) { await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve() }) }
async function render(author?: InputAuthor, selectedViewer = viewer) {
  await settle(() => root.render(<InputViewerProvider {...selectedViewer}><InputIdentity author={author} /></InputViewerProvider>))
}
const trigger = () => host.querySelector<HTMLButtonElement>('[data-input-identity]')!
const label = () => host.querySelector('[data-input-identity-label]')?.textContent
const copy = () => document.querySelector<HTMLButtonElement>('[aria-label="复制账号 ID"]')!

describe('input identity', () => {
  it('shows only You inline and reveals the canonical username and ID on click', async () => {
    await render(alice)
    expect(label()).toBe('你')
    expect(host.textContent).not.toContain('alice')
    expect(document.body.textContent).not.toContain(alice.user_id)
    expect(trigger().title).toBe('')
    await settle(() => trigger().click())
    const details = document.querySelector('[role="dialog"]')!
    expect(details.textContent).toContain('alice')
    expect(details.querySelector('[data-input-account-id]')?.textContent).toBe(alice.user_id)
    await settle(() => copy().click())
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(alice.user_id)
    expect(details.querySelector('[role="status"]')?.textContent).toBe('已复制账号 ID')
    await settle(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('keeps another account username inline without guessing from the current viewer', async () => {
    await render(bob)
    expect(label()).toBe('bob')
    expect(host.textContent).not.toContain(bob.user_id)
    await settle(() => trigger().click())
    expect(document.querySelector('[data-input-account-id]')?.textContent).toBe(bob.user_id)
    expect(document.body.textContent).not.toContain(alice.user_id)
  })

  it('closes stale identity details when the active account changes', async () => {
    await render(alice)
    await settle(() => trigger().click())
    await render(alice, { local: false, user: bob })
    expect(label()).toBe('alice')
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    await settle(() => trigger().click())
    expect(document.querySelector('[data-input-account-id]')?.textContent).toBe(alice.user_id)
  })

  it.each([
    [undefined, { local: true, user: null }, '身份未记录', '未记录提交者'],
    [undefined, viewer, '身份未记录', '未记录提交者'],
    [{ kind: 'local' }, { local: true, user: null }, '你', '未关联平台账号'],
    [{ kind: 'local' }, viewer, '本机用户', '未关联平台账号'],
    [{ kind: 'automation', source: 'schedule' }, viewer, '定时任务', '定时任务自动发起'],
    [{ kind: 'automation', source: 'subagent' }, viewer, '子 Agent', '子 Agent 协作任务'],
  ] as const)('keeps non-account author %j distinct', async (author, selectedViewer, expected, description) => {
    await render(author, selectedViewer)
    expect(label()).toBe(expected)
    await settle(() => trigger().click())
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain(description)
    expect(document.querySelector('[data-input-account-id]')).toBeNull()
    expect(copy()).toBeNull()
  })

  it('keeps the full ID selectable when clipboard permission is denied', async () => {
    vi.mocked(navigator.clipboard.writeText).mockRejectedValueOnce(new Error('clipboard denied'))
    await render(bob)
    await settle(() => trigger().click())
    await settle(() => copy().click())
    expect(document.querySelector('[role="alert"]')?.textContent).toContain('请手动选择并复制账号 ID')
    expect(document.querySelector('[data-input-account-id]')?.textContent).toBe(bob.user_id)
  })
})
