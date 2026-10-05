import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { beginOidcLogin } from '@/auth/oidc'
import type { ServerAuthConfig } from '@/auth/server'
import { api, ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import { ServerLogin } from './server-login'

const workbench = vi.hoisted(() => ({
  authRequired: true,
  accessPaused: false,
  oidcRegistrationRequired: false,
  registerOidcUsername: vi.fn<() => Promise<'active' | 'pending' | void>>(async () => 'active'),
  serverAuthConfig: { initialized: true, mode: 'single_user', native_enabled: true, oidc_enabled: false, oidc_providers: [] as ServerAuthConfig['oidc_providers'], registration: { mode: 'invite', require_approval: false, oidc_only: false, revision: 1 } },
  error: '',
  authenticate: vi.fn<() => Promise<'active' | 'pending' | void>>(async () => undefined),
  login: vi.fn(async () => undefined),
  retryAuthentication: vi.fn(),
  logout: vi.fn(),
  selectTenant: vi.fn(async () => undefined),
  notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/auth/oidc', async original => ({ ...await original<typeof import('@/auth/oidc')>(), beginOidcLogin: vi.fn() }))

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  workbench.serverAuthConfig = { initialized: true, mode: 'single_user', native_enabled: true, oidc_enabled: false, oidc_providers: [] as ServerAuthConfig['oidc_providers'], registration: { mode: 'invite', require_approval: false, oidc_only: false, revision: 1 } }
  sessionStorage.clear()
  workbench.accessPaused = false
  workbench.oidcRegistrationRequired = false
  workbench.registerOidcUsername.mockResolvedValue('active')
  workbench.authRequired = true
  workbench.error = ''
  workbench.authenticate.mockResolvedValue(undefined)
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  history.replaceState({}, '', '/')
  vi.clearAllMocks()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

async function settle(action?: () => void) {
  await act(async () => { action?.(); await Promise.resolve(); await Promise.resolve() })
}
async function mount() { await settle(() => root.render(<LocaleProvider><ServerLogin /></LocaleProvider>)) }
function input(id: string) { return document.getElementById(id) as HTMLInputElement }
function fill(id: string, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input(id), value)
  input(id).dispatchEvent(new Event('input', { bubbles: true }))
}
function submit() { document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })) }

describe('Server account form', () => {
  it('explains paused access without treating it as an expired account', async () => {
    workbench.accessPaused = true
    await mount()
    expect(document.body.textContent).toContain('你的数据、机器归属和正在执行的任务仍然保留')
    expect(document.getElementById('server-password')).toBeNull()
    await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '重试访问')!.click())
    expect(workbench.retryAuthentication).toHaveBeenCalledOnce()
    await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '使用其他账号')!.click())
    expect(workbench.logout).toHaveBeenCalledOnce()
  })
  it('opens an invitation arriving in the current tab without reloading or retaining its URL secret', async () => {
    workbench.serverAuthConfig.mode = 'multi_user'
    await mount()
    await settle(() => {
      history.replaceState({}, '', '/#invite=later-invitation')
      window.dispatchEvent(new HashChangeEvent('hashchange'))
    })
    expect(location.hash).toBe('')
    expect(input('server-display-name')).toBeNull()
    expect(input('server-account-token').value).toBe('later-invitation')
    expect(document.body.textContent).toContain('创建账号')
  })
  it('signs in natively in single-user mode without requiring an OIDC provider', async () => {
    await mount()
    expect(document.body.textContent).not.toContain('使用 Organization 登录')
    await settle(() => { fill('server-username', ' alice '); fill('server-password', ' password ') })
    await settle(submit)
    expect(workbench.authenticate).toHaveBeenCalledWith({ action: 'login', username: 'alice', password: ' password ' })
    expect(workbench.login).not.toHaveBeenCalled()
  })

  it('uses a setup fragment once and submits the owner account fields', async () => {
    workbench.serverAuthConfig.initialized = false
    history.replaceState({}, '', '/#setup_token=private-setup')
    await mount()
    expect(location.hash).toBe('')
    expect(input('server-display-name')).toBeNull()
    expect(input('server-account-token').value).toBe('private-setup')
    await settle(() => { fill('server-username', 'owner'); fill('server-password', 'password') })
    await settle(() => fill('server-email', 'contact@example.test'))
    await settle(submit)
    expect(workbench.authenticate).toHaveBeenCalledWith({ action: 'setup', email: 'contact@example.test', setup_token: 'private-setup', username: 'owner', password: 'password' })
    expect(input('server-password').value).toBe('')
    expect(input('server-account-token').value).toBe('')
  })

  it('accepts an invitation and keeps entered fields when the server rejects it', async () => {
    workbench.serverAuthConfig.mode = 'multi_user'
    history.replaceState({}, '', '/#invite=invitation-secret')
    workbench.authenticate.mockRejectedValueOnce(new Error('Invitation expired'))
    await mount()
    expect(location.hash).toBe('')
    expect(input('server-display-name')).toBeNull()
    await settle(() => { fill('server-username', 'invited'); fill('server-password', 'password') })
    await settle(() => fill('server-email', 'contact@example.test'))
    await settle(submit)
    expect(workbench.authenticate).toHaveBeenCalledWith({ action: 'accept', email: 'contact@example.test', token: 'invitation-secret', username: 'invited', password: 'password' })
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('Invitation expired')
    expect(input('server-username').value).toBe('invited')
  })

  it('offers organization sign-in only when the server enables OIDC', async () => {
    workbench.serverAuthConfig.oidc_enabled = true
    workbench.serverAuthConfig.oidc_providers = [{ id: 'organization', name: 'Organization', config: { authorization_endpoint: '', client_id: '', redirect_uri: '', scope: 'openid' } }]
    await mount()
    const button = [...document.querySelectorAll('button')].find(button => button.textContent === '使用 Organization 登录')!
    await settle(() => button.click())
    expect(beginOidcLogin).toHaveBeenCalledWith('organization', undefined)
    expect(workbench.authenticate).not.toHaveBeenCalled()
  })
})

it('joins an invited team with the current account without creating another account', async () => {
  workbench.authRequired = false
  history.replaceState({}, '', '/#team_invite=team-token')
  vi.spyOn(api, 'request').mockResolvedValue({ tenant_id: 'team-one', kind: 'team', display_name: 'Team', slug: 'team', role: 'member' })
  await mount()
  expect(input('server-password')).toBeNull()
  expect(document.body.textContent).toContain('个人空间和已有项目将保留')
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '加入团队')!.click())
  expect(api.request).toHaveBeenCalledWith('/invitations/accept', { method: 'POST', body: { token: 'team-token' } })
  expect(workbench.selectTenant).toHaveBeenCalledWith('team-one')
  expect(workbench.authenticate).not.toHaveBeenCalled()
})

it('keeps the team invitation while an existing account signs in', async () => {
  history.replaceState({}, '', '/#team_invite=team-token')
  await mount()
  expect(input('server-account-token')).toBeNull()
  await settle(() => { fill('server-username', 'existing'); fill('server-password', 'password') })
  await settle(submit)
  expect(workbench.authenticate).toHaveBeenCalledWith({ action: 'login', username: 'existing', password: 'password' })
  workbench.authRequired = false
  await mount()
  expect(document.body.textContent).toContain('加入邀请中的团队')
})

it('offers public signup without an invitation and explains pending registration before sign-in', async () => {
  workbench.serverAuthConfig.mode = 'multi_user'
  workbench.serverAuthConfig.registration = { mode: 'open', require_approval: true, oidc_only: false, revision: 2 }
  workbench.authenticate.mockResolvedValueOnce('pending')
  await mount()
  expect(document.body.textContent).not.toContain('使用邀请创建账号')
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '注册新账号')!.click())
  expect(input('server-account-token')).toBeNull()
  await settle(() => { fill('server-username', 'candidate'); fill('server-password', 'password') })
  await settle(() => fill('server-email', 'contact@example.test'))
  await settle(submit)
  expect(workbench.authenticate).toHaveBeenCalledWith({ action: 'register', email: 'contact@example.test', username: 'candidate', password: 'password' })
  expect(document.querySelector('[data-registration-pending]')?.textContent).toContain('candidate')
  expect(input('server-password')).toBeNull()
  expect(workbench.login).not.toHaveBeenCalled()
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '返回登录')!.click())
  expect(input('server-password').value).toBe('')
})

it.each([
  ['account registration is pending approval', '等待管理员审核'],
  ['account registration was rejected', '未通过审核'],
  ['account is banned', '账号已被封禁'],
  ['account was removed', '账号已注销'],
])('translates account status denial: %s', async (message, expected) => {
  workbench.authenticate.mockRejectedValueOnce(new ApiError(message, 403, 'policy_denied'))
  await mount()
  await settle(() => { fill('server-username', 'candidate'); fill('server-password', 'password') })
  await settle(submit)
  expect(document.querySelector('[role="alert"]')?.textContent).toContain(expected)
})

it('does not expose signup in single-user mode even when the stored policy is open', async () => {
  workbench.serverAuthConfig.registration.mode = 'open'
  await mount()
  expect(document.body.textContent).not.toContain('注册新账号')
  expect(document.body.textContent).not.toContain('使用邀请创建账号')
})

it('keeps a team invitation through review registration without turning it into account invitation signup', async () => {
  workbench.serverAuthConfig.mode = 'multi_user'
  workbench.serverAuthConfig.registration = { mode: 'open', require_approval: true, oidc_only: false, revision: 2 }
  history.replaceState({}, '', '/#team_invite=team-token')
  workbench.authenticate.mockResolvedValueOnce('pending')
  await mount()
  expect(document.body.textContent).toContain('登录已有账号后接受团队邀请')
  expect(input('server-account-token')).toBeNull()
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '注册新账号')!.click())
  await settle(() => { fill('server-username', 'candidate'); fill('server-password', 'password') })
  await settle(() => fill('server-email', 'contact@example.test'))
  await settle(submit)
  expect(workbench.authenticate).toHaveBeenCalledWith(expect.objectContaining({ action: 'register' }))
  expect(sessionStorage.getItem('ternilo.pending-team-invitation')).toBe('team-token')
  expect(document.querySelector('[data-registration-pending]')).not.toBeNull()
})

it('explains reviewed registration in English without an invitation field', async () => {
  vi.stubGlobal('localStorage', { getItem: () => 'en', setItem: vi.fn() })
  workbench.serverAuthConfig.mode = 'multi_user'
  workbench.serverAuthConfig.registration = { mode: 'open', require_approval: true, oidc_only: false, revision: 2 }
  workbench.authenticate.mockResolvedValueOnce('pending')
  await mount()
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === 'Create a new account')!.click())
  expect(document.body.textContent).toContain('after an administrator approves')
  expect(input('server-account-token')).toBeNull()
  await settle(() => { fill('server-username', 'candidate'); fill('server-password', 'password') })
  await settle(() => fill('server-email', 'contact@example.test'))
  await settle(submit)
  expect(document.body.textContent).toContain('Registration submitted')
  expect(document.querySelector('[data-registration-pending]')?.textContent).toContain('do not need to register again')
})

it('translates pending OIDC identity errors on the login screen', async () => {
  workbench.error = 'account registration is pending approval'
  workbench.serverAuthConfig.oidc_enabled = true
    workbench.serverAuthConfig.oidc_providers = [{ id: 'organization', name: 'Organization', config: { authorization_endpoint: '', client_id: '', redirect_uri: '', scope: 'openid' } }]
  await mount()
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('等待管理员审核')
  expect(document.body.textContent).toContain('使用 Organization 登录')
})


it('asks a verified OIDC user for their platform username and contact email, keeping conflict input available', async () => {
  workbench.oidcRegistrationRequired = true
  workbench.registerOidcUsername.mockRejectedValueOnce(new ApiError('username is already registered', 409, 'conflict'))
  await mount()
  expect(document.body.textContent).toContain('完善账号信息')
  expect(input('server-password')).toBeNull()
  expect(input('server-display-name')).toBeNull()
  expect(input('server-account-token')).toBeNull()
  await settle(() => { fill('server-username', 'chosen-user'); fill('server-email', 'chosen@example.test') })
  await settle(submit)
  expect(workbench.registerOidcUsername).toHaveBeenCalledWith('chosen-user', 'chosen@example.test', undefined, undefined)
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('这个用户名已被使用')
  expect(input('server-username').value).toBe('chosen-user')
  await settle(() => fill('server-username', 'available-user'))
  await settle(submit)
  expect(workbench.registerOidcUsername).toHaveBeenLastCalledWith('available-user', 'chosen@example.test', undefined, undefined)
  expect(document.querySelector('[role="alert"]')).toBeNull()
  expect(workbench.authenticate).not.toHaveBeenCalled()
  expect(workbench.login).not.toHaveBeenCalled()
})

it('uses the provider email without an editable contact field', async () => {
  workbench.oidcRegistrationRequired = true
  sessionStorage.setItem('ternilo.oidc.email', 'provider@example.test')
  await mount()
  expect(document.getElementById('server-email')).toBeNull()
  expect(document.querySelector('[data-oidc-email]')?.textContent).toContain('provider@example.test')
  await settle(() => fill('server-username', 'chosen-user'))
  await settle(submit)
  expect(workbench.registerOidcUsername).toHaveBeenCalledWith('chosen-user', '', undefined, undefined)
})

it('automatically registers once using a supplied email and usable upstream username', async () => {
  workbench.oidcRegistrationRequired = true
  workbench.serverAuthConfig.mode = 'multi_user'
  workbench.serverAuthConfig.registration.mode = 'open'
  sessionStorage.setItem('ternilo.oidc.access', 'oidc-session')
  sessionStorage.setItem('ternilo.oidc.email', 'provider@example.test')
  sessionStorage.setItem('ternilo.oidc.username', 'upstream-user')
  await mount()
  expect(workbench.registerOidcUsername).toHaveBeenCalledExactlyOnceWith('upstream-user', '', undefined, undefined)
  await mount()
  expect(workbench.registerOidcUsername).toHaveBeenCalledOnce()
})

it('waits for required human verification before registering a complete upstream profile', async () => {
  workbench.oidcRegistrationRequired = true
  workbench.serverAuthConfig.mode = 'multi_user'
  workbench.serverAuthConfig.registration.mode = 'open'
  ;(workbench.serverAuthConfig as ServerAuthConfig).turnstile = { site_key: 'fixture-site' }
  sessionStorage.setItem('ternilo.oidc.access', 'oidc-session')
  sessionStorage.setItem('ternilo.oidc.email', 'provider@example.test')
  sessionStorage.setItem('ternilo.oidc.username', 'upstream-user')
  await mount()
  expect(workbench.registerOidcUsername).not.toHaveBeenCalled()
})

it('explains that pending OIDC applicants return with the same organization account', async () => {
  workbench.oidcRegistrationRequired = true
  workbench.serverAuthConfig.registration.require_approval = true
  workbench.registerOidcUsername.mockResolvedValueOnce('pending')
  await mount()
  await settle(() => { fill('server-username', 'reviewed-user'); fill('server-email', 'reviewed-user@example.test') })
  await settle(submit)
  expect(document.querySelector('[data-registration-pending]')?.textContent).toContain('reviewed-user')
  expect(document.body.textContent).toContain('使用同一个组织账号登录')
  expect(document.body.textContent).not.toContain('用户名和密码登录')
  expect(workbench.authenticate).not.toHaveBeenCalled()
})


it('offers a fresh organization sign-in when the verified username registration expires', async () => {
  const { OidcFlowError } = await import('@/auth/oidc')
  workbench.oidcRegistrationRequired = true
  workbench.registerOidcUsername.mockRejectedValueOnce(new OidcFlowError('expired'))
  await mount()
  await settle(() => { fill('server-username', 'chosen-user'); fill('server-email', 'chosen-user@example.test') })
  await settle(submit)
  expect(document.querySelector('[role="alert"]')?.textContent).toBe('登录已过期，请重新登录。')
  await settle(() => [...document.querySelectorAll('button')].find(button => button.textContent === '使用其他账号')!.click())
  expect(workbench.logout).toHaveBeenCalledOnce()
})
