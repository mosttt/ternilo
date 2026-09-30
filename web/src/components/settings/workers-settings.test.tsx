import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { WorkersSettings } from './workers-settings'
import type { WorkerRecord } from './workers-api'

const workbench = vi.hoisted(() => ({
  serverIdentity: {
    user: { user_id: 'owner' }, is_instance_owner: true, platform_role: 'owner',
    instance: { managed_execution_enabled: true, mode: 'single_user' },
  },
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
let root: Root
let host: HTMLDivElement
let records: WorkerRecord[]
function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size }, clear: () => values.clear(),
    getItem: key => values.get(key) ?? null, key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) }, setItem: (key, value) => { values.set(key, value) },
  }
}
const record = (id: string, overrides: Partial<WorkerRecord> = {}): WorkerRecord => ({
  worker_id: id, storage_id: id, registered: false, online: false,
  created_at_ms: 1_900_000_000_000, revoked_at_ms: null, last_seen_at_ms: null, lease_expires_at_ms: null,
  ...overrides,
})
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  workbench.serverIdentity.is_instance_owner = true
  workbench.serverIdentity.platform_role = 'owner'
  workbench.serverIdentity.instance.managed_execution_enabled = true
  workbench.serverIdentity.instance.mode = 'single_user'
  workbench.serverIdentity.user.user_id = 'owner'
  records = []
  vi.stubGlobal('localStorage', memoryStorage())
  vi.stubGlobal('sessionStorage', memoryStorage())
  vi.spyOn(api, 'request').mockImplementation(async path => (path === '/admin/execution' ? { claims_paused: false, active_runs: 0, active_commands: 0 } : [...records]) as never)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals() })
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() { await settle(() => root.render(<LocaleProvider><WorkersSettings /></LocaleProvider>)) }
function button(label: string, within: ParentNode = document) {
  return [...within.querySelectorAll('button')].find(button => button.textContent === label)!
}
function input(id: string, value: string) {
  const element = document.getElementById(id) as HTMLInputElement
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('Instance Worker settings', () => {
  it('keeps Worker management private and explains disabled managed execution', async () => {
    workbench.serverIdentity.is_instance_owner = false
    workbench.serverIdentity.platform_role = 'user'
    await mount()
    expect(host.textContent).toBe('')
    expect(api.request).not.toHaveBeenCalled()
    workbench.serverIdentity.is_instance_owner = true
  workbench.serverIdentity.platform_role = 'owner'
    workbench.serverIdentity.instance.managed_execution_enabled = false
    await mount()
    expect(host.querySelector('[data-workers-disabled]')).not.toBeNull()
    expect(api.request).not.toHaveBeenCalled()
  })

  it.each(['single_user', 'multi_user'])('uses canonical online state independently of registration and %s mode', async mode => {
    workbench.serverIdentity.instance.mode = mode
    records = [record('pending'), record('offline', { registered: true }), record('online', { registered: true, online: true }), record('revoked', { registered: true, online: true, revoked_at_ms: 1_900_000_001_000 })]
    await mount()
    for (const [id, text] of [['pending', '待接入'], ['offline', '离线'], ['online', '在线'], ['revoked', '已撤销']]) {
      expect(host.querySelector(`[data-worker-id="${id}"]`)?.textContent).toContain(text)
    }
    expect(button('撤销接入', host.querySelector('[data-worker-id="revoked"]')!).disabled).toBe(true)
  })

  it('creates, copies and clears a one-time credential without saving it in browser storage', async () => {
    await mount()
    vi.mocked(api.request).mockImplementation(async (_path, options) => {
      if (options?.method === 'POST') { records = [record('worker-01')]; return { worker_id: 'worker-01', storage_id: 'worker-01', token: 'ter_w_once_secret' } as never }
      return [...records] as never
    })
    await settle(() => input('instance-worker-id', 'worker-01'))
    await settle(() => button('生成接入命令').click())
    expect(api.request).toHaveBeenCalledWith('/admin/workers', { method: 'POST', body: { worker_id: 'worker-01' } })
    expect(document.querySelector('[data-worker-setup-command]')?.textContent).toContain('ter_w_once_secret')
    await settle(() => button('复制配置命令').click())
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(expect.stringContaining("TERNILO_WORKER_TOKEN='ter_w_once_secret'"))
    await settle(() => button('我已保存，关闭').click())
    expect(document.querySelector('[data-worker-launch-dialog]')).toBeNull()
    expect(document.body.textContent).not.toContain('ter_w_once_secret')
    for (const storage of [window.localStorage, window.sessionStorage]) {
      for (let index = 0; index < storage.length; index++) expect(storage.getItem(storage.key(index)!)).not.toContain('ter_w_once_secret')
    }
  })

  it('requires a concrete confirmation before revoking the selected Worker', async () => {
    records = [record('worker-01', { registered: true, online: true })]
    await mount()
    await settle(() => button('撤销接入').click())
    const dialog = document.querySelector('[role="dialog"]')!
    expect(dialog.textContent).toContain('正在执行的任务可能中断')
    expect(vi.mocked(api.request).mock.calls.some(([, options]) => options?.method === 'DELETE')).toBe(false)
    vi.mocked(api.request).mockImplementation(async (_path, options) => {
      if (options?.method === 'DELETE') { records = [record('worker-01', { revoked_at_ms: 1_900_000_001_000 })]; return undefined as never }
      return [...records] as never
    })
    await settle(() => button('撤销接入', dialog).click())
    expect(api.request).toHaveBeenCalledWith('/admin/workers/worker-01', { method: 'DELETE' })
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    expect(host.querySelector('[data-worker-id="worker-01"]')?.textContent).toContain('已撤销')
  })
})

it('shows workers and execution as read-only for auditors', async () => {
  workbench.serverIdentity.platform_role = 'auditor'
  records = [record('online', { registered: true, online: true })]
  await mount()
  expect(host.querySelector('[data-worker-id="online"]')).not.toBeNull()
  expect(host.querySelector('#instance-worker-id')).toBeNull()
  expect([...host.querySelectorAll('button')].some(button => button.textContent === '撤销接入')).toBe(false)
  expect([...host.querySelectorAll('button')].some(button => button.textContent === '暂停领取')).toBe(false)
})
