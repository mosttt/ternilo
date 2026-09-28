import { act } from 'react'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { ResourceAccess, ResourceShare, ShareSubject } from '@/types'
import { ResourceSharingDialog } from './resource-sharing-dialog'

const workbench = vi.hoisted(() => ({ currentTenantId: 'space', tenants: [{ tenant_id: 'space', kind: 'team' }] }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))

let host: HTMLDivElement
let root: Root
const onClose = vi.fn()
const onChanged = vi.fn(async () => {})
const permissions = { view: true, submit: false, stop: false, configure: false }
const owner: ResourceAccess = { owner_user_id: 'owner', is_owner: true, permissions: { view: true, submit: true, stop: true, configure: true }, sources: [], role_limited: false }
const member: ShareSubject = { kind: 'user', user: { user_id: 'member', username: 'Alice' } }
const group: ShareSubject = { kind: 'group', group: { group_id: 'group/research', tenant_id: 'space', name: 'Research team', description: null, member_count: 3, created_at_ms: 1, updated_at_ms: 1 } }
const target = { kind: 'session' as const, id: 'public/session', title: 'Research' }
let shares: ResourceShare[]
let access: ResourceAccess

beforeEach(() => {
  setupChoiceSelect()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  workbench.tenants[0]!.kind = 'team'
  shares = []; access = owner
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    if (options?.method === 'PUT') {
      shares = [{ subject: path.includes('/group/') ? group : member, inherited: false, permissions: options.body as typeof permissions, created_at_ms: 1, updated_at_ms: 1 }]
      return undefined as never
    }
    if (options?.method === 'DELETE') { shares = []; return undefined as never }
    if (path.includes('/candidates?')) return { candidates: path.includes('kind=group') ? [group] : [member], next_cursor: null } as never
    return { access, shares, next_cursor: null } as never
  })
})
afterEach(() => {
  act(() => root.unmount()); host.remove(); history.replaceState({}, '', '/')
  vi.restoreAllMocks(); vi.clearAllMocks(); vi.unstubAllGlobals()
})
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() { await settle(() => root.render(<LocaleProvider><ResourceSharingDialog target={target} onClose={onClose} onChanged={onChanged} /></LocaleProvider>)) }
function button(label: string) { return [...document.querySelectorAll('button')].find(button => button.textContent === label)! }
async function selectKind(kind: string) {
  await selectChoice(document.getElementById('sharing-kind')!, kind)
}

describe('Resource sharing', () => {
  it('saves explicitly selected user actions and revokes through the typed subject route', async () => {
    await mount()
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing?limit=25', { signal: expect.any(AbortSignal) })
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/candidates?limit=25&kind=user', { signal: expect.any(AbortSignal) })
    expect(document.body.textContent).toContain('不会开放机器管理、Provider 凭据或插件安装权限')
    await settle(() => button('Alice').click())
    await settle(() => [...document.querySelectorAll('label')].find(label => label.textContent === '发送任务')!.querySelector('input')!.click())
    await settle(() => button('保存共享权限').click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/user/member', { method: 'PUT', body: { ...permissions, submit: true } })
    expect(onChanged).toHaveBeenCalledOnce()
    await settle(() => document.querySelector<HTMLButtonElement>('[aria-label="移除“Alice”的共享权限"]')!.click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/user/member', { method: 'DELETE' })
    expect(document.body.textContent).toContain('当前资源没有直接共享授权')
  })

  it('shares with a group without expanding its members or assigning user grants', async () => {
    await mount()
    await selectKind('group')
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/candidates?limit=25&kind=group', { signal: expect.any(AbortSignal) })
    await settle(() => document.querySelector<HTMLButtonElement>('[data-sharing-candidate="group:group/research"]')!.click())
    await settle(() => button('保存共享权限').click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/group/group%2Fresearch', { method: 'PUT', body: permissions })
    expect(vi.mocked(api.request).mock.calls.some(([path]) => path.includes('/members'))).toBe(false)
    expect(document.querySelector('[data-sharing-grant="group:group/research"]')).not.toBeNull()
    await selectKind('user')
    expect(document.querySelector('[data-sharing-permissions]')).toBeNull()
    expect(button('保存共享权限')).toBeUndefined()
  })

  it('pages and searches recipients on the server without reusing stale selections', async () => {
    await mount()
    vi.mocked(api.request).mockResolvedValueOnce({ candidates: [group], next_cursor: 'next-group' })
    await selectKind('group')
    const picker = document.querySelector('[data-sharing-candidates]')!
    await settle(() => [...picker.querySelectorAll('button')].find(button => button.textContent === '下一页')!.click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/candidates?limit=25&cursor=next-group&kind=group', { signal: expect.any(AbortSignal) })
    const search = document.getElementById('sharing-search') as HTMLInputElement
    await settle(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(search, 'Research')
      search.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await settle(() => button('搜索').click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/candidates?limit=25&query=Research&kind=group', { signal: expect.any(AbortSignal) })
  })

  it('shows server effective permissions and inherited sources without exposing sharing administration', async () => {
    access = { ...owner, is_owner: false, permissions, role_limited: true, sources: [{ kind: 'group', resource_kind: 'workspace', resource_id: 'workspace', group_id: 'group', group_name: 'Research team', permissions: owner.permissions }] }
    await mount()
    expect(document.getElementById('sharing-kind')).toBeNull()
    expect(button('保存共享权限')).toBeUndefined()
    expect(document.body.textContent).toContain('通过权限组“Research team”')
    expect(document.body.textContent).toContain('继承自工作区')
    expect(document.body.textContent).toContain('实际权限仍受团队角色限制')
    expect(document.querySelector('[data-sharing-effective-access] > p')?.textContent).toBe('查看内容')
    expect(vi.mocked(api.request).mock.calls.some(([path]) => path.includes('/candidates'))).toBe(false)
  })

  it('explains when editing a fork grant establishes independent user access', async () => {
    shares = [{ subject: member, inherited: true, permissions, created_at_ms: 1, updated_at_ms: 1 }]
    await mount()
    expect(document.body.textContent).toContain('分叉继承')
    await settle(() => button('调整权限').click())
    expect(document.body.textContent).toContain('保存后会为该账号建立独立授权')
    await settle(() => button('保存共享权限').click())
    expect(api.request).toHaveBeenCalledWith('/sessions/public%2Fsession/sharing/user/member', { method: 'PUT', body: permissions })
  })

  it('discards a previous resource response after switching the dialog target', async () => {
    let resolve!: (value: unknown) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise(done => { resolve = done }) as never)
    await mount()
    await settle(() => root.render(<LocaleProvider><ResourceSharingDialog key="next" target={{ ...target, id: 'next', title: 'Next' }} onClose={vi.fn()} onChanged={onChanged} /></LocaleProvider>))
    await settle(() => resolve({ access, shares: [{ subject: { kind: 'user', user: { user_id: 'private', username: 'Old private member' } }, permissions, inherited: false }], next_cursor: null }))
    expect(document.body.textContent).not.toContain('Old private member')
    expect(document.body.textContent).toContain('Next')
  })

  it('keeps personal resources private and opens real space management without requesting grants', async () => {
    workbench.tenants[0]!.kind = 'personal'
    await mount()
    expect(document.body.textContent).toContain('当前共享仅支持同一团队内的账号')
    expect(document.body.textContent).toContain('创建团队不会移动或转移你已有的文件')
    expect(document.getElementById('sharing-kind')).toBeNull()
    expect(api.request).not.toHaveBeenCalled()
    const link = document.querySelector<HTMLAnchorElement>('[data-personal-sharing-guidance] a')!
    await settle(() => link.click())
    expect(onClose).toHaveBeenCalledOnce()
    expect(location.pathname).toBe('/spaces/current')
    expect(onChanged).not.toHaveBeenCalled()
  })
})
