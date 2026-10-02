import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { navigate } from '@/app/navigation'
import { LocaleProvider } from '@/i18n/provider'
import { resetProviderInventoryForTest } from '@/domain/provider-inventory'
import type { ProviderProfile } from '@/types'
import { AccountModels, ComputerModels, type ModelComputer } from './model-sources'

const workbench = vi.hoisted(() => ({
  platform: true, currentTenantId: 'team', currentTenantRole: 'viewer',
  tenants: [{ tenant_id: 'personal', kind: 'personal', display_name: 'Personal' }, { tenant_id: 'team', kind: 'team', display_name: 'Team' }],
  serverIdentity: { user: { user_id: 'alice', username: 'alice' }, personal_tenant_id: 'personal' },
  currentSession: { identity: { session_id: 'chat-on-another-device' }, model: { provider: 'profile_default' } },
  updateSession: vi.fn(), notify: vi.fn(), refresh: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))
vi.mock('@/components/workbench/model-picker', () => ({
  ModelPicker: ({ defaultTenantId }: { defaultTenantId: string }) => <div data-default-space={defaultTenantId} />,
  persistModelSelection: vi.fn(),
}))

const profile: ProviderProfile = {
  id: 'same', source: 'user', display_name: 'Private Provider', base_url: 'https://example.test/v1',
  protocol: 'openai-chat-completions', api_key_ref: null,
  defaults: { context_window: 10000, max_output_tokens: 1000 },
  models: [{ id: 'same-model', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 100,
}
const snapshot: ModelComputer[] = [{ executor_id: 'node-one',name: '工作电脑', connected: true, can_configure: true, workspace_id: null, session_id: null }]
let root: Root, host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  history.replaceState({}, '', '/models')
  resetProviderInventoryForTest()
  workbench.currentTenantId = 'team'
  window.__TERNILO_BOOT__ = { platform: true, providerAuthoring: true }
  vi.mocked(api.request).mockImplementation(async path => path === '/model-computers' ? snapshot : path.startsWith('/providers') ? [profile] : { references: [], records: [] })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => {
  act(() => root.unmount()); host.remove(); vi.clearAllMocks(); resetProviderInventoryForTest()
  history.replaceState({}, '', '/'); delete window.__TERNILO_BOOT__
})
async function render(element: React.ReactNode) { await act(async () => root.render(<LocaleProvider>{element}</LocaleProvider>)) }
async function visit(path: string) { await act(async () => navigate(path)) }

it('keeps account configuration in the personal space while chat and team permissions change', async () => {
  await render(<AccountModels />)
  expect(api.request).toHaveBeenCalledWith('/providers', { headers: { 'x-ternilo-tenant': 'personal' } })
  expect(host.querySelector('[data-default-space]')?.getAttribute('data-default-space')).toBe('personal')
  const edit = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '编辑')!
  await act(async () => edit.click())
  const editor = host.querySelector('[data-provider-editor]')
  workbench.currentTenantId = 'another-chat-space'
  await render(<AccountModels />)
  expect(host.querySelector('[data-provider-editor]')).toBe(editor)
  expect(workbench.updateSession).not.toHaveBeenCalled()
  expect(vi.mocked(api.request).mock.calls.every(([path]) => !path.includes('session_id'))).toBe(true)
  await visit('/models?account_space=team')
  expect(api.request).not.toHaveBeenCalledWith('/providers', { headers: { 'x-ternilo-tenant': 'team' } })
  expect(host.querySelector('[data-provider-editor]')).toBe(editor)
  expect(host.querySelector('[data-default-space]')?.getAttribute('data-default-space')).toBe('personal')
})

it('does not reuse old device resources under a new space while its response is pending', async () => {
  history.replaceState({}, '', '/models?source=device&space=team&computer=node-one')
  await render(<ComputerModels />)
  expect(host.textContent).toContain('工作电脑')
  expect(host.textContent).not.toContain('node-one')
  expect(api.request).toHaveBeenCalledWith('/providers?executor_id=node-one', { headers: { 'x-ternilo-tenant': 'team' } })
  let resolveState!: (value: ModelComputer[]) => void
  vi.mocked(api.request).mockImplementation(async path => path === '/model-computers' ? new Promise(resolve => { resolveState = resolve }) : path.startsWith('/providers') ? [profile] : { references: [], records: [] })
  await visit('/models?source=device&space=personal&computer=node-one')
  expect(host.querySelector('[data-provider-scope="node"]')).toBeNull()
  expect(api.request).not.toHaveBeenCalledWith('/providers?executor_id=node-one', { headers: { 'x-ternilo-tenant': 'personal' } })
  await act(async () => resolveState([]))
  expect(host.querySelector('[data-model-computer]')).toBeNull()
})

it('keeps a shared device catalog read-only and does not require ownership of the chat to edit account sources', async () => {
  history.replaceState({}, '', '/models?source=device&space=team&computer=node-one')
  const shared = structuredClone(snapshot)
  shared[0].can_configure = false
  shared[0].session_id = 'shared-session'
  vi.mocked(api.request).mockImplementation(async path => path === '/model-computers' ? shared : path.startsWith('/providers') ? [profile] : { references: [], records: [] })
  await render(<ComputerModels />)
  expect(host.textContent).toContain('Private Provider')
  expect(host.textContent).toContain('共享来源')
  expect([...host.querySelectorAll('button')].some(button => /编辑|删除|添加 Provider/.test(button.textContent ?? ''))).toBe(false)
  await render(<AccountModels />)
  expect([...host.querySelectorAll('button')].some(button => button.textContent === '编辑')).toBe(true)
})

it('does not contact an offline device or alter the active chat when managing its directory', async () => {
  history.replaceState({}, '', '/models?source=device&space=team&computer=node-one')
  const offline = structuredClone(snapshot)
  offline[0].connected = false
  vi.mocked(api.request).mockResolvedValue(offline)
  await render(<ComputerModels />)
  expect(host.textContent).toContain('设备离线')
  expect(vi.mocked(api.request).mock.calls.map(([path]) => path)).toEqual(['/model-computers'])
  expect(workbench.updateSession).not.toHaveBeenCalled()
})

it('refreshes an expanded computer inventory after a local edit without discarding its open draft', async () => {
  history.replaceState({}, '', '/models?source=device&space=team&computer=node-one')
  await render(<ComputerModels />)
  const edit = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '编辑')!
  await act(async () => edit.click())
  const editor = host.querySelector('[data-provider-editor]')!
  const name = editor.querySelector<HTMLInputElement>('input[id$="-provider-name-same"]')!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(name, 'Unsaved name')
    name.dispatchEvent(new Event('input', { bubbles: true }))
  })
  const added = { ...profile, id: 'new-provider', display_name: 'New Local Provider' }
  vi.mocked(api.request).mockImplementation(async path => path === '/model-computers' ? snapshot : path.startsWith('/providers') ? [profile, added] : { references: [], records: [] })
  await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="刷新"]')!.click())
  expect(host.textContent).toContain('New Local Provider')
  expect(vi.mocked(api.request).mock.calls.filter(([path]) => path === '/providers?executor_id=node-one')).toHaveLength(2)
  expect(host.querySelector('[data-provider-editor]')).toBe(editor)
  expect(name.value).toBe('Unsaved name')
  expect(workbench.updateSession).not.toHaveBeenCalled()
})

it('invalidates a collapsed computer cache when refreshing the computer list', async () => {
  history.replaceState({}, '', '/models?source=device&space=team&computer=node-one')
  await render(<ComputerModels />)
  await visit('/models?source=device&space=team')
  vi.mocked(api.request).mockImplementation(async path => path === '/model-computers' ? snapshot : path.startsWith('/providers') ? [{ ...profile, display_name: 'Changed while collapsed' }] : { references: [], records: [] })
  await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="刷新"]')!.click())
  expect(vi.mocked(api.request).mock.calls.filter(([path]) => path.startsWith('/providers'))).toHaveLength(1)
  await visit('/models?source=device&space=team&computer=node-one')
  expect(host.textContent).toContain('Changed while collapsed')
  expect(vi.mocked(api.request).mock.calls.filter(([path]) => path.startsWith('/providers'))).toHaveLength(2)
})
