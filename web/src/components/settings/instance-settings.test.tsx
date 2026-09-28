import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, ApiError } from '@/api/client'
import type { ServerInstance } from '@/auth/server'
import { LocaleProvider } from '@/i18n/provider'
import { InstanceSettings } from './instance-settings'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'

const workbench = vi.hoisted(() => ({
  serverIdentity: {
    user: { user_id: 'owner' }, is_instance_owner: true, platform_role: 'owner', personal_tenant_id: 'space', personal_project_id: 'project',
    instance: { managed_execution_enabled: false, mode: 'multi_user', revision: 2, owner_user_id: 'owner' },
  },
  currentTenantId: 'space',
  acceptInstance: vi.fn(),
  notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('./server-security-settings', () => ({ ServerSecuritySettingsPanel: () => null }))

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  setupChoiceSelect()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.serverIdentity.is_instance_owner = true
  workbench.serverIdentity.platform_role = 'owner'
  workbench.serverIdentity.instance.mode = 'multi_user'
  workbench.serverIdentity.instance.revision = 2
  workbench.acceptInstance.mockImplementation((instance: ServerInstance) => { workbench.serverIdentity.instance = instance })
  vi.spyOn(api, 'request').mockImplementation(async () => ({ ...workbench.serverIdentity.instance }) as never)
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.clearAllMocks()
  vi.unstubAllGlobals()
})
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() { await settle(() => root.render(<LocaleProvider><InstanceSettings /></LocaleProvider>)) }
function button(label: string, within: ParentNode = document) {
  return [...within.querySelectorAll('button')].find(button => button.textContent === label)!
}
async function changeMode() {
  await selectChoice(document.getElementById('instance-mode')!, 'single_user')
}

describe('Instance access settings', () => {
  it('waits for the current instance revision before allowing mode edits', async () => {
    let resolve!: (instance: ServerInstance) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise<ServerInstance>(done => { resolve = done }) as never)
    await mount()
    const select = document.getElementById('instance-mode') as HTMLButtonElement
    expect(select.disabled).toBe(true)
    expect(button('保存访问模式').disabled).toBe(true)
    await settle(() => resolve({ ...workbench.serverIdentity.instance, mode: 'single_user', revision: 5 } as ServerInstance))
    expect(select.disabled).toBe(false)
    expect(select.getAttribute('data-choice-value')).toBe('single_user')
    expect(workbench.acceptInstance).toHaveBeenLastCalledWith(expect.objectContaining({ revision: 5 }))
  })

  it('explains retained data and requires confirmation before applying the current revision', async () => {
    vi.mocked(api.request).mockImplementation(async (_path, options) => options?.method === 'PATCH'
      ? { ...workbench.serverIdentity.instance, mode: 'single_user', revision: 3 } as never
      : { ...workbench.serverIdentity.instance } as never)
    await mount()
    await changeMode()
    await settle(() => button('保存访问模式').click())
    const dialog = document.querySelector('[role="dialog"]')!
    expect(dialog.textContent).toContain('数据、机器归属和正在执行的任务都会保留')
    expect(api.request).not.toHaveBeenCalledWith('/admin/instance', expect.objectContaining({ method: 'PATCH' }))
    await settle(() => button('保存访问模式', dialog).click())
    expect(api.request).toHaveBeenCalledWith('/admin/instance', { method: 'PATCH', body: { mode: 'single_user', revision: 2 } })
    expect(workbench.acceptInstance).toHaveBeenLastCalledWith(expect.objectContaining({ mode: 'single_user', revision: 3 }))
    expect(document.body.textContent).not.toContain('生成邀请链接')
  })

  it('refreshes a conflicting revision without silently resubmitting the change', async () => {
    await mount()
    vi.mocked(api.request).mockRejectedValueOnce(new ApiError('Changed elsewhere', 409, 'conflict'))
      .mockResolvedValueOnce({ ...workbench.serverIdentity.instance, revision: 4 })
    await changeMode()
    await settle(() => button('保存访问模式').click())
    await settle(() => button('保存访问模式', document.querySelector('[role="dialog"]')!).click())
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('Changed elsewhere')
    expect(workbench.acceptInstance).toHaveBeenLastCalledWith(expect.objectContaining({ revision: 4 }))
    expect(vi.mocked(api.request).mock.calls.filter(([, options]) => options?.method === 'PATCH')).toHaveLength(1)
  })

  it('does not expose instance operations to ordinary space administrators', async () => {
    workbench.serverIdentity.is_instance_owner = false
    workbench.serverIdentity.platform_role = 'user'
    await mount()
    expect(document.querySelector('button')).toBeNull()
  })
})
