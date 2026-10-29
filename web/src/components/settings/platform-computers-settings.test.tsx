import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { ManagedExecutionTarget, ProjectRecord } from '@/types'
import { PlatformComputersSettings } from './platform-computers-settings'
import {
  createOwnedNodeLaunch,
  listManagedComputers,
  listOwnedComputers,
  listPlatformProjects,
  revokeOwnedComputer,
  getComputerDetails,
  updateComputer,
  setComputerSuspended,
  removeComputerRegistration,
  recoverNodeLaunch,
} from './platform-admin-api'

vi.mock('./platform-admin-api', () => ({
  createNodeLaunch: vi.fn(),
  createOwnedNodeLaunch: vi.fn(),
  listManagedComputers: vi.fn(),
  listOwnedComputers: vi.fn(),
  listPlatformProjects: vi.fn(),
  revokeComputer: vi.fn(),
  revokeOwnedComputer: vi.fn(),
  getComputerDetails: vi.fn(), updateComputer: vi.fn(), setComputerSuspended: vi.fn(), removeComputerRegistration: vi.fn(),
  recoverNodeLaunch: vi.fn(),
}))

const computer: ManagedExecutionTarget = {
  executor_id: 'home-node', name: '工作电脑', project_id: 'project-a', state: 'active', connected: true,
  enrolled_at_ms: 1_700_000_000_000, last_seen_at_ms: 1_700_000_000_100,
  management: { name: '工作电脑', notes: '', suspended_at_ms: null, removed_at_ms: null, revision: 0 },
}
const project: ProjectRecord = {
  tenant_id: 'tenant-a', project_id: 'project-a', name: 'Project A', created_at_ms: 1,
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  vi.spyOn(api, 'request').mockResolvedValue({ connected: true, target_version: '0.2.23', availability: 'current', info: { version: '0.2.23' }, job: null })
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.mocked(listManagedComputers).mockResolvedValue([computer])
  vi.mocked(listOwnedComputers).mockResolvedValue([computer])
  vi.mocked(listPlatformProjects).mockResolvedValue([project])
  vi.mocked(createOwnedNodeLaunch).mockResolvedValue({
    command: 'ternilo serve --token secret --node-id home-node',
    executorId: 'home-node',
    name: '工作电脑',
  })
  vi.mocked(revokeOwnedComputer).mockResolvedValue(undefined)
  vi.mocked(setComputerSuspended).mockResolvedValue({ management: computer.management })
  vi.mocked(removeComputerRegistration).mockResolvedValue(undefined)
  vi.mocked(updateComputer).mockResolvedValue({ management: { ...computer.management, name: '开发电脑', revision: 1 } })
  vi.mocked(getComputerDetails).mockResolvedValue({ connected: true, details: { executor: computer, management: computer.management, owner: { user_id: 'owner', username: 'owner' }, hello: null, workspace_count: 2, session_count: 5, credential_issued_at_ms: 1_700_000_000_000, credential_last_used_at_ms: null } })
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  vi.clearAllMocks()
  vi.useRealTimers()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function settle(action?: () => void) {
  await act(async () => {
    action?.()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

function refreshButton() {
  const found = [...host.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent?.trim() === '刷新')
  if (!found) throw new Error('missing refresh button')
  return found
}

function setInput(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
  setter?.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

function button(text: string, root: ParentNode = document) {
  const found = [...root.querySelectorAll<HTMLButtonElement>('button')]
    .find(item => item.textContent?.trim() === text)
  if (!found) throw new Error(`missing button ${text}`)
  return found
}

describe('Platform computer states', () => {
  it('distinguishes loading, empty, and ready data', async () => {
    let resolveComputers!: (value: ManagedExecutionTarget[]) => void
    let resolveProjects!: (value: ProjectRecord[]) => void
    vi.mocked(listManagedComputers).mockReturnValueOnce(new Promise(resolve => { resolveComputers = resolve }))
    vi.mocked(listPlatformProjects).mockReturnValueOnce(new Promise(resolve => { resolveProjects = resolve }))
    act(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" /></LocaleProvider>))
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('loading')

    await act(async () => {
      resolveComputers([])
      resolveProjects([])
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('empty')

    vi.mocked(listManagedComputers).mockResolvedValueOnce([computer])
    vi.mocked(listPlatformProjects).mockResolvedValueOnce([project])
    await settle(() => refreshButton().click())
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('工作电脑')
    expect(host.textContent).toContain('Project A')
  })

  it('keeps the last ready list visible while refresh is pending and after a local error', async () => {
    await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" /></LocaleProvider>))
    expect(host.textContent).toContain('工作电脑')
    let rejectComputers!: (cause: Error) => void
    vi.mocked(listManagedComputers).mockReturnValueOnce(new Promise((_resolve, reject) => { rejectComputers = reject }))
    vi.mocked(listPlatformProjects).mockResolvedValueOnce([project])

    await act(async () => {
      refreshButton().click()
      await Promise.resolve()
    })
    expect(host.textContent).toContain('工作电脑')
    expect(host.textContent).toContain('正在加载')

    await act(async () => {
      rejectComputers(new Error('offline'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('工作电脑')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('电脑列表加载失败')
  })

  it('uses member-owned copy, enrollment, listing, and revoke paths for My machines', async () => {
    await settle(() => root.render(
      <LocaleProvider><PlatformComputersSettings tenantId="tenant-member" scope="owned" /></LocaleProvider>,
    ))

    expect(host.querySelector('[data-my-computers]')).toBeTruthy()
    expect(host.querySelector('[data-platform-computers]')).toBeNull()
    expect(host.textContent).toContain('我的机器')
    expect(host.textContent).toContain('当前账号、当前空间')
    expect(host.textContent).toContain('我的已登记电脑')
    expect(listOwnedComputers).toHaveBeenCalledWith('tenant-member',false)
    expect(listManagedComputers).not.toHaveBeenCalled()

    const id = host.querySelector<HTMLInputElement>('#platform-computer-name')!
    await settle(() => setInput(id, 'member-laptop'))
    await settle(() => button('生成启动命令', host).click())
    expect(createOwnedNodeLaunch).toHaveBeenCalledWith('tenant-member', {
      name: 'member-laptop',
      projectId: undefined,
    })
    expect(document.querySelector('[data-node-launch-command]')?.textContent).toContain('--token secret')

    await settle(() => button('吊销电脑', host).click())
    expect(document.body.textContent).toContain('吊销这台电脑？')
    const confirmation = document.querySelector<HTMLElement>('[data-settings-dialog]')!
    await settle(() => button('吊销电脑', confirmation).click())
    expect(revokeOwnedComputer).toHaveBeenCalledWith('tenant-member', 'home-node')
    expect(host.textContent).toContain('电脑已吊销')
  })
})

it('loads details on demand and saves metadata with the observed version', async () => {
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  expect(getComputerDetails).not.toHaveBeenCalled()
  await settle(() => button('详情与编辑', host).click())
  expect(getComputerDetails).toHaveBeenCalledWith('tenant-a', 'home-node', 'owned', expect.any(AbortSignal))
  const dialog = document.querySelector('[data-computer-details]')!
  await settle(() => setInput(dialog.querySelector<HTMLInputElement>('input')!, '开发电脑'))
  await settle(() => button('保存电脑信息', dialog).click())
  expect(updateComputer).toHaveBeenCalledWith('tenant-a', 'home-node', 'owned', { name: '开发电脑', notes: '', expected_revision: 0 })
  expect(document.querySelector('[data-computer-details]')).toBeNull()
})

it('requires confirmation for suspending access and removing only the registration', async () => {
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  await settle(() => button('暂停接入', host).click())
  expect(setComputerSuspended).not.toHaveBeenCalled()
  const confirmation = document.querySelector('[data-settings-dialog]')!
  await settle(() => button('暂停接入', confirmation).click())
  expect(setComputerSuspended).toHaveBeenCalledWith('tenant-a', 'home-node', 'owned', { suspended: true, expected_revision: 0 })
  await settle(() => button('移除登记', host).click())
  expect(document.body.textContent).toContain('关联工作区、会话历史和电脑上的项目文件全部保留')
  expect(removeComputerRegistration).not.toHaveBeenCalled()
  await settle(() => button('移除登记', document.querySelector('[data-settings-dialog]')!).click())
  expect(removeComputerRegistration).toHaveBeenCalledWith('tenant-a', 'home-node', 'owned', 0)
})

it('closes the current computer details when the space changes', async () => {
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  await settle(() => button('详情与编辑', host).click())
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-b" scope="owned" /></LocaleProvider>))
  expect(document.querySelector('[data-computer-details]')).toBeNull()
  expect(getComputerDetails).toHaveBeenCalledTimes(1)
})

it('loads removed registrations only on request and recovers the observed original identity', async () => {
  const removed: ManagedExecutionTarget = { ...computer, state: 'revoked', connected: false, management: { ...computer.management, removed_at_ms: 42, revision: 7 } }
  vi.mocked(listOwnedComputers).mockImplementation(async (_tenant, includeRemoved) => includeRemoved ? [removed] : [])
  vi.mocked(recoverNodeLaunch).mockResolvedValue({ command: 'ternilo serve --node-id home-node --token ter_n_recovered',executorId: 'home-node',name: '恢复电脑',recovery:true })
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  expect(listOwnedComputers).toHaveBeenCalledWith('tenant-a',false)
  expect(host.querySelector('[data-platform-computer]')).toBeNull()
  await settle(() => button('查看已移除登记',host).click())
  expect(listOwnedComputers).toHaveBeenLastCalledWith('tenant-a',true)
  expect(host.textContent).toContain('工作电脑')
  expect(host.textContent).not.toContain('home-node')
  expect(getComputerDetails).not.toHaveBeenCalled()
  await settle(() => button('恢复原电脑接入',host).click())
  const dialog = document.querySelector('[data-computer-recovery]')!
  await settle(() => setInput(dialog.querySelector<HTMLInputElement>('input')!,'恢复电脑'))
  expect(recoverNodeLaunch).not.toHaveBeenCalled()
  await settle(() => button('生成恢复命令',dialog).click())
  expect(recoverNodeLaunch).toHaveBeenCalledWith('tenant-a',removed,'owned','恢复电脑')
  expect(document.querySelector('[data-node-launch-command]')?.textContent).toContain('home-node')
  expect(document.body.textContent).toContain('正常停止原实例')
})

it('shares refreshed version status from the update dialog back to its computer row', async () => {
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  const row = host.querySelector('[data-platform-computer="home-node"]')!
  expect(row.textContent).toContain('当前 0.2.23 → Server 配套版本 0.2.23')
  expect(row.querySelector('[data-computer-version-state="current"]')).not.toBeNull()
  expect(vi.mocked(api.request).mock.calls.every(([path]) => !path.includes('refresh=true'))).toBe(true)
  vi.mocked(api.request).mockResolvedValue({ connected: false, target_version: '0.2.23', availability: 'offline', info: { version: '0.2.22' }, job: null })
  await settle(() => button('更新客户端', row).click())
  expect(row.textContent).toContain('当前 0.2.22 → Server 配套版本 0.2.23')
  expect(row.textContent).toContain('维护通道离线，连接后才能更新')
  expect(row.querySelector('[data-computer-version-state="current"]')).toBeNull()
  const dialog = document.querySelector('[data-computer-updates]')!
  expect(dialog.textContent).toContain('当前 0.2.22 → Server 配套版本 0.2.23')
  expect(dialog.querySelector('[data-computer-version-state="offline"]')).not.toBeNull()
})

it('refreshes an expired maintenance lease without reopening settings or querying the computer', async () => {
  vi.useFakeTimers()
  vi.mocked(listOwnedComputers).mockResolvedValue([{ ...computer, connected: false }])
  const initial = { connected: true, execution_connected: false, target_version: '0.2.23', availability: 'available', info: { version: '0.2.22' }, job: null }
  vi.mocked(api.request).mockResolvedValue(initial)
  await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" scope="owned" /></LocaleProvider>))
  expect(host.querySelector('[data-computer-version-state="available"]')).not.toBeNull()
  vi.mocked(api.request).mockResolvedValue({ ...initial, connected: false, availability: 'offline' })
  await act(async () => { await vi.advanceTimersByTimeAsync(3000) })
  expect(host.querySelector('[data-computer-version-state="offline"]')).not.toBeNull()
  expect(vi.mocked(api.request).mock.calls.every(([url]) => !url.includes('refresh=true'))).toBe(true)
})
