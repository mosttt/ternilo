import { act } from 'react'
import { openChoiceSelect, selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { MemberPage, MembershipRecord } from '@/types'
import { PlatformMembersSettings } from './platform-members-settings'
import { listMemberships, removeMembership, setMembership } from './platform-admin-api'

vi.mock('./platform-admin-api', () => ({
  listMemberships: vi.fn(),
  removeMembership: vi.fn(),
  setMembership: vi.fn(),
}))

const member: MembershipRecord = {
  user_id: 'usr-existing',
  username: 'Existing User',
  role: 'member',
  created_at_ms: 1,
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  setupChoiceSelect()
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.mocked(listMemberships).mockResolvedValue({ memberships: [member], next_cursor: null })
  vi.mocked(setMembership).mockResolvedValue(undefined)
  vi.mocked(removeMembership).mockResolvedValue(undefined)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  vi.clearAllMocks()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function render() {
  await act(async () => {
    root.render(<LocaleProvider><PlatformMembersSettings tenantId="tenant-a" /></LocaleProvider>)
    await Promise.resolve()
    await Promise.resolve()
  })
}

async function settle(action?: () => void) {
  await act(async () => {
    action?.()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

function setInput(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
  setter?.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

function button(text: string) {
  const match = [...document.querySelectorAll<HTMLButtonElement>('button')]
    .find(item => item.textContent?.trim() === text || item.getAttribute('aria-label')?.startsWith(text))
  if (!match) throw new Error(`missing button ${text}`)
  return match
}

describe('Platform member settings', () => {
  it('requests bounded server pages and resets the cursor when searching', async () => {
    vi.mocked(listMemberships).mockResolvedValueOnce({ memberships: [member], next_cursor: 'page-two' })
    await render()
    expect(listMemberships).toHaveBeenLastCalledWith('tenant-a', { query: '', cursor: null }, expect.any(AbortSignal))
    await settle(() => button('下一页').click())
    expect(listMemberships).toHaveBeenLastCalledWith('tenant-a', { query: '', cursor: 'page-two' }, expect.any(AbortSignal))
    await settle(() => setInput(host.querySelector<HTMLInputElement>('#platform-member-search')!, ' Existing '))
    await settle(() => button('搜索').click())
    expect(listMemberships).toHaveBeenLastCalledWith('tenant-a', { query: 'Existing', cursor: null }, expect.any(AbortSignal))
  })
  it('explains the login prerequisite and performs add, role update, and removal through the API', async () => {
    await render()
    expect(host.textContent).toContain('添加已有账号时，请输入对方的用户 ID')
    expect(host.textContent).toContain('通过上方邀请链接注册并加入团队')
    expect(host.textContent).toContain('usr-existing')

    const id = host.querySelector<HTMLInputElement>('#platform-member-id')!
    const addRole = host.querySelector<HTMLElement>('#platform-member-role')!
    await settle(() => setInput(id, 'usr-new'))
    await selectChoice(addRole, 'admin')
    await settle(() => button('添加或更新成员').click())
    expect(setMembership).toHaveBeenCalledWith('tenant-a', 'usr-new', 'admin')

    const row = host.querySelector<HTMLElement>('[data-platform-member="usr-existing"]')!
    const role = row.querySelector<HTMLElement>('[role="combobox"]')!
    await selectChoice(role, 'admin')
    await settle(() => button('保存角色').click())
    expect(setMembership).toHaveBeenCalledWith('tenant-a', 'usr-existing', 'admin')

    await settle(() => button('删除成员').click())
    expect(document.body.textContent).toContain('删除该成员？')
    const confirmation = document.querySelector<HTMLElement>('[data-settings-dialog]')!
    const confirm = [...confirmation.querySelectorAll<HTMLButtonElement>('button')]
      .find(item => item.textContent?.trim() === '删除成员')!
    await settle(() => confirm.click())
    expect(removeMembership).toHaveBeenCalledWith('tenant-a', 'usr-existing')
  })

  it('renders an explicit permission state with a visible retry action', async () => {
    vi.mocked(listMemberships).mockRejectedValue(new ApiError('forbidden', 403, 'policy'))
    await render()
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('error')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('没有管理当前空间')
    expect(button('重试')).toBeTruthy()
  })

  it('distinguishes loading and empty, then preserves ready rows during a failed refresh', async () => {
    let resolveInitial!: (value: MemberPage) => void
    vi.mocked(listMemberships).mockReturnValueOnce(new Promise(resolve => { resolveInitial = resolve }))
    act(() => root.render(<LocaleProvider><PlatformMembersSettings tenantId="tenant-a" /></LocaleProvider>))
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('loading')

    await act(async () => {
      resolveInitial({ memberships: [], next_cursor: null })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('empty')

    vi.mocked(listMemberships).mockResolvedValueOnce({ memberships: [member], next_cursor: null })
    await settle(() => button('刷新').click())
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('usr-existing')

    let rejectRefresh!: (cause: Error) => void
    vi.mocked(listMemberships).mockReturnValueOnce(new Promise((_resolve, reject) => { rejectRefresh = reject }))
    await act(async () => {
      button('刷新').click()
      await Promise.resolve()
    })
    expect(host.textContent).toContain('usr-existing')
    expect(host.textContent).toContain('正在加载')

    await act(async () => {
      rejectRefresh(new Error('offline'))
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('usr-existing')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('成员列表加载失败')
  })

  it('does not let an admin grant, change, or remove the owner role', async () => {
    vi.mocked(listMemberships).mockResolvedValue({ memberships: [
      member,
      {
        user_id: 'usr-owner', username: 'Owner',
        role: 'owner', created_at_ms: 1,
      },
    ], next_cursor: null })
    await act(async () => {
      root.render(
        <LocaleProvider><PlatformMembersSettings tenantId="tenant-a" actorRole="admin" /></LocaleProvider>,
      )
      await Promise.resolve()
      await Promise.resolve()
    })

    const addRole = host.querySelector<HTMLElement>('#platform-member-role')!
    await openChoiceSelect(addRole)
    expect([...document.querySelectorAll<HTMLElement>('[role="option"]')].map(option => option.dataset.choiceOption)).toEqual(['viewer', 'member', 'admin'])
    await act(async () => document.querySelector<HTMLElement>('[role="listbox"]')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))

    const memberRow = host.querySelector<HTMLElement>('[data-platform-member="usr-existing"]')!
    expect(memberRow.querySelector('[role="combobox"]')?.textContent).not.toContain('所有者')

    const ownerRow = host.querySelector<HTMLElement>('[data-platform-member="usr-owner"]')!
    expect(ownerRow.querySelector<HTMLButtonElement>('[role="combobox"]')?.disabled).toBe(true)
    expect(ownerRow.querySelector('[role="combobox"]')?.getAttribute('data-choice-value')).toBe('owner')
    expect([...ownerRow.querySelectorAll<HTMLButtonElement>('button')].every(item => item.disabled)).toBe(true)
    expect(setMembership).not.toHaveBeenCalled()
    expect(removeMembership).not.toHaveBeenCalled()
  })
})
