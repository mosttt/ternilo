import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { TooltipProvider } from '@/components/ui/tooltip'
import type { LocalSession, Workspace, ResourceAccess, TenantSummary } from '@/types'
import type { ServerIdentity } from '@/auth/server'

const mocks = vi.hoisted(() => ({
  chooseWorkspace: vi.fn<(createSession?: boolean) => void>(),
  apiRequest: vi.fn(),
  workbench: {
    snapshot: { workspaces: [] as Workspace[], sessions: [] as LocalSession[] },
    currentSessionId: null,
    currentSession: null,
    currentWorkspaceId: null as string | null,
    currentTenantId: null as string | null,
    accountScope: 'local',
    refresh: vi.fn(),
    platform: false,
    tenants: [] as TenantSummary[],
    serverIdentity: undefined as ServerIdentity | undefined,
    authRequired: false,
    loading: false,
    selectWorkspace: vi.fn(),
    selectSession: vi.fn(),
    createSession: vi.fn(),
    renameWorkspace: vi.fn(),
    unregisterWorkspace: vi.fn(),
    updateSession: vi.fn(),
    forkSession: vi.fn(),
    forkOperation: null,
    archiveSession: vi.fn(),
    deleteSession: vi.fn(),
    notify: vi.fn(),
  },
}))

vi.mock('@/api/client', () => ({
  api: { request: mocks.apiRequest },
}))

vi.mock('@/state/workbench', () => ({
  storage: { sidebarView: 'ternilo.sidebar-view-v1' },
  useWorkbench: () => mocks.workbench,
}))

import { WorkspaceBrowser } from './workspace-browser'

let host: HTMLDivElement
let root: Root

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('localStorage', memoryStorage())
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    disconnect() {}
  })
  localStorage.clear()
  mocks.chooseWorkspace.mockReset()
  mocks.apiRequest.mockReset().mockResolvedValue({ workspace_order: [], session_order_by_account: {} })
  mocks.workbench.notify.mockReset()
  mocks.workbench.deleteSession.mockReset().mockResolvedValue(undefined)
  mocks.workbench.selectWorkspace.mockReset()
  mocks.workbench.createSession.mockReset().mockResolvedValue(undefined)
  mocks.workbench.authRequired = false
  mocks.workbench.loading = false
  mocks.workbench.platform = false
  mocks.workbench.currentTenantId = null
  mocks.workbench.accountScope = 'local'
  mocks.workbench.currentWorkspaceId = null
  mocks.workbench.snapshot = { workspaces: [], sessions: [] }
  mocks.workbench.tenants = []
  mocks.workbench.serverIdentity = undefined
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function renderBrowser(wide: boolean, readOnly = false) {
  await act(async () => root.render(
    <LocaleProvider>
      <TooltipProvider>
        <WorkspaceBrowser
          wide={wide}
          readOnly={readOnly}
          expandSidebar={vi.fn()}
          onChooseWorkspace={mocks.chooseWorkspace}
          onSessionActivated={vi.fn()}
        />
      </TooltipProvider>
    </LocaleProvider>,
  ))
}

it('closes the archive panel synchronously when account or space changes', async () => {
  mocks.apiRequest.mockImplementation(async (path: string) => path === '/sessions/archived' ? [] : { workspace_order: [], session_order_by_account: {} })
  await renderBrowser(true)
  await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="归档会话"]')!.click())
  expect(document.querySelector('[data-session-archive]')).not.toBeNull()
  mocks.workbench.accountScope = 'another-account'
  await renderBrowser(true)
  expect(document.querySelector('[data-session-archive]')).toBeNull()
  await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="归档会话"]')!.click())
  expect(document.querySelector('[data-session-archive]')).not.toBeNull()
  mocks.workbench.currentTenantId = 'another-space'
  await renderBrowser(true)
  expect(document.querySelector('[data-session-archive]')).toBeNull()
})

describe('WorkspaceBrowser creation intent', () => {
  it('selects another folder when toggled and creates directly in the explicitly requested folder', async () => {
    mocks.workbench.currentWorkspaceId = 'folder-a'
    mocks.workbench.snapshot = {
      workspaces: ['folder-a', 'folder-b'].map(id => ({ workspace_id: id, title: id, path: `/${id}`, created_at_ms: 1, updated_at_ms: 1 })),
      sessions: [],
    }
    await renderBrowser(true)
    const group = [...host.querySelectorAll<HTMLElement>('[data-sidebar-workspace-group]')]
      .find(item => item.querySelector('[data-sidebar-workspace-title]')?.textContent === 'folder-b')!
    await act(async () => group.querySelector<HTMLButtonElement>('[data-sidebar-workspace-button]')!.click())
    expect(mocks.workbench.selectWorkspace).toHaveBeenCalledExactlyOnceWith('folder-b')
    await act(async () => group.querySelector<HTMLButtonElement>('[aria-label="在“folder-b”中新建会话"]')!.click())
    expect(mocks.workbench.createSession).toHaveBeenCalledExactlyOnceWith('folder-b')
    mocks.workbench.currentWorkspaceId = 'folder-b'
    mocks.workbench.selectWorkspace.mockClear()
    await renderBrowser(true)
    expect(group.querySelector('[data-sidebar-workspace-button]')?.hasAttribute('data-active')).toBe(true)
    await act(async () => group.querySelector<HTMLButtonElement>('[data-sidebar-workspace-button]')!.click())
    expect(mocks.workbench.selectWorkspace).not.toHaveBeenCalled()
  })
  it('lets a shared viewer inspect server permissions from workspace and session menus without grant controls', async () => {
    const access: ResourceAccess = {
      owner_user_id: 'owner', is_owner: false, role_limited: true,
      permissions: { view: true, submit: false, stop: false, configure: false },
      sources: [{ kind: 'group', resource_kind: 'workspace', resource_id: 'workspace', group_name: 'Reviewers', permissions: { view: true, submit: false, stop: false, configure: false } }],
    }
    const workspace: Workspace = { workspace_id: 'workspace', title: 'Shared project', path: '/project', created_at_ms: 1, updated_at_ms: 1, access }
    const session: LocalSession = {
      identity: { tenant_id: 'team', user_id: 'owner', agent_id: 'agent', session_id: 'session' },
      workspace_id: 'workspace', workspace_path: '/project', title: 'Shared session', permissions: 'read_only',
      model: { provider: 'profile_default' }, agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 1, access,
    }
    Object.assign(mocks.workbench, {
      platform: true, currentTenantId: 'team', currentWorkspaceId: 'workspace',
      tenants: [{ tenant_id: 'team', kind: 'team', slug: 'team', display_name: 'Team', role: 'viewer' }],
      serverIdentity: { user: { user_id: 'viewer' }, instance: { mode: 'multi_user' } },
      snapshot: { workspaces: [workspace], sessions: [session] },
    })
    mocks.apiRequest.mockImplementation(async (path: string) => path.includes('/sharing?') ? { access, shares: [], next_cursor: null } : { workspace_order: [], session_order_by_account: {} })
    await renderBrowser(true, true)
    for (const [label, path] of [['工作区“Shared project”的操作', '/workspaces/workspace/sharing?limit=25'], ['会话“Shared session”的操作', '/sessions/session/sharing?limit=25']]) {
      await act(async () => host.querySelector(`[aria-label="${label}"]`)!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
      const item = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent === '查看权限…')!
      expect(item).toBeTruthy()
      await act(async () => { item.click(); await Promise.resolve(); await Promise.resolve() })
      expect(mocks.apiRequest).toHaveBeenCalledWith(path, { signal: expect.any(AbortSignal) })
      expect(document.querySelector('[data-sharing-effective-access]')?.textContent).toContain('通过权限组“Reviewers”')
      expect(document.querySelector('[data-sharing-candidates]')).toBeNull()
      expect(document.querySelector('[data-sharing-permissions]')).toBeNull()
      await act(async () => document.querySelector<HTMLButtonElement>('[data-slot="dialog-close"]')!.click())
    }
    expect(mocks.apiRequest.mock.calls.some(([, options]) => options?.method === 'PUT')).toBe(false)
  })
  it('does not request ordering or emit a 401 toast before remote authentication finishes', async () => {
    mocks.workbench.authRequired = true
    mocks.workbench.loading = true

    await renderBrowser(true)

    expect(mocks.apiRequest).not.toHaveBeenCalled()
    expect(mocks.workbench.notify).not.toHaveBeenCalled()
  })

  it('creates a Session after the desktop Add workspace flow', async () => {
    await renderBrowser(true)

    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="添加工作区"]')!.click())

    expect(mocks.chooseWorkspace).toHaveBeenLastCalledWith(true)

    const chooseOnly = [...host.querySelectorAll('button')]
      .find(button => button.textContent === '选择一个文件夹开始') as HTMLButtonElement
    await act(async () => chooseOnly.click())
    expect(mocks.chooseWorkspace).toHaveBeenLastCalledWith(true)
  })

  it('keeps the collapsed Add workspace flow equivalent to desktop', async () => {
    await renderBrowser(false)

    await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="添加工作区"]')!.click())

    expect(mocks.chooseWorkspace).toHaveBeenCalledWith(true)
  })
})

function pendingDeletion() {
  let resolve!: () => void
  let reject!: (error: Error) => void
  const promise = new Promise<void>((done, fail) => { resolve = done; reject = fail })
  return { promise, resolve, reject }
}

async function openBatchDeletion() {
  mocks.workbench.snapshot.sessions = ['first', 'second', 'third'].map(id => ({
    identity: { tenant_id: 'local', user_id: 'owner', agent_id: 'agent', session_id: id },
    workspace_id: 'removed', workspace_path: '/removed', title: id, permissions: 'workspace_write',
    model: { provider: 'profile_default' }, agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 1,
  }))
  await renderBrowser(true)
  await act(async () => host.querySelector('[aria-label="未分组的操作"]')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
  await act(async () => [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent?.startsWith('删除全部会话'))!.click())
}

async function dialogButton(label: string) {
  const button = [...document.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')].find(button => button.textContent === label)
  expect(button, label).toBeDefined()
  expect(button!.disabled).toBe(false)
  await act(async () => button!.click())
}

it('cancels only remaining deletions, retaining successful and in-flight operations', async () => {
  const first = pendingDeletion(), second = pendingDeletion()
  mocks.workbench.deleteSession.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise)
  await openBatchDeletion()
  await dialogButton('全部删除')
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first']])
  await act(async () => first.resolve())
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first'], ['second']])
  await dialogButton('取消剩余删除')
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain('等待当前删除完成')
  await act(async () => second.resolve())
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first'], ['second']])
  expect(document.querySelector('[role="dialog"]')).toBeNull()
  expect(mocks.workbench.notify).toHaveBeenCalledWith('已停止删除：已删除 2 个，保留 1 个会话')
})

it('retries a failed deletion and remaining items without deleting completed items twice', async () => {
  const second = pendingDeletion()
  mocks.workbench.deleteSession.mockResolvedValueOnce(undefined).mockReturnValueOnce(second.promise)
  await openBatchDeletion()
  await dialogButton('全部删除')
  await act(async () => second.reject(new Error('Delete failed')))
  expect(document.querySelector('[role="alert"]')?.textContent).toBe('Delete failed')
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first'], ['second']])
  await dialogButton('全部删除')
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first'], ['second'], ['second'], ['third']])
  expect(mocks.workbench.notify).toHaveBeenCalledWith('已删除 2 个未分组会话')
})

it('stops pending deletions when the account space changes', async () => {
  const first = pendingDeletion()
  mocks.workbench.deleteSession.mockReturnValueOnce(first.promise)
  await openBatchDeletion()
  await dialogButton('全部删除')
  Object.assign(mocks.workbench, { currentTenantId: 'another-space' })
  await renderBrowser(true)
  await act(async () => first.resolve())
  expect(mocks.workbench.deleteSession.mock.calls).toEqual([['first']])
  expect(document.querySelector('[role="dialog"]')).toBeNull()
  expect(mocks.workbench.notify.mock.calls.some(([text]) => String(text).startsWith('已删除'))).toBe(false)
})
