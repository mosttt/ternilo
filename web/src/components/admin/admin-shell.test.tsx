import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { AdminShell } from './admin-shell'

const workbench = vi.hoisted(() => ({
  serverIdentity: { platform_role: 'user', user: { user_id: 'user' }, instance: { mode: 'multi_user' } },
  platform: true, authRequired: false, currentTenantId: 'personal-user', currentTenantRole: 'owner',
  tenants: [{ tenant_id: 'personal-user', kind: 'personal', display_name: 'Personal' }], logout: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('./accounts-page', () => ({ AccountsPage: () => <div data-directory /> }))
vi.mock('./space-switcher', () => ({ SpaceSwitcher: () => <div data-space-switcher /> }))
vi.mock('@/components/settings/instance-settings', () => ({ InstanceSettings: () => <div data-instance /> }))
vi.mock('@/components/settings/workers-settings', () => ({ WorkersSettings: () => <div data-workers /> }))
vi.mock('@/components/settings/platform-settings', () => ({ PlatformSettings: ({ kind }: { kind: string }) => <div data-space-kind={kind} /> }))
let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  workbench.serverIdentity.platform_role = 'user'
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.clearAllMocks() })
async function render(path: string) { await act(async () => root.render(<LocaleProvider><AdminShell path={path} /></LocaleProvider>)) }

it('does not treat a personal space owner as a platform administrator', async () => {
  await render('/admin/accounts')
  expect(host.querySelector('[data-directory]')).toBeNull()
  expect(host.textContent).toContain('你没有这个页面的管理权限')
  await render('/spaces/current')
  expect(host.querySelector('[data-space-kind="personal"]')).not.toBeNull()
})

it('gives operators execution administration while keeping account lists restricted', async () => {
  workbench.serverIdentity.platform_role = 'operator'
  await render('/admin')
  expect(host.querySelector('[data-workers]')).not.toBeNull()
  expect(host.querySelector('a[href="/admin/accounts"]')).toBeNull()
  await render('/admin/accounts')
  expect(host.querySelector('[data-directory]')).toBeNull()
})

it('keeps unknown administration paths from rendering a privileged default page', async () => {
  workbench.serverIdentity.platform_role = 'owner'
  await render('/admin/unknown')
  expect(host.querySelector('[data-instance]')).toBeNull()
  expect(host.querySelector('[role="alert"]')).not.toBeNull()
})


it('keeps platform administration navigation separate from personal and space settings', async () => {
  workbench.serverIdentity.platform_role = 'owner'
  await render('/admin/accounts')
  expect(host.querySelector('[data-directory]')).not.toBeNull()
  expect(host.querySelector('a[href="/models"]')).toBeNull()
  expect(host.querySelector('a[href="/settings"]')).toBeNull()
  expect(host.querySelector('a[href="/spaces/current"]')).toBeNull()
  await render('/spaces/current')
  expect(host.querySelector('[data-space-management]')).not.toBeNull()
  expect(host.querySelector('a[href="/admin"]')).toBeNull()
  expect(host.querySelector('a[href="/admin/accounts"]')).toBeNull()
})
