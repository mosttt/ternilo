import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { ComputerUsage } from './computer-usage'

const workbench = vi.hoisted(() => ({
  tenants: [{ tenant_id: 'space-a', kind: 'personal', display_name: 'A' }, { tenant_id: 'space-b', kind: 'personal', display_name: 'B' }],
  serverIdentity: { personal_tenant_id: 'space-a', user: { user_id: 'alice', username: 'Alice' } },
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/components/workbench/model-picker', () => ({ ModelPicker: () => null }))
const observation = (seq: number, model: string, usage: unknown = null) => ({
  session_id: 'public-session', session_title: 'Private task', run_id: 'run', started_seq: seq, started_at_ms: 1000,
  step: 1, attempt: seq, route: { provider: 'provider', model, protocol: 'openai-chat-completions' }, input_author: null,
  finished_at_ms: usage ? 2000 : null, usage, error_code: null, upstream_request_id: null,
})
const computers = (id: string) => ({ executors: [{ executor_id: id, state: 'active', connected: false,management: { name: '工作电脑' } }] })
const page = (observations: unknown[]) => ({ source: 'device_reported', period: '2026-09', observations, next_cursor: null })
let host: HTMLDivElement
let root: Root
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  history.replaceState({}, '', '/models?tab=usage&usage_source=device')
  workbench.serverIdentity = { personal_tenant_id: 'space-a', user: { user_id: 'alice', username: 'Alice' } }
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.spyOn(api, 'request')
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals() })
async function render() { await act(async () => root.render(<LocaleProvider><ComputerUsage /></LocaleProvider>)) }

it('distinguishes missing counters from reported zero and keeps offline reports read-only', async () => {
  vi.mocked(api.request).mockImplementation(async path => (path.includes('/my-computers') ? computers('computer-a') : page([
    observation(1, 'unknown-model'), observation(2, 'zero-model', { input_tokens: 0, output_tokens: 0, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null }),
  ])) as never)
  await render()
  const rows = host.querySelectorAll('[data-computer-usage-record]')
  expect(rows).toHaveLength(2)
  expect(rows[0]!.textContent).toContain('未提供')
  expect(rows[0]!.textContent).toContain('结束结果未同步')
  expect(rows[1]!.querySelector('dd')?.textContent).toBe('0')
  expect(host.textContent).toContain('电脑当前离线')
  expect(host.textContent).toContain('本页设备报告')
  expect(vi.mocked(api.request).mock.calls.every(([, options]) => options?.method === undefined)).toBe(true)
  expect(vi.mocked(api.request).mock.calls.every(([, options]) => options?.headers && (options.headers as Record<string, string>)['x-ternilo-tenant'] === 'space-a')).toBe(true)
})

it('aborts an old report and discards its late private rows after changing account and space', async () => {
  let resolveOld!: (value: unknown) => void
  let oldSignal: AbortSignal | undefined
  vi.mocked(api.request).mockImplementation(async (path, options) => {
    if (path.includes('/space-a/my-computers')) return computers('computer-a') as never
    if (path.includes('/space-b/my-computers')) return computers('computer-b') as never
    if (path.includes('/computer-a/usage')) {
      oldSignal = options?.signal ?? undefined
      return new Promise(resolve => { resolveOld = resolve }) as never
    }
    return page([observation(2, 'new-account-model')]) as never
  })
  await render()
  workbench.serverIdentity = { personal_tenant_id: 'space-b', user: { user_id: 'bob', username: 'Bob' } }
  await render()
  expect(oldSignal?.aborted).toBe(true)
  await act(async () => resolveOld(page([observation(1, 'old-private-model')])))
  expect(host.textContent).toContain('new-account-model')
  expect(host.textContent).not.toContain('old-private-model')
})
