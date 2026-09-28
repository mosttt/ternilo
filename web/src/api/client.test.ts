import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiClient, ApiError } from './client'

function jsonResponse(status: number, body: unknown) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

afterEach(() => vi.unstubAllGlobals())

describe('ApiClient token refresh', () => {
  it('shares one rotating refresh across concurrent 401 responses and retries every request', async () => {
    const client = new ApiClient('expired')
    let release!: (token: string) => void
    const refresh = vi.fn(() => new Promise<string>(resolve => { release = resolve }))
    client.setRefreshHandler(refresh)
    let releaseSecondUnauthorized!: (response: Response) => void
    const secondUnauthorized = new Promise<Response>(resolve => { releaseSecondUnauthorized = resolve })
    const fetchMock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
      const authorization = new Headers(init?.headers).get('authorization')
      if (authorization !== 'Bearer fresh') {
        if (String(input).endsWith('/second')) return secondUnauthorized
        return jsonResponse(401, { error: { message: 'expired', code: 'expired' } })
      }
      return jsonResponse(200, { path: String(input) })
    })
    vi.stubGlobal('fetch', fetchMock)

    const first = client.request<{ path: string }>('/first')
    const second = client.request<{ path: string }>('/second')
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledOnce())
    release('fresh')
    await expect(first).resolves.toEqual({ path: '/api/v1/first' })
    releaseSecondUnauthorized(jsonResponse(401, { error: { message: 'expired', code: 'expired' } }))

    await expect(second).resolves.toEqual({ path: '/api/v1/second' })
    expect(refresh).toHaveBeenCalledOnce()
    expect(fetchMock).toHaveBeenCalledTimes(4)
    expect(fetchMock.mock.calls.slice(2).map(([, init]) => new Headers(init?.headers).get('authorization')))
      .toEqual(['Bearer fresh', 'Bearer fresh'])
  })

  it('clears a failed shared refresh and permits a later refresh attempt', async () => {
    const client = new ApiClient('expired')
    const refresh = vi.fn<() => Promise<string>>()
      .mockRejectedValueOnce(new Error('refresh rejected'))
      .mockResolvedValueOnce('recovered')
    client.setRefreshHandler(refresh)
    vi.stubGlobal('fetch', vi.fn(async (_input: string | URL | Request, init?: RequestInit) => {
      const authorization = new Headers(init?.headers).get('authorization')
      return authorization === 'Bearer recovered'
        ? jsonResponse(200, { ok: true })
        : jsonResponse(401, { error: { message: 'expired', code: 'expired' } })
    }))

    const failed = await Promise.allSettled([client.request('/one'), client.request('/two')])
    expect(failed.every(result => result.status === 'rejected' && result.reason instanceof ApiError)).toBe(true)
    expect(refresh).toHaveBeenCalledOnce()
    expect(client.hasToken()).toBe(false)

    await expect(client.request('/three')).resolves.toEqual({ ok: true })
    expect(refresh).toHaveBeenCalledTimes(2)
    expect(client.hasToken()).toBe(true)
  })
})

describe('ApiClient empty responses', () => {
  it('consumes a 204 response before resolving the request', async () => {
    const response = new Response(null, { status: 204 })
    const consume = vi.spyOn(response, 'arrayBuffer')
    vi.stubGlobal('fetch', vi.fn(async () => response))

    await expect(new ApiClient().request('/empty')).resolves.toBeUndefined()
    expect(consume).toHaveBeenCalledOnce()
  })
})

describe('ApiClient live credentials', () => {
  it('increments the credential generation only when token or tenant changes', () => {
    const client = new ApiClient('token-a')
    const listener = vi.fn()
    const dispose = client.onCredentialsChanged(listener)

    expect(client.liveCredentials()).toEqual({
      bearerToken: 'token-a',
      tenantId: undefined,
      revision: 0,
    })
    client.setToken('token-a')
    expect(listener).not.toHaveBeenCalled()

    client.setTenant('tenant-a')
    client.setToken('token-b')
    client.clearTenant()
    expect(listener).toHaveBeenCalledTimes(3)
    expect(client.liveCredentials()).toEqual({
      bearerToken: 'token-b',
      tenantId: undefined,
      revision: 3,
    })

    dispose()
    client.clearToken()
    expect(listener).toHaveBeenCalledTimes(3)
  })
})

describe('ApiClient account and space isolation', () => {
  it.each([200, 204, 401])('rejects a late %i without clearing or retrying the new account', async status => {
    const client = new ApiClient('account-a')
    const unauthorized = vi.fn()
    const refresh = vi.fn(async () => 'refreshed-a')
    client.onUnauthorized(unauthorized)
    client.setRefreshHandler(refresh)
    let resolve!: (response: Response) => void
    const fetchMock = vi.fn(() => new Promise<Response>(done => { resolve = done }))
    vi.stubGlobal('fetch', fetchMock)
    const request = client.request('/settings', { method: 'PUT', body: { model: 'a' } })
    client.setToken('account-b')
    resolve(status === 204 ? new Response(null, { status }) : jsonResponse(status, {}))
    await expect(request).rejects.toMatchObject({ code: 'request_scope_changed' })
    expect(client.liveCredentials().bearerToken).toBe('account-b')
    expect(fetchMock).toHaveBeenCalledOnce()
    expect(refresh).not.toHaveBeenCalled()
    expect(unauthorized).not.toHaveBeenCalled()
  })

  it('does not replay the old space write while renewing the same account for the new space', async () => {
    const client = new ApiClient('account-a')
    client.setTenant('space-a')
    let release!: (token: string) => void
    const refresh = vi.fn(() => new Promise<string>(resolve => { release = resolve }))
    client.setRefreshHandler(refresh)
    const fetchMock = vi.fn(async (_path, options) => new Headers(options.headers).get('authorization') === 'Bearer refreshed-a'
      ? jsonResponse(200, { ok: true }) : jsonResponse(401, {}))
    vi.stubGlobal('fetch', fetchMock)
    const request = client.request('/settings', { method: 'PUT', body: { model: 'a' } })
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledOnce())
    client.setTenant('space-b')
    const newSpaceRequest = client.request('/state')
    await Promise.resolve()
    release('refreshed-a')
    await expect(request).rejects.toMatchObject({ code: 'request_scope_changed' })
    await expect(newSpaceRequest).resolves.toEqual({ ok: true })
    expect(refresh).toHaveBeenCalledOnce()
    expect(fetchMock).toHaveBeenCalledTimes(3)
    expect(fetchMock.mock.calls.filter(([, options]) => options.method === 'PUT')).toHaveLength(1)
    expect(client.liveCredentials()).toMatchObject({ bearerToken: 'refreshed-a', tenantId: 'space-b' })
  })

  it('does not install an old rotating token after signing in as another account', async () => {
    const client = new ApiClient('account-a')
    let release!: (token: string) => void
    const refresh = vi.fn(() => new Promise<string>(resolve => { release = resolve }))
    client.setRefreshHandler(refresh)
    vi.stubGlobal('fetch', vi.fn(async () => jsonResponse(401, {})))
    const request = client.request('/state')
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledOnce())
    client.setToken('account-b')
    release('refreshed-a')
    await expect(request).rejects.toMatchObject({ code: 'request_scope_changed' })
    expect(client.liveCredentials().bearerToken).toBe('account-b')
  })
})

describe('Platform API request scope', () => {
  it.each(['/admin/accounts?limit=25', '/model-access/catalog?limit=25', '/model-access/keys?limit=25', '/model-access/keys'])('keeps account-wide endpoint %s independent of the active workbench space', async path => {
    const client = new ApiClient('account-a')
    client.setTenant('space-a')
    let release!: (response: Response) => void
    const fetchMock = vi.fn((_input: string, _options?: RequestInit) => new Promise<Response>(resolve => { release = resolve }))
    vi.stubGlobal('fetch', fetchMock)
    const request = client.request(path, path === '/model-access/keys' ? { method: 'POST', body: { name: 'Laptop', grant_id: 'grant', model_ids: ['model'] } } : undefined)
    client.setTenant('space-b')
    release(jsonResponse(200, { accounts: [], next_cursor: null }))
    await expect(request).resolves.toEqual({ accounts: [], next_cursor: null })
    expect(new Headers(fetchMock.mock.calls[0]?.[1]?.headers).get('x-ternilo-tenant')).toBeNull()
  })

  it.each(['/admin/accounts?limit=25', '/model-access/catalog?limit=25', '/model-access/keys?limit=25', '/model-access/keys'])('still discards %s responses from a previous account', async path => {
    const client = new ApiClient('account-a')
    let release!: (response: Response) => void
    vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(resolve => { release = resolve })))
    const request = client.request(path, path === '/model-access/keys' ? { method: 'POST', body: { name: 'Laptop', grant_id: 'grant', model_ids: ['model'] } } : undefined)
    client.setToken('account-b')
    release(jsonResponse(200, { accounts: [{ user_id: 'private' }], next_cursor: null }))
    await expect(request).rejects.toMatchObject({ code: 'request_scope_changed' })
  })
})
