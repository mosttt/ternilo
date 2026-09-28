import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiClient } from '@/api/client'
import { refreshLocalToken, usesLocalBootstrap } from './local'

const html = (boot: unknown) => `<script>window.__TERNILO_BOOT__ = ${JSON.stringify(boot)};</script><script>throw new Error('must not execute')</script>`

afterEach(() => { vi.unstubAllGlobals(); window.__TERNILO_BOOT__ = undefined })

describe('local bootstrap recovery', () => {
  it('reads fresh same-origin bootstrap data without evaluating its scripts', async () => {
    const fetchMock = vi.fn(async () => new Response(html({ remote: false, apiToken: 'rotated-local-token' })))
    vi.stubGlobal('fetch', fetchMock)
    expect(await refreshLocalToken()).toBe('rotated-local-token')
    expect(fetchMock).toHaveBeenCalledWith('/', expect.objectContaining({ cache: 'no-store', credentials: 'same-origin', redirect: 'error' }))
  })

  it.each([
    { remote: true, apiToken: 'remote-token' },
    { remote: false, platform: true, apiToken: 'account-token' },
    { remote: false, offline: true, apiToken: 'old-token' },
    { remote: false, apiToken: '' },
  ])('rejects a bootstrap from a different authentication context: %j', async boot => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(html(boot))))
    await expect(refreshLocalToken()).rejects.toThrow('not a local Ternilo bootstrap')
  })

  it('only enables recovery for a token-bearing local shell', () => {
    window.__TERNILO_BOOT__ = { remote: false, apiToken: 'local-token' }
    expect(usesLocalBootstrap()).toBe(true)
    window.__TERNILO_BOOT__ = { remote: true, apiToken: 'remote-token' }
    expect(usesLocalBootstrap()).toBe(false)
    window.__TERNILO_BOOT__ = { platform: true, apiToken: 'account-token' }
    expect(usesLocalBootstrap()).toBe(false)
    window.__TERNILO_BOOT__ = { offline: true }
    expect(usesLocalBootstrap()).toBe(false)
  })

  it('shares recovery for concurrent expired requests without signing out or storing credentials', async () => {
    const client = new ApiClient('expired-local-token')
    client.setRefreshHandler(refreshLocalToken)
    const signedOut = vi.fn()
    client.onUnauthorized(signedOut)
    let bootstrapReads = 0
    vi.stubGlobal('fetch', vi.fn(async (url: string, options?: RequestInit) => {
      if (url === '/') { bootstrapReads += 1; return new Response(html({ remote: false, apiToken: 'new-local-token' })) }
      const token = (options?.headers as Record<string, string>).authorization
      return new Response(JSON.stringify(token === 'Bearer new-local-token' ? { url } : { error: { message: 'expired' } }), { status: token === 'Bearer new-local-token' ? 200 : 401 })
    }))
    expect(await Promise.all([client.request('/state'), client.request('/catalog')])).toEqual([{ url: '/api/v1/state' }, { url: '/api/v1/catalog' }])
    expect(bootstrapReads).toBe(1)
    expect(signedOut).not.toHaveBeenCalled()
    expect(client.liveCredentials().bearerToken).toBe('new-local-token')
  })
})
