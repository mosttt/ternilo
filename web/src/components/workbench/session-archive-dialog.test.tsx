import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { LocalSession } from '@/types'
import { SessionArchiveDialog } from './session-archive-dialog'

const session: LocalSession = {
  identity: { tenant_id: 'tenant', user_id: 'owner', agent_id: 'agent', session_id: 'archived/session' },
  workspace_id: 'workspace', workspace_path: '/workspace', title: 'Retained history', archived_at_ms: 30,
  permissions: 'workspace_write', model: { provider: 'profile_default' }, agent_preset: 'standard',
  preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 10, updated_at_ms: 20,
}
let host: HTMLDivElement
let root: Root
let mounted: boolean
const refreshed = vi.fn()

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  mounted = true
  refreshed.mockReset()
  vi.spyOn(api, 'request').mockResolvedValue([session])
})
afterEach(() => {
  if (mounted) act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve() })
}
async function render(platform = false) {
  await settle(() => root.render(<LocaleProvider><SessionArchiveDialog
    tenantId={platform ? 'tenant' : null} platform={platform} readOnly={false} workspaces={[]}
    onClose={vi.fn()} onChanged={refreshed}
  /></LocaleProvider>))
}
function restoreButton() {
  return [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '恢复会话')!
}

it('restores the existing identifier and refreshes without opening or running it', async () => {
  await render()
  expect(document.querySelector('[data-archived-session]')?.textContent).toContain('Retained history')
  vi.mocked(api.request).mockResolvedValueOnce({ ...session, archived_at_ms: null }).mockResolvedValueOnce([])
  await settle(() => restoreButton().click())
  expect(api.request).toHaveBeenCalledWith('/sessions/archived%2Fsession/restore', { method: 'POST', headers: undefined })
  expect(document.querySelector('[data-archived-session]')).toBeNull()
  expect(refreshed).toHaveBeenCalledTimes(1)
  expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'POST')).toHaveLength(1)
})

it('allows shared metadata to be read but only the owner can restore', async () => {
  vi.mocked(api.request).mockResolvedValue([{ ...session, access: {
    owner_user_id: 'owner', storage_user_id: 'owner', ownership_revision: 0, is_execution_owner: false, is_owner: false, role_limited: false, sources: [],
    permissions: { view: true, submit: true, stop: true, configure: true },
  } }])
  await render(true)
  expect(api.request).toHaveBeenCalledWith('/sessions/archived', expect.objectContaining({ headers: { 'x-ternilo-tenant': 'tenant' } }))
  expect(restoreButton().disabled).toBe(true)
  expect(button('删除').disabled).toBe(true)
  expect(document.querySelector('[data-session-archive]')?.textContent).toContain('只有会话所有者可以恢复')
})

it('keeps the archive row visible on permission or deletion failure', async () => {
  await render()
  vi.mocked(api.request).mockRejectedValueOnce(new Error('session does not exist'))
  await settle(() => restoreButton().click())
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('session does not exist')
  expect(restoreButton().disabled).toBe(false)
  expect(refreshed).not.toHaveBeenCalled()
})

it('does not refresh a new account after an old restore completes', async () => {
  await render()
  let finish: (value: unknown) => void = () => undefined
  vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { finish = resolve }) as never)
  await settle(() => restoreButton().click())
  act(() => root.unmount())
  mounted = false
  await settle(() => finish({ ...session, archived_at_ms: null }))
  expect(refreshed).not.toHaveBeenCalled()
})

function button(label: string) {
  const found = [...document.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent?.trim() === label)
  if (!found) throw new Error(`missing button ${label}`)
  return found
}

it('refreshes the archive family after restoring the parent', async () => {
  const child = { ...session, identity: { ...session.identity, session_id: 'child' }, parent_session_id: session.identity.session_id,
    subagent: { subagent_id: 'worker', provider: 'in-process', transcript_kind: 'conversation' }, title: 'Child' }
  vi.mocked(api.request).mockResolvedValueOnce([session, child])
  await render()
  expect(document.querySelectorAll('[data-archived-session]')).toHaveLength(2)
  vi.mocked(api.request).mockResolvedValueOnce({ ...session, archived_at_ms: null }).mockResolvedValueOnce([])
  await settle(() => restoreButton().click())
  expect(document.querySelectorAll('[data-archived-session]')).toHaveLength(0)
})

it('confirms one archived record and coalesces duplicate delete clicks', async () => {
  await render()
  await settle(() => button('删除').click())
  expect(document.body.textContent).toContain('永久删除“Retained history”')
  await settle(() => button('取消').click())
  expect(vi.mocked(api.request).mock.calls.some(([, options]) => options?.method === 'DELETE')).toBe(false)
  await settle(() => button('删除').click())
  let finish!: () => void
  vi.mocked(api.request).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve }) as never).mockResolvedValueOnce([])
  await settle(() => { button('永久删除').click(); button('永久删除').click() })
  expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'DELETE')).toHaveLength(1)
  expect(api.request).toHaveBeenCalledWith('/sessions/archived%2Fsession', { method: 'DELETE', headers: undefined })
  await settle(() => finish())
  expect(document.querySelector('[data-archived-session]')).toBeNull()
  expect(refreshed).toHaveBeenCalledTimes(1)
  expect(document.body.textContent).toContain('已删除“Retained history”')
})

it('keeps failed deletion visible and permits retry', async () => {
  await render()
  await settle(() => button('删除').click())
  vi.mocked(api.request).mockRejectedValueOnce(new Error('computer offline'))
  await settle(() => button('永久删除').click())
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('computer offline')
  expect(document.querySelector('[data-archived-session]')).not.toBeNull()
  expect(button('永久删除').disabled).toBe(false)
  expect(refreshed).not.toHaveBeenCalled()
})
