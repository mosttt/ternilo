import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { LocalSession, SessionEvent } from '@/types'
import { SessionArchiveDialog } from './session-archive-dialog'

const session: LocalSession = {
  identity: { tenant_id: 'team', user_id: 'owner', agent_id: 'agent', session_id: 'history/session' },
  workspace_id: 'workspace', workspace_path: '/archived-workspace', title: 'Archived task', archived_at_ms: 50,
  permissions: 'read_only', model: { provider: 'profile_default' }, agent_preset: 'standard',
  preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 40,
  access: { owner_user_id: 'owner', is_owner: false, role_limited: false, sources: [], permissions: { view: true, submit: false, stop: false, configure: false } },
}
const history: SessionEvent[] = [
  { seq: 0, occurred_at_ms: 1, run_id: 'run', type: 'user_message', content: 'Read the archived notes', provenance: { input_id: 'input-one', author: { kind: 'account', user_id: 'alice', username: 'Alice' } } },
  { seq: 1, occurred_at_ms: 2, run_id: 'run', type: 'assistant_message', response: { provider: 'fixture', model: 'model', content: '**Saved answer** <script>alert(1)</script>', reasoning_content: 'Saved reasoning', tool_calls: [], finish_reason: 'stop' } },
]
let host: HTMLDivElement
let root: Root
const restored = vi.fn()

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  restored.mockClear()
  vi.spyOn(api, 'request').mockImplementation(async path => path === '/sessions/archived' ? [session] as never : history as never)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals() })
async function render() {
  await act(async () => root.render(<LocaleProvider><SessionArchiveDialog tenantId="team" platform readOnly workspaces={[]} onClose={vi.fn()} onRestored={restored} /></LocaleProvider>))
}
function button(text: string) {
  const target = [...document.querySelectorAll<HTMLButtonElement>('button')].find(element => element.textContent === text)
  expect(target, text).toBeDefined()
  return target!
}
async function click(text: string) { await act(async () => button(text).click()) }

it('lazily previews shared history without restoring or starting anything and renders sanitized content', async () => {
  await render()
  expect(api.request).toHaveBeenCalledTimes(1)
  expect(button('恢复会话').disabled).toBe(true)
  await click('查看历史')
  expect(api.request).toHaveBeenLastCalledWith('/sessions/history%2Fsession/archive-events', expect.objectContaining({ headers: { 'x-ternilo-tenant': 'team' }, cache: 'no-store', signal: expect.any(AbortSignal) }))
  const preview = document.querySelector('[data-archive-preview]')!
  expect(preview.textContent).toContain('Alice')
  expect(preview.textContent).toContain('Saved answer')
  expect(preview.querySelector('script')).toBeNull()
  expect(preview.querySelector('strong')?.textContent).toBe('Archived task')
  const details = preview.querySelector<HTMLDetailsElement>('details')!
  await act(async () => { details.open = true; details.dispatchEvent(new Event('toggle')) })
  expect(details.textContent).toContain('Saved reasoning')
  await click('原始事件')
  expect(preview.querySelectorAll('[data-archive-event]')).toHaveLength(2)
  expect(vi.mocked(api.request).mock.calls.every(([, options]) => options?.method === undefined)).toBe(true)
  expect(restored).not.toHaveBeenCalled()
  await click('返回归档列表')
  expect(button('恢复会话').disabled).toBe(true)
})

it('discards a late closed preview and clears previous history when a refresh loses permission', async () => {
  await render()
  let finish!: (events: SessionEvent[]) => void
  vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { finish = resolve as typeof finish }))
  await click('查看历史')
  const signal = vi.mocked(api.request).mock.calls.at(-1)![1]!.signal!
  await click('返回归档列表')
  expect(signal.aborted).toBe(true)
  await click('查看历史')
  await act(async () => finish([{ ...history[0], content: 'Stale private history' }]))
  expect(document.body.textContent).not.toContain('Stale private history')
  expect(document.body.textContent).toContain('Saved answer')
  vi.mocked(api.request).mockRejectedValueOnce(new Error('access revoked'))
  await click('刷新')
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('access revoked')
  expect(document.body.textContent).not.toContain('Saved answer')
})

it('bounds rendered history pages and exposes every original event without another network read', async () => {
  await render()
  vi.mocked(api.request).mockResolvedValueOnce(Array.from({ length: 103 }, (_, index) => ({ ...history[0], seq: index, content: `Archived entry ${index}` })))
  await click('查看历史')
  expect(document.querySelectorAll('[data-archive-item]')).toHaveLength(50)
  await click('下一页')
  expect(document.querySelector('[data-archive-item]')?.textContent).toContain('Archived entry 50')
  await click('下一页')
  expect(document.querySelectorAll('[data-archive-item]')).toHaveLength(3)
  expect(button('下一页').disabled).toBe(true)
  await click('原始事件')
  expect(document.querySelector('[data-archive-event]')?.getAttribute('data-archive-event')).toBe('0')
  expect(button('上一页').disabled).toBe(true)
  expect(api.request).toHaveBeenCalledTimes(2)
})
