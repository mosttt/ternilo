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

const current = { login_kind: 'native', access_expires_at_ms: 1_700_604_800_000, session_id: 'ter_s_current', created_at_ms: 1_700_000_000_000, expires_at_ms: 1_700_604_800_000, is_current: true }
const other = { login_kind: 'native', access_expires_at_ms: 1_700_604_801_000, session_id: 'ter_s_other', created_at_ms: 1_700_000_001_000, expires_at_ms: 1_700_604_801_000, is_current: false }
const sessionRequest = vi.fn<typeof api.request>()
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
  sessionRequest.mockReset().mockResolvedValue({ current_login: 'native', current_session_managed: true, sessions: [current, other] })
  vi.spyOn(api, 'request').mockImplementation((resource, options) => {
    if (resource === '/auth/oidc-link') return Promise.resolve({ native: true, oidc: null }) as never
    if (resource === '/auth/mfa') return Promise.resolve({ enabled: false, enabled_at_ms: null, recovery_codes_remaining: 0 }) as never
    return sessionRequest(resource, options)
  })
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

  it('manages a current local OIDC session alongside password sign-ins and signs out only when it is revoked', async () => {
    const oidc = { ...current, login_kind: 'oidc', issuer: 'https://login.example.test', access_expires_at_ms: current.created_at_ms + 3600000, user_agent: 'Mozilla/5.0 (X11; Linux x86_64) Firefox/123.0', first_ip: '192.0.2.4', last_ip: '2001:db8::4', last_active_at_ms: current.created_at_ms + 60000 }
    sessionRequest.mockResolvedValueOnce({ current_login: 'oidc', current_session_managed: true, sessions: [oidc, other] })
    await render()
    expect(host.querySelector('[data-account-sessions-oidc]')).toBeNull()
    expect(host.textContent).toContain('OIDC 登录')
    expect(host.textContent).toContain('密码登录')
    expect(host.textContent).toContain('身份提供方：https://login.example.test')
    expect(host.textContent).toContain('访问凭据到期：')
    expect(host.textContent).toContain('首次来源 IP：192.0.2.4')
    await act(async () => button('撤销其他会话').click())
    expect(dialog().textContent).toContain('其他密码登录和本站 OIDC 会话')
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    sessionRequest.mockResolvedValueOnce({ current_login: 'oidc', current_session_managed: true, sessions: [oidc] })
    await act(async () => button('撤销其他会话', dialog()).click())
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(1)
    expect(workbench.logout).not.toHaveBeenCalled()
    await act(async () => button('撤销当前会话').click())
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: true })
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(workbench.logout).toHaveBeenCalledOnce()
  })

  it('shows missing OIDC login metadata honestly and always makes its public ID inspectable', async () => {
    sessionRequest.mockResolvedValueOnce({ current_login: 'native', current_session_managed: true, sessions: [current, { ...other, login_kind: 'oidc', created_at_ms: null }] })
    await render()
    const row = host.querySelector('[data-account-session="ter_s_other"]')!
    expect(row.textContent).toContain('登录时间：尚未记录')
    expect(row.textContent).not.toContain('Invalid Date')
    expect(row.querySelector('details')?.textContent).toContain('ter_s_other')
  })

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
    expect(vi.mocked(api.request).mock.calls.every(([, options]) => !options?.method || options.method === 'GET')).toBe(true)
    await act(async () => button('取消', dialog()).click())
    expect(workbench.logout).not.toHaveBeenCalled()
    await act(async () => button('撤销当前会话').click())
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: true })
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(api.request).toHaveBeenLastCalledWith('/auth/sessions/ter_s_current', { method: 'DELETE' })
    expect(workbench.logout).toHaveBeenCalledOnce()
  })

  it('keeps the current login when revoking another session or all others', async () => {
    await render()
    await act(async () => button('撤销会话').click())
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    sessionRequest.mockResolvedValueOnce({ current_login: 'native', current_session_managed: true, sessions: [current] })
    await act(async () => button('撤销会话', dialog()).click())
    expect(api.request).toHaveBeenCalledWith('/auth/sessions/ter_s_other', { method: 'DELETE' })
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(1)
    expect(host.textContent).toContain('已撤销 1 个登录会话')
    expect(button('撤销其他会话').disabled).toBe(true)
    sessionRequest.mockResolvedValueOnce({ current_login: 'native', current_session_managed: true, sessions: [current, other] })
    await act(async () => button('刷新').click())
    await act(async () => button('撤销其他会话').click())
    expect(dialog().textContent).toContain('保留当前登录')
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    sessionRequest.mockResolvedValueOnce({ current_login: 'native', current_session_managed: true, sessions: [current] })
    await act(async () => button('撤销其他会话', dialog()).click())
    expect(api.request).toHaveBeenCalledWith('/auth/sessions/revoke-others', { method: 'POST' })
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('keeps failed revocation visible and does not log out prematurely', async () => {
    await render()
    await act(async () => button('撤销当前会话').click())
    sessionRequest.mockRejectedValueOnce(new Error('offline'))
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(dialog().querySelector('[role="alert"]')?.textContent).toBe('撤销失败：offline')
    expect(workbench.logout).not.toHaveBeenCalled()
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(2)
  })

  it('shows loading, a retryable failure, and an empty OIDC list', async () => {
    let reject!: (cause: Error) => void
    sessionRequest.mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail }) as never)
    await render()
    expect(host.textContent).toContain('正在加载登录会话')
    expect(button('刷新').disabled).toBe(true)
    await act(async () => reject(new Error('offline')))
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('offline')
    sessionRequest.mockResolvedValueOnce({ current_login: 'oidc', current_session_managed: false, sessions: [] })
    await act(async () => button('刷新').click())
    expect(host.textContent).toContain('没有有效的本站登录会话')
    expect(host.querySelector('[data-account-sessions-oidc]')?.textContent).toContain('不会撤销当前外部凭据')
    expect(button('撤销全部本站会话').disabled).toBe(true)
  })

  it('explains OIDC limits in English and revokes native sessions without signing out', async () => {
    localStorage.setItem('ternilo.locale', 'en')
    sessionRequest.mockResolvedValueOnce({ current_login: 'oidc', current_session_managed: false, sessions: [other] })
    await render()
    expect(host.textContent).toContain('Browser sign-in sessions')
    expect(host.querySelector('[data-account-sessions-oidc]')?.textContent).toContain('does not revoke that external credential')
    expect(host.querySelector('[data-current-session]')).toBeNull()
    await act(async () => button('Revoke all local sessions').click())
    expect(dialog().textContent).toContain('identity-provider sessions are unaffected')
    sessionRequest.mockResolvedValueOnce({ revoked_count: 1, current_revoked: false })
    sessionRequest.mockResolvedValueOnce({ current_login: 'oidc', current_session_managed: false, sessions: [] })
    await act(async () => button('Revoke all local sessions', dialog()).click())
    expect(host.textContent).toContain('No active local sign-in sessions')
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('clears the ready list and confirmation immediately when the account changes in place', async () => {
    await render()
    await act(async () => button('撤销当前会话').click())
    let resolveList!: (value: unknown) => void
    sessionRequest.mockImplementationOnce(() => new Promise(resolve => { resolveList = resolve }) as never)
    workbench.serverIdentity.user.user_id = 'second'
    await render()
    expect(host.querySelectorAll('[data-account-session]')).toHaveLength(0)
    expect(dialog()).toBeNull()
    expect(host.textContent).toContain('正在加载登录会话')
    await act(async () => resolveList({ current_login: 'native', current_session_managed: true, sessions: [{ ...current, session_id: 'ter_s_second-account' }] }))
    expect(host.querySelector('[data-account-session="ter_s_second-account"]')).not.toBeNull()
    expect(host.querySelector('[data-account-session="ter_s_current"]')).toBeNull()
    expect(workbench.logout).not.toHaveBeenCalled()
  })

  it('discards a previous account list and a late revocation cannot log out the new account', async () => {
    let resolveList!: (value: unknown) => void
    sessionRequest.mockImplementationOnce(() => new Promise(resolve => { resolveList = resolve }) as never)
    await render()
    workbench.serverIdentity.user.user_id = 'second'
    await render()
    await act(async () => resolveList({ current_login: 'native', current_session_managed: true, sessions: [{ ...other, session_id: 'ter_s_stale' }] }))
    expect(host.querySelector('[data-account-session="ter_s_stale"]')).toBeNull()
    await act(async () => button('撤销当前会话').click())
    let resolveRevoke!: (value: unknown) => void
    sessionRequest.mockImplementationOnce(() => new Promise(resolve => { resolveRevoke = resolve }) as never)
    await act(async () => button('撤销当前会话', dialog()).click())
    expect(button('正在撤销…', dialog()).disabled).toBe(true)
    workbench.serverIdentity.user.user_id = 'third'
    await render()
    await act(async () => resolveRevoke({ revoked_count: 1, current_revoked: true }))
    expect(workbench.logout).not.toHaveBeenCalled()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('third')
  })
})
