import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import type { ModelConnection } from '@/components/models/model-device-types'
import { LocaleProvider } from '@/i18n/provider'
import { ModelConnectionsSettings } from './model-connections-settings'

let root: Root
let host: HTMLDivElement
let connection: ModelConnection
const onChange = vi.fn(async () => {})

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  connection = {
    connection_id: 'local-server', name: 'My Server', server_url: 'https://models.example.test',
    session: { grants: [], providers: [], identity: {
      device_id: 'model-device', device_name: 'My laptop', user_id: 'owner', username: 'Owner',
      scope: { kind: 'account' }, limits: { monthly_tokens: 100_000, max_concurrent_requests: 2, expires_at_ms: null },
      revoked_at_ms: null, created_at_ms: 1, last_used_at_ms: null,
    } },
  }
  onChange.mockClear()
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (path === '/model-connections' && !options?.method) return [structuredClone(connection)] as never
    if (path === '/model-connections/local-server/refresh' && options?.method === 'POST') return connection as never
    throw new Error(`Unexpected request ${path}`)
  })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount()); host.remove(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals()
})

async function render() { await act(async () => root.render(<LocaleProvider><ModelConnectionsSettings onChange={onChange} /></LocaleProvider>)) }

it('shows cached model-device limits and replaces them with the explicit refresh response', async () => {
  await render()
  const summary = host.querySelector('[data-connection-limits]')!
  expect(summary.textContent).toContain('100,000')
  expect(summary.textContent).toContain('同时请求上限：2')
  expect(summary.textContent).toContain('以 Server 当前配置为准')
  expect(host.querySelector('input[name="monthly_tokens"]')).toBeNull()
  connection.session.identity.limits = { monthly_tokens: null, max_concurrent_requests: 1, expires_at_ms: null }
  await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="刷新 My Server 的模型"]')!.click())
  expect(summary.textContent).toContain('每月 token 上限：不另限制')
  expect(summary.textContent).toContain('同时请求上限：1')
  expect(summary.textContent).not.toContain('100,000')
  expect(onChange).toHaveBeenCalledOnce()
  expect(api.request).toHaveBeenCalledWith('/model-connections/local-server/refresh', { method: 'POST' })
})

it('marks the cached expiry while idle without pretending to revoke or refresh the Server credential', async () => {
  vi.useFakeTimers()
  vi.setSystemTime(new Date('2031-09-26T12:00:00Z'))
  connection.session.identity.limits!.expires_at_ms = Date.now() + 5_000
  await render()
  expect(host.textContent).not.toContain('已记录的授权时间已到期')
  await act(async () => { await vi.advanceTimersByTimeAsync(6_000) })
  expect(host.textContent).toContain('已记录的授权时间已到期，请刷新确认当前授权')
  expect(api.request).toHaveBeenCalledTimes(1)
  expect(onChange).not.toHaveBeenCalled()
})
