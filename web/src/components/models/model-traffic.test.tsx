import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { ModelTrafficAdmin, OwnModelTraffic } from './model-traffic'

const state = vi.hoisted(() => ({ serverIdentity: { user: { user_id: 'alice' } } }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => state }))
vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))
const unlimited = { requests_per_minute: null, max_concurrent_requests: null }
const defaults = { requests_per_minute: 25, max_concurrent_requests: 2 }
const policy = { revision: 4, policy: { platform: unlimited, account_default: defaults } }
const account = { user_id: 'alice', username: 'Alice', revision: 2, limits: null, effective: defaults, platform: unlimited, recent_requests: 5, active_requests: 1 }
let host: HTMLDivElement, root: Root
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  state.serverIdentity.user.user_id = 'alice'
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.mocked(api.request).mockImplementation(async path => {
    if (path.includes('/accounts?')) return { accounts: [{ user_id: 'alice', username: 'Alice', name: 'Alice', kind: 'user', space_name: null }], next_cursor: null }
    return path.endsWith('/accounts/alice') || path === '/model-access/traffic' ? account : policy
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.resetAllMocks() })
async function render(element: React.ReactNode) { await act(async () => root.render(<LocaleProvider>{element}</LocaleProvider>)) }
async function click(element: HTMLElement) { await act(async () => element.click()) }
function button(name: string, scope: ParentNode = host) { return [...scope.querySelectorAll<HTMLButtonElement>('button')].find(value => value.textContent === name)! }
async function submit(form: HTMLFormElement) { await act(async () => form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))) }
async function input(element: HTMLInputElement, value: string) {
  await act(async () => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value); element.dispatchEvent(new Event('input', { bubbles: true })) })
}
it('loads own limits only when opened and discards a late result after changing account', async () => {
  let resolve: (value: unknown) => void = () => {}
  vi.mocked(api.request).mockImplementationOnce(() => new Promise(done => { resolve = done }))
  await render(<OwnModelTraffic />)
  expect(api.request).not.toHaveBeenCalled()
  const details = host.querySelector('details')!
  await act(async () => { details.open = true; details.dispatchEvent(new Event('toggle')) })
  const signal = vi.mocked(api.request).mock.calls[0][1]?.signal
  expect(api.request).toHaveBeenCalledTimes(1)
  state.serverIdentity.user.user_id = 'bob'
  await render(<OwnModelTraffic />)
  expect(signal?.aborted).toBe(true)
  await act(async () => resolve({ ...account, recent_requests: 987654 }))
  expect(host.textContent).not.toContain('987654')
  expect(host.textContent).toContain('5 个请求')
  await act(async () => { details.open = false; details.dispatchEvent(new Event('toggle')) })
  expect(host.textContent).not.toContain('5 个请求')
})
it('keeps auditor controls read-only and omits the account directory for model-only operators', async () => {
  await render(<ModelTrafficAdmin editable={false} readAccounts manageAccounts={false} />)
  expect(host.querySelectorAll('fieldset:disabled')).toHaveLength(2)
  expect(button('保存')).toBeUndefined()
  await click(button('Alice'))
  const editor = host.querySelector('[data-account-traffic-editor]')!
  expect(editor.querySelector<HTMLInputElement>('input[type="checkbox"]')?.disabled).toBe(true)
  expect(button('保存', editor)).toBeUndefined()
  await render(<ModelTrafficAdmin editable readAccounts={false} manageAccounts={false} />)
  expect(host.querySelector('[data-account-traffic-editor]')).toBeNull()
  expect(host.textContent).not.toContain('账号与服务账号的自定义限制')
})
it('submits policy revisions and keeps unsaved fields when another administrator wins', async () => {
  await render(<ModelTrafficAdmin editable readAccounts={false} manageAccounts={false} />)
  await input(host.querySelector<HTMLInputElement>('input')!, '100')
  vi.mocked(api.request).mockRejectedValueOnce(new Error('reload before saving'))
  await submit(host.querySelector('form')!)
  expect(api.request).toHaveBeenLastCalledWith('/admin/models/traffic', { method: 'PUT', body: { revision: 4, policy: { platform: { ...unlimited, requests_per_minute: 100 }, account_default: defaults } } })
  expect(host.querySelector('[role="alert"]')?.textContent).toBe('reload before saving')
  expect(host.querySelector<HTMLInputElement>('input')?.value).toBe('100')
  expect(host.textContent).not.toContain('请求限制已保存。')
})
it('distinguishes an explicit unlimited override from inheriting defaults', async () => {
  await render(<ModelTrafficAdmin editable readAccounts manageAccounts />)
  await click(button('Alice'))
  const editor = host.querySelector<HTMLFormElement>('[data-account-traffic-editor]')!
  const inherit = editor.querySelector<HTMLInputElement>('input[type="checkbox"]')!
  expect(inherit.checked).toBe(true)
  await click(inherit)
  for (const field of editor.querySelectorAll<HTMLInputElement>('input[type="number"]')) await input(field, '')
  await submit(editor)
  expect(api.request).toHaveBeenCalledWith('/admin/models/traffic/accounts/alice', { method: 'PUT', body: { revision: 2, limits: unlimited } })
  await submit(editor)
  expect(api.request).toHaveBeenCalledWith('/admin/models/traffic/accounts/alice', { method: 'PUT', body: { revision: 2, limits: null } })
})
it('removes stale editable policy fields immediately when refreshing', async () => {
  await render(<ModelTrafficAdmin editable readAccounts={false} manageAccounts={false} />)
  let resolve: (value: unknown) => void = () => {}
  vi.mocked(api.request).mockImplementationOnce(() => new Promise(done => { resolve = done }))
  await click(button('刷新'))
  expect(host.querySelector('input')).toBeNull()
  expect(button('保存')?.disabled).toBe(true)
  await act(async () => resolve({ ...policy, revision: 5 }))
  expect(host.querySelector('input')).not.toBeNull()
})
