import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { TooltipProvider } from '@/components/ui/tooltip'
import type { LocalSession, ResourcePermissions, SessionServiceSnapshot } from '@/types'
import { SessionServicesDialog } from './session-services-dialog'
import { SessionHeaderActions } from './session-toolbar'

const session: LocalSession = {
  identity: { tenant_id: 'tenant', user_id: 'owner', agent_id: 'agent', session_id: 'shared/session' },
  workspace_id: 'workspace', placement: 'local_node', workspace_path: '/workspace', title: 'Shared work',
  permissions: 'workspace_write', model: { provider: 'profile_default' }, agent_preset: 'standard',
  preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 1,
}
const view: ResourcePermissions = { view: true, submit: false, stop: false, configure: false }
const full: ResourcePermissions = { ...view, submit: true, stop: true }
const path = '/sessions/shared%2Fsession/services'
const idle: SessionServiceSnapshot = { id: 'lsp:language/server', name: 'Language server', kind: 'lsp', status: 'idle', active_calls: 0 }
const running: SessionServiceSnapshot = { id: 'mcp:files', name: 'Project files', kind: 'mcp', status: 'running', active_calls: 0 }
let host: HTMLDivElement
let root: Root
let live: boolean
let services: SessionServiceSnapshot[]

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.useFakeTimers()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host); live = true
  services = [{ ...idle }, { ...running }]
  vi.spyOn(api, 'request').mockImplementation(async (url, options) => {
    if (options?.method === 'POST') {
      const service = services.find(item => url === `${path}/${encodeURIComponent(item.id)}/${url.endsWith('/start') ? 'start' : 'stop'}`)!
      service.status = url.endsWith('/start') ? 'running' : 'stopped'
      service.error = null
      return { ...service } as never
    }
    return services.map(service => ({ ...service })) as never
  })
})
afterEach(() => {
  if (live) act(() => root.unmount())
  host.remove(); vi.clearAllTimers(); vi.useRealTimers(); vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function render(permissions: ResourcePermissions = full) {
  await settle(() => root.render(<LocaleProvider><SessionServicesDialog
    session={{ ...session, access: { owner_user_id: 'owner', is_owner: false, permissions, sources: [], role_limited: false } }}
    onClose={vi.fn()}
  /></LocaleProvider>))
}
function row(id: string) {
  const element = [...document.querySelectorAll<HTMLElement>('[data-session-service]')].find(item => item.dataset.sessionService === id)
  expect(element).toBeDefined()
  return element!
}
function button(id: string, text: string) {
  const element = [...row(id).querySelectorAll('button')].find(item => item.textContent === text)
  expect(element).toBeDefined()
  return element!
}
function status(id: string) { return row(id).querySelector<HTMLElement>('[data-status]')!.dataset.status }
function posts() { return vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'POST') }
function unmount() { act(() => root.unmount()); live = false }

it('opens the service panel directly from the read-only session header', async () => {
  await settle(() => root.render(<LocaleProvider><TooltipProvider><SessionHeaderActions
    session={{ ...session, access: { owner_user_id: 'owner', is_owner: false, permissions: view, sources: [], role_limited: false } }}
    presets={{ default_id: 'standard', authorable: false, presets: [] }} readOnly
    onSetPreset={vi.fn(async () => {})} onTogglePlan={vi.fn(async () => {})} onChooseWorkspace={vi.fn()}
    onRename={vi.fn()} onFork={vi.fn(async () => {})} onArchive={vi.fn(async () => {})} onExport={vi.fn(async () => {})}
  /></TooltipProvider></LocaleProvider>))
  const entry = host.querySelector<HTMLButtonElement>('[aria-label="后台服务"]')!
  expect(entry).not.toBeNull()
  expect(entry.disabled).toBe(false)
  await settle(() => entry.click())
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain('后台服务')
  expect(button(idle.id, '启动').disabled).toBe(true)
  expect(button(running.id, '停止').disabled).toBe(true)
  expect(posts()).toHaveLength(0)
})

it('lets viewers inspect status while disabling both kinds of mutation', async () => {
  await render(view)
  expect(api.request).toHaveBeenCalledWith(path, { signal: expect.any(AbortSignal) })
  expect(status(idle.id)).toBe('idle')
  expect(status(running.id)).toBe('running')
  expect(button(idle.id, '启动').disabled).toBe(true)
  expect(button(running.id, '停止').disabled).toBe(true)
  await settle(() => { button(idle.id, '启动').click(); button(running.id, '停止').click() })
  expect(posts()).toHaveLength(0)
})

it.each([
  { permissions: { ...view, submit: true }, id: idle.id, action: 'start', label: '启动', blocked: running.id, blockedLabel: '停止', result: 'running' },
  { permissions: { ...view, stop: true }, id: running.id, action: 'stop', label: '停止', blocked: idle.id, blockedLabel: '启动', result: 'stopped' },
])('checks independent permissions for $action and routes the chosen service ID', async ({ permissions, id, action, label, blocked, blockedLabel, result }) => {
  await render(permissions)
  expect(button(id, label).disabled).toBe(false)
  expect(button(blocked, blockedLabel).disabled).toBe(true)
  await settle(() => button(id, label).click())
  expect(posts()).toEqual([[`${path}/${encodeURIComponent(id)}/${action}`, { method: 'POST' }]])
  expect(status(id)).toBe(result)
})

it.each([
  { permissions: { ...view, submit: true }, id: idle.id, label: '启动' },
  { permissions: { ...view, stop: true }, id: running.id, label: '停止' },
])('revokes $label immediately while the panel stays open', async ({ permissions, id, label }) => {
  await render(permissions)
  const previouslyEnabled = button(id, label)
  expect(previouslyEnabled.disabled).toBe(false)
  await render(view)
  expect(previouslyEnabled.disabled).toBe(true)
  await settle(() => previouslyEnabled.click())
  expect(posts()).toHaveLength(0)
})

it('keeps Stop available during startup and ignores the cancelled Start after Stop succeeds', async () => {
  services = [{ ...idle }]
  const startup = Promise.withResolvers<SessionServiceSnapshot>()
  vi.mocked(api.request).mockImplementation(async (url, options) => {
    if (url.endsWith('/start')) return startup.promise as never
    if (options?.method === 'POST') services[0] = { ...idle, status: 'stopped' }
    return services.map(service => ({ ...service })) as never
  })
  await render()
  await settle(() => button(idle.id, '启动').click())
  expect(status(idle.id)).toBe('starting')
  expect(button(idle.id, '停止').disabled).toBe(false)
  await settle(() => button(idle.id, '停止').click())
  expect(status(idle.id)).toBe('stopped')
  const requestsAfterStop = vi.mocked(api.request).mock.calls.length
  await settle(() => startup.reject(new Error('Startup was cancelled by Stop')))
  expect(status(idle.id)).toBe('stopped')
  expect(document.querySelector('[role="alert"]')).toBeNull()
  expect(button(idle.id, '启动').disabled).toBe(false)
  expect(vi.mocked(api.request).mock.calls).toHaveLength(requestsAfterStop)
})

it.each(['response', 'error'])('ignores an old polling $0 after a newer Stop refresh', async result => {
  services = [{ ...running }]
  await render()
  const polling = Promise.withResolvers<SessionServiceSnapshot[]>()
  vi.mocked(api.request).mockImplementationOnce(() => polling.promise as never)
  await act(async () => { await vi.advanceTimersByTimeAsync(1_000) })
  await settle(() => button(running.id, '停止').click())
  expect(status(running.id)).toBe('stopped')
  await settle(() => result === 'response' ? polling.resolve([{ ...running }]) : polling.reject(new Error('Old polling failure')))
  expect(status(running.id)).toBe('stopped')
  expect(document.querySelector('[role="alert"]')).toBeNull()
})

it('requires stopping the active task before stopping a service with an active RPC', async () => {
  services = [{ ...running, active_calls: 2 }]
  await render()
  expect(button(running.id, '停止').disabled).toBe(true)
  expect(row(running.id).textContent).toContain('正在处理请求，请先停止当前任务。')
  await settle(() => button(running.id, '停止').click())
  expect(posts()).toHaveLength(0)
})

it('offers independent restart and stop actions for a failed service so tasks can continue without it', async () => {
  services = [{ ...running, status: 'failed', error: 'Connection failed' }]
  await render({ ...view, stop: true })
  expect(button(running.id, '启动').disabled).toBe(true)
  expect(button(running.id, '停止').disabled).toBe(false)
  expect(row(running.id).textContent).toContain('先停止服务再继续任务')
  await render({ ...view, submit: true })
  expect(button(running.id, '启动').disabled).toBe(false)
  expect(button(running.id, '停止').disabled).toBe(true)
  await render({ ...view, stop: true })
  await settle(() => button(running.id, '停止').click())
  expect(status(running.id)).toBe('stopped')
  expect(row(running.id).textContent).not.toContain('Connection failed')
  expect(posts()).toEqual([[`${path}/${encodeURIComponent(running.id)}/stop`, { method: 'POST' }]])
})

it('aborts an in-flight poll on unmount and never schedules another poll from its late reply', async () => {
  const polling = Promise.withResolvers<SessionServiceSnapshot[]>()
  vi.mocked(api.request).mockImplementationOnce(() => polling.promise as never)
  const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
  await render()
  const signal = vi.mocked(api.request).mock.calls[0]![1]!.signal!
  unmount()
  expect(signal.aborted).toBe(true)
  await settle(() => polling.resolve([{ ...running }]))
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000) })
  expect(api.request).toHaveBeenCalledTimes(1)
  expect(document.querySelector('[data-session-service]')).toBeNull()
  expect(errors).not.toHaveBeenCalled()
})

it('does not refresh, reschedule polling, or report a late mutation failure after unmount', async () => {
  services = [{ ...idle }]
  const startup = Promise.withResolvers<SessionServiceSnapshot>()
  await render()
  vi.mocked(api.request).mockImplementationOnce(() => startup.promise as never)
  const errors = vi.spyOn(console, 'error').mockImplementation(() => {})
  await settle(() => button(idle.id, '启动').click())
  expect(status(idle.id)).toBe('starting')
  unmount()
  const requests = vi.mocked(api.request).mock.calls.length
  await settle(() => startup.reject(new Error('Late startup rejection')))
  await act(async () => { await vi.advanceTimersByTimeAsync(5_000) })
  expect(vi.mocked(api.request).mock.calls).toHaveLength(requests)
  expect(document.querySelector('[role="alert"]')).toBeNull()
  expect(errors).not.toHaveBeenCalled()
})
