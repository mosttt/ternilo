import { act, type ReactNode, useEffect, useState } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { App } from './App'
import { navigate } from './navigation'
import { useSessionRuntime } from '@/state/use-session-runtime'

const workbench = vi.hoisted(() => ({
  accountScope: 'owner:tenant', platform: true, currentTenantRole: 'owner',
  loading: false, error: null as string | null, authRequired: false,
  currentSessionId: null, currentWorkspace: null, forkOperation: null,
  tenants: [{}], toasts: [],
  createSession: vi.fn(), createWorkspace: vi.fn(), refresh: vi.fn(), notify: vi.fn(),
}))
const runtimeLifecycle = vi.hoisted(() => ({ start: vi.fn(), stop: vi.fn() }))
vi.mock('@/state/workbench', () => ({
  WorkbenchProvider: ({ children }: { children: ReactNode }) => children,
  useWorkbench: () => workbench,
  storage: { theme: 'theme' },
}))
vi.mock('@/state/use-session-runtime', () => ({ useSessionRuntime: vi.fn(() => {
  useEffect(() => { runtimeLifecycle.start(); return runtimeLifecycle.stop }, [])
  return { events: [], loading: false }
}) }))
vi.mock('@/hooks/use-workbench-layout', () => ({ useWorkbenchLayout: () => ({}) }))
vi.mock('@/pwa', () => ({ registerPwa: async () => () => undefined }))
vi.mock('@/components/layout/app-frame', () => ({ AppFrame: ({ sidebar, conversation }: { sidebar: ReactNode; conversation: ReactNode }) => <><aside>{sidebar}</aside><main>{conversation}</main></> }))
vi.mock('@/components/workbench/sidebar', () => ({ Sidebar: ({ onChooseWorkspace, onOpenSettings }: { onChooseWorkspace(): void; onOpenSettings(): void }) => <><button onClick={() => onChooseWorkspace()}>Open picker</button><button onClick={onOpenSettings}>Open settings</button></> }))
vi.mock('@/components/workbench/conversation-column', () => ({ ConversationColumn: () => null }))
vi.mock('@/components/files/files-page', () => ({ FilesPage: () => <div data-files-page /> }))
vi.mock('@/components/workbench/details-panel', () => ({ DetailsPanel: () => null }))
vi.mock('@/components/workbench/server-login', () => ({ ServerLogin: () => null }))
vi.mock('@/components/models/model-service-pages', () => ({ ModelAccessShell: () => <div data-model-center /> }))
vi.mock('@/components/workbench/directory-picker', () => ({
  DirectoryPicker: ({ open, onOpenChange }: { open: boolean; onOpenChange(open: boolean): void }) => {
    const [draft, setDraft] = useState('')
    return open ? <div data-picker-instance><input aria-label="Folder draft" value={draft} onChange={event => setDraft(event.target.value)} /><button onClick={() => onOpenChange(false)}>Close picker</button></div> : null
  },
}))
vi.mock('@/components/settings/settings-dialog', () => {
  function SettingsDialog({ open }: { open: boolean }) {
    const [draft, setDraft] = useState('')
    return open ? <div data-settings-instance><input aria-label="Settings draft" value={draft} onChange={event => setDraft(event.target.value)} /></div> : null
  }
  return { SettingsDialog, UserSettingsPage: () => <div data-user-settings><SettingsDialog key={workbench.accountScope} open /></div> }
})

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('localStorage', { getItem: () => null, setItem() {} })
  vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }))
  history.replaceState({}, '', '/')
  vi.mocked(useSessionRuntime).mockClear()
  runtimeLifecycle.start.mockClear()
  runtimeLifecycle.stop.mockClear()
  workbench.accountScope = 'owner:tenant'
  workbench.platform = true
  workbench.loading = false
  workbench.error = null
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); history.replaceState({}, '', '/'); vi.unstubAllGlobals(); vi.restoreAllMocks() })
async function render() { await act(async () => { root.render(<App />) }) }
async function click(text: string) {
  await act(async () => { [...host.querySelectorAll('button')].find(button => button.textContent === text)!.click() })
}
async function input(label: string, value: string) {
  await act(async () => {
    const field = host.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`)!
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(field, value)
    field.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

it('keeps one directory dialog and its draft while resource refreshes change surrounding notices', async () => {
  const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined)
  await render()
  await click('Open picker')
  await input('Folder draft', '/workspace/project')
  workbench.loading = true
  await render()
  workbench.loading = false
  workbench.error = 'A refresh failed'
  await render()
  workbench.error = null
  await render()
  expect(host.querySelectorAll('[data-picker-instance]')).toHaveLength(1)
  expect(host.querySelector<HTMLInputElement>('input[aria-label="Folder draft"]')?.value).toBe('/workspace/project')
  await click('Close picker')
  expect(host.querySelectorAll('[data-picker-instance]')).toHaveLength(0)
  expect(errors.mock.calls.flat().join(' ')).not.toContain('same key')
})

it('resets directory and settings drafts when switching accounts without leaving an old dialog', async () => {
  const errors = vi.spyOn(console, 'error').mockImplementation(() => undefined)
  await render()
  await click('Open picker')
  await click('Open settings')
  await input('Folder draft', '/private-owner-folder')
  await input('Settings draft', 'owner-only-value')
  workbench.accountScope = 'member:tenant'
  await render()
  expect(host.querySelectorAll('[data-picker-instance]')).toHaveLength(1)
  expect(host.querySelectorAll('[data-settings-instance]')).toHaveLength(1)
  expect(host.querySelector<HTMLInputElement>('input[aria-label="Folder draft"]')?.value).toBe('')
  expect(host.querySelector<HTMLInputElement>('input[aria-label="Settings draft"]')?.value).toBe('')
  expect(errors.mock.calls.flat().join(' ')).not.toContain('same key')
})

it('opens administration without initializing a conversation runtime', async () => {
  history.replaceState({}, '', '/admin/accounts')
  await render()
  expect(host.querySelector('[data-platform-admin]')).not.toBeNull()
  expect(useSessionRuntime).not.toHaveBeenCalled()
})

it('retains workbench drafts across administration navigation', async () => {
  await render()
  await click('Open picker')
  await input('Folder draft', '/unsent-project')
  await act(async () => navigate('/admin/accounts'))
  expect(host.querySelector('[data-platform-admin]')).not.toBeNull()
  await act(async () => navigate('/'))
  expect(host.querySelector('[data-platform-admin]')).toBeNull()
  expect(host.querySelector<HTMLInputElement>('input[aria-label="Folder draft"]')?.value).toBe('/unsent-project')
})


it('redirects the removed model settings deep link to the only model center without a conversation runtime', async () => {
  history.replaceState({}, '', '/settings/models')
  await render()
  expect(location.pathname).toBe('/models')
  expect(host.querySelector('[data-model-center]')).not.toBeNull()
  expect(host.querySelector('[data-user-settings]')).toBeNull()
  expect(host.querySelector('[data-platform-admin]')).toBeNull()
  expect(useSessionRuntime).not.toHaveBeenCalled()
})

it('keeps an administrator on the workbench until an explicit settings navigation', async () => {
  await render()
  expect(host.querySelector('[data-platform-admin]')).toBeNull()
  await click('Open settings')
  expect(location.pathname).toBe('/settings/general')
  expect(host.querySelector('[data-user-settings]')).not.toBeNull()
  expect(host.querySelector('[data-platform-admin]')).toBeNull()
})

it.each([false, true])('opens the Files user page directly without a conversation runtime (platform=%s)', async platform => {
  workbench.platform = platform
  history.replaceState({}, '', '/files?session_id=session-a')
  await render()
  expect(host.querySelector('[data-files-page]')).not.toBeNull()
  expect(host.querySelector('[data-platform-admin]')).toBeNull()
  expect(useSessionRuntime).not.toHaveBeenCalled()
})

it('keeps the existing conversation connection alive across files and settings navigation', async () => {
  await render()
  for (const path of ['/files', '/', '/settings', '/files', '/']) await act(async () => navigate(path))
  expect(runtimeLifecycle.start).toHaveBeenCalledTimes(1)
  expect(runtimeLifecycle.stop).not.toHaveBeenCalled()
})
