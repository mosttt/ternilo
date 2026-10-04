import { readNativeToken } from './server'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { beginOidcLogin, clearOidcSession, completeOidcLink, initializeOidcSession, refreshOidcSession } from './oidc'

beforeEach(() => {
  const values = new Map<string, string>()
  vi.stubGlobal('sessionStorage', {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  })
})
afterEach(() => vi.unstubAllGlobals())

describe('OIDC session lifecycle', () => {
  it('keeps token refresh on the existing root endpoint', async () => {
    sessionStorage.setItem('ternilo.oidc.provider', 'organization')
    sessionStorage.setItem('ternilo.oidc.refresh', 'refresh-token')
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ access_token: 'new-token', expires_in: 3600 })))
    vi.stubGlobal('fetch', fetchMock)
    await expect(refreshOidcSession()).resolves.toBe('new-token')
    expect(fetchMock).toHaveBeenCalledWith('/auth/refresh', expect.objectContaining({ method: 'POST', body: JSON.stringify({ provider_id: 'organization', refresh_token: 'refresh-token' }) }))
    await expect(initializeOidcSession()).resolves.toBe('new-token')
    expect(fetchMock).toHaveBeenCalledOnce()
  })

  it('does not restore old OIDC credentials after switching to a native account', async () => {
    sessionStorage.setItem('ternilo.oidc.provider', 'organization')
    sessionStorage.setItem('ternilo.oidc.refresh', 'old-account-refresh')
    let release!: (response: Response) => void
    vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(resolve => { release = resolve })))
    const request = refreshOidcSession()
    clearOidcSession()
    release(new Response(JSON.stringify({ access_token: 'old-account-token', refresh_token: 'old-next-refresh', provider_id: 'organization', expires_in: 3600 })))
    await expect(request).rejects.toMatchObject({ failure: 'expired' })
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBeNull()
    expect(sessionStorage.getItem('ternilo.oidc.refresh')).toBeNull()
  })
})

describe('Explicit native account linking', () => {
  function callback(native = 'native-original') {
    sessionStorage.setItem('ternilo.native.session', JSON.stringify({ access_token: native, expires_at_ms: Date.now() + 60_000 }))
    sessionStorage.setItem('ternilo.oidc.link-native', 'native-original')
    sessionStorage.setItem('ternilo.oidc.state', 'expected')
    sessionStorage.setItem('ternilo.oidc.verifier', 'verifier')
    sessionStorage.setItem('ternilo.oidc.attempt-provider', 'organization')
    sessionStorage.setItem('ternilo.oidc.nonce', 'expected-nonce-for-login')
    vi.stubGlobal('location', { pathname: '/auth/callback', search: '?code=code&state=expected' })
    vi.stubGlobal('history', { replaceState: vi.fn() })
  }

  it('binds with the original native token and never provisions or stores a separate OIDC login', async () => {
    callback()
    const nativeSession = sessionStorage.getItem('ternilo.native.session')
    const fetchMock = vi.fn(async path => new Response(JSON.stringify(path === '/auth/token'
      ? { access_token: 'oidc-token', refresh_token: 'oidc-refresh', provider_id: 'organization', expires_in: 3600 }
      : { native: true, oidc: { issuer: 'https://issuer.test', subject: 'subject' } })))
    vi.stubGlobal('fetch', fetchMock)
    await expect(completeOidcLink()).resolves.toBe(true)
    expect(fetchMock.mock.calls.map(([path]) => path)).toEqual(['/auth/token', '/api/v1/auth/oidc-link'])
    expect(fetchMock).toHaveBeenLastCalledWith('/api/v1/auth/oidc-link', expect.objectContaining({
      headers: { 'content-type': 'application/json', authorization: 'Bearer native-original' },
      body: JSON.stringify({ access_token: 'oidc-token' }),
    }))
    expect(sessionStorage.getItem('ternilo.native.session')).toBe(nativeSession)
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBeNull()
    expect(sessionStorage.getItem('ternilo.oidc.refresh')).toBeNull()
    expect(sessionStorage.getItem('ternilo.oidc.link-native')).toBeNull()
    expect(history.replaceState).toHaveBeenLastCalledWith({}, '', '/')
  })

  it.each(['before', 'during'] as const)('does not bind a changed account %s token exchange', async timing => {
    callback(timing === 'before' ? 'native-new' : undefined)
    const fetchMock = vi.fn(async () => {
      sessionStorage.setItem('ternilo.native.session', JSON.stringify({ access_token: 'native-new', expires_at_ms: Date.now() + 60_000 }))
      return new Response(JSON.stringify({ access_token: 'oidc-token', expires_in: 3600 }))
    })
    vi.stubGlobal('fetch', fetchMock)
    await expect(completeOidcLink()).rejects.toMatchObject({ failure: 'expired' })
    expect(fetchMock).toHaveBeenCalledTimes(timing === 'before' ? 0 : 1)
    expect(readNativeToken()).toBe('native-new')
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBeNull()
    expect(sessionStorage.getItem('ternilo.oidc.link-native')).toBeNull()
  })

  it('keeps the native login when the OIDC account is already linked elsewhere', async () => {
    callback()
    vi.stubGlobal('fetch', vi.fn(async path => path === '/auth/token'
      ? new Response(JSON.stringify({ access_token: 'oidc-token', expires_in: 3600 }))
      : new Response(JSON.stringify({ error: { message: 'already linked' } }), { status: 409 })))
    await expect(completeOidcLink()).rejects.toThrow('already linked')
    expect(readNativeToken()).toBe('native-original')
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBeNull()
    expect(sessionStorage.getItem('ternilo.oidc.link-native')).toBeNull()
  })

  it('rejects callback state mismatch before exchanging a linking token', async () => {
    callback()
    vi.stubGlobal('location', { pathname: '/auth/callback', search: '?code=code&state=wrong' })
    const fetchMock = vi.fn()
    vi.stubGlobal('fetch', fetchMock)
    await expect(completeOidcLink()).rejects.toMatchObject({ failure: 'state_mismatch' })
    expect(fetchMock).not.toHaveBeenCalled()
    expect(readNativeToken()).toBe('native-original')
  })
})

describe('Management page OIDC return paths', () => {
  it.each(['/admin/accounts', '/admin/models', '/models', '/spaces/current', '/files'])('restores the authorized application path %s after login', async path => {
    sessionStorage.setItem('ternilo.oidc.state', 'expected')
    sessionStorage.setItem('ternilo.oidc.verifier', 'verifier')
    sessionStorage.setItem('ternilo.oidc.attempt-provider', 'organization')
    sessionStorage.setItem('ternilo.oidc.nonce', 'expected-nonce-for-login')
    sessionStorage.setItem('ternilo.oidc.return-path', path)
    vi.stubGlobal('location', { pathname: '/auth/callback', search: '?code=code&state=expected' })
    vi.stubGlobal('history', { replaceState: vi.fn() })
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ access_token: 'access', expires_in: 3600 }))))
    await expect(initializeOidcSession()).resolves.toBe('access')
    expect(history.replaceState).toHaveBeenCalledWith({}, '', path)
    expect(sessionStorage.getItem('ternilo.oidc.return-path')).toBeNull()
  })

  it.each(['https://evil.test/', '//evil.test/', '/admin/accounts?redirect=https://evil.test', 'https://evil.test/files?session_id=private', '//evil.test/files?session_id=private', '/\\evil.test/files'])('discards an unsupported return path %s', async path => {
    sessionStorage.setItem('ternilo.oidc.state', 'expected')
    sessionStorage.setItem('ternilo.oidc.verifier', 'verifier')
    sessionStorage.setItem('ternilo.oidc.attempt-provider', 'organization')
    sessionStorage.setItem('ternilo.oidc.nonce', 'expected-nonce-for-login')
    sessionStorage.setItem('ternilo.oidc.return-path', path)
    vi.stubGlobal('location', { pathname: '/auth/callback', search: '?code=code&state=expected' })
    vi.stubGlobal('history', { replaceState: vi.fn() })
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ access_token: 'access', expires_in: 3600 }))))
    await initializeOidcSession()
    expect(history.replaceState).toHaveBeenCalledWith({}, '', '/')
  })

  it('retains only Files filters across login and discards stale cursors, unknown query fields and fragments', async () => {
    vi.stubGlobal('location', { origin: 'https://app.test', pathname: '/files', search: '?session_id=session%2Fa&query=release+notes&cursor=old&redirect=https://evil.test', assign: vi.fn() })
    vi.stubGlobal('crypto', { getRandomValues: (value: Uint8Array) => value.fill(7), subtle: { digest: async () => new ArrayBuffer(32) } })
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({
      initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: true,
      oidc_providers: [{ id: 'organization', name: 'Organization', config: { authorization_endpoint: 'https://identity.test/authorize', client_id: 'client', redirect_uri: 'https://app.test/auth/callback', scope: 'openid' } }],
    }))))
    await beginOidcLogin('organization')
    expect(sessionStorage.getItem('ternilo.oidc.return-path')).toBe('/files?session_id=session%2Fa&query=release+notes')

    sessionStorage.setItem('ternilo.oidc.state', 'expected')
    sessionStorage.setItem('ternilo.oidc.return-path', '/files?workspace_id=workspace-a&session_id=session%2Fa&kind=generated&query=release+notes&cursor=old&next=//evil.test/#fragment')
    vi.stubGlobal('location', { pathname: '/auth/callback', search: '?code=code&state=expected' })
    vi.stubGlobal('history', { replaceState: vi.fn() })
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ access_token: 'access', expires_in: 3600 }))))
    await initializeOidcSession()
    expect(history.replaceState).toHaveBeenCalledWith({}, '', '/files?workspace_id=workspace-a&session_id=session%2Fa&kind=generated&query=release+notes')
  })
})

describe('OIDC browser prerequisites', () => {
  function configure(redirect = 'https://app.test/auth/callback') {
    vi.stubGlobal('location', { origin: 'https://app.test', pathname: '/', search: '', assign: vi.fn() })
    vi.stubGlobal('crypto', { getRandomValues: (value: Uint8Array) => value.fill(7), subtle: { digest: async () => new ArrayBuffer(32) } })
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      oidc_enabled: true,
      oidc_providers: [{ id: 'organization', name: 'Organization', config: { authorization_endpoint: 'https://identity.test/authorize', client_id: 'client', redirect_uri: redirect, scope: 'openid' } }],
    })))
    vi.stubGlobal('fetch', fetchMock)
    sessionStorage.setItem('ternilo.oidc.access', 'existing-session')
    return fetchMock
  }

  it('reports unavailable browser cryptography before requests or session changes', async () => {
    const fetchMock = configure()
    vi.stubGlobal('crypto', { getRandomValues: vi.fn() })
    await expect(beginOidcLogin('organization')).rejects.toMatchObject({ failure: 'secure_context_required' })
    expect(fetchMock).not.toHaveBeenCalled()
    expect(location.assign).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBe('existing-session')
    expect(sessionStorage.getItem('ternilo.oidc.verifier')).toBeNull()
  })

  it.each(['http://app.test/auth/callback', 'https://other.test/auth/callback', 'https://app.test:4321/auth/callback'])('rejects a different callback origin: %s', async callback => {
    configure(callback)
    await expect(beginOidcLogin('organization')).rejects.toMatchObject({ failure: 'origin_mismatch' })
    expect(location.assign).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ternilo.oidc.access')).toBe('existing-session')
    expect(sessionStorage.getItem('ternilo.oidc.verifier')).toBeNull()
  })

  it('does not start a new authorization after logout during PKCE calculation', async () => {
    configure()
    let finish!: (digest: ArrayBuffer) => void
    const digest = vi.fn(() => new Promise<ArrayBuffer>(resolve => { finish = resolve }))
    vi.stubGlobal('crypto', { getRandomValues: (value: Uint8Array) => value.fill(7), subtle: { digest } })
    const login = beginOidcLogin('organization')
    await vi.waitFor(() => expect(digest).toHaveBeenCalledOnce())
    clearOidcSession()
    finish(new ArrayBuffer(32))
    await expect(login).rejects.toMatchObject({ failure: 'expired' })
    expect(location.assign).not.toHaveBeenCalled()
    expect(sessionStorage.getItem('ternilo.oidc.verifier')).toBeNull()
  })
})
