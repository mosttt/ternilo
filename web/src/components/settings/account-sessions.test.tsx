import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { AccountSettings } from './account-settings'

const workbench = vi.hoisted(() => ({
  serverIdentity: { email: 'member@example.test', user: { user_id: 'member', username: 'Member' } },
  serverAuthConfig: { oidc_enabled: false },
  logout: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))

const current = { session_id: 'ter_s_current', created_at_ms: 1_700_000_000_000, expires_at_ms: 1_700_604_800_000, is_current: true }
const other = { session_id: 'ter_s_other', created_at_ms: 1_700_000_001_000, expires_at_ms: 1_700_604_801_000, is_current: false }
let root: Root
let host: HTMLDivElement

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const storage = new Map<string, string>()
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value) },
    removeItem: (key: string) => { storage.delete(key) },
    clear: () => storage.clear(),
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.serverIdentity.user.user_id = 'member'
  workbench.logout.mockReset()
  vi.spyOn(api, 'request').mockResolvedValue({ current_login: 'native', sessions: [current, other] })
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

async function render() {
  await act(async () => root.render(<LocaleProvider><AccountSettings /></LocaleProvider>))
}

function button(label: string, container: ParentNode = host) {
  const found = [...container.querySelectorAll<HTMLButtonElement>('button')].find(item => item.textContent === label)
  if (!found) throw new Error(`missing button: ${label}`)
  return found
}

function dialog() { return document.querySelector<HTMLElement>('[data-settings-dialog]')! }

describe('Account browser sessions', () => {
  it('preserves account details and identifies the current native session', async () => {
    await render()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('member')
    expect(host.querySelector('[data-account-email]')?.textContent).toBe('member@example.test')
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(2)
    expect(host.querySelector('[data-current-session]')?.textContent).toBe('当前会话')
    expect(host.textContent).toContain('登录时间：')
    expect(host.textContent).toContain('到期时间：')
    expect(api.request).toHaveBeenCalledWith('/auth/sessions')
  })

  it('requires confirmation before revoking the current session and then signs out', async () => {
    await render()
    await act(async () => button('撤销当前会话').click())
    expect(dialog().textContent).toContain('立即退出当前登录')
    expect(api.request).toHaveBeenCalledTimes(1)
    await act(async () => button('取消', dialog()).click())
    expect(workbench.logout).not.toHaveBeenCalled()
    await act(async () => button('撤销当前会话').click())
    vi.mocked(api.request).mockResolvedValueOnce({ revoked_count: 1, current_revoked: true })
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(api.request).toHaveBeenLastCalledWith('/auth/sessions/ter_s_current', { method: 'DELETE' })
    expect(workbench.logout).toHaveBeenCalledOnce()
  })

  it('keeps the current login when revoking another session or all others', async () => {
    await render()
    await act(async () => button('撤销会话').click())
    vi.mocked(api.request).mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'native', sessions: [current] })
    await act(async () => button('撤销会话', dialog()).click())
    expect(api.request).toHaveBeenCalledWith('/auth/sessions/ter_s_other', { method: 'DELETE' })
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(1)
    expect(host.textContent).toContain('已撤销 1 个登录会话')
    expect(button('撤销其他会话').disabled).toBe(true)
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'native', sessions: [current, other] })
    await act(async () => button('刷新').click())
    await act(async () => button('撤销其他会话').click())
    expect(dialog().textContent).toContain('保留当前登录')
    vi.mocked(api.request).mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'native', sessions: [current] })
    await act(async () => button('撤销其他会话', dialog()).click())
    expect(api.request).toHaveBeenCalledWith('/auth/sessions/revoke-others', { method: 'POST' })
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('keeps failed revocation visible and does not log out prematurely', async () => {
    await render()
    await act(async () => button('撤销当前会话').click())
    vi.mocked(api.request).mockRejectedValueOnce(new Error('offline'))
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(dialog().querySelector('[role="alert"]')?.textContent).toBe('撤销失败：offline')
    expect(workbench.logout).not.toHaveBeenCalled()
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(2)
  })

  it('shows loading, a retryable failure, and an empty OIDC list', async () => {
    let reject!: (cause: Error) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail }) as never)
    await render()
    expect(host.textContent).toContain('正在加载登录会话')
    expect(button('刷新').disabled).toBe(true)
    await act(async () => reject(new Error('offline')))
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('offline')
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'oidc', sessions: [] })
    await act(async () => button('刷新').click())
    expect(host.textContent).toContain('没有有效的原生登录会话')
    expect(host.querySelector('[data-account-sessions-oidc]')?.textContent).toContain('不会退出当前 OIDC 登录')
    expect(button('撤销全部原生会话').disabled).toBe(true)
  })

  it('explains OIDC limits in English and revokes native sessions without signing out', async () => {
    localStorage.setItem('ternilo.locale', 'en')
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'oidc', sessions: [other] })
    await render()
    expect(host.textContent).toContain('Browser sign-in sessions')
    expect(host.querySelector('[data-account-sessions-oidc]')?.textContent).toContain('does not sign you out of OIDC')
    expect(host.querySelector('[data-current-session]')).toBeNull()
    await act(async () => button('Revoke all native sessions').click())
    expect(dialog().textContent).toContain('external sessions at your identity provider are unaffected')
    vi.mocked(api.request).mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    vi.mocked(api.request).mockResolvedValueOnce({ current_login: 'oidc', sessions: [] })
    await act(async () => button('Revoke all native sessions', dialog()).click())
    expect(host.textContent).toContain('No active native sign-in sessions')
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('clears the ready list and confirmation immediately when the account changes in place', async () => {
    await render()
    await act(async () => button('撤销当前会话').click())
    let resolveList!: (value: unknown) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { resolveList = resolve }) as never)
    workbench.serverIdentity.user.user_id = 'second'
    await render()
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(0)
    expect(dialog()).toBeNull()
    expect(host.textContent).toContain('正在加载登录会话')
    await act(async () => resolveList({ current_login: 'native', sessions: [{ ...current, session_id: 'ter_s_second-account' }] }))
    expect(host.querySelector('[data-account-session="ter_s_second-account"]')).not.toBeNull()
    expect(host.querySelector('[data-account-session="ter_s_current"]')).toBeNull()
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('discards a previous account list and a late revocation cannot log out the new account', async () => {
    let resolveList!: (value: unknown) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { resolveList = resolve }) as never)
    await render()
    workbench.serverIdentity.user.user_id = 'second'
    await render()
    await act(async () => resolveList({ current_login: 'native', sessions: [{ ...other, session_id: 'ter_s_stale' }] }))
    expect(host.querySelector('[data-account-session="ter_s_stale"]')).toBeNull()
    await act(async () => button('撤销当前会话').click())
    let resolveRevoke!: (value: unknown) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { resolveRevoke = resolve }) as never)
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(button('正在撤销…', dialog()).disabled).toBe(true)
    workbench.serverIdentity.user.user_id = 'third'
    await render()
    await act(async () => resolveRevoke({ revoked_count: 1, current_revoked: true }))
    expect(workbench.logout).not.toHaveBeenCalled()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('third')
  })
})
