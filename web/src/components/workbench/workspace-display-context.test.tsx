import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { Workspace } from '@/types'
import { useWorkspaceDisplay } from './workspace-display-context'

const fixture = vi.hoisted(() => ({
  request: vi.fn(), scope: 'alice:personal', role: 'owner', platform: true,
  workspace: null as Workspace | null,
}))
vi.mock('@/api/client', () => ({ api: { request: fixture.request } }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => ({
  currentWorkspace: fixture.workspace, currentSession: { placement: 'local_node' }, platform: fixture.platform,
  accountScope: fixture.scope, currentTenantId: 'personal', currentTenantRole: fixture.role,
}) }))

let root: Root, host: HTMLDivElement
function Display() { const value = useWorkspaceDisplay(); return <div data-path={value.path} data-replacement={value.replacement}>{value.label} {value.hint}</div> }
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true })
  fixture.scope = 'alice:personal'; fixture.role = 'owner'; fixture.platform = true
  fixture.workspace = { workspace_id: 'work', node_id: 'desktop-alice', title: 'Reader', path: '<local-workspace>', placement: 'local_node', status: 'online', created_at_ms: 0, updated_at_ms: 0 }
  fixture.request.mockReset()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove() })
const render = () => act(async () => root.render(<Display />))

it('shows computer, workspace and owner-authorized path without storing the path', async () => {
  fixture.request.mockResolvedValue({ status: 'available', path: '/home/alice/project' })
  await render()
  expect(host.textContent).toContain('电脑 desktop-alice · Reader')
  expect(host.querySelector('[data-path]')?.getAttribute('data-path')).toBe('/home/alice/project')
  expect(fixture.request).toHaveBeenCalledWith('/workspaces/work/location', { signal: expect.any(AbortSignal), cache: 'no-store', headers: { 'x-ternilo-tenant': 'personal' } })
})

it('does not read a private path for a shared viewer or an offline computer', async () => {
  fixture.workspace!.access = { owner_user_id: 'alice', storage_user_id: 'alice', ownership_revision: 0, is_execution_owner: false, is_owner: false, role_limited: false, permissions: { view: true, submit: true, stop: true, configure: false }, sources: [] }
  await render()
  expect(fixture.request).not.toHaveBeenCalled()
  expect(host.querySelector('[data-replacement]')?.getAttribute('data-replacement')).toBe('当前工作区「Reader」')
  fixture.workspace!.access = undefined; fixture.workspace!.status = 'offline'
  await render()
  expect(fixture.request).not.toHaveBeenCalled()
  expect(host.querySelector('[data-path]')?.getAttribute('data-path')).toBe('')
})

it('discards a late private path response after an account switch', async () => {
  let resolve!: (value: unknown) => void
  fixture.request.mockReturnValueOnce(new Promise(done => { resolve = done })).mockResolvedValue({ status: 'available', path: '/bob/project' })
  await render()
  const signal = fixture.request.mock.calls[0][1].signal as AbortSignal
  fixture.scope = 'bob:personal'
  await render()
  await act(async () => resolve({ status: 'available', path: '/alice/private' }))
  expect(signal.aborted).toBe(true)
  expect(host.querySelector('[data-path]')?.getAttribute('data-path')).toBe('/bob/project')
  expect(host.innerHTML).not.toContain('/alice/private')
})
