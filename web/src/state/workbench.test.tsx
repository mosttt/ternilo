import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, ApiError } from '@/api/client'
import { REMOTE_TOKEN_STORAGE_KEY } from '@/auth/remote-token'
import { NATIVE_SESSION_KEY } from '@/auth/server'
import { defaultPermissionStorageKey } from '@/domain/default-permission'
import type {
  AgentPresetRoster, ApplicationCatalog, ApplicationState, LocalSession, TenantSummary, Workspace,
} from '@/types'
import { chooseSelection, useWorkbench, WorkbenchProvider } from './workbench'

const auth = vi.hoisted(() => ({
  begin: vi.fn(async () => undefined),
  clear: vi.fn(),
  initialize: vi.fn(async () => 'oidc-token'),
  refresh: vi.fn(async () => 'refreshed-token'),
}))

vi.mock('@/auth/oidc', async importOriginal => {
  const actual = await importOriginal<typeof import('@/auth/oidc')>()
  return {
    ...actual,
    beginOidcLogin: auth.begin,
    clearOidcSession: auth.clear,
    initializeOidcSession: auth.initialize,
    refreshOidcSession: auth.refresh,
  }
})

interface Deferred<Value> {
  promise: Promise<Value>
  resolve(value: Value): void
}

function deferred<Value>(): Deferred<Value> {
  let resolve!: (value: Value) => void
  const promise = new Promise<Value>(onResolve => { resolve = onResolve })
  return { promise, resolve }
}

const catalog = (revision: string): ApplicationCatalog => ({ revision, plugin_kinds: [], plugins: [] })
const presets = (defaultId: string): AgentPresetRoster => ({ presets: [], default_id: defaultId, authorable: false })
const workspace = (id: string, updatedAt = 1): Workspace => ({
  workspace_id: id,
  path: `/${id}`,
  title: id,
  created_at_ms: 1,
  updated_at_ms: updatedAt,
})
const session = (id: string, workspaceId: string, updatedAt: number, title = id): LocalSession => ({
  identity: { tenant_id: 'tenant', user_id: 'user', agent_id: 'agent', session_id: id },
  workspace_id: workspaceId,
  workspace_path: `/${workspaceId}`,
  title,
  permissions: 'workspace_write',
  model: { provider: 'profile_default' },
  agent_preset: 'standard',
  preset_plugins: [],
  profile_plugins: [],
  mode: 'execute',
  created_at_ms: 1,
  updated_at_ms: updatedAt,
})
const tenant = (id: string): TenantSummary => ({ kind: 'team', tenant_id: id, slug: id, display_name: id, role: 'owner' })
const route = (path: string) => path.split('?', 1)[0]

let root: Root | undefined
let host: HTMLDivElement | undefined
let current!: ReturnType<typeof useWorkbench>

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

function Probe() {
  current = useWorkbench()
  return null
}

async function mount() {
  await act(async () => {
    root!.render(<WorkbenchProvider><Probe /></WorkbenchProvider>)
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

const serverIdentity = {
  user: { user_id: 'native-owner', username: 'owner' },
  is_instance_owner: true, platform_role: 'owner', personal_tenant_id: 'space', personal_project_id: 'project',
  instance: { managed_execution_enabled: false, mode: 'single_user', owner_user_id: 'native-owner', revision: 1 },
}

function nativeServerRoutes() {
  return vi.spyOn(api, 'request').mockImplementation(async (path: string) => {
    if (path === '/auth/session') return serverIdentity as never
    if (path === '/auth/logout') return undefined as never
    if (path === '/tenants') return { tenants: [tenant('space')] } as never
    if (route(path) === '/state') return { workspaces: [workspace('native-workspace')], sessions: [] } as never
    if (route(path) === '/catalog') return catalog('native') as never
    if (route(path) === '/agent-presets') return presets('native') as never
    throw new Error(`unexpected native request ${path}`)
  })
}

describe('WorkbenchProvider Server accounts', () => {
  it.each(['active', 'pending'] as const)('completes OIDC username registration with %s status without creating a native session', async status => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    let registered = false
    const fetchMock = vi.fn(async (path: string) => {
      if (path === '/auth/config') return new Response(JSON.stringify({ initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: true, registration: { mode: 'open', require_approval: status === 'pending', revision: 2 } }))
      if (path === '/api/v1/auth/oidc/register') {
        registered = true
        return new Response(JSON.stringify({ status, user_id: 'oidc-user' }), { status: 201 })
      }
      throw new Error(`Unexpected path ${path}`)
    })
    vi.stubGlobal('fetch', fetchMock)
    const request = nativeServerRoutes()
    request.mockImplementation(async (path: string) => {
      if (path === '/auth/session') {
        if (!registered) throw new ApiError('choose a platform username to finish registration', 403, 'policy_denied')
        return { ...serverIdentity, user: { user_id: 'oidc-user', username: 'chosen-user' } } as never
      }
      if (path === '/tenants') return { tenants: [tenant('space')] } as never
      if (route(path) === '/state') return { workspaces: [], sessions: [] } as never
      if (route(path) === '/catalog') return catalog('oidc') as never
      if (route(path) === '/agent-presets') return presets('standard') as never
      throw new Error(`Unexpected path ${path}`)
    })
    await mount()
    expect(current.oidcRegistrationRequired).toBe(true)
    expect(current.authRequired).toBe(true)
    expect(current.serverIdentity).toBeNull()
    expect(api.hasToken()).toBe(false)
    expect(auth.clear).not.toHaveBeenCalled()
    request.mockClear()
    let result: unknown
    await act(async () => { result = await current.registerOidcUsername(' chosen-user ', 'chosen@example.test') })
    expect(result).toBe(status)
    expect(fetchMock).toHaveBeenCalledWith('/api/v1/auth/oidc/register', {
      method: 'POST', headers: { 'content-type': 'application/json', authorization: 'Bearer oidc-token' }, body: JSON.stringify({ username: 'chosen-user', email: 'chosen@example.test' }),
    })
    expect(current.oidcRegistrationRequired).toBe(false)
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toBeNull()
    if (status === 'active') {
      expect(current.authRequired).toBe(false)
      expect(current.serverIdentity?.user).toEqual({ user_id: 'oidc-user', username: 'chosen-user' })
      expect(api.liveCredentials().bearerToken).toBe('oidc-token')
      expect(request).toHaveBeenCalledWith('/auth/session')
      expect(auth.clear).not.toHaveBeenCalled()
    } else {
      expect(current.authRequired).toBe(true)
      expect(current.serverIdentity).toBeNull()
      expect(api.hasToken()).toBe(false)
      expect(auth.clear).toHaveBeenCalledOnce()
      expect(request).not.toHaveBeenCalled()
    }
  })

  it('retains the verified OIDC registration flow after a username conflict', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    vi.stubGlobal('fetch', vi.fn(async (path: string) => new Response(JSON.stringify(path === '/auth/config'
      ? { initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: true, registration: { mode: 'open', require_approval: false, revision: 2 } }
      : { error: { code: 'conflict', message: 'username is already registered' } }), { status: path === '/auth/config' ? 200 : 409 })))
    vi.spyOn(api, 'request').mockRejectedValue(new ApiError('choose a platform username to finish registration', 403, 'policy_denied'))
    await mount()
    await act(async () => { await expect(current.registerOidcUsername('taken', 'taken@example.test')).rejects.toMatchObject({ status: 409 }) })
    expect(current.oidcRegistrationRequired).toBe(true)
    expect(current.authRequired).toBe(true)
    expect(api.hasToken()).toBe(false)
    expect(auth.clear).not.toHaveBeenCalled()
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toBeNull()
  })

  it('keeps pending registration unauthenticated and accepts an active registration session directly', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    let approved = false
    const fetchMock = vi.fn(async (path: string) => new Response(JSON.stringify(path === '/auth/config'
      ? { initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: false, registration: { mode: 'open', require_approval: true, revision: 2 } }
      : { status: approved ? 'active' : 'pending', user_id: 'native-owner', session: approved ? { ...serverIdentity, access_token: 'native-token', expires_at_ms: Date.now() + 60_000 } : null })))
    vi.stubGlobal('fetch', fetchMock)
    nativeServerRoutes()
    await mount()
    let status: unknown
    await act(async () => { status = await current.authenticate({ action: 'register', username: 'candidate', email: 'candidate@example.test', password: 'password' }) })
    expect(status).toBe('pending')
    expect(current.authRequired).toBe(true)
    expect(current.serverIdentity).toBeNull()
    expect(api.hasToken()).toBe(false)
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toBeNull()
    approved = true
    await act(async () => { status = await current.authenticate({ action: 'register', username: 'other', email: 'other@example.test', password: 'password' }) })
    expect(status).toBe('active')
    expect(current.authRequired).toBe(false)
    expect(api.liveCredentials().bearerToken).toBe('native-token')
    expect(fetchMock.mock.calls.some(([path]) => path.endsWith('/auth/login'))).toBe(false)
  })

  it('stops a paused account and clears visible data while retaining its credential for a later retry', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    sessionStorage.setItem(NATIVE_SESSION_KEY, JSON.stringify({ access_token: 'native-token', expires_at_ms: Date.now() + 60_000 }))
    const configResponse = () => new Response(JSON.stringify({ initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: false }))
    vi.stubGlobal('fetch', vi.fn(async () => configResponse()))
    const routes = nativeServerRoutes()
    await mount()
    expect(current.snapshot.workspaces).not.toEqual([])
    routes.mockRestore()
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ error: { code: 'policy_denied', message: 'this account is paused while the server is in single-user mode' } }), { status: 403 })))
    await act(async () => { await api.request('/state').catch(() => undefined) })
    expect(current.accessPaused).toBe(true)
    expect(current.authRequired).toBe(true)
    expect(current.snapshot.workspaces).toEqual([])
    expect(api.hasToken()).toBe(false)
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toContain('native-token')
    nativeServerRoutes()
    vi.stubGlobal('fetch', vi.fn(async () => configResponse()))
    await act(async () => { current.retryAuthentication(); await Promise.resolve(); await Promise.resolve() })
    expect(current.accessPaused).toBe(false)
    expect(current.authRequired).toBe(false)
    expect(current.currentWorkspaceId).toBe('native-workspace')
  })
  it('reloads account-owned resources and rejects a late prior-account refresh in the same space', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    vi.stubGlobal('fetch', vi.fn(async (path: string, options?: RequestInit) => new Response(JSON.stringify(path === '/auth/config'
      ? { initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: false }
      : { ...serverIdentity, user: { user_id: JSON.parse(String(options?.body)).username, username: JSON.parse(String(options?.body)).username }, access_token: JSON.parse(String(options?.body)).username, expires_at_ms: Date.now() + 60_000 }))))
    const stale = deferred<ApplicationState>()
    let holdOldState = false
    vi.spyOn(api, 'request').mockImplementation(async (path: string) => {
      if (path === '/tenants') return { tenants: [tenant('space')] } as never
      if (route(path) === '/state') {
        if (holdOldState && api.liveCredentials().bearerToken === 'alice') return stale.promise as never
        return { workspaces: [workspace(`${api.liveCredentials().bearerToken}-workspace`)], sessions: [] } as never
      }
      if (route(path) === '/catalog') return catalog(path) as never
      if (route(path) === '/agent-presets') return presets('standard') as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()
    await act(async () => { await current.authenticate({ action: 'login', username: 'alice', password: 'password' }) })
    expect(current.currentWorkspaceId).toBe('alice-workspace')
    holdOldState = true
    let previous!: Promise<void>
    act(() => { previous = current.refresh() })
    await act(async () => { await current.authenticate({ action: 'login', username: 'bob', password: 'password' }) })
    expect(current.currentWorkspaceId).toBe('bob-workspace')
    expect(current.accountScope).toBe(JSON.stringify(['bob', 'space']))
    await act(async () => { stale.resolve({ workspaces: [workspace('alice-private')], sessions: [] }); await previous })
    expect(current.currentWorkspaceId).toBe('bob-workspace')
    expect(current.snapshot.workspaces.map(item => item.workspace_id)).toEqual(['bob-workspace'])
  })
  it('restores a native account in single-user mode while retaining the Server resource model', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    sessionStorage.setItem(NATIVE_SESSION_KEY, JSON.stringify({ access_token: 'native-token', expires_at_ms: Date.now() + 60_000 }))
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ initialized: true, mode: 'single_user', native_enabled: true, oidc_enabled: false }))))
    nativeServerRoutes()
    await mount()
    await act(async () => { await Promise.resolve(); await Promise.resolve() })
    expect(current.authRequired).toBe(false)
    expect(current.platform).toBe(true)
    expect(current.serverIdentity?.is_instance_owner).toBe(true)
    expect(current.accountScope).toBe(JSON.stringify(['native-owner', 'space']))
    expect(current.currentWorkspaceId).toBe('native-workspace')
    expect(auth.initialize).not.toHaveBeenCalled()
    expect(api.liveCredentials()).toMatchObject({ bearerToken: 'native-token', tenantId: 'space' })
  })

  it('signs out and signs back in natively without a page reload or OIDC redirect', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    let registrationMode = 'invite'
    const fetchMock = vi.fn(async (path: string) => new Response(JSON.stringify(path === '/auth/config'
      ? { initialized: true, mode: 'single_user', native_enabled: true, oidc_enabled: false, registration: { mode: registrationMode, require_approval: false, revision: 1 } }
      : { ...serverIdentity, access_token: 'native-token', expires_at_ms: Date.now() + 60_000 })))
    vi.stubGlobal('fetch', fetchMock)
    const request = nativeServerRoutes()
    await mount()
    expect(current.authRequired).toBe(true)
    await act(async () => { await current.authenticate({ action: 'login', username: 'owner', password: 'password' }) })
    expect(current.authRequired).toBe(false)
    expect(current.serverIdentity?.user.user_id).toBe('native-owner')
    registrationMode = 'open'
    await act(async () => { current.logout(); await Promise.resolve(); await Promise.resolve() })
    expect(current.serverAuthConfig?.registration.mode).toBe('open')
    expect(request).toHaveBeenCalledWith('/auth/logout', { method: 'POST', headers: { authorization: 'Bearer native-token' } })
    expect(current.authRequired).toBe(true)
    expect(current.serverIdentity).toBeNull()
    expect(current.accountScope).toBeUndefined()
    expect(current.snapshot.workspaces).toEqual([])
    expect(sessionStorage.getItem(NATIVE_SESSION_KEY)).toBeNull()
    await act(async () => { await current.authenticate({ action: 'login', username: 'owner', password: 'password' }) })
    expect(current.authRequired).toBe(false)
    expect(current.currentWorkspaceId).toBe('native-workspace')
    expect(auth.begin).not.toHaveBeenCalled()
  })
})

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('localStorage', memoryStorage())
  vi.stubGlobal('sessionStorage', memoryStorage())
  localStorage.clear()
  sessionStorage.clear()
  auth.initialize.mockResolvedValue('oidc-token')
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  if (root) act(() => root!.unmount())
  host?.remove()
  root = undefined
  host = undefined
  delete window.__TERNILO_BOOT__
  api.clearToken()
  api.clearTenant()
  api.setRefreshHandler(null)
  vi.restoreAllMocks()
  auth.begin.mockClear()
  auth.clear.mockClear()
  auth.initialize.mockClear()
  auth.refresh.mockClear()
  vi.unstubAllGlobals()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('chooseSelection', () => {
  it('preserves a valid empty workspace before falling back to another workspace session', () => {
    const state: ApplicationState = {
      workspaces: [workspace('empty', 1), workspace('busy', 2)],
      sessions: [session('busy-session', 'busy', 3)],
    }
    expect(chooseSelection(state, 'empty', null)).toEqual({ workspaceId: 'empty', sessionId: null })
    expect(chooseSelection(state, 'missing', null)).toEqual({ workspaceId: 'busy', sessionId: 'busy-session' })
  })
})

describe('remote access token lifetime', () => {
  it('does not show remote login when local bootstrap recovery fails', async () => {
    window.__TERNILO_BOOT__ = { remote: false, apiToken: 'expired-local-token' }
    api.setToken('expired-local-token')
    vi.stubGlobal('fetch', vi.fn(async (url: string) => url === '/'
      ? new Response('unavailable', { status: 503 })
      : new Response('{}', { status: 401 })))
    await mount()
    await act(async () => { await current.refresh() })
    expect(current.authRequired).toBe(false)
    expect(current.loading).toBe(false)
    expect(current.error).toContain('本机服务')
  })

  it('restores tab-scoped authentication and clears both lifetimes on logout', async () => {
    window.__TERNILO_BOOT__ = { remote: true }
    sessionStorage.setItem(REMOTE_TOKEN_STORAGE_KEY, 'tab-token')
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      if (path === '/state') return Promise.resolve({ workspaces: [], sessions: [] }) as never
      if (path === '/catalog') return Promise.resolve(catalog('remote')) as never
      if (path === '/agent-presets') return Promise.resolve(presets('standard')) as never
      throw new Error(`unexpected request ${path}`)
    })

    await mount()

    expect(current.authRequired).toBe(false)
    expect(api.liveCredentials().bearerToken).toBe('tab-token')
    localStorage.setItem(REMOTE_TOKEN_STORAGE_KEY, 'persistent-token')
    act(() => current.logout())
    expect(current.authRequired).toBe(true)
    expect(sessionStorage.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()
    expect(localStorage.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()
    expect(api.hasToken()).toBe(false)
  })
})

describe('WorkbenchProvider request epochs', () => {
  it('keeps an offline Node session and its cached state visible when target configuration is unavailable', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem('ternilo.current-workspace', 'workspace-node')
    localStorage.setItem('ternilo.current-session', 'session-node')
    const state: ApplicationState = {
      workspaces: [{ ...workspace('workspace-node'), placement: 'local_node' }],
      sessions: [{ ...session('session-node', 'workspace-node', 1), placement: 'local_node' }],
    }
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      if (path === '/state') return Promise.resolve(state) as never
      if (path === '/catalog?session_id=session-node' || path === '/agent-presets?session_id=session-node') {
        return Promise.reject(new Error('Node is offline')) as never
      }
      throw new Error(`unexpected request ${path}`)
    })

    await mount()

    expect(current.loading).toBe(false)
    expect(current.error).toBe('')
    expect(current.snapshot).toEqual(state)
    expect(current.currentWorkspaceId).toBe('workspace-node')
    expect(current.currentSessionId).toBe('session-node')
    expect(current.catalog).toBeNull()
    expect(current.presets).toEqual(presets('standard'))
  })

  it('reloads catalogs and presets for the selected execution target', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem('ternilo.current-workspace', 'workspace-cloud')
    localStorage.setItem('ternilo.current-session', 'session-cloud')
    const state: ApplicationState = {
      workspaces: [workspace('workspace-cloud'), workspace('workspace-node')],
      sessions: [
        session('session-cloud', 'workspace-cloud', 1),
        session('session-node', 'workspace-node', 2),
      ],
    }
    const paths: string[] = []
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      paths.push(path)
      if (path === '/state') return Promise.resolve(state) as never
      if (path === '/catalog?session_id=session-cloud') return Promise.resolve(catalog('cloud')) as never
      if (path === '/agent-presets?session_id=session-cloud') return Promise.resolve(presets('cloud-default')) as never
      if (path === '/catalog?session_id=session-node') return Promise.resolve(catalog('node')) as never
      if (path === '/agent-presets?session_id=session-node') return Promise.resolve(presets('node-default')) as never
      throw new Error(`unexpected request ${path}`)
    })

    await mount()
    expect(current.catalog?.revision).toBe('cloud')
    expect(current.presets.default_id).toBe('cloud-default')

    act(() => current.selectSession('session-node'))
    await vi.waitFor(() => expect(current.catalog?.revision).toBe('node'))
    expect(current.presets.default_id).toBe('node-default')
    expect(paths).toContain('/catalog?session_id=session-node')
    expect(paths).toContain('/agent-presets?session_id=session-node')
  })

  it('applies the saved full-access default only to local-node Session creation', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem(defaultPermissionStorageKey, 'full_access')
    const cloudWorkspace = { ...workspace('cloud-workspace'), placement: 'cloud' as const }
    const nodeWorkspace = { ...workspace('node-workspace'), placement: 'local_node' as const }
    const state: ApplicationState = { workspaces: [cloudWorkspace, nodeWorkspace], sessions: [] }
    const submitted: Array<Record<string, unknown>> = []
    vi.spyOn(api, 'request').mockImplementation((path: string, options?: { body?: unknown }) => {
      if (route(path) === '/catalog') return Promise.resolve(catalog('initial')) as never
      if (route(path) === '/agent-presets') return Promise.resolve(presets('standard')) as never
      if (route(path) === '/state') return Promise.resolve(state) as never
      if (route(path) === '/sessions') {
        submitted.push(options?.body as Record<string, unknown>)
        const body = options?.body as { workspace_id: string }
        return Promise.resolve({
          ...session(`session-${submitted.length}`, body.workspace_id, submitted.length),
          placement: body.workspace_id === cloudWorkspace.workspace_id ? 'cloud' : 'local_node',
        }) as never
      }
      throw new Error(`unexpected request ${path}`)
    })
    await mount()

    await act(async () => { await current.createSession(cloudWorkspace.workspace_id) })
    await act(async () => { await current.createSession(nodeWorkspace.workspace_id) })

    expect(submitted).toEqual([
      expect.objectContaining({ workspace_id: cloudWorkspace.workspace_id, agent_preset: 'standard', permissions: 'workspace_write' }),
      expect.objectContaining({ workspace_id: nodeWorkspace.workspace_id, agent_preset: 'standard', permissions: 'full_access' }),
    ])
  })

  it('publishes one shared creating and hydration lifecycle for every fork entry point', async () => {
    window.__TERNILO_BOOT__ = {}
    const source = session('source', 'workspace-a', 1)
    const child = {
      ...session('child', 'workspace-a', 2, 'source (1)'),
      parent_session_id: source.identity.session_id,
    }
    const initial: ApplicationState = {
      workspaces: [workspace('workspace-a')],
      sessions: [source],
    }
    const forked: ApplicationState = {
      workspaces: initial.workspaces,
      sessions: [source, child],
    }
    const pendingFork = deferred<LocalSession>()
    let stateReads = 0
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      if (route(path) === '/catalog') return Promise.resolve(catalog('initial')) as never
      if (route(path) === '/agent-presets') return Promise.resolve(presets('standard')) as never
      if (route(path) === '/state') {
        stateReads += 1
        return Promise.resolve(stateReads === 1 ? initial : forked) as never
      }
      if (route(path) === '/sessions/source/fork') return pendingFork.promise as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()

    let request!: Promise<LocalSession>
    act(() => { request = current.forkSession('source', 7) })
    expect(current.forkOperation).toEqual({
      sourceSessionId: 'source', childSessionId: null, phase: 'creating',
    })

    await act(async () => {
      pendingFork.resolve(child)
      await request
    })
    expect(current.currentSessionId).toBe('child')
    expect(current.forkOperation).toEqual({
      sourceSessionId: 'source', childSessionId: 'child', phase: 'hydrating',
    })

    act(() => current.completeForkHydration('source'))
    expect(current.forkOperation?.phase).toBe('hydrating')
    act(() => current.selectSession('source'))
    expect(current.forkOperation).toBeNull()
  })

  it('removes a Session from polling state before its DELETE request completes', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem('ternilo.current-workspace', 'workspace-a')
    localStorage.setItem('ternilo.current-session', 'session-a')
    const initial: ApplicationState = {
      workspaces: [workspace('workspace-a')],
      sessions: [session('session-a', 'workspace-a', 1)],
    }
    const deleted: ApplicationState = { workspaces: initial.workspaces, sessions: [] }
    const pendingDelete = deferred<void>()
    let stateReads = 0
    vi.spyOn(api, 'request').mockImplementation((path: string, options?: { method?: string }) => {
      if (route(path) === '/catalog') return Promise.resolve(catalog('initial')) as never
      if (route(path) === '/agent-presets') return Promise.resolve(presets('standard')) as never
      if (route(path) === '/state') {
        stateReads += 1
        return Promise.resolve(stateReads === 1 ? initial : deleted) as never
      }
      if (route(path) === '/sessions/session-a' && options?.method === 'DELETE') return pendingDelete.promise as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()

    let deletion!: Promise<void>
    act(() => { deletion = current.deleteSession('session-a') })
    expect(current.snapshot.sessions).toEqual([])
    expect(current.currentSessionId).toBeNull()

    await act(async () => {
      pendingDelete.resolve(undefined)
      await deletion
    })
    expect(current.snapshot).toEqual(deleted)
  })

  it.each(['state', 'catalog'] as const)('preserves live permissions when an older refresh is awaiting %s', async stage => {
    window.__TERNILO_BOOT__ = {}
    const initialSession = { ...session('session-a', 'workspace-a', 1), access: { owner_user_id: 'owner', is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: true, stop: false, configure: false } } }
    const initial: ApplicationState = { workspaces: [workspace('workspace-a')], sessions: [initialSession] }
    const staleState = deferred<ApplicationState>()
    const staleCatalog = deferred<ApplicationCatalog>()
    let refreshing = false
    vi.spyOn(api, 'request').mockImplementation(async (path: string) => {
      if (route(path) === '/state') return refreshing && stage === 'state' ? staleState.promise as never : initial as never
      if (route(path) === '/catalog') return refreshing ? staleCatalog.promise as never : catalog('initial') as never
      if (route(path) === '/agent-presets') return presets('standard') as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()
    refreshing = true
    let request!: Promise<void>
    await act(async () => { request = current.refresh(); await Promise.resolve() })
    const fresh: ApplicationState = { ...initial, sessions: [{ ...initialSession, access: { ...initialSession.access, permissions: { ...initialSession.access.permissions, stop: true } } }] }
    act(() => current.acceptLiveWorkbench(fresh, 2, []))
    expect(current.currentSession?.access?.permissions.stop).toBe(true)
    await act(async () => { staleState.resolve(initial); staleCatalog.resolve(catalog('fresh-metadata')); await request })
    expect(current.currentSession?.access?.permissions.stop).toBe(true)
    expect(current.catalog?.revision).toBe('fresh-metadata')
  })

  it('keeps a live snapshot received before the initial HTTP load completes', async () => {
    window.__TERNILO_BOOT__ = {}
    const pending = deferred<ApplicationState>()
    vi.spyOn(api, 'request').mockImplementation(async (path: string) => {
      if (route(path) === '/state') return pending.promise as never
      if (route(path) === '/catalog') return catalog('initial') as never
      if (route(path) === '/agent-presets') return presets('standard') as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()
    const fresh: ApplicationState = { workspaces: [], sessions: [session('fresh', 'hidden-parent', 2)] }
    act(() => current.acceptLiveWorkbench(fresh, 1, []))
    await act(async () => { pending.resolve({ workspaces: [], sessions: [] }); await Promise.resolve(); await Promise.resolve() })
    expect(current.currentSessionId).toBe('fresh')
    expect(current.snapshot).toEqual(fresh)
    expect(current.loading).toBe(false)
  })

  it.each(['state', 'catalog'] as const)('does not resurrect revoked resources when an older refresh is awaiting %s', async stage => {
    window.__TERNILO_BOOT__ = {}
    const initial: ApplicationState = { workspaces: [workspace('workspace-a')], sessions: [session('session-a', 'workspace-a', 1)] }
    const staleState = deferred<ApplicationState>()
    const staleCatalog = deferred<ApplicationCatalog>()
    let refreshing = false
    vi.spyOn(api, 'request').mockImplementation(async (path: string) => {
      if (route(path) === '/state') return refreshing && stage === 'state' ? staleState.promise as never : initial as never
      if (route(path) === '/catalog') return refreshing ? staleCatalog.promise as never : catalog('initial') as never
      if (route(path) === '/agent-presets') return presets('standard') as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()
    refreshing = true
    let request!: Promise<void>
    await act(async () => { request = current.refresh(); await Promise.resolve() })
    act(() => current.acceptLiveWorkbench({ workspaces: [], sessions: [] }, 2, []))
    await act(async () => { staleState.resolve(initial); staleCatalog.resolve(catalog('old')); await request })
    expect(current.snapshot).toEqual({ workspaces: [], sessions: [] })
    expect(current.currentSessionId).toBeNull()
    expect(current.currentWorkspaceId).toBeNull()
  })

  it('reconciles an in-flight refresh against the latest manual selection', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem('ternilo.current-workspace', 'workspace-a')
    localStorage.setItem('ternilo.current-session', 'session-a')
    const state: ApplicationState = {
      workspaces: [workspace('workspace-a'), workspace('workspace-b')],
      sessions: [session('session-a', 'workspace-a', 1), session('session-b', 'workspace-b', 2)],
    }
    const pending = {
      catalog: deferred<ApplicationCatalog>(), state: deferred<ApplicationState>(), presets: deferred<AgentPresetRoster>(),
    }
    const calls = new Map<string, number>()
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      const key = route(path)
      const call = calls.get(key) ?? 0
      calls.set(key, call + 1)
      if (call === 0) {
        if (route(path) === '/catalog') return Promise.resolve(catalog('initial')) as never
        if (route(path) === '/state') return Promise.resolve(state) as never
        if (route(path) === '/agent-presets') return Promise.resolve(presets('initial')) as never
      }
      if (route(path) === '/catalog') return pending.catalog.promise as never
      if (route(path) === '/state') return pending.state.promise as never
      if (route(path) === '/agent-presets') return pending.presets.promise as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()

    let refreshing!: Promise<void>
    act(() => { refreshing = current.refresh() })
    act(() => current.selectSession('session-b'))
    await act(async () => {
      pending.catalog.resolve(catalog('refreshed'))
      pending.state.resolve(state)
      pending.presets.resolve(presets('refreshed'))
      await refreshing
    })

    expect(current.currentWorkspaceId).toBe('workspace-b')
    expect(current.currentSessionId).toBe('session-b')
  })

  it('ignores an older refresh that resolves after the latest response', async () => {
    window.__TERNILO_BOOT__ = {}
    localStorage.setItem('ternilo.current-workspace', 'workspace-a')
    localStorage.setItem('ternilo.current-session', 'session-a')
    const initial: ApplicationState = {
      workspaces: [workspace('workspace-a'), workspace('workspace-b')],
      sessions: [session('session-a', 'workspace-a', 1), session('session-b', 'workspace-b', 2)],
    }
    const staleState: ApplicationState = {
      workspaces: [workspace('workspace-a')],
      sessions: [session('session-a', 'workspace-a', 1, 'stale')],
    }
    const freshState: ApplicationState = {
      workspaces: [workspace('workspace-a'), workspace('workspace-b')],
      sessions: [session('session-a', 'workspace-a', 1), session('session-b', 'workspace-b', 3, 'fresh')],
    }
    const stale = {
      catalog: deferred<ApplicationCatalog>(), state: deferred<ApplicationState>(), presets: deferred<AgentPresetRoster>(),
    }
    const fresh = {
      catalog: deferred<ApplicationCatalog>(), state: deferred<ApplicationState>(), presets: deferred<AgentPresetRoster>(),
    }
    const calls = new Map<string, number>()
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      const key = route(path)
      const call = calls.get(key) ?? 0
      calls.set(key, call + 1)
      if (call === 0) {
        if (route(path) === '/catalog') return Promise.resolve(catalog('initial')) as never
        if (route(path) === '/state') return Promise.resolve(initial) as never
        if (route(path) === '/agent-presets') return Promise.resolve(presets('initial')) as never
      }
      const source = call === 1 ? stale : fresh
      if (route(path) === '/catalog') return source.catalog.promise as never
      if (route(path) === '/state') return source.state.promise as never
      if (route(path) === '/agent-presets') return source.presets.promise as never
      throw new Error(`unexpected request ${path}`)
    })
    await mount()
    expect(current.currentSessionId).toBe('session-a')

    let older!: Promise<void>
    let latest!: Promise<void>
    act(() => { older = current.refresh() })
    act(() => current.selectSession('session-b'))
    act(() => { latest = current.refresh() })
    await act(async () => {
      fresh.catalog.resolve(catalog('fresh'))
      fresh.state.resolve(freshState)
      fresh.presets.resolve(presets('fresh'))
      await latest
    })
    expect(current.currentSessionId).toBe('session-b')
    expect(current.snapshot.sessions.find(item => item.identity.session_id === 'session-b')?.title).toBe('fresh')

    await act(async () => {
      stale.catalog.resolve(catalog('stale'))
      stale.state.resolve(staleState)
      stale.presets.resolve(presets('stale'))
      await older
    })
    expect(current.currentSessionId).toBe('session-b')
    expect(current.snapshot.sessions.find(item => item.identity.session_id === 'session-b')?.title).toBe('fresh')
    expect(current.catalog?.revision).toBe('fresh')
  })

  it('prevents an old tenant load from retargeting the API or ending the active load', async () => {
    window.__TERNILO_BOOT__ = { remote: true, platform: true }
    vi.stubGlobal('fetch', vi.fn(async () => new Response(JSON.stringify({ initialized: true, mode: 'multi_user', native_enabled: true, oidc_enabled: true }))))
    localStorage.setItem('ternilo.current-tenant', 'tenant-a')
    const oldRoster = deferred<{ tenants: TenantSummary[] }>()
    const nextRoster = deferred<{ tenants: TenantSummary[] }>()
    const nextCatalog = deferred<ApplicationCatalog>()
    const nextState = deferred<ApplicationState>()
    const nextPresets = deferred<AgentPresetRoster>()
    let rosterCalls = 0
    let stateCalls = 0
    vi.spyOn(api, 'request').mockImplementation((path: string) => {
      if (path === '/auth/session') return Promise.resolve({ user: { user_id: 'owner' }, is_instance_owner: true, platform_role: 'owner', personal_tenant_id: 'space', personal_project_id: 'project', instance: { managed_execution_enabled: false, mode: 'multi_user', owner_user_id: 'owner', revision: 1 } }) as never
      if (path === '/tenants') {
        rosterCalls += 1
        return (rosterCalls === 1 ? oldRoster.promise : nextRoster.promise) as never
      }
      stateCalls += 1
      if (stateCalls > 3) throw new Error(`stale tenant requested ${path}`)
      if (route(path) === '/catalog') return nextCatalog.promise as never
      if (route(path) === '/state') return nextState.promise as never
      if (route(path) === '/agent-presets') return nextPresets.promise as never
      throw new Error(`unexpected request ${path}`)
    })
    const setTenant = vi.spyOn(api, 'setTenant')
    await mount()
    await vi.waitFor(() => expect(rosterCalls).toBe(1))

    let switching!: Promise<void>
    act(() => { switching = current.selectTenant('tenant-b') })
    await act(async () => {
      nextRoster.resolve({ tenants: [tenant('tenant-a'), tenant('tenant-b')] })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(stateCalls).toBe(1)
    expect(current.loading).toBe(true)

    await act(async () => {
      oldRoster.resolve({ tenants: [tenant('tenant-a'), tenant('tenant-b')] })
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(current.loading).toBe(true)
    expect(stateCalls).toBe(1)
    expect(setTenant.mock.calls.every(([id]) => id === 'tenant-b')).toBe(true)

    const state: ApplicationState = {
      workspaces: [workspace('workspace-b')],
      sessions: [session('session-b', 'workspace-b', 1)],
    }
    await act(async () => {
      nextCatalog.resolve(catalog('tenant-b'))
      nextState.resolve(state)
      nextPresets.resolve(presets('tenant-b'))
      await switching
    })
    expect(current.loading).toBe(false)
    expect(current.currentTenantId).toBe('tenant-b')
    expect(current.currentSessionId).toBe('session-b')
    expect(current.catalog?.revision).toBe('tenant-b')
  })
})
