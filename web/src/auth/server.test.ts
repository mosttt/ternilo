import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { clearAccountLink, clearNativeSession, invitationUrl, isServerAccessPaused, loadServerAuthConfig, NATIVE_SESSION_KEY, readAccountLink, readNativeToken, signInNative, storeNativeSession } from './server'

beforeEach(() => {
  const values = new Map<string, string>()
  vi.stubGlobal('sessionStorage', {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  })
})
afterEach(() => { vi.unstubAllGlobals(); history.replaceState({}, '', '/') })

describe('Server account endpoints', () => {
  it('recognizes only the agreed access pause error, leaving ordinary permission errors unchanged', () => {
    expect(isServerAccessPaused({ code: 'policy_denied', message: 'this account is paused while the server is in single-user mode' })).toBe(true)
    expect(isServerAccessPaused({ code: 'policy_denied', message: 'workspace access denied' })).toBe(false)
    expect(isServerAccessPaused({ code: 'unavailable', message: 'this account is paused while the server is in single-user mode' })).toBe(false)
  })
  it('reads the one root public config, including optional OIDC', async () => {
    const config = { initialized: true, mode: 'single_user', native_enabled: true, oidc_enabled: false }
    const fetchMock = vi.fn(async () => new Response(JSON.stringify(config)))
    vi.stubGlobal('fetch', fetchMock)
    await expect(loadServerAuthConfig()).resolves.toEqual(config)
    expect(fetchMock).toHaveBeenCalledWith('/auth/config', undefined)
  })

  it.each([
    [{ action: 'login', username: 'alice', password: ' p@ssword ' }, '/api/v1/auth/login'],
    [{ action: 'setup', setup_token: 'private', username: 'owner', email: 'owner@example.test', password: 'p@ssword' }, '/api/v1/auth/setup'],
    [{ action: 'accept', token: 'private', username: 'invitee', email: 'invitee@example.test', password: 'p@ssword' }, '/api/v1/auth/invitations/accept'],
  ] as const)('posts account input without trimming the password or putting secrets in the URL', async (input, path) => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ access_token: 'native' })))
    vi.stubGlobal('fetch', fetchMock)
    await signInNative(input)
    const { action: _, ...body } = input
    expect(fetchMock).toHaveBeenCalledWith(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
  })

  it('retains only an unexpired token in tab storage', () => {
    storeNativeSession({ email: 'owner@example.test', access_token: 'native', expires_at_ms: Date.now() + 1_000, user: { user_id: 'owner', username: 'owner' }, is_instance_owner: true, platform_role: 'owner', personal_tenant_id: 'space', personal_project_id: 'project', instance: { managed_execution_enabled: false, mode: 'single_user', owner_user_id: 'owner', revision: 1 } })
    expect(readNativeToken()).toBe('native')
    expect(JSON.parse(sessionStorage.getItem(NATIVE_SESSION_KEY)!)).not.toHaveProperty('user')
    expect(JSON.parse(sessionStorage.getItem(NATIVE_SESSION_KEY)!)).not.toHaveProperty('email')
    clearNativeSession()
    expect(readNativeToken()).toBe('')
    sessionStorage.setItem(NATIVE_SESSION_KEY, JSON.stringify({ access_token: 'expired', expires_at_ms: 1 }))
    expect(readNativeToken()).toBe('')
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toBeNull()
  })

  it('uses a fragment for account links and clears it without dropping unrelated query parameters', () => {
    history.replaceState({}, '', '/?language=en#setup_token=secret%2Bvalue')
    expect(readAccountLink()).toEqual({ setupToken: 'secret+value', invitationToken: '', teamInvitationToken: '' })
    clearAccountLink()
    expect(location.hash).toBe('')
    expect(location.search).toBe('?language=en')
    const url = new URL(invitationUrl('invite+value'))
    expect(url.search).toBe('')
    expect(new URLSearchParams(url.hash.slice(1)).get('invite')).toBe('invite+value')
    history.replaceState({}, '', invitationUrl('team+value', true))
    expect(readAccountLink()).toEqual({ setupToken: '', invitationToken: '', teamInvitationToken: 'team+value' })
    clearAccountLink()
    expect(location.hash).toBe('')
  })
})
