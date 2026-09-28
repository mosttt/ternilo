import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { ModelDeviceIdentity } from './model-device-types'
import { ModelDevices } from './model-devices'

const workbench = vi.hoisted(() => ({ authRequired: false, serverIdentity: { user: { user_id: 'alice', username: 'Alice' } } }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))

const unlimited = { monthly_tokens: null, max_concurrent_requests: null, expires_at_ms: null }
const usage = { month: '2031-09', used_tokens: 321, reserved_tokens: 987, active_requests: 0 }
const original: ModelDeviceIdentity = {
  device_id: 'client-one', device_name: 'Model client', user_id: 'alice', username: 'Alice',
  scope: { kind: 'selected', grants: [{ grant_id: 'grant-one', model_ids: ['alpha'] }], providers: [{ provider_id: 'private', model_ids: ['beta'] }] },
  limits: unlimited, revoked_at_ms: null, created_at_ms: 1, last_used_at_ms: null,
}
let devices: ModelDeviceIdentity[]
let root: Root
let host: HTMLDivElement

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  workbench.authRequired = false
  workbench.serverIdentity.user.user_id = 'alice'
  devices = [{ ...original, limits: { ...unlimited } }]
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (path.endsWith('/usage')) return usage as never
    if (options?.method === 'PATCH') return { ...devices[0], limits: options.body } as never
    if (options?.method === 'DELETE') return undefined as never
    return { devices, next_cursor: null } as never
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.unstubAllEnvs() })

async function render() { await act(async () => root.render(<LocaleProvider><ModelDevices /></LocaleProvider>)) }
function button(label: string, scope: ParentNode = document) {
  const element = [...scope.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === label)
  expect(element, label).toBeDefined()
  return element!
}
function dialog() { return document.querySelector<HTMLElement>('[role="dialog"]')! }
function field(name: string) { return dialog().querySelector<HTMLInputElement>(`input[name="${name}"]`)! }
async function click(element: HTMLElement) { await act(async () => element.click()) }
async function input(name: string, value: string) {
  const element = field(name)
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value)
    element.dispatchEvent(new Event('input', { bubbles: true }))
  })
}
function mutations() { return vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'PATCH') }
function deferred() {
  let resolve!: (value: unknown) => void
  let reject!: (error: Error) => void
  const promise = new Promise((accept, fail) => { resolve = accept; reject = fail })
  return { promise, resolve, reject }
}

it('distinguishes expired and revoked model credentials and lazily reads historical usage without enabling edits', async () => {
  devices.push({ ...original, device_id: 'expired', limits: { ...unlimited, expires_at_ms: 1 } }, { ...original, device_id: 'revoked', revoked_at_ms: 2 })
  await render()
  expect(api.request).toHaveBeenCalledTimes(1)
  expect(host.textContent).toContain('账号自有和平台授权合计共享')
  expect(host.textContent).toContain('不是可远程控制的电脑登记')
  expect(host.querySelector('[data-model-device="client-one"]')?.textContent).toContain('未撤销')
  const expired = host.querySelector<HTMLElement>('[data-model-device="expired"]')!
  const revoked = host.querySelector<HTMLElement>('[data-model-device="revoked"]')!
  expect(expired.textContent).toContain('已到期')
  expect(revoked.textContent).toContain('已撤销')
  expect(button('编辑限制', expired).disabled).toBe(true)
  expect(button('编辑限制', revoked).disabled).toBe(true)
  expect(button('撤销', expired).disabled).toBe(false)
  expect(button('撤销', revoked).disabled).toBe(true)
  await click(button('查看用量', revoked))
  expect(api.request).toHaveBeenCalledWith('/model-access/devices/revoked/usage', expect.anything())
  expect(dialog().querySelector('form')).toBeNull()
  expect(dialog().querySelector('[data-device-reserved]')?.textContent).toBe('987')
  await click(button('关闭', dialog()))
  await click(button('撤销', expired))
  await click(button('撤销', dialog()))
  expect(api.request).toHaveBeenCalledWith('/model-access/devices/expired', { method: 'DELETE' })
})

it('cancels without mutation and sends all three nulls for blank limits without changing scope', async () => {
  devices[0].limits = undefined
  await render()
  await click(button('编辑限制'))
  expect(field('monthly_tokens').value).toBe('')
  expect(field('max_concurrent_requests').value).toBe('')
  expect(field('expires_at_ms').value).toBe('')
  await input('monthly_tokens', '500')
  await click(button('取消', dialog()))
  expect(mutations()).toHaveLength(0)
  await click(button('编辑限制'))
  expect(field('monthly_tokens').value).toBe('')
  await click(button('保存', dialog()))
  expect(mutations()).toEqual([['/model-access/devices/client-one', { method: 'PATCH', body: unlimited }]])
  expect(dialog()).toBeNull()
  expect(host.textContent).toContain('已限制到 1 份模型授权')
})

it('validates, preserves and clears the shared rolling request rate', async () => {
  devices[0].limits = { ...unlimited, requests_per_minute: 25 }
  await render()
  expect(host.textContent).toContain('每分钟请求上限：25')
  await click(button('编辑限制'))
  expect(field('requests_per_minute').value).toBe('25')
  for (const value of ['0', '-1', '1.5', '10001']) {
    await input('requests_per_minute', value)
    await click(button('保存', dialog()))
    expect(dialog().querySelector('[role="alert"]')?.textContent).toContain('1–10000')
    expect(mutations()).toHaveLength(0)
  }
  await input('requests_per_minute', '30')
  await click(button('保存', dialog()))
  expect(mutations()[0][1]?.body).toEqual({ ...unlimited, requests_per_minute: 30 })
  await click(button('编辑限制'))
  await input('requests_per_minute', '')
  await click(button('保存', dialog()))
  expect(mutations()[1][1]?.body).toEqual(unlimited)
})

it('preserves absolute expiry precision when changing limits and supports clearing an existing limit', async () => {
  const expiry = new Date('2037-09-26T08:10:23.456Z').getTime()
  devices[0].limits = { monthly_tokens: 900, max_concurrent_requests: 3, expires_at_ms: expiry }
  await render()
  await click(button('编辑限制'))
  expect(new Date(field('expires_at_ms').value).getTime()).toBe(expiry)
  await input('monthly_tokens', '')
  await input('max_concurrent_requests', '10000')
  const form = dialog().querySelector('form')!
  await act(async () => { form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) })
  expect(mutations()[0][1]?.body).toEqual({ monthly_tokens: null, max_concurrent_requests: 10000, expires_at_ms: expiry })
})

it('keeps the later DST instant when only the token limit changes', async () => {
  vi.stubEnv('TZ', 'America/New_York')
  const expiry = new Date('2037-11-01T06:30:23.456Z').getTime()
  devices[0].limits = { monthly_tokens: 900, max_concurrent_requests: 3, expires_at_ms: expiry }
  await render()
  await click(button('编辑限制'))
  expect(field('expires_at_ms').value).toBe('2037-11-01T01:30:23.456')
  expect(new Date(field('expires_at_ms').value).getTime()).toBe(expiry - 3_600_000)
  await input('monthly_tokens', '800')
  await click(button('保存', dialog()))
  expect(mutations()[0][1]?.body).toEqual({ monthly_tokens: 800, max_concurrent_requests: 3, expires_at_ms: expiry })
})

it.each([
  ['9999-12-31T18:59:59.999', true],
  ['9999-12-31T19:00', false],
] as const)('validates the absolute expiry ceiling after converting local time %s', async (value, accepted) => {
  vi.stubEnv('TZ', 'America/New_York')
  await render()
  await click(button('编辑限制'))
  await input('expires_at_ms', value)
  await click(button('保存', dialog()))
  if (accepted) {
    expect(mutations()[0][1]?.body).toEqual({ ...unlimited, expires_at_ms: 253_402_300_799_999 })
    expect(dialog()).toBeNull()
  } else {
    expect(new Date(value).getTime()).toBe(253_402_300_800_000)
    expect(mutations()).toHaveLength(0)
    expect(field('expires_at_ms').value).toBe(value)
    expect(field('expires_at_ms').getAttribute('aria-invalid')).toBe('true')
    expect(dialog().querySelector('[role="alert"]')?.textContent).toContain('9999 年末（UTC）')
  }
})

it.each([
  ['monthly_tokens', '0'], ['monthly_tokens', '-1'], ['monthly_tokens', '1.5'], ['monthly_tokens', '1e3'],
  ['monthly_tokens', '9007199254740992'], ['monthly_tokens', 'abc'],
  ['max_concurrent_requests', '0'], ['max_concurrent_requests', '10001'], ['max_concurrent_requests', '2.1'],
  ['expires_at_ms', '2020-01-01T12:00'],
])('keeps invalid %s=%s visible and makes no PATCH', async (name, value) => {
  await render()
  await click(button('编辑限制'))
  await input(name, value)
  await click(button('保存', dialog()))
  expect(mutations()).toHaveLength(0)
  expect(field(name).value).toBe(value)
  expect(field(name).getAttribute('aria-invalid')).toBe('true')
  expect(document.getElementById(field(name).getAttribute('aria-describedby')!)?.getAttribute('role')).toBe('alert')
})

it('retains a failed save draft, allows correction, and never sends scope or credential fields', async () => {
  await render()
  await click(button('编辑限制'))
  await input('monthly_tokens', '500')
  await input('max_concurrent_requests', '2')
  await input('expires_at_ms', '2037-09-26T18:30')
  vi.mocked(api.request).mockRejectedValueOnce(new Error('server conflict'))
  await click(button('保存', dialog()))
  expect(dialog().textContent).toContain('保存失败：server conflict')
  expect(field('monthly_tokens').value).toBe('500')
  expect(field('max_concurrent_requests').value).toBe('2')
  expect(field('expires_at_ms').value).toBe('2037-09-26T18:30')
  await input('monthly_tokens', '9007199254740991')
  await click(button('保存', dialog()))
  expect(mutations()[1][1]?.body).toEqual({ monthly_tokens: Number.MAX_SAFE_INTEGER, max_concurrent_requests: 2, expires_at_ms: new Date('2037-09-26T18:30').getTime() })
  expect(dialog()).toBeNull()
})

it('shows unavailable usage rather than zero and preserves unknown reservations even with no active calls', async () => {
  await render()
  const pending = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => pending.promise as never)
  await click(button('编辑限制'))
  expect(dialog().querySelector('[data-device-usage] [role="status"]')).not.toBeNull()
  expect(dialog().querySelector('[data-device-used]')).toBeNull()
  await act(async () => pending.reject(new Error('unavailable')))
  expect(dialog().textContent).toContain('无法读取用量：unavailable')
  expect(dialog().querySelector('[data-device-reserved]')).toBeNull()
  await click(button('重试', dialog()))
  expect(dialog().textContent).toContain('2031-09（UTC）')
  expect(dialog().querySelector('[data-device-used]')?.textContent).toBe('321')
  expect(dialog().querySelector('[data-device-reserved]')?.textContent).toBe('987')
  expect(dialog().querySelector('[data-device-active]')?.textContent).toBe('0')
  expect(dialog().textContent).toContain('结果未知的调用仍占用预留 token')
})

it('disables editing when the original credential expires while its dialog is open', async () => {
  vi.useFakeTimers({ toFake: ['Date', 'setTimeout', 'clearTimeout'] })
  vi.setSystemTime(new Date('2031-09-26T00:00:00Z'))
  devices[0].limits = { ...unlimited, expires_at_ms: Date.now() + 2000 }
  await render()
  await click(button('编辑限制'))
  await input('expires_at_ms', '2037-09-26T18:30')
  await act(async () => { vi.advanceTimersByTime(2100) })
  expect(button('保存', dialog()).disabled).toBe(true)
  expect(dialog().textContent).toContain('请在客户端重新发起模型授权')
  await click(button('保存', dialog()))
  expect(mutations()).toHaveLength(0)
  await click(button('取消', dialog()))
  expect(button('编辑限制').disabled).toBe(true)
  expect(button('撤销').disabled).toBe(false)
})

it('does not carry a closed device draft or late usage into another target', async () => {
  devices.push({ ...original, device_id: 'client-two', device_name: 'Second', limits: { ...unlimited, monthly_tokens: 200 } })
  await render()
  const firstUsage = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => firstUsage.promise as never)
  await click(button('编辑限制', host.querySelector('[data-model-device="client-one"]')!))
  await input('monthly_tokens', '777')
  await click(button('取消', dialog()))
  await click(button('编辑限制', host.querySelector('[data-model-device="client-two"]')!))
  await act(async () => firstUsage.resolve({ ...usage, reserved_tokens: 44444 }))
  expect(field('monthly_tokens').value).toBe('200')
  expect(dialog().querySelector('[data-device-reserved]')?.textContent).toBe('987')
  await click(button('保存', dialog()))
  expect(mutations()[0][0]).toBe('/model-access/devices/client-two')
})

it.each(['resolve', 'reject'] as const)('isolates late %s from an old account save and usage request', async outcome => {
  await render()
  const oldUsage = deferred()
  const oldSave = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldUsage.promise as never)
  await click(button('编辑限制'))
  await input('monthly_tokens', '777')
  vi.mocked(api.request).mockImplementationOnce(() => oldSave.promise as never)
  await click(button('保存', dialog()))
  workbench.serverIdentity.user.user_id = 'bob'
  devices = [{ ...original, device_id: 'bob-client', device_name: 'Bob client', user_id: 'bob', limits: { ...unlimited, monthly_tokens: 50 } }]
  await render()
  expect(dialog()).toBeNull()
  expect(host.textContent).not.toContain('client-one')
  await click(button('编辑限制'))
  const callsBefore = vi.mocked(api.request).mock.calls.length
  await act(async () => {
    oldUsage.resolve({ ...usage, reserved_tokens: 77777 })
    if (outcome === 'resolve') oldSave.resolve(original)
    else oldSave.reject(new Error('old account error'))
  })
  expect(api.request).toHaveBeenCalledTimes(callsBefore)
  expect(field('monthly_tokens').value).toBe('50')
  expect(dialog().querySelector('[data-device-reserved]')?.textContent).toBe('987')
  expect(dialog().textContent).not.toContain('old account error')
  expect(button('保存', dialog()).disabled).toBe(false)
})

it('discards a late directory page and revocation after an account boundary', async () => {
  const oldList = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldList.promise as never)
  await render()
  workbench.serverIdentity.user.user_id = 'bob'
  devices = [{ ...original, device_id: 'bob-client' }]
  await render()
  await act(async () => oldList.resolve({ devices: [original], next_cursor: null }))
  expect(host.querySelector('[data-model-device="client-one"]')).toBeNull()
  await click(button('撤销'))
  const oldRevoke = deferred()
  vi.mocked(api.request).mockImplementationOnce(() => oldRevoke.promise as never)
  await click(button('撤销', dialog()))
  workbench.serverIdentity.user.user_id = 'carol'
  devices = [{ ...original, device_id: 'carol-client' }]
  await render()
  const calls = vi.mocked(api.request).mock.calls.length
  await act(async () => oldRevoke.resolve(undefined))
  expect(api.request).toHaveBeenCalledTimes(calls)
  expect(dialog()).toBeNull()
  expect(host.querySelector('[data-model-device="carol-client"]')).not.toBeNull()
})

it('renders the same limits and usage semantics in English', async () => {
  localStorage.setItem('ternilo.locale', 'en')
  await render()
  await click(button('Edit limits'))
  expect(dialog().textContent).toContain('tokens, not money')
  expect(dialog().textContent).toContain('Already accepted calls still settle')
  expect(dialog().textContent).toContain('2031-09 (UTC)')
  expect(dialog().querySelector('label[for]')).not.toBeNull()
  expect(field('monthly_tokens').inputMode).toBe('numeric')
})
