import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { CredentialInventory, ProviderProfile } from '@/types'

const request = vi.hoisted(() => vi.fn())
vi.mock('@/api/client', () => ({ api: { request } }))

import {
  invalidateProviderInventory,
  loadProviderInventory,
  peekProviderInventory,
  resetProviderInventoryForTest,
  subscribeProviderInventory,
} from './provider-inventory'

const providers: ProviderProfile[] = []
const credentials: CredentialInventory = { references: [], records: [] }

beforeEach(() => {
  request.mockReset()
  resetProviderInventoryForTest()
  request.mockImplementation(async (path: string) => path === '/providers' ? providers : credentials)
})

describe('provider inventory cache', () => {
  it('isolates Cloud, Session, and Workspace execution targets', async () => {
    const nodeProvider: ProviderProfile = {
      id: 'node', source: 'user', display_name: 'Node', base_url: 'https://node.test/v1',
      protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 128_000, max_output_tokens: 16_000 },
      models: [{ id: 'node-model', settings: { mode: 'inherit' } }],
      timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 100,
    }
    request.mockImplementation(async (path: string) => {
      if (path === '/providers') return providers
      if (path === '/providers?session_id=session-node') return [nodeProvider]
      if (path.startsWith('/providers')) return []
      return credentials
    })

    const cloud = await loadProviderInventory()
    const node = await loadProviderInventory(false, { sessionId: 'session-node' })
    const emptyWorkspace = await loadProviderInventory(false, { workspaceId: 'workspace-node' })

    expect(cloud.providers).toEqual([])
    expect(node.providers).toEqual([nodeProvider])
    expect(emptyWorkspace.providers).toEqual([])
    expect(request.mock.calls.map(([path]) => path)).toEqual([
      '/providers', '/credentials',
      '/providers?session_id=session-node', '/credentials?session_id=session-node',
      '/providers?workspace_id=workspace-node', '/credentials?workspace_id=workspace-node',
    ])

    invalidateProviderInventory({ sessionId: 'session-node' })
    expect(peekProviderInventory()).toEqual(cloud)
    expect(peekProviderInventory({ sessionId: 'session-node' })).toBeNull()
  })

  it('deduplicates concurrent reads and reuses them across Session remounts', async () => {
    const [first, second] = await Promise.all([
      loadProviderInventory(),
      loadProviderInventory(),
    ])

    expect(first).toEqual({ providers, credentials })
    expect(second).toEqual(first)
    expect(request).toHaveBeenCalledTimes(2)

    await loadProviderInventory()
    expect(request).toHaveBeenCalledTimes(2)
  })

  it('refreshes explicitly after Provider or credential settings change', async () => {
    await loadProviderInventory()
    invalidateProviderInventory()
    expect(peekProviderInventory()).toBeNull()

    await loadProviderInventory(true)
    expect(request).toHaveBeenCalledTimes(4)
  })

  it('refreshes session and workspace aliases after editing a computer without invalidating another space', async () => {
    const session = { tenantId: 'team', sessionId: 'session-node' }
    const workspace = { tenantId: 'team', workspaceId: 'workspace-node' }
    const otherSpace = { tenantId: 'other', sessionId: 'other-session' }
    await loadProviderInventory(false, session)
    await loadProviderInventory(false, workspace)
    const unrelated = await loadProviderInventory(false, otherSpace)
    const fresh: ProviderProfile = {
      id: 'fresh', source: 'user', display_name: 'Fresh', base_url: 'https://node.test/v1',
      protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 128_000, max_output_tokens: 16_000 },
      models: [{ id: 'node-model', settings: { mode: 'inherit' } }],
      timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 100,
    }
    request.mockImplementation(async (path: string) => path.startsWith('/providers') ? [fresh] : credentials)
    invalidateProviderInventory({ tenantId: 'team', executorId: 'computer-node' })
    expect(peekProviderInventory(session)).toBeNull()
    expect(peekProviderInventory(workspace)).toBeNull()
    expect(peekProviderInventory(otherSpace)).toBe(unrelated)
    expect((await loadProviderInventory(false, session)).providers).toEqual([fresh])
    expect((await loadProviderInventory(false, workspace)).providers).toEqual([fresh])
  })

  it('publishes invalidation and the refreshed inventory to mounted consumers', async () => {
    const observed: Array<ProviderProfile[] | null> = []
    const unsubscribe = subscribeProviderInventory(snapshot => observed.push(snapshot?.providers ?? null))

    await loadProviderInventory()
    invalidateProviderInventory()
    await loadProviderInventory(true)

    expect(observed).toEqual([providers, null, providers])
    unsubscribe()
  })

  it('does not let an invalidated in-flight read restore stale data', async () => {
    let resolveOldProviders!: (value: ProviderProfile[]) => void
    const oldProvider: ProviderProfile = {
      id: 'old', source: 'user', display_name: 'Old', base_url: 'https://old.test/v1',
      protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 128_000, max_output_tokens: 16_000 },
      models: [{ id: 'old-model', settings: { mode: 'inherit' } }],
      timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 100,
    }
    const freshProvider = { ...oldProvider, id: 'fresh', display_name: 'Fresh' }
    request
      .mockReturnValueOnce(new Promise(resolve => { resolveOldProviders = resolve }))
      .mockResolvedValueOnce(credentials)

    const staleRead = loadProviderInventory()
    invalidateProviderInventory()
    request
      .mockResolvedValueOnce([freshProvider])
      .mockResolvedValueOnce(credentials)
    const freshRead = loadProviderInventory(true)
    resolveOldProviders([oldProvider])

    await expect(freshRead).resolves.toEqual({ providers: [freshProvider], credentials })
    await expect(staleRead).resolves.toEqual({ providers: [freshProvider], credentials })
    expect(peekProviderInventory()?.providers).toEqual([freshProvider])
  })
  it('keeps personal account credentials separate from a shared computer with the same Provider ID', async () => {
    const provider: ProviderProfile = {
      id: 'same', source: 'user', display_name: 'Same', base_url: 'https://personal.test/v1',
      protocol: 'openai-responses', api_key_ref: 'SAME_KEY',
      defaults: { context_window: 4096, max_output_tokens: 1024 },
      models: [{ id: 'same-model', settings: { mode: 'inherit' } }],
      timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 100,
    }
    request.mockImplementation((path: string, options?: { headers?: Record<string, string> }) => {
      const personal = options?.headers?.['x-ternilo-tenant'] === 'personal-space'
      if (path.startsWith('/credentials')) return Promise.resolve({ references: [{ reference: 'SAME_KEY', configured: personal, writable: personal }], records: [] })
      return Promise.resolve([{ ...provider, base_url: personal ? provider.base_url : '' }])
    })
    const account = await loadProviderInventory(false, { tenantId: 'personal-space' })
    const computer = await loadProviderInventory(false, { sessionId: 'shared-session' })
    expect(account.providers[0].base_url).toBe('https://personal.test/v1')
    expect(computer.providers[0].base_url).toBe('')
    expect(account.credentials.references[0].configured).toBe(true)
    expect(computer.credentials.references[0].configured).toBe(false)
    invalidateProviderInventory({ tenantId: 'personal-space' })
    expect(peekProviderInventory({ sessionId: 'shared-session' })).toBe(computer)
  })

})
