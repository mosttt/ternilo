import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { api } from '@/api/client'
import type { LocalSession } from '@/types'
import { SettingsDialog, UserSettingsPage } from './settings-dialog'

const workbench = vi.hoisted(() => ({
  accountScope: 'owner:team', platform: true, remote: false, authRequired: false, loading: false,
  currentTenantId: 'team', currentTenantRole: 'owner',
  tenants: [{ tenant_id: 'team', display_name: 'Shared team' }],
  currentWorkspace: { workspace_id: 'workspace', title: 'Selected project', placement: 'cloud' },
  currentSession: null as LocalSession | null,
  serverIdentity: { is_instance_owner: true, platform_role: 'owner', instance: { managed_execution_enabled: true } },
  notify: vi.fn(), refresh: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('./general-settings', () => ({ GeneralSettings: () => <div data-personal-preferences /> }))
vi.mock('./models-settings', () => ({ ModelsSettings: () => <div data-personal-models /> }))
vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))
vi.mock('./platform-computers-settings', () => ({ PlatformComputersSettings: ({ scope }: { scope: string }) => <div data-machine-scope={scope} /> }))

let host: HTMLDivElement
let root: Root
beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  workbench.authRequired = false
  workbench.platform = true
  workbench.loading = false
  workbench.currentSession = null
  workbench.accountScope = 'owner:team'
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.clearAllMocks(); vi.unstubAllGlobals() })
async function render(section: 'general' | 'models' | 'computers') {
  await act(async () => root.render(<LocaleProvider><UserSettingsPage section={section} /></LocaleProvider>))
}

it('removes the Server models section and duplicate link while keeping account and computer settings', async () => {
  await render('general')
  expect(host.querySelector('[data-user-settings]')).not.toBeNull()
  expect(host.querySelector('[role="dialog"]')).toBeNull()
  expect(host.querySelector('[data-settings-target]')?.textContent).toBe('个人偏好与账号')
  const navigation = host.querySelector('nav')!
  expect(navigation.textContent).toContain('我的机器')
  for (const administrative of ['平台管理', 'Worker', '空间管理', '成员']) expect(navigation.textContent).not.toContain(administrative)
  expect(host.querySelector('a[href="/models"]')).toBeNull()
  expect(navigation.textContent).not.toContain('模型')
  expect(host.querySelector('a[href^="/admin"]')).toBeNull()
  await render('models')
  expect(host.querySelector('[data-personal-models]')).toBeNull()
  expect(host.querySelector('[data-settings-target]')?.textContent).toBe('个人偏好与账号')
  await render('computers')
  expect(host.querySelector('[data-machine-scope="owned"]')).not.toBeNull()
  expect(host.querySelector('[data-settings-target]')?.textContent).toContain('Shared team')
})


it('does not open private account settings before authentication finishes', async () => {
  workbench.authRequired = true
  await render('general')
  expect(host.querySelector('[role="status"]')).not.toBeNull()
  expect(host.querySelector('[data-personal-preferences]')).toBeNull()
  workbench.authRequired = false
  await render('general')
  expect(host.querySelector('[data-personal-preferences]')).not.toBeNull()
})

it('waits for deep-link space restoration and preserves the mounted form during later refreshes', async () => {
  workbench.loading = true
  await render('general')
  expect(host.querySelector('[data-personal-preferences]')).toBeNull()
  workbench.loading = false
  await render('general')
  const panel = host.querySelector('[data-personal-preferences]')
  expect(panel).not.toBeNull()
  workbench.loading = true
  await render('general')
  expect(host.querySelector('[data-personal-preferences]')).toBe(panel)
  workbench.accountScope = 'other:team'
  await render('general')
  expect(host.querySelector('[data-personal-preferences]')).toBeNull()
})


it('does not read a Node model profile from the removed Server settings section', async () => {
  workbench.currentSession = { identity: { session_id: 'node-session' }, placement: 'local_node' } as LocalSession
  await render('models')
  expect(api.request).not.toHaveBeenCalled()
  expect(host.querySelector('[data-personal-models]')).toBeNull()
})

it('preserves standalone Local model settings', async () => {
  workbench.platform = false
  await act(async () => root.render(<LocaleProvider><SettingsDialog page open initialSection="models" onOpenChange={() => {}} onSessionChanged={async () => {}} /></LocaleProvider>))
  expect(host.querySelector('[data-personal-models]')).not.toBeNull()
})
