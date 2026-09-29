import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { ManagedExecutionTarget, ProjectRecord } from '@/types'
import { PlatformComputersSettings } from './platform-computers-settings'
import {
  createOwnedNodeLaunch,
  listManagedComputers,
  listOwnedComputers,
  listPlatformProjects,
  revokeOwnedComputer,
} from './platform-admin-api'

vi.mock('./platform-admin-api', () => ({
  createNodeLaunch: vi.fn(),
  createOwnedNodeLaunch: vi.fn(),
  listManagedComputers: vi.fn(),
  listOwnedComputers: vi.fn(),
  listPlatformProjects: vi.fn(),
  revokeComputer: vi.fn(),
  revokeOwnedComputer: vi.fn(),
}))

const computer: ManagedExecutionTarget = {
  executor_id: 'home-node', project_id: 'project-a', state: 'active', connected: true,
  enrolled_at_ms: 1_700_000_000_000, last_seen_at_ms: 1_700_000_000_100,
}
const project: ProjectRecord = {
  tenant_id: 'tenant-a', project_id: 'project-a', name: 'Project A', created_at_ms: 1,
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
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
  })
  vi.mocked(revokeOwnedComputer).mockResolvedValue(undefined)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  vi.clearAllMocks()
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
    expect(host.textContent).toContain('home-node')
    expect(host.textContent).toContain('Project A')
  })

  it('keeps the last ready list visible while refresh is pending and after a local error', async () => {
    await settle(() => root.render(<LocaleProvider><PlatformComputersSettings tenantId="tenant-a" /></LocaleProvider>))
    expect(host.textContent).toContain('home-node')
    let rejectComputers!: (cause: Error) => void
    vi.mocked(listManagedComputers).mockReturnValueOnce(new Promise((_resolve, reject) => { rejectComputers = reject }))
    vi.mocked(listPlatformProjects).mockResolvedValueOnce([project])

    await act(async () => {
      refreshButton().click()
      await Promise.resolve()
    })
    expect(host.textContent).toContain('home-node')
    expect(host.textContent).toContain('正在加载')

    await act(async () => {
      rejectComputers(new Error('offline'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('home-node')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('电脑列表加载失败')
  })

  it('uses member-owned copy, enrollment, listing, and revoke paths for My machines', async () => {
    await settle(() => root.render(
      <LocaleProvider><PlatformComputersSettings tenantId="tenant-member" scope="owned" /></LocaleProvider>,
    ))

    expect(host.querySelector('[data-my-computers]')).toBeTruthy()
    expect(host.querySelector('[data-platform-computers]')).toBeNull()
    expect(host.textContent).toContain('我的机器')
    expect(host.textContent).toContain('只属于当前账号')
    expect(host.textContent).toContain('我的已登记电脑')
    expect(listOwnedComputers).toHaveBeenCalledWith('tenant-member')
    expect(listManagedComputers).not.toHaveBeenCalled()

    const id = host.querySelector<HTMLInputElement>('#platform-computer-id')!
    await settle(() => setInput(id, 'member-laptop'))
    await settle(() => button('生成启动命令', host).click())
    expect(createOwnedNodeLaunch).toHaveBeenCalledWith('tenant-member', {
      executorId: 'member-laptop',
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
