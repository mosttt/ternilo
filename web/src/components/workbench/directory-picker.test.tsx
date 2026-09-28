import { act } from 'react'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { DirectoryPicker } from './directory-picker'

const workbench = vi.hoisted(() => ({
  platform: true,
  currentTenantRole: 'owner',
  currentTenantId: 'personal',
  tenants: [{ tenant_id: 'personal', display_name: 'Personal' }],
  currentWorkspace: { node_id: 'my-vps', project_id: 'project' },
  serverIdentity: { instance: { mode: 'single_user', managed_execution_enabled: false } },
  createWorkspace: vi.fn(async () => ({ workspace_id: 'public-workspace', title: 'demo' })),
  createSession: vi.fn(),
  notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('./local-directory-picker', () => ({
  LocalDirectoryPicker: ({ open, apiBase, onChoosePath, onOpenChange }: {
    open: boolean; apiBase: string; onChoosePath(path: string): Promise<void>; onOpenChange(open: boolean): void
  }) => open ? <button data-directory-endpoint={apiBase} onClick={() => {
    void onChoosePath('/projects/demo').then(() => onOpenChange(false))
  }}>Select fixture directory</button> : null,
}))

let root: Root
let host: HTMLDivElement
let projectCreates: number
beforeEach(() => {
  setupChoiceSelect()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  projectCreates = 0
  HTMLElement.prototype.scrollIntoView = vi.fn()
  HTMLElement.prototype.hasPointerCapture = vi.fn(() => false)
  HTMLElement.prototype.setPointerCapture = vi.fn()
  HTMLElement.prototype.releasePointerCapture = vi.fn()
  workbench.currentTenantRole = 'owner'
  workbench.serverIdentity.instance = { mode: 'single_user', managed_execution_enabled: false }
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (path === '/projects' && options?.method === 'POST') {
      projectCreates += 1
      return { project: { project_id: 'new-project', name: 'New project' } } as never
    }
    return (path === '/projects'
      ? { projects: [{ project_id: 'project', name: 'My project' }] }
      : { executors: [
        { executor_id: 'other-computer', connected: true },
        { executor_id: 'my-vps', connected: true },
      ] }) as never
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals() })
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() {
  await settle(() => root.render(<LocaleProvider><DirectoryPicker open createSessionAfter onOpenChange={() => undefined} /></LocaleProvider>))
}
function button(label: string) {
  const result = [...document.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === label)
  if (!result) throw new Error(`Missing button: ${label}`)
  return result
}
async function fill(id: string, value: string) {
  if (document.getElementById(id)?.getAttribute('role') === 'combobox') {
    await selectChoice(document.getElementById(id)!, value)
    return
  }
  await settle(() => {
    const input = document.getElementById(id) as HTMLInputElement
    const type = input.tagName === 'SELECT' ? HTMLSelectElement : HTMLInputElement
    Object.getOwnPropertyDescriptor(type.prototype, 'value')?.set?.call(input, value)
    input.dispatchEvent(new Event(input.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true }))
  })
}
async function chooseDirectory() {
  await settle(() => button('选择文件夹').click())
  const directory = document.querySelector<HTMLButtonElement>('[data-directory-endpoint]')!
  expect(directory.dataset.directoryEndpoint).toBe('/executors/my-vps/directories')
  await settle(() => directory.click())
}

describe('Server workspace setup', () => {
  it.each(['single_user', 'multi_user'])('opens a folder on the selected machine before naming in %s mode', async mode => {
    workbench.serverIdentity.instance.mode = mode
    await mount()
    expect(document.body.textContent).not.toContain('托管环境')
    expect(document.getElementById('workspace-executor')!.getAttribute('data-choice-value')).toBe('my-vps')
    await chooseDirectory()
    expect(workbench.createWorkspace).not.toHaveBeenCalled()
    expect((document.getElementById('workspace-name') as HTMLInputElement).value).toBe('demo')
    expect(document.querySelector('[data-selected-directory]')?.textContent).toBe('/projects/demo')
    expect(document.getElementById('workspace-project')?.getAttribute('data-choice-value')).toBe('project')
    expect(button('打开并开始会话').disabled).toBe(false)
    await settle(() => button('打开并开始会话').click())
    expect(workbench.createWorkspace).toHaveBeenCalledWith({ project_id: 'project', name: 'demo', placement: 'local_node', executor_id: 'my-vps', path: '/projects/demo' })
    expect(workbench.createSession).toHaveBeenCalledWith('public-workspace')
  })

  it('clears a chosen directory when changing computers', async () => {
    await mount()
    await chooseDirectory()
    await fill('workspace-executor', 'other-computer')
    expect(document.querySelector('[data-selected-directory]')).toBeNull()
    expect(button('打开并开始会话').disabled).toBe(true)
  })

  it('keeps a created project when workspace creation fails so retry does not create another', async () => {
    await mount()
    await chooseDirectory()
    await fill('workspace-project', '__new_project__')
    await fill('new-cloud-project', 'New project')
    workbench.createWorkspace.mockRejectedValueOnce(new Error('Computer disconnected'))
    await settle(() => button('创建项目并开始会话').click())
    expect(projectCreates).toBe(1)
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('Computer disconnected')
    await settle(() => button('打开并开始会话').click())
    expect(projectCreates).toBe(1)
    expect(workbench.createWorkspace).toHaveBeenLastCalledWith(expect.objectContaining({ project_id: 'new-project', path: '/projects/demo' }))
  })

  it('keeps standalone project creation independent of folders and sessions', async () => {
    await mount()
    await fill('workspace-project', '__new_project__')
    await fill('new-cloud-project', 'Project without a folder')
    await settle(() => button('仅创建项目').click())
    expect(projectCreates).toBe(1)
    expect(workbench.createWorkspace).not.toHaveBeenCalled()
    expect(workbench.createSession).not.toHaveBeenCalled()
  })

  it('retries session creation without recreating its workspace', async () => {
    await mount()
    await chooseDirectory()
    workbench.createSession.mockRejectedValueOnce(new Error('Session unavailable'))
    await settle(() => button('打开并开始会话').click())
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('Session unavailable')
    await settle(() => button('重试打开会话').click())
    expect(workbench.createWorkspace).toHaveBeenCalledTimes(1)
    expect(workbench.createSession).toHaveBeenCalledTimes(2)
  })

  it.each(['single_user', 'multi_user'])('offers managed execution in %s mode only when enabled', async mode => {
    workbench.serverIdentity.instance = { mode, managed_execution_enabled: true }
    await mount()
    expect(document.body.textContent).toContain('托管环境')
    expect(document.body.textContent).toContain('我的电脑或 VPS')
  })

  it('retains the read-only role boundary even when managed execution is enabled', async () => {
    workbench.currentTenantRole = 'viewer'
    workbench.serverIdentity.instance.managed_execution_enabled = true
    await mount()
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    expect(api.request).not.toHaveBeenCalled()
  })
})
