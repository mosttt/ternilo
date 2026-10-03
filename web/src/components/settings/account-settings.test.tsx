import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { beginOidcLink } from '@/auth/oidc'
import { LocaleProvider } from '@/i18n/provider'
import { AccountSettings } from './account-settings'

const workbench = vi.hoisted(() => ({
  serverIdentity: { email: 'member@example.test', user: { user_id: 'member', username: 'My account' }, is_instance_owner: false },
  serverAuthConfig: { oidc_enabled: true },
  currentTenantId: 'team-a',
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('./account-sessions', () => ({ AccountSessions: () => null }))
vi.mock('@/auth/oidc', async original => ({ ...await original<typeof import('@/auth/oidc')>(), beginOidcLink: vi.fn() }))
let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.serverIdentity.user.user_id = 'member'
  workbench.serverIdentity.user.username = 'My account'
  workbench.serverAuthConfig.oidc_enabled = true
  workbench.currentTenantId = 'team-a'
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn().mockResolvedValue(undefined) } })
  vi.spyOn(api, 'request').mockResolvedValue({ native: true, oidc: null })
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.clearAllMocks()
})
async function render() { await act(async () => root.render(<LocaleProvider><AccountSettings /></LocaleProvider>)) }
function button(text: string) { return [...host.querySelectorAll('button')].find(button => button.textContent === text) }
function copyIdButton() { return host.querySelector<HTMLButtonElement>('button[aria-label="复制账号 ID"]')! }

describe('Account sign-in settings', () => {
  it('lets a native member explicitly begin linking without instance-owner rights', async () => {
    await render()
    expect(host.textContent).toContain('My account')
    expect(host.querySelector('[data-account-email]')?.textContent).toBe('member@example.test')
    const link = button('绑定 OIDC 登录')!
    await act(async () => link.click())
    expect(beginOidcLink).toHaveBeenCalledOnce()
    expect(link.disabled).toBe(true)
  })

  it('shows an existing issuer and does not offer linking from an OIDC-only login', async () => {
    vi.mocked(api.request).mockResolvedValueOnce({ native: false, oidc: { issuer: 'https://identity.example', subject: 'subject' } })
    await render()
    expect(host.textContent).toContain('OIDC 登录已绑定')
    expect(host.textContent).toContain('https://identity.example')
    expect(button('绑定 OIDC 登录')).toBeUndefined()
    expect(host.querySelector('[data-account-password]')).toBeNull()
  })

  it('drops an old account response after switching the current identity', async () => {
    let release!: (value: unknown) => void
    vi.mocked(api.request).mockImplementationOnce(() => new Promise(resolve => { release = resolve }) as never)
    await render()
    workbench.serverIdentity.user.user_id = 'other'
    await render()
    await act(async () => release({ native: false, oidc: { issuer: 'https://old.example', subject: 'old' } }))
    expect(host.textContent).not.toContain('https://old.example')
    expect(button('绑定 OIDC 登录')).toBeDefined()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('other')
    await act(async () => copyIdButton().click())
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('other')
  })

  it('loads native password settings even when OIDC is disabled', async () => {
    workbench.serverAuthConfig.oidc_enabled = false
    await render()
    expect(api.request).toHaveBeenCalledWith('/auth/oidc-link')
    expect(host.querySelector('[data-account-password]')).not.toBeNull()
    expect(button('绑定 OIDC 登录')).toBeUndefined()
    expect(host.textContent).toContain('My account')
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('member')
    await act(async () => copyIdButton().click())
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith('member')
    expect(host.querySelector('[role="status"]')?.textContent).toBe('已复制账号 ID')
  })

  it('copies the stable platform ID independently of the username, space and OIDC subject', async () => {
    workbench.serverIdentity.user.user_id = 'usr_stable-account'
    workbench.serverIdentity.user.username = 'aa'
    vi.mocked(api.request).mockResolvedValue({ native: true, oidc: { issuer: 'https://identity.example', subject: 'different-oidc-subject' } })
    await render()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('usr_stable-account')
    expect(host.textContent).toContain('aa')
    expect(host.textContent).not.toContain('different-oidc-subject')
    await act(async () => copyIdButton().click())
    expect(navigator.clipboard.writeText).toHaveBeenLastCalledWith('usr_stable-account')
    workbench.serverIdentity.user.username = 'Renamed account'
    workbench.currentTenantId = 'team-b'
    await render()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('usr_stable-account')
    await act(async () => copyIdButton().click())
    expect(navigator.clipboard.writeText).toHaveBeenLastCalledWith('usr_stable-account')
    workbench.serverIdentity.user.user_id = 'usr_other-account'
    await render()
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('usr_other-account')
    expect(host.textContent).not.toContain('已复制账号 ID')
  })

  it('keeps the ID visible and reports a rejected clipboard write', async () => {
    workbench.serverAuthConfig.oidc_enabled = false
    vi.mocked(navigator.clipboard.writeText).mockRejectedValue(new Error('clipboard denied'))
    await render()
    await act(async () => copyIdButton().click())
    expect(host.querySelector('[data-account-id]')?.textContent).toBe('member')
    expect(host.querySelector('[role="alert"]')?.textContent).toBe('复制失败，请手动选择并复制账号 ID。')
  })
})
