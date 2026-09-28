import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { GroupRecord, MembershipRecord } from '@/types'
import { PlatformGroupsSettings } from './platform-groups-settings'

let host: HTMLDivElement
let root: Root
let groups: GroupRecord[]
let members: MembershipRecord[]
const group: GroupRecord = { group_id: 'group/a', tenant_id: 'team', name: 'Reviewers', description: null, member_count: 0, created_at_ms: 1, updated_at_ms: 1 }
const member: MembershipRecord = { user_id: 'user/a', username: 'Alice', role: 'member', created_at_ms: 1 }

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  groups = []; members = []
  vi.spyOn(api, 'request').mockImplementation(async (path, options) => {
    const method = options?.method ?? 'GET'
    if (path.endsWith('/groups') && method === 'POST') { groups = [{ ...group, ...options!.body as object }]; return groups[0] as never }
    if (path.includes('/groups/group%2Fa/members/user%2Fa')) { members = method === 'PUT' ? [member] : []; return undefined as never }
    if (path.includes('/groups/group%2Fa/members?')) return { memberships: members, next_cursor: null } as never
    if (path.endsWith('/groups/group%2Fa')) {
      if (method === 'DELETE') { groups = []; return undefined as never }
      if (method === 'PATCH') groups = [{ ...groups[0]!, ...options!.body as object }]
      return { ...groups[0], member_count: members.length } as never
    }
    if (path.includes('/groups?')) return { groups, next_cursor: null } as never
    if (path.includes('/members?')) return { memberships: [member], next_cursor: null } as never
    throw new Error(`Unexpected request: ${method} ${path}`)
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.clearAllMocks() })
async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() { await settle(() => root.render(<LocaleProvider><PlatformGroupsSettings tenantId="team" /></LocaleProvider>)) }
function button(label: string, scope: ParentNode = document) {
  const match = [...scope.querySelectorAll('button')].find(button => button.textContent?.trim() === label)
  if (!match) throw new Error(`Button not found: ${label}`)
  return match
}
function input(id: string, value: string) {
  const element = document.getElementById(id) as HTMLInputElement
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

describe('Team permission groups', () => {
  it('creates and edits a group, adds and removes real team members, and deletes the group', async () => {
    await mount()
    await settle(() => button('新建权限组').click())
    await settle(() => input('group-name', 'Reviewers'))
    await settle(() => button('新建权限组', document.querySelector('[role="dialog"]')!).click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups', { method: 'POST', body: { name: 'Reviewers', description: null } })
    expect(document.querySelector('[data-group-members]')).not.toBeNull()
    await settle(() => input('group-name', 'Maintainers'))
    await settle(() => button('保存组信息').click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups/group%2Fa', { method: 'PATCH', body: { name: 'Maintainers', description: null } })
    await settle(() => button('添加组员').click())
    await settle(() => button('加入').click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups/group%2Fa/members/user%2Fa', { method: 'PUT' })
    expect(document.body.textContent).toContain('成员已加入权限组')
    await settle(() => button('返回组员列表').click())
    expect(document.querySelector('[data-group-member="user/a"]')).not.toBeNull()
    await settle(() => document.querySelector<HTMLButtonElement>('[aria-label="将“Alice”移出权限组"]')!.click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups/group%2Fa/members/user%2Fa', { method: 'DELETE' })
    expect(document.body.textContent).toContain('通过该组获得的权限已撤销')
    await settle(() => document.querySelector<HTMLButtonElement>('[data-slot="dialog-close"]')!.click())
    await settle(() => document.querySelector<HTMLButtonElement>('[aria-label="删除权限组“Maintainers”"]')!.click())
    expect(document.body.textContent).toContain('撤销通过该组获得的权限')
    await settle(() => button('删除权限组', document.querySelector('[data-settings-dialog]')!).click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups/group%2Fa', { method: 'DELETE' })
  })

  it('uses server cursors for group pages and resets pagination when searching', async () => {
    groups = [group]
    vi.mocked(api.request).mockResolvedValueOnce({ groups, next_cursor: 'group-next' })
    await mount()
    await settle(() => button('下一页').click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups?limit=25&cursor=group-next', { signal: expect.any(AbortSignal) })
    await settle(() => input('platform-group-search', ' review '))
    await settle(() => button('搜索').click())
    expect(api.request).toHaveBeenCalledWith('/tenants/team/groups?limit=25&query=review', { signal: expect.any(AbortSignal) })
  })

  it('shows a denied group management response instead of editable fake data', async () => {
    vi.mocked(api.request).mockRejectedValueOnce(new Error('Group management is not allowed'))
    await mount()
    expect(document.querySelector('[role="alert"]')?.textContent).toContain('Group management is not allowed')
    expect(document.querySelector('[data-platform-group]')).toBeNull()
  })
})
