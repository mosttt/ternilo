import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { ManagedExecutionTarget } from '@/types'
import { ComputerUpdatesDialog } from './computer-updates-dialog'
const request = vi.hoisted(() => vi.fn())
vi.mock('@/api/client', () => ({ api: { request } }))
let host: HTMLDivElement, root: Root
const computer = (id: string): ManagedExecutionTarget => ({ executor_id: id, name: id, project_id: null, connected: true, state: 'active', enrolled_at_ms: 1, last_seen_at_ms: 1, management: { name: id, notes: '', suspended_at_ms: null, removed_at_ms: null, revision: 0 } })
const status = (availability: string, job: unknown = null) => ({ connected: availability !== 'offline', target_version: '0.2.21', availability, info: { version: availability === 'current' ? '0.2.21' : '0.2.20', target: 'x86_64-unknown-linux-gnu', enabled: availability !== 'not_enabled', restart_method: 'direct', progress: null }, job })
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  request.mockReset()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.unstubAllGlobals(); vi.useRealTimers(); vi.restoreAllMocks() })
const buttons = () => [...document.querySelectorAll<HTMLButtonElement>('[data-computer-updates] button')]
const button = (label: string) => buttons().find(button => button.textContent === label)!
async function render(ids: string[]) { await act(async () => root.render(<LocaleProvider><ComputerUpdatesDialog tenantId="personal-space" computers={ids.map(computer)} onClose={vi.fn()} /></LocaleProvider>)) }

it('batch submission only targets selected eligible owned computers', async () => {
  request.mockImplementation(async (path: string, options: { method?: string; body?: { id: string } }) => {
    if (options?.method === 'POST') return { job: { id: options.body!.id, version: '0.2.21', phase: 'preparing', revision: 0, error: null } }
    return status(path.includes('/ready/') ? 'available' : path.includes('/current/') ? 'current' : 'offline')
  })
  await render(['ready', 'current', 'offline'])
  const checkboxes = [...document.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')]
  expect(checkboxes.map(input => input.disabled)).toEqual([false, true, true])
  await act(async () => button('选择所有可更新的电脑').click())
  await act(async () => button('更新所选电脑（1）').click())
  const calls = request.mock.calls.filter(([, options]) => options?.method === 'POST')
  expect(calls).toHaveLength(1)
  expect(calls[0][0]).toBe('/tenants/personal-space/my-computers/ready/updates')
  expect(calls[0][1].body.id).toMatch(/^[a-z0-9-]{16,64}$/)
  expect(document.querySelector('[data-computer-update="ready"]')?.textContent).toContain('Server 正在准备并验证发行包')
})

it('offers cancellation while waiting for work, but not while binaries are being installed', async () => {
  let waiting = { id: 'operation-12345678', version: '0.2.21', phase: 'waiting_idle', revision: 2, error: null }
  request.mockImplementation(async (path: string, options: { method?: string }) => {
    if (options?.method === 'DELETE') { waiting = { ...waiting, phase: 'cancelled', revision: 3 }; return {} }
    return status('available', path.includes('/waiting/') ? waiting : { ...waiting, id: 'installing-12345678', phase: 'installing' })
  })
  await render(['waiting', 'installing'])
  expect(buttons().filter(button => button.textContent === '取消更新')).toHaveLength(1)
  expect(button('更新所选电脑（0）').disabled).toBe(true)
  await act(async () => button('取消更新').click())
  expect(request).toHaveBeenCalledWith('/tenants/personal-space/my-computers/waiting/updates/operation-12345678', expect.objectContaining({ method: 'DELETE' }))
  expect(document.querySelector('[data-computer-update="waiting"]')?.textContent).toContain('更新已取消')
})

it('requires explicit local enablement and never silently enables or updates a computer', async () => {
  request.mockResolvedValue(status('not_enabled'))
  await render(['disabled'])
  expect(button('更新所选电脑（0）').disabled).toBe(true)
  expect(document.querySelector('[data-computer-update]')?.textContent).toContain('ternilo updates enable --help')
  expect(request.mock.calls.every(([, options]) => !options?.method)).toBe(true)
})

it('keeps current offline state visible after an earlier successful update', async () => {
  request.mockResolvedValue(status('offline', { id: 'completed-12345678', version: '0.2.21', phase: 'completed', revision: 5, error: null }))
  await render(['offline'])
  const row = document.querySelector('[data-computer-update]')!
  expect(row.querySelector('[role="status"]')?.textContent).toContain('维护通道离线，连接后才能更新')
  expect(row.textContent).toContain('最近一次更新：更新完成，电脑已重新连接')
  expect(button('更新所选电脑（0）').disabled).toBe(true)
})

it.each([
  ['unavailable: client update blocked by an unobserved execution resource: process-previous; session=previous-session', '电脑保留了上次运行的进程记录'],
  ['policy_denied: configured service does not own this Ternilo process; check its exact service name before updating', 'Supervisor 请使用 status 列出的完整名称'],
])('explains a failed update without hiding the next action in diagnostics: %s', async (error, explanation) => {
  request.mockResolvedValue(status('available', { id: 'failed-operation-123', version: '0.2.21', phase: 'failed', revision: 3, error }))
  await render(['blocked'])
  const row = document.querySelector('[data-computer-update="blocked"]')!
  expect([...row.querySelectorAll('p')].some(paragraph => !paragraph.closest('details') && paragraph.textContent?.includes(explanation))).toBe(true)
  expect(row.querySelector('details')?.textContent).toContain(error)
  expect(button('更新所选电脑（1）').disabled).toBe(false)
  expect(request.mock.calls.every(([, options]) => !options?.method)).toBe(true)
})

it('shows versions with a check or update arrow, and gives unavailable computers an X with a reason', async () => {
  request.mockImplementation(async (path: string) => status(path.includes('/current/') ? 'current' : path.includes('/ready/') ? 'available' : 'offline'))
  await render(['current', 'ready', 'offline'])
  const current = document.querySelector('[data-computer-update="current"]')!
  const ready = document.querySelector('[data-computer-update="ready"]')!
  const offline = document.querySelector('[data-computer-update="offline"]')!
  expect(current.textContent).toContain('当前 0.2.21 → Server 配套版本 0.2.21')
  expect(current.querySelector('svg[aria-label="已与 Server 配套，无需更新"]')).not.toBeNull()
  expect(current.textContent).not.toContain('已与 Server 配套，无需更新')
  expect(ready.querySelector('svg[aria-label="可更新，任务结束后安装"]')).not.toBeNull()
  expect(ready.textContent).not.toContain('可更新，任务结束后安装')
  expect(offline.querySelector('svg[aria-label="维护通道离线，连接后才能更新"]')).not.toBeNull()
  expect(offline.textContent).toContain('维护通道离线，连接后才能更新')
})

it('marks a matching online version as current even when remote upgrades are not enabled', async () => {
  const matching = status('not_enabled')
  matching.info.version = matching.target_version
  request.mockResolvedValue(matching)
  await render(['matching'])
  const row = document.querySelector('[data-computer-update]')!
  expect(row.querySelector('[data-computer-version-state="current"]')).not.toBeNull()
  expect(row.textContent).not.toContain('请先在这台电脑上启用远程升级')
  expect(row.textContent).not.toContain('ternilo updates enable --help')
  expect(button('更新所选电脑（0）').disabled).toBe(true)
})

it('keeps a healthy matching release distinct from an unconfirmed earlier update result', async () => {
  const error = 'conflict: another process is using this client installation; stop it before retrying the update'
  request.mockResolvedValue(status('current', { id: 'installed-operation-123', version: '0.2.21', phase: 'failed', revision: 5, error }))
  await render(['installed'])
  const row = document.querySelector('[data-computer-update="installed"]')!
  expect(row.querySelector('[data-computer-version-state="current"]')).not.toBeNull()
  expect([...row.querySelectorAll('p')].filter(paragraph => !paragraph.closest('details')).map(paragraph => paragraph.textContent).join(' ')).toBe('新版已正常运行，无需再次更新；上次自动更新未能正常确认结果，详情保留在诊断中。')
  expect(row.querySelector('details')?.textContent).toContain(error)
  expect(button('更新所选电脑（0）').disabled).toBe(true)
})

it('updates a maintenance-online computer even when execution is incompatible', async () => {
  request.mockImplementation(async (_path: string, options: { method?: string; body?: { id: string } }) => options?.method === 'POST'
    ? { job: { id: options.body!.id, version: '0.2.21', phase: 'preparing', revision: 0, error: null } }
    : { ...status('available'), execution_connected: false, execution_protocol_version: 1, server_execution_protocol_version: 2 })
  await render(['maintenance'])
  expect(document.querySelector('[data-computer-version-state="upgrade_required"]')?.textContent).toContain('已连接，需要升级才能执行任务')
  expect(button('更新所选电脑（1）').disabled).toBe(false)
  await act(async () => button('更新所选电脑（1）').click())
  expect(request.mock.calls.filter(([, options]) => options?.method === 'POST')).toHaveLength(1)
})

it('does not mark incompatible builds current merely because version strings match', async () => {
  request.mockResolvedValue({ ...status('current'), execution_connected: false, execution_protocol_version: 1, server_execution_protocol_version: 2 })
  await render(['incompatible'])
  expect(document.querySelector('[data-computer-version-state="protocol_mismatch"]')).not.toBeNull()
  expect(document.querySelector('[data-computer-version-state="current"]')).toBeNull()
})

it('does not show a matching-version check when execution is incompatible and updates are disabled', async () => {
  const snapshot = status('not_enabled')
  snapshot.info.version = snapshot.target_version
  request.mockResolvedValue({ ...snapshot, execution_connected: false, execution_protocol_version: 1, server_execution_protocol_version: 2 })
  await render(['disabled'])
  expect(document.querySelector('[data-computer-version-state="current"]')).toBeNull()
  expect(document.querySelector('[data-computer-version-state="not_enabled"]')).not.toBeNull()
})

it('disables an idle update dialog when its maintenance lease expires', async () => {
  vi.useFakeTimers()
  request.mockResolvedValue(status('available'))
  await render(['changing'])
  expect(button('更新所选电脑（1）').disabled).toBe(false)
  request.mockResolvedValue(status('offline'))
  await act(async () => { await vi.advanceTimersByTimeAsync(3000) })
  expect(document.querySelector('[data-computer-version-state="offline"]')).not.toBeNull()
  expect(button('更新所选电脑（0）').disabled).toBe(true)
  expect(request.mock.calls.at(-1)?.[0]).not.toContain('refresh=true')
})

it('keeps an update action error visible when passive status reads succeed', async () => {
  vi.useFakeTimers()
  request.mockImplementation(async (_path: string, options: { method?: string }) => {
    if (options?.method === 'POST') throw new Error('Release package could not be verified')
    return status('available')
  })
  await render(['failed'])
  await act(async () => button('更新所选电脑（1）').click())
  expect(host.textContent).toContain('Release package could not be verified')
  await act(async () => { await vi.advanceTimersByTimeAsync(3000) })
  expect(host.textContent).toContain('Release package could not be verified')
})
