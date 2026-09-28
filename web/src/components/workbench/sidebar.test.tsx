import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { TooltipProvider } from '@/components/ui/tooltip'
import type { Workspace } from '@/types'

const workbench = vi.hoisted(() => ({
  currentWorkspace: null as Workspace | null,
  remote: false,
  platform: false,
  platformRole: 'user',
  createSession: vi.fn(),
  notify: vi.fn(),
}))

vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    currentWorkspaceId: workbench.currentWorkspace?.workspace_id ?? null,
    currentWorkspace: workbench.currentWorkspace,
    remote: workbench.remote,
    platform: workbench.platform,
    serverIdentity: { platform_role: workbench.platformRole, instance: { mode: 'single_user' } },
    tenants: [],
    currentTenantId: null,
    currentTenantRole: null,
    selectTenant: vi.fn(),
    logout: vi.fn(),
    createSession: workbench.createSession,
    notify: workbench.notify,
  }),
}))

vi.mock('./workspace-browser', () => ({
  WorkspaceBrowser: () => <div data-workspace-browser-test="" />,
}))

import { Sidebar } from './sidebar'

let host: HTMLDivElement
let root: Root
const chooseWorkspace = vi.fn<(createSession?: boolean) => void>()
const mobileOpenChange = vi.fn<(value: boolean) => void>()

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    disconnect() {}
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.currentWorkspace = null
  workbench.remote = false
  workbench.platform = false
  workbench.platformRole = 'user'
  workbench.createSession.mockReset().mockResolvedValue(undefined)
  workbench.notify.mockReset()
  chooseWorkspace.mockReset()
  mobileOpenChange.mockReset()
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function renderSidebar(liveStatus: 'ready' | 'reconnecting' = 'ready') {
  act(() => root.render(
    <LocaleProvider>
      <TooltipProvider>
        <Sidebar
          collapsed={false}
          mobileOpen={false}
          onCollapsedChange={vi.fn()}
          onMobileOpenChange={mobileOpenChange}
          onChooseWorkspace={chooseWorkspace}
          onOpenSettings={vi.fn()}
          liveStatus={liveStatus}
        />
      </TooltipProvider>
    </LocaleProvider>,
  ))
}

describe('Sidebar new session target', () => {
  it('creates directly in the selected Workspace', async () => {
    workbench.currentWorkspace = {
      workspace_id: 'workspace-selected',
      path: '/tmp/selected',
      title: 'Selected',
      created_at_ms: 1,
      updated_at_ms: 1,
    }
    renderSidebar()

    await act(async () => host.querySelector<HTMLButtonElement>('[data-sidebar-new-session]')!.click())

    expect(workbench.createSession).toHaveBeenCalledWith('workspace-selected')
    expect(chooseWorkspace).not.toHaveBeenCalled()
    expect(mobileOpenChange).toHaveBeenCalledWith(false)
  })

  it('opens the Workspace chooser only when no Workspace is selected', async () => {
    renderSidebar()

    await act(async () => host.querySelector<HTMLButtonElement>('[data-sidebar-new-session]')!.click())

    expect(chooseWorkspace).toHaveBeenCalledWith(true)
    expect(workbench.createSession).not.toHaveBeenCalled()
  })

  it('shows the live reconnect state without hiding the Workbench', () => {
    renderSidebar('reconnecting')

    const connection = host.querySelector<HTMLElement>('[data-sidebar-connection]')!
    expect(connection.dataset.liveState).toBe('reconnecting')
    expect(connection.textContent).toContain('连接中断，正在重连…')
    expect(connection.getAttribute('role')).toBe('status')
    expect(host.querySelector('[data-workspace-browser-test]')).not.toBeNull()
  })

  it('does not claim an executor node is enrolled when only the Relay transport is ready', () => {
    workbench.remote = true
    renderSidebar()

    const connection = host.querySelector<HTMLElement>('[data-sidebar-connection]')!
    expect(connection.textContent).toContain('远程入口已连接')
    expect(connection.textContent).not.toContain('节点已连接')
    expect(connection.title).toBe('通过 Ternilo 中继连接')
  })
})


it.each(['user', 'owner', 'operator'])('separates user settings and model access from privileged administration for %s', role => {
  workbench.platform = true
  workbench.platformRole = role
  renderSidebar()
  const labels = [...host.querySelectorAll('button')].map(button => button.getAttribute('aria-label'))
  expect(labels).toContain('用户设置')
  expect(labels).toContain('我的模型')
  expect(labels).not.toContain('设置')
  expect(host.querySelector('[data-administration-entry]') !== null).toBe(role !== 'user')
  const adminEntry = host.querySelector('[data-administration-entry]')
  if (adminEntry) expect(adminEntry.textContent).toBe('平台管理')
})

it.each([false, true])('opens the same Files library for Local and ordinary Server users (platform=%s)', platform => {
  workbench.platform = platform
  renderSidebar()
  const entry = host.querySelector<HTMLButtonElement>('[data-sidebar-files]')!
  expect(entry.getAttribute('aria-label')).toBe('文件')
  act(() => entry.click())
  expect(location.pathname).toBe('/files')
  expect(mobileOpenChange).toHaveBeenCalledWith(false)
  expect(host.querySelector('[data-administration-entry]')).toBeNull()
  history.replaceState({}, '', '/')
})
