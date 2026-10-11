import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { ApiError, api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { WorkspaceProjectDialog } from './workspace-project-dialog'

const workbench = vi.hoisted(() => ({
  tenants: [{ tenant_id: 'space', kind: 'team' }], serverIdentity: { instance: { mode: 'multi_user' } }, notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))

let host: HTMLDivElement
let root: Root
let snapshot: { project_id: string; project_name: string; execution_project_id: string; revision: number; inherit_project_sharing: boolean; can_move: boolean }
let conflict = false
const onClose = vi.fn()
const onChanged = vi.fn(async () => {})
const workspace = { workspace_id: 'workspace/one', title: 'Research', path: '', created_at_ms: 1, updated_at_ms: 1 }
const endpoint = '/workspaces/workspace%2Fone/project'

beforeEach(() => {
  setupChoiceSelect()
  vi.stubGlobal('localStorage', { getItem: () => null, setItem: vi.fn(), removeItem: vi.fn() })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  snapshot = { project_id: 'source', project_name: 'Source', execution_project_id: 'execution', revision: 4, inherit_project_sharing: true, can_move: true }
  conflict = false
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (options?.method === 'PUT') {
      if (conflict) throw new ApiError('workspace project changed; refresh before moving', 409, 'conflict')
      return snapshot as never
    }
    if (path === '/projects') return { projects: ['source', 'destination', 'another'].map(project_id => ({ project_id, name: project_id })) } as never
    return snapshot as never
  })
})
afterEach(() => {
  act(() => root.unmount()); host.remove()
  vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals()
})
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() {
  await settle(() => root.render(<LocaleProvider><WorkspaceProjectDialog workspace={workspace} tenantId="space" onClose={onClose} onChanged={onChanged} /></LocaleProvider>))
}
function button(label: string) { return [...document.querySelectorAll('button')].find(button => button.textContent === label)! }
function checkbox(label: string) { return [...document.querySelectorAll('label')].find(element => element.textContent?.includes(label))!.querySelector<HTMLInputElement>('input')! }
async function choose(value: string) { await selectChoice(document.getElementById('workspace-move-project')!, value) }

it('requires confirmation after changing the destination or inherited access, and submits the observed version', async () => {
  await mount(); await choose('destination')
  expect(checkbox('采用目标项目').checked).toBe(false)
  expect(button('移动工作区').disabled).toBe(true)
  await settle(() => checkbox('确认移动').click())
  expect(button('移动工作区').disabled).toBe(false)
  await settle(() => checkbox('采用目标项目').click())
  expect(button('移动工作区').disabled).toBe(true)
  await settle(() => checkbox('确认移动').click())
  await choose('another')
  expect(checkbox('采用目标项目').checked).toBe(false)
  expect(button('移动工作区').disabled).toBe(true)
  await settle(() => checkbox('确认移动').click())
  await settle(() => button('移动工作区').click())
  expect(api.request).toHaveBeenCalledWith(endpoint, { method: 'PUT', headers: { 'x-ternilo-tenant': 'space' }, body: {
    project_id: 'another', expected_project_id: 'source', expected_revision: 4, inherit_project_sharing: false,
  } })
  expect(onChanged).toHaveBeenCalledOnce(); expect(onClose).toHaveBeenCalledOnce()
})

it('keeps a stale move open and refreshes its version before a new explicit confirmation', async () => {
  conflict = true
  await mount(); await choose('destination')
  await settle(() => checkbox('确认移动').click())
  await settle(() => button('移动工作区').click())
  expect(onClose).not.toHaveBeenCalled()
  expect(document.querySelector('[role="alert"]')!.textContent).toContain('refresh before moving')
  expect(button('移动工作区').disabled).toBe(true)
  snapshot = { ...snapshot, revision: 6 }; conflict = false
  await settle(() => button('刷新项目信息').click())
  expect(button('移动工作区').disabled).toBe(true)
  await choose('destination')
  await settle(() => checkbox('确认移动').click())
  await settle(() => button('移动工作区').click())
  expect(api.request).toHaveBeenLastCalledWith(endpoint, expect.objectContaining({ body: expect.objectContaining({ expected_revision: 6 }) }))
  expect(onClose).toHaveBeenCalledOnce()
})

it('does not offer a move after the server reports that management changed', async () => {
  snapshot.can_move = false
  await mount()
  expect(document.querySelector('#workspace-move-project')).toBeNull()
  expect(button('移动工作区').disabled).toBe(true)
  expect(document.querySelector('[role="alert"]')!.textContent).toContain('当前可写管理者')
})
