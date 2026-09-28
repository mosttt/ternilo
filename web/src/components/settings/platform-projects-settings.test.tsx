import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { ProjectRecord } from '@/types'
import { PlatformProjectsSettings } from './platform-projects-settings'

const workbench = vi.hoisted(() => ({
  snapshot: { workspaces: [] }, selectWorkspace: vi.fn(), notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/app/navigation', () => ({ navigate: vi.fn() }))
vi.mock('@/api/client', async importOriginal => ({
  ...await importOriginal<typeof import('@/api/client')>(), api: { request: vi.fn() },
}))

const project: ProjectRecord = { tenant_id: 'tenant-a', project_id: 'project-a', name: 'Original', created_at_ms: 10 }
const other: ProjectRecord = { ...project, project_id: 'project-b', name: 'Other' }
let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
    clear: () => storage.clear(),
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.mocked(api.request).mockResolvedValue({ projects: [project, other] })
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  localStorage.clear()
  vi.resetAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve() })
}

async function render(locale = 'zh') {
  localStorage.setItem('ternilo.locale', locale)
  await settle(() => root.render(<LocaleProvider><PlatformProjectsSettings tenantId="tenant-a" /></LocaleProvider>))
}

function button(label: string, container: ParentNode = document) {
  const result = [...container.querySelectorAll<HTMLButtonElement>('button')].find(element => element.textContent?.trim() === label)
  if (!result) throw new Error(`Missing button: ${label}`)
  return result
}

function row() {
  return host.querySelector<HTMLElement>('[data-project-id="project-a"]')!
}

function dialog() {
  return document.querySelector<HTMLElement>('[role="dialog"]')!
}

function setName(value: string) {
  const input = document.querySelector<HTMLInputElement>('#project-rename-name')!
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('Platform project settings', () => {
  it('renames through the tenant-scoped API and updates only the selected project', async () => {
    await render()
    await settle(() => button('重命名项目', row()).click())
    expect(button('保存').disabled).toBe(true)
    await settle(() => setName('   '))
    expect(button('保存').disabled).toBe(true)
    await settle(() => setName('  Renamed  '))
    vi.mocked(api.request).mockResolvedValueOnce({ project: { ...project, name: 'Renamed' } })
    await settle(() => dialog().querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
    expect(api.request).toHaveBeenLastCalledWith('/projects/project-a', {
      method: 'PATCH', headers: { 'x-ternilo-tenant': 'tenant-a' }, body: { name: 'Renamed' },
    })
    expect(dialog()).toBeNull()
    expect(row().textContent).toContain('Renamed')
    expect(host.querySelector('[data-project-id="project-b"]')?.textContent).toContain('Other')
    expect(workbench.notify).toHaveBeenCalledWith('项目已重命名')
  })

  it('cancels rename and delete without sending mutation requests', async () => {
    await render()
    await settle(() => button('重命名项目', row()).click())
    await settle(() => setName('Discarded'))
    await settle(() => button('取消').click())
    await settle(() => button('删除项目', row()).click())
    expect(dialog().textContent).toContain('不会删除磁盘目录、工作区或会话')
    await settle(() => button('取消').click())
    expect(api.request).toHaveBeenCalledTimes(1)
    expect(row().textContent).toContain('Original')
  })

  it('keeps a failed rename editable and retries after permission is restored', async () => {
    await render()
    await settle(() => button('重命名项目', row()).click())
    await settle(() => setName('Retry name'))
    vi.mocked(api.request).mockRejectedValueOnce(new ApiError('tenant role member does not allow ProjectManage', 403, 'policy_denied'))
    await settle(() => button('保存').click())
    expect(dialog().querySelector('[role="alert"]')?.textContent).toContain('只有当前空间的所有者或管理员')
    expect(row().textContent).toContain('Original')
    expect(document.querySelector<HTMLInputElement>('#project-rename-name')?.value).toBe('Retry name')
    vi.mocked(api.request).mockResolvedValueOnce({ project: { ...project, name: 'Retry name' } })
    await settle(() => button('保存').click())
    expect(dialog()).toBeNull()
    expect(row().textContent).toContain('Retry name')
  })

  it.each([
    ['project has associated resources and cannot be deleted', '项目仍有关联资源'],
    ['personal default project cannot be deleted', '个人默认项目不能删除'],
    ['the last project in a space cannot be deleted', '空间必须保留至少一个项目'],
  ])('keeps the confirmation and project on conflict: %s', async (message, expected) => {
    await render()
    await settle(() => button('删除项目', row()).click())
    vi.mocked(api.request).mockRejectedValueOnce(new ApiError(message, 409, 'conflict'))
    await settle(() => button('删除项目', dialog()).click())
    expect(dialog().querySelector('[role="alert"]')?.textContent).toContain(expected)
    expect(row().textContent).toContain('Original')
    expect(workbench.notify).not.toHaveBeenCalled()
  })

  it('submits deletion once, blocks dismissal while saving and removes only after success', async () => {
    await render()
    await settle(() => button('删除项目', row()).click())
    let complete!: (value: unknown) => void
    vi.mocked(api.request).mockReturnValueOnce(new Promise(resolve => { complete = resolve }))
    await settle(() => { const confirm = button('删除项目', dialog()); confirm.click(); confirm.click() })
    expect(api.request).toHaveBeenCalledTimes(2)
    expect(api.request).toHaveBeenLastCalledWith('/projects/project-a', { method: 'DELETE', headers: { 'x-ternilo-tenant': 'tenant-a' } })
    expect(button('取消').disabled).toBe(true)
    expect(button('正在删除…').disabled).toBe(true)
    expect(row()).not.toBeNull()
    await settle(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    expect(dialog()).not.toBeNull()
    await settle(() => complete(undefined))
    expect(dialog()).toBeNull()
    expect(row()).toBeNull()
    expect(host.querySelector('[data-project-id="project-b"]')).not.toBeNull()
    expect(workbench.notify).toHaveBeenCalledWith('项目已删除')
  })

  it('translates English controls and reference rejection', async () => {
    await render('en')
    await settle(() => button('Delete project', row()).click())
    expect(dialog().textContent).toContain('Delete empty project?')
    expect(dialog().textContent).toContain('resources are not moved')
    vi.mocked(api.request).mockRejectedValueOnce(new ApiError('project has associated resources and cannot be deleted', 409, 'conflict'))
    await settle(() => button('Delete project', dialog()).click())
    expect(dialog().querySelector('[role="alert"]')?.textContent).toContain('No resources were deleted or moved.')
    expect(dialog().textContent).not.toMatch(/[\u3400-\u9fff]/)
  })
})
