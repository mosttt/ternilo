import { act } from 'react'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { AccountsPage } from './accounts-page'
import type { PlatformAccount } from './admin-api'

const workbench = vi.hoisted(() => ({ serverIdentity: { user: { user_id: 'owner', username: 'owner' }, platform_role: 'owner', instance: { mode: 'multi_user', owner_user_id: 'owner' } }, notify: vi.fn() }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
let root: Root
let host: HTMLDivElement
const account = (id: string, role: PlatformAccount['platform_role'] = 'user'): PlatformAccount => ({ user_id: id, username: id, email: `${id}@example.test`, platform_role: role, role_revision: 3, status: 'active', status_revision: 1, created_at_ms: 1900000000000, personal_tenant_id: `personal-${id}` })
beforeEach(() => {
  setupChoiceSelect()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  workbench.serverIdentity.platform_role = 'owner'
  workbench.serverIdentity.user.user_id = 'owner'
  vi.spyOn(api, 'request').mockImplementation(async path => (path === '/admin/registration' ? { mode: 'invite', require_approval: false, revision: 1 } : { accounts: [account('owner', 'owner'), account('alice')], next_cursor: null }) as never)
})

async function chooseAction(id: string, action: string) {
  const trigger = host.querySelector<HTMLButtonElement>(`[data-admin-account="${id}"] button[aria-label="账号“${id}”的操作"]`)!
  await settle(() => trigger.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
  const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(element => element.textContent === action)!
  await settle(() => item.click())
}

it('keeps rejected applications available for approval and shows their contact email', async () => {
  const rejected = { ...account('reconsider'), status: 'rejected', status_revision: 8 }
  vi.mocked(api.request).mockImplementation(async path => (path === '/admin/registration'
    ? { mode: 'open', require_approval: true, revision: 2 }
    : { accounts: [rejected], next_cursor: null }) as never)
  await mount()
  const row = host.querySelector('[data-admin-account="reconsider"]')!
  expect(row.querySelector('[data-account-email]')?.textContent).toBe('reconsider@example.test')
  expect(button('拒绝申请', row)).toBeUndefined()
  await settle(() => button('通过审核', row).click())
  await settle(() => button('通过审核', document.querySelector('[role="dialog"]')!).click())
  expect(api.request).toHaveBeenCalledWith('/admin/accounts/reconsider/review', { method: 'POST', body: { decision: 'approve', status_revision: 8 } })
})

it('protects the instance owner and current administrator from account actions', async () => {
  workbench.serverIdentity.platform_role = 'admin'
  workbench.serverIdentity.user.user_id = 'my-admin'
  vi.mocked(api.request).mockImplementation(async path => (path === '/admin/registration'
    ? { mode: 'invite', require_approval: false, revision: 1 }
    : { accounts: [account('owner', 'owner'), account('my-admin', 'admin'), account('other')], next_cursor: null }) as never)
  await mount()
  expect(host.querySelector('[aria-label="账号“owner”的操作"]')).toBeNull()
  expect(host.querySelector('[aria-label="账号“my-admin”的操作"]')).toBeNull()
  expect(host.querySelector('[aria-label="账号“other”的操作"]')).not.toBeNull()
})

it.each([
  { action: 'ban', status: 'active', label: '封禁账号', description: '之后可以解除封禁' },
  { action: 'unban', status: 'banned', label: '解除封禁', description: '原账号 ID 与已有记录保留' },
  { action: 'remove', status: 'rejected', label: '注销账号', description: '不会自动删除项目文件' },
] as const)('confirms $action and submits the displayed account revision', async ({ action, status, label, description }) => {
  vi.mocked(api.request).mockImplementation(async path => (path === '/admin/registration'
    ? { mode: 'invite', require_approval: false, revision: 1 }
    : { accounts: [{ ...account('target'), status, status_revision: 9 }], next_cursor: null }) as never)
  await mount()
  await chooseAction('target', label)
  const dialog = document.querySelector('[role="dialog"]')!
  expect(dialog.textContent).toContain('target')
  expect(dialog.textContent).toContain(description)
  if (action === 'remove') expect(dialog.textContent).toContain('不能恢复')
  await settle(() => button(label, dialog).click())
  expect(api.request).toHaveBeenCalledWith('/admin/accounts/target/status', { method: 'POST', body: { action, status_revision: 9 } })
})

it('retains closed accounts for lookup without offering restoration or mutation', async () => {
  vi.mocked(api.request).mockImplementation(async path => (path === '/admin/registration'
    ? { mode: 'invite', require_approval: false, revision: 1 }
    : { accounts: [{ ...account('closed'), status: 'removed' }], next_cursor: null }) as never)
  await mount()
  expect(host.querySelector('[data-account-status="removed"]')?.textContent).toBe('已注销')
  expect(host.querySelector('[data-admin-account="closed"] button')).toBeNull()
  await selectChoice(host.querySelector<HTMLElement>('#admin-account-status-filter')!, 'removed')
  expect(api.request).toHaveBeenCalledWith('/admin/accounts?limit=25&status=removed', expect.anything())
})

it('refreshes a conflicting status change without silently repeating it', async () => {
  await mount()
  await chooseAction('alice', '封禁账号')
  vi.mocked(api.request).mockRejectedValueOnce(new ApiError('Status changed', 409, 'conflict'))
  await settle(() => button('封禁账号', document.querySelector('[role="dialog"]')!).click())
  expect(vi.mocked(api.request).mock.calls.filter(([path]) => path.endsWith('/status'))).toHaveLength(1)
  expect(workbench.notify).toHaveBeenCalledWith(expect.stringContaining('账号状态已发生变化'), 'error')
  expect(document.querySelector('[role="dialog"]')).toBeNull()
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals() })
async function settle(action?: () => void) { await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() }) }
async function mount() { await settle(() => root.render(<LocaleProvider><AccountsPage /></LocaleProvider>)) }
function button(label: string, within: ParentNode = document) { return [...within.querySelectorAll('button')].find(item => item.textContent === label)! }
function fill(id: string, value: string) {
  const element = document.getElementById(id) as HTMLInputElement
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('Platform account directory', () => {
  it('uses bounded server cursors and restarts paging for a new search', async () => {
    vi.mocked(api.request).mockImplementation(async path => {
      if (path === '/admin/registration') return { mode: 'invite', require_approval: false, revision: 1 } as never
      const query = new URL(path, 'http://server.test').searchParams
      return { accounts: [account(query.has('cursor') ? 'second' : 'first')], next_cursor: query.has('cursor') ? null : 'cursor+page2' } as never
    })
    await mount()
    expect(api.request).toHaveBeenCalledWith('/admin/accounts?limit=25', expect.objectContaining({ signal: expect.any(AbortSignal) }))
    await settle(() => button('下一页').click())
    expect(host.querySelector('[data-admin-account="second"]')).not.toBeNull()
    expect(api.request).toHaveBeenLastCalledWith('/admin/accounts?limit=25&cursor=cursor%2Bpage2', expect.anything())
    await settle(() => fill('admin-account-search', ' alice '))
    await settle(() => button('搜索').click())
    expect(api.request).toHaveBeenLastCalledWith('/admin/accounts?limit=25&query=alice', expect.anything())
    expect(button('上一页').disabled).toBe(true)
  })

  it('keeps platform role changes owner-only and preserves the role revision in a confirmed update', async () => {
    await mount()
    expect(host.querySelector('[data-admin-account="owner"] select')).toBeNull()
    const row = host.querySelector('[data-admin-account="alice"]')!
    await selectChoice(row.querySelector<HTMLElement>('[role="combobox"]')!, 'operator')
    await settle(() => button('保存职责', row).click())
    expect(vi.mocked(api.request).mock.calls.some(([, options]) => options?.method === 'PATCH')).toBe(false)
    await settle(() => button('保存职责', document.querySelector('[role="dialog"]')!).click())
    expect(api.request).toHaveBeenCalledWith('/admin/accounts/alice/role', { method: 'PATCH', body: { role: 'operator', role_revision: 3 } })
  })

  it('refreshes a conflicting account without repeating its update', async () => {
    await mount()
    const row = host.querySelector('[data-admin-account="alice"]')!
    await selectChoice(row.querySelector<HTMLElement>('[role="combobox"]')!, 'admin')
    await settle(() => button('保存职责', row).click())
    vi.mocked(api.request).mockRejectedValueOnce(new ApiError('Role changed', 409, 'conflict'))
    await settle(() => button('保存职责', document.querySelector('[role="dialog"]')!).click())
    expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'PATCH')).toHaveLength(1)
    expect(workbench.notify).toHaveBeenCalledWith(expect.stringContaining('已经变更'), 'error')
    expect(document.querySelector('[role="dialog"]')).toBeNull()
  })

  it('lets auditors read accounts without role controls or invitation creation', async () => {
    workbench.serverIdentity.platform_role = 'auditor'
    await mount()
    expect(host.querySelector('[data-admin-account="alice"]')).not.toBeNull()
    expect(host.querySelectorAll('[data-admin-account] select')).toHaveLength(0)
    expect(host.querySelector('[data-admin-invitations]')).toBeNull()
  })
})

it('filters pending accounts server-side and reviews with the displayed status revision', async () => {
  vi.mocked(api.request).mockImplementation(async (path, options) => {
    if (path === '/admin/registration') return { mode: 'open', require_approval: true, revision: 2 } as never
    return { accounts: [{ ...account('candidate'), status: options?.method ? 'active' : 'pending', status_revision: 7 }], next_cursor: null } as never
  })
  await mount()
  expect(host.querySelector('[data-admin-invitations]')).toBeNull()
  await selectChoice(document.getElementById('admin-account-status-filter')!, 'pending')
  expect(api.request).toHaveBeenCalledWith('/admin/accounts?limit=25&status=pending', expect.anything())
  const row = host.querySelector('[data-admin-account="candidate"]')!
  expect(row.querySelector('select')).toBeNull()
  await settle(() => button('通过审核', row).click())
  await settle(() => button('通过审核', document.querySelector('[role="dialog"]')!).click())
  expect(api.request).toHaveBeenCalledWith('/admin/accounts/candidate/review', { method: 'POST', body: { decision: 'approve', status_revision: 7 } })
})

it('refreshes a conflicting review without applying a second decision', async () => {
  vi.mocked(api.request).mockImplementation(async path => (path === '/admin/registration' ? { mode: 'open', require_approval: true, revision: 2 } : { accounts: [{ ...account('candidate'), status: 'pending', status_revision: 7 }], next_cursor: null }) as never)
  await mount()
  await settle(() => button('拒绝申请', host.querySelector('[data-admin-account="candidate"]')!).click())
  vi.mocked(api.request).mockRejectedValueOnce(new ApiError('Status changed', 409, 'conflict'))
  await settle(() => button('拒绝申请', document.querySelector('[role="dialog"]')!).click())
  expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'POST')).toHaveLength(1)
  expect(workbench.notify).toHaveBeenCalledWith(expect.stringContaining('已经被审核'), 'error')
  expect(document.querySelector('[role="dialog"]')).toBeNull()
})
