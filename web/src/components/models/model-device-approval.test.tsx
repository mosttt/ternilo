import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { ModelDeviceApproval } from './model-device-approval'

const workbench = vi.hoisted(() => ({ platform: true, authRequired: false, serverIdentity: { user: { user_id: 'alice', username: 'Alice' } }, logout: vi.fn() }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))

const unlimited = { monthly_tokens: null, max_concurrent_requests: null, expires_at_ms: null }
const review = {
  device_name: 'Requested model client', user_code: 'ABCD-EFGH', expires_at_ms: 2_500_000_000_000,
  providers: [{ provider_id: 'private', provider_name: 'Private provider', models: [{ model_id: 'alpha', display_name: 'Alpha', protocol: 'openai-responses', defaults: { context_window: 32000, max_output_tokens: 1024 } }] }],
}
let root: Root
let host: HTMLDivElement

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  setupChoiceSelect()
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  history.replaceState({}, '', '/model-device?code=ABCD-EFGH')
  workbench.platform = true; workbench.authRequired = false
  workbench.serverIdentity.user = { user_id: 'alice', username: 'Alice' }
  workbench.logout.mockReset()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (options?.method === 'POST') return undefined as never
    if (path.startsWith('/model-access/catalog')) return { entitlements: [], next_cursor: null } as never
    return review as never
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); history.replaceState({}, '', '/'); vi.restoreAllMocks(); vi.unstubAllGlobals() })

async function render() { await act(async () => root.render(<LocaleProvider><ModelDeviceApproval /></LocaleProvider>)) }
function button(label: string) {
  const element = [...host.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === label)
  expect(element, label).toBeDefined()
  return element!
}
async function click(element: HTMLElement) { await act(async () => element.click()) }
function field(name: string) { return host.querySelector<HTMLInputElement>(`input[name="${name}"]`)! }
async function input(element: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value)
    element.dispatchEvent(new Event('input', { bubbles: true }))
  })
}
function approvals() { return vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'POST') }
function deferred() {
  let resolve!: (value: unknown) => void
  let reject!: (error: Error) => void
  const promise = new Promise((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

it('approves from a keyboard-submitted form with three explicit null limits and unchanged account scope', async () => {
  await render()
  expect(field('monthly_tokens').value).toBe('')
  expect(field('expires_at_ms').value).toBe('')
  expect(host.textContent).toContain('不是金额')
  const form = host.querySelector('form')!
  await act(async () => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })
  expect(approvals()).toHaveLength(1)
  expect(approvals()[0][1]?.body).toEqual({ user_code: 'ABCD-EFGH', scope: { kind: 'account' }, limits: unlimited })
  expect(host.querySelector('[role="status"]')?.textContent).toBe('已允许连接。回到 Ternilo 即可继续。')
})

it('submits a restricted provider scope with future expiry, monthly tokens and concurrency', async () => {
  await render()
  await selectChoice(host.querySelector<HTMLElement>('[role="combobox"]')!, 'selected')
  expect(button('允许连接').disabled).toBe(true)
  await click(host.querySelector<HTMLInputElement>('[data-device-provider="private"] input')!)
  await input(field('monthly_tokens'), '100000')
  await input(field('max_concurrent_requests'), '3')
  await input(field('expires_at_ms'), '2037-01-02T03:04:05.678')
  await click(button('允许连接'))
  expect(approvals()[0][1]?.body).toEqual({
    user_code: 'ABCD-EFGH', scope: { kind: 'selected', grants: [], providers: [{ provider_id: 'private', model_ids: ['alpha'] }] },
    limits: { monthly_tokens: 100000, max_concurrent_requests: 3, expires_at_ms: new Date('2037-01-02T03:04:05.678').getTime() },
  })
})

it.each([
  ['monthly_tokens', '9007199254740992'], ['max_concurrent_requests', '10001'], ['expires_at_ms', '2020-01-02T03:04'],
])('rejects invalid %s on approval but still permits denial without limits', async (name, value) => {
  await render()
  await input(field(name), value)
  await click(button('允许连接'))
  expect(approvals()).toHaveLength(0)
  expect(field(name).value).toBe(value)
  expect(host.querySelector('[role="alert"]')).not.toBeNull()
  await click(button('拒绝连接'))
  expect(approvals()[0][1]?.body).toEqual({ user_code: 'ABCD-EFGH', scope: null })
  expect(host.querySelector('[role="status"]')?.textContent).toBe('已拒绝此次连接。')
})

it('preserves limits and model scope after approval fails and permits retry', async () => {
  await render()
  await click(host.querySelector<HTMLInputElement>('input[type="checkbox"]')!)
  await input(field('monthly_tokens'), '1234')
  await input(field('max_concurrent_requests'), '1')
  vi.mocked(api.request).mockRejectedValueOnce(new Error('authorization failed'))
  await click(button('允许连接'))
  expect(host.querySelector('[role="alert"]')?.textContent).toBe('authorization failed')
  expect(field('monthly_tokens').value).toBe('1234')
  expect(field('max_concurrent_requests').value).toBe('1')
  expect(host.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true)
  await click(button('允许连接'))
  expect(approvals()[1][1]?.body).toEqual({
    user_code: 'ABCD-EFGH', scope: { kind: 'account', include_account_providers: true },
    limits: { monthly_tokens: 1234, max_concurrent_requests: 1, expires_at_ms: null },
  })
})

it.each(['resolve', 'reject'] as const)('ignores a late inspect %s when the authenticated account changes', async outcome => {
  const oldReview = deferred()
  const oldCatalog = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldReview.promise as never).mockImplementationOnce(() => oldCatalog.promise as never)
  await render()
  expect(host.querySelector('[data-device-limits]')).toBeNull()
  workbench.serverIdentity.user = { user_id: 'bob', username: 'Bob' }
  vi.mocked(api.request).mockResolvedValueOnce({ ...review, device_name: 'Bob requested client', providers: [] })
  await render()
  expect(host.textContent).toContain('当前账号：Bob')
  await act(async () => {
    if (outcome === 'resolve') oldReview.resolve({ ...review, device_name: 'Alice old client' })
    else oldReview.reject(new Error('Alice old error'))
    oldCatalog.resolve({ entitlements: [], next_cursor: 'alice-cursor' })
  })
  expect(host.textContent).toContain('Bob requested client')
  expect(host.textContent).not.toContain('Alice old')
  expect(field('monthly_tokens').value).toBe('')
  expect([...host.querySelectorAll('button')].some(element => element.textContent === '加载更多')).toBe(false)
  await click(button('允许连接'))
  expect(approvals()[0][1]?.body).toEqual({ user_code: 'ABCD-EFGH', scope: { kind: 'account' }, limits: unlimited })
})

it.each(['resolve', 'reject'] as const)('does not apply an old account decision %s to the new approval', async outcome => {
  await render()
  await input(field('monthly_tokens'), '555')
  const oldDecision = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldDecision.promise as never)
  await click(button('允许连接'))
  expect(button('允许连接').disabled).toBe(true)
  workbench.serverIdentity.user = { user_id: 'bob', username: 'Bob' }
  await render()
  await input(field('monthly_tokens'), '888')
  await act(async () => {
    if (outcome === 'resolve') oldDecision.resolve(undefined)
    else oldDecision.reject(new Error('Old decision failed'))
  })
  expect(field('monthly_tokens').value).toBe('888')
  expect(host.querySelector('[role="status"]')).toBeNull()
  expect(host.querySelector('[role="alert"]')).toBeNull()
  expect(button('允许连接').disabled).toBe(false)
})

it('clears ready approval state on sign-out and ignores pending work after explicit account switching', async () => {
  await render()
  await input(field('monthly_tokens'), '555')
  workbench.authRequired = true
  await render()
  expect(host.querySelector('[data-device-limits]')).toBeNull()
  expect(host.textContent).not.toContain('Requested model client')
  const pending = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => pending.promise as never)
  workbench.authRequired = false
  await render()
  await click(button('切换账号'))
  expect(workbench.logout).toHaveBeenCalledOnce()
  await act(async () => pending.resolve(review))
  expect(host.querySelector('[data-device-limits]')).toBeNull()
  expect(button('查看连接请求').disabled).toBe(false)
})

it('does not replace a new connection-code review with the prior delayed result', async () => {
  history.replaceState({}, '', '/model-device')
  await render()
  const oldReview = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldReview.promise as never)
  await input(host.querySelector<HTMLInputElement>('#device-code')!, 'FIRST')
  await click(button('查看连接请求'))
  await input(host.querySelector<HTMLInputElement>('#device-code')!, 'SECOND')
  vi.mocked(api.request).mockResolvedValueOnce({ ...review, user_code: 'SECOND', device_name: 'Second target' })
  await click(button('查看连接请求'))
  await act(async () => oldReview.resolve({ ...review, user_code: 'FIRST', device_name: 'First target' }))
  expect(host.textContent).toContain('Second target')
  expect(host.textContent).not.toContain('First target')
  await click(button('允许连接'))
  expect(approvals()[0][1]?.body).toEqual({ user_code: 'SECOND', scope: { kind: 'account' }, limits: unlimited })
})

it('does not import an old account catalog page into the current review', async () => {
  vi.mocked(api.request).mockResolvedValueOnce(review).mockResolvedValueOnce({ entitlements: [], next_cursor: 'page-two' })
  await render()
  const pending = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => pending.promise as never)
  await click(button('加载更多'))
  workbench.serverIdentity.user = { user_id: 'bob', username: 'Bob' }
  await render()
  await act(async () => pending.reject(new Error('Old page error')))
  expect(host.querySelector('[role="alert"]')).toBeNull()
  expect(button('允许连接').disabled).toBe(false)
})

it('localizes optional limits and validation in English', async () => {
  localStorage.setItem('ternilo.locale', 'en')
  await render()
  expect(host.textContent).toContain('Device model request limits')
  expect(host.textContent).toContain('Each platform budget still applies')
  await input(field('max_concurrent_requests'), '-1')
  await click(button('Approve connection'))
  expect(host.querySelector('[role="alert"]')?.textContent).toContain('from 1 to 10000')
})
