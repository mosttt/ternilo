import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api, ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { RegistrationPanel } from './registration-panel'

const workbench = vi.hoisted(() => ({ serverIdentity: { platform_role: 'owner', instance: { mode: 'multi_user' } }, notify: vi.fn() }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
let host: HTMLDivElement
let root: Root
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  workbench.serverIdentity = { platform_role: 'owner', instance: { mode: 'multi_user' } }
  vi.spyOn(api, 'request').mockImplementation(async (_path, options) => options?.body ? { ...options.body, revision: 4 } as never : { mode: 'invite', require_approval: false, revision: 3 } as never)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.clearAllMocks() })
async function settle(action?: () => void) { await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() }) }
async function mount() { await settle(() => root.render(<LocaleProvider><RegistrationPanel /></LocaleProvider>)) }
function button(label: string) { return [...host.querySelectorAll('button')].find(item => item.textContent === label)! }
async function mode(value: string) { await settle(() => { const select = host.querySelector('select')!; select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })) }) }

it('offers review only for open signup and removes account invitations before saving it', async () => {
  await mount()
  expect(host.querySelector('[data-admin-invitations]')).not.toBeNull()
  expect(host.querySelector('input[type="checkbox"]')).toBeNull()
  await mode('open')
  expect(host.querySelector('[data-admin-invitations]')).toBeNull()
  await settle(() => (host.querySelector('input[type="checkbox"]') as HTMLInputElement).click())
  await settle(() => button('保存注册设置').click())
  expect(api.request).toHaveBeenCalledWith('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: true, revision: 3 } })
  await mode('invite')
  expect(host.querySelector('input[type="checkbox"]')).toBeNull()
  await settle(() => button('保存注册设置').click())
  expect(api.request).toHaveBeenCalledWith('/admin/registration', { method: 'PATCH', body: { mode: 'invite', require_approval: false, revision: 4 } })
  expect(host.querySelector('[data-admin-invitations]')).not.toBeNull()
})

it('reloads a concurrent policy change without retrying the stale write', async () => {
  await mount()
  await mode('open')
  vi.mocked(api.request).mockRejectedValueOnce(new ApiError('Changed', 409, 'conflict'))
  await settle(() => button('保存注册设置').click())
  expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'PATCH')).toHaveLength(1)
  expect(workbench.notify).toHaveBeenCalledWith(expect.stringContaining('其他管理员修改'), 'error')
  expect(host.querySelector('select')!.value).toBe('invite')
})

it('shows policy read-only to an auditor and retains settings without invitations in single-user mode', async () => {
  workbench.serverIdentity.platform_role = 'auditor'
  workbench.serverIdentity.instance.mode = 'single_user'
  await mount()
  expect(host.querySelector('select')!.disabled).toBe(true)
  expect(host.textContent).toContain('切换到多用户后生效')
  expect(host.querySelector('[data-admin-invitations]')).toBeNull()
  expect(button('保存注册设置')).toBeUndefined()
})
