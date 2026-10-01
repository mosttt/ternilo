import * as React from 'react'
import { InputViewerProvider } from './input-viewer'
import { randomUuid } from '@/lib/random-id'
import { api, ApiError } from '@/api/client'
import type { SessionLiveActivity } from '@/api/live-client'
import { refreshLocalToken, usesLocalBootstrap } from '@/auth/local'
import { beginOidcLogin, completeOidcLink, clearOidcSession, initializeOidcSession, OidcFlowError, readOidcToken, refreshOidcSession } from '@/auth/oidc'
import {
  clearAccountLink, clearNativeSession, isOidcUsernameRequired, isServerAccessPaused, loadServerAuthConfig, loadServerIdentity,
  readNativeToken, registerNative, registerOidcAccount, signInNative, storeNativeSession,
  type NativeLoginInput, type ServerAuthConfig, type ServerIdentity, type ServerInstance,
} from '@/auth/server'
import { clearRemoteToken, readRemoteToken, storeRemoteToken } from '@/auth/remote-token'
import { useTranslate } from '@/i18n/provider'
import { permissionForPlacement, readDefaultPermission } from '@/domain/default-permission'
import { invalidateAllProviderInventories } from '@/domain/provider-inventory'
import { invalidateFileInventory, updateFileInventoryWorkbench } from '@/domain/file-inventory'
import { readSidebarView } from '@/domain/sidebar-view'
import {
  executionTargetKey,
  executionTargetPath,
  type ExecutionTarget,
} from '@/domain/execution-target'
import type {
  AgentPresetRoster,
  ApplicationCatalog,
  ApplicationState,
  LocalSession,
  PlatformWorkspaceCreateInput,
  TenantSummary,
  ToastMessage,
  Workspace,
} from '@/types'

const storage = {
  session: 'ternilo.current-session',
  workspace: 'ternilo.current-workspace',
  theme: 'ternilo.theme',
  sidebarView: 'ternilo.sidebar-view-v1',
  tenant: 'ternilo.current-tenant',
}

interface WorkbenchContextValue {
  snapshot: ApplicationState
  sessionActivity: Record<string, SessionLiveActivity>
  catalog: ApplicationCatalog | null
  presets: AgentPresetRoster
  loading: boolean
  error: string
  remote: boolean
  platform: boolean
  authRequired: boolean
  accessPaused: boolean
  oidcRegistrationRequired: boolean
  registerOidcUsername(username: string, email: string, turnstileToken?: string): Promise<'active' | 'pending' | void>
  pauseAccess(): void
  serverAuthConfig: ServerAuthConfig | null
  serverIdentity: ServerIdentity | null
  accountScope: string | undefined
  authenticate(input: NativeLoginInput): Promise<'active' | 'pending' | void>
  retryAuthentication(): void
  acceptInstance(instance: ServerInstance): void
  tenants: TenantSummary[]
  currentTenantId: string | null
  currentTenantRole: TenantSummary['role'] | null
  currentWorkspace: Workspace | null
  currentSession: LocalSession | null
  currentWorkspaceId: string | null
  currentSessionId: string | null
  forkOperation: ForkOperation | null
  toasts: ToastMessage[]
  login(token: string, remember?: boolean): Promise<void>
  logout(): void
  selectTenant(id: string): Promise<void>
  createTenant(displayName: string, slug: string): Promise<void>
  refresh(): Promise<void>
  onlineComputersOnly: boolean
  setOnlineComputersOnly(onlineOnly: boolean): Promise<void>
  acceptLiveWorkbench(state: ApplicationState, revision: number, activity: SessionLiveActivity[]): void
  acceptLiveActivity(activity: SessionLiveActivity): void
  selectWorkspace(id: string): void
  selectSession(id: string): void
  createSession(workspaceId?: string, agentPreset?: string): Promise<LocalSession>
  createWorkspace(input: string | PlatformWorkspaceCreateInput): Promise<Workspace>
  renameWorkspace(id: string, title: string): Promise<Workspace>
  unregisterWorkspace(id: string): Promise<void>
  updateSession(id: string, update: Record<string, unknown>): Promise<LocalSession>
  forkSession(id: string, atSeq?: number): Promise<LocalSession>
  completeForkHydration(sessionId: string): void
  archiveSession(id: string): Promise<LocalSession>
  deleteSession(id: string): Promise<void>
  notify(message: string, kind?: ToastMessage['kind']): void
  dismissToast(id: string): void
}

export interface ForkOperation {
  sourceSessionId: string
  childSessionId: string | null
  phase: 'creating' | 'hydrating'
}

const emptyState: ApplicationState = { workspaces: [], sessions: [] }
const emptyPresets: AgentPresetRoster = { presets: [], default_id: 'standard', authorable: false }
const WorkbenchContext = React.createContext<WorkbenchContextValue | null>(null)

function activeSessions(snapshot: ApplicationState) {
  return snapshot.sessions.filter(session => session.archived_at_ms == null)
}

function selectionTarget(workspaceId: string | null, sessionId: string | null): ExecutionTarget {
  return { sessionId, workspaceId }
}

async function readTargetConfiguration(target: ExecutionTarget) {
  const [catalog, presets] = await Promise.all([
    api.request<ApplicationCatalog>(executionTargetPath('/catalog', target)),
    api.request<AgentPresetRoster>(executionTargetPath('/agent-presets', target)),
  ])
  return { catalog, presets }
}

async function readAvailableTargetConfiguration(target: ExecutionTarget) {
  try {
    return await readTargetConfiguration(target)
  } catch {
    return null
  }
}

export function chooseSelection(snapshot: ApplicationState, requestedWorkspace: string | null, requestedSession: string | null) {
  const sessions = activeSessions(snapshot)
  const requestedSessionMatch = sessions.find(item => item.identity.session_id === requestedSession) ?? null
  if (requestedSessionMatch) {
    return {
      workspaceId: snapshot.workspaces.some(item => item.workspace_id === requestedSessionMatch.workspace_id)
        ? requestedSessionMatch.workspace_id
        : null,
      sessionId: requestedSessionMatch.identity.session_id,
    }
  }
  const requestedWorkspaceMatch = snapshot.workspaces.find(item => item.workspace_id === requestedWorkspace) ?? null
  if (requestedWorkspaceMatch) {
    const latestInWorkspace = sessions
      .filter(item => item.workspace_id === requestedWorkspaceMatch.workspace_id)
      .sort((a, b) => b.updated_at_ms - a.updated_at_ms)[0] ?? null
    return {
      workspaceId: requestedWorkspaceMatch.workspace_id,
      sessionId: latestInWorkspace?.identity.session_id ?? null,
    }
  }
  const selectedSession = [...sessions].sort((a, b) => b.updated_at_ms - a.updated_at_ms)[0] ?? null
  const selectedWorkspace = selectedSession
    ? snapshot.workspaces.find(item => item.workspace_id === selectedSession.workspace_id) ?? null
    : [...snapshot.workspaces].sort((a, b) => b.updated_at_ms - a.updated_at_ms)[0] ?? null
  return {
    workspaceId: selectedWorkspace?.workspace_id ?? null,
    sessionId: selectedSession?.identity.session_id ?? null,
  }
}

export function WorkbenchProvider({ children }: { children: React.ReactNode }) {
  const t = useTranslate('app')
  const boot = window.__TERNILO_BOOT__
  const remote = Boolean(boot?.remote)
  const platform = Boolean(boot?.platform)
  const [onlineComputersOnly, setOnlineComputersOnlyState] = React.useState(() => {
    const view = readSidebarView(localStorage.getItem(storage.sidebarView))
    return platform && view.groupBy === 'computer' && view.onlineComputersOnly
  })
  const onlineComputersOnlyRef = React.useRef(onlineComputersOnly)
  const computerFilterEpoch = React.useRef(0)
  const computerFilterLoading = React.useRef(false)
  const readWorkbenchState = React.useCallback(async () => {
    const epoch = computerFilterEpoch.current
    const next = await api.request<ApplicationState>(onlineComputersOnlyRef.current ? '/state?online_computers_only=true' : '/state')
    if (epoch !== computerFilterEpoch.current) throw new DOMException('Workbench filter changed', 'AbortError')
    return next
  }, [])
  const [snapshot, setSnapshot] = React.useState<ApplicationState>(emptyState)
  React.useLayoutEffect(() => updateFileInventoryWorkbench(snapshot), [snapshot])
  const [sessionActivity, setSessionActivity] = React.useState<Record<string, SessionLiveActivity>>({})
  const [catalog, setCatalog] = React.useState<ApplicationCatalog | null>(null)
  const [presets, setPresets] = React.useState<AgentPresetRoster>(emptyPresets)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [authRequired, setAuthRequired] = React.useState(() => {
    if (remote && !platform && !api.hasToken()) {
      const stored = readRemoteToken()
      if (stored) api.setToken(stored.token)
    }
    return platform || (remote && !api.hasToken())
  })
  const [accessPaused, setAccessPaused] = React.useState(false)
  const [oidcRegistrationRequired, setOidcRegistrationRequired] = React.useState(false)
  const [authReady, setAuthReady] = React.useState(!platform)
  const [serverAuthConfig, setServerAuthConfig] = React.useState<ServerAuthConfig | null>(null)
  const initialTurnstile = React.useRef<boolean | undefined>(undefined)
  const [serverIdentity, setServerIdentity] = React.useState<ServerIdentity | null>(null)
  const [authenticationRevision, retryAuthentication] = React.useReducer(value => value + 1, 0)
  const authenticationRequestRef = React.useRef(0)
  const [tenants, setTenants] = React.useState<TenantSummary[]>([])
  const [currentTenantId, setCurrentTenantId] = React.useState<string | null>(() => localStorage.getItem(storage.tenant))
  const [currentWorkspaceId, setCurrentWorkspaceId] = React.useState<string | null>(() => localStorage.getItem(storage.workspace))
  const [currentSessionId, setCurrentSessionId] = React.useState<string | null>(() => localStorage.getItem(storage.session))
  const [forkOperation, setForkOperation] = React.useState<ForkOperation | null>(null)
  const [toasts, setToasts] = React.useState<ToastMessage[]>([])

  const tenantRef = React.useRef(currentTenantId)
  const selectionRef = React.useRef({ workspaceId: currentWorkspaceId, sessionId: currentSessionId })
  const targetEpochRef = React.useRef(0)
  const loadRequestRef = React.useRef(0)
  const refreshRequestRef = React.useRef(0)
  // Live snapshots must survive older HTTP reads that are still loading metadata.
  const liveWorkbenchEpochRef = React.useRef(0)
  const configurationRequestRef = React.useRef(0)
  const configurationTargetRef = React.useRef<string | null>(null)
  const forkOperationRef = React.useRef<ForkOperation | null>(null)

  const replaceForkOperation = React.useCallback((operation: ForkOperation | null) => {
    forkOperationRef.current = operation
    setForkOperation(operation)
  }, [])

  const replaceSelection = React.useCallback((workspaceId: string | null, sessionId: string | null) => {
    const operation = forkOperationRef.current
    if (operation?.phase === 'hydrating' && operation.childSessionId !== sessionId) {
      replaceForkOperation(null)
    }
    const current = selectionRef.current
    if (current.workspaceId === workspaceId && current.sessionId === sessionId) return
    selectionRef.current = { workspaceId, sessionId }
    setCurrentWorkspaceId(workspaceId)
    setCurrentSessionId(sessionId)
  }, [replaceForkOperation])

  const replaceTenant = React.useCallback((tenantId: string | null) => {
    if (tenantRef.current === tenantId) return
    tenantRef.current = tenantId
    setCurrentTenantId(tenantId)
  }, [])

  const invalidateTarget = React.useCallback(() => {
    computerFilterLoading.current = false
    targetEpochRef.current += 1
    loadRequestRef.current += 1
    refreshRequestRef.current += 1
    configurationRequestRef.current += 1
    configurationTargetRef.current = null
    invalidateAllProviderInventories()
    invalidateFileInventory()
    return targetEpochRef.current
  }, [])

  const pauseAccess = React.useCallback(() => {
    invalidateTarget()
    api.clearToken()
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    setToasts([])
    setError('')
    setLoading(false)
    setAccessPaused(true)
    setAuthRequired(true)
  }, [invalidateTarget])

  React.useEffect(() => api.onForbidden(cause => {
    if (platform && isServerAccessPaused(cause)) pauseAccess()
  }), [pauseAccess, platform])

  React.useEffect(() => api.onUnauthorized(() => {
    setAccessPaused(false)
    invalidateTarget()
    if (!remote && !platform) {
      setAuthRequired(false)
      setLoading(false)
      setError(t('error.localReconnect'))
      return
    }
    if (platform) {
      clearOidcSession()
      clearNativeSession()
      setServerIdentity(null)
    }
    if (remote && !platform) clearRemoteToken()
    setToasts([])
    setAuthRequired(true)
  }), [invalidateTarget, platform, remote, t])

  React.useEffect(() => {
    if (!usesLocalBootstrap()) return
    api.setRefreshHandler(refreshLocalToken)
    return () => api.setRefreshHandler(null)
  }, [platform, remote])

  React.useEffect(() => {
    if (!platform) return
    const requestId = ++authenticationRequestRef.current
    let cancelled = false
    const isCurrent = () => !cancelled && requestId === authenticationRequestRef.current
    setAuthReady(false)
    setOidcRegistrationRequired(false)
    setError('')
    void (async () => {
      const config = await loadServerAuthConfig()
      if (!isCurrent()) return
      const requiresTurnstile = Boolean(config.turnstile)
      if (initialTurnstile.current === false && requiresTurnstile) {
        location.reload()
        return
      }
      initialTurnstile.current ??= requiresTurnstile
      setServerAuthConfig(config)
      try {
        if (await completeOidcLink() && isCurrent()) notify(t('auth.oidcLinked'))
      } catch (cause) {
        if (isCurrent()) notify(t('auth.oidcLinkFailed', { error: cause instanceof OidcFlowError
          ? t(cause.translationKey)
          : cause instanceof Error ? cause.message : String(cause) }), 'error')
      }
      const nativeToken = config.initialized ? readNativeToken() : ''
      const token = nativeToken || (config.initialized && config.oidc_enabled ? await initializeOidcSession() : '')
      if (!isCurrent()) return
      api.setRefreshHandler(nativeToken ? null : config.oidc_enabled ? refreshOidcSession : null)
      if (!token) {
        setLoading(false)
        setAuthRequired(true)
        return
      }
      api.setToken(token)
      const identity = await loadServerIdentity()
      if (!isCurrent()) return
      setServerIdentity(identity)
      setAccessPaused(false)
      setLoading(true)
      setServerAuthConfig(current => current && { ...current, mode: identity.instance.mode })
      setAuthRequired(false)
    })().catch(cause => {
      if (!isCurrent()) return
      if (cause instanceof ApiError && isServerAccessPaused(cause)) {
        pauseAccess()
        return
      }
      if (cause instanceof ApiError && isOidcUsernameRequired(cause)) {
        api.clearToken()
        api.setRefreshHandler(null)
        setServerIdentity(null)
        setOidcRegistrationRequired(true)
        setLoading(false)
        setAuthRequired(true)
        return
      }
      const message = cause instanceof OidcFlowError
        ? t(cause.translationKey)
        : cause instanceof Error ? cause.message : String(cause)
      api.clearToken()
      clearNativeSession()
      clearOidcSession()
      setServerIdentity(null)
      setError(message)
      setLoading(false)
      setAuthRequired(true)
    }).finally(() => { if (isCurrent()) setAuthReady(true) })
    return () => {
      cancelled = true
      api.setRefreshHandler(null)
    }
  }, [platform, t, authenticationRevision, pauseAccess])

  const registerOidcUsername = React.useCallback(async (username: string, email: string, turnstileToken?: string) => {
    if (!oidcRegistrationRequired) return
    const requestId = ++authenticationRequestRef.current
    const token = await initializeOidcSession()
    if (requestId !== authenticationRequestRef.current) return
    if (!token) {
      setOidcRegistrationRequired(false)
      throw new OidcFlowError('expired')
    }
    const result = await registerOidcAccount(username.trim(), email.trim(), token, turnstileToken)
    if (requestId !== authenticationRequestRef.current) return
    setOidcRegistrationRequired(false)
    if (result.status === 'pending') {
      clearOidcSession()
      clearNativeSession()
      api.clearToken()
      api.clearTenant()
      setServerIdentity(null)
      setAuthRequired(true)
    } else {
      retryAuthentication()
    }
    return result.status
  }, [oidcRegistrationRequired])

  const acceptInstance = React.useCallback((instance: ServerInstance) => {
    setServerIdentity(current => current && { ...current, instance })
    setServerAuthConfig(current => current && { ...current, mode: instance.mode })
  }, [])

  const authenticate = React.useCallback(async (input: NativeLoginInput) => {
    const requestId = ++authenticationRequestRef.current
    const registration = input.action === 'register' ? await registerNative(input) : null
    const identity = input.action !== 'register' ? await signInNative(input) : registration?.session
    if (requestId !== authenticationRequestRef.current) return
    if (registration?.status === 'pending') return 'pending' as const
    if (!identity) throw new Error('Registration did not return an active session')
    invalidateTarget()
    setOidcRegistrationRequired(false)
    clearOidcSession()
    api.setRefreshHandler(null)
    api.clearTenant()
    api.setToken(identity.access_token)
    storeNativeSession(identity)
    clearAccountLink()
    replaceTenant(identity.personal_tenant_id)
    replaceSelection(null, null)
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    setServerIdentity(identity)
    setAccessPaused(false)
    setServerAuthConfig(current => current && { ...current, initialized: true, mode: identity.instance.mode })
    setLoading(true)
    setAuthReady(true)
    setAuthRequired(false)
    setError('')
    return 'active' as const
  }, [invalidateTarget, replaceSelection, replaceTenant])

  const notify = React.useCallback((message: string, kind: ToastMessage['kind'] = 'success') => {
    const id = randomUuid()
    setToasts(current => [...current, { id, message, kind }])
    window.setTimeout(() => setToasts(current => current.filter(item => item.id !== id)), 4_500)
  }, [])

  const dismissToast = React.useCallback((id: string) => {
    setToasts(current => current.filter(item => item.id !== id))
  }, [])

  const load = React.useCallback(async (requestedTenant?: string, expectedTargetEpoch = targetEpochRef.current) => {
    if (remote && authRequired) return
    const requestId = ++loadRequestRef.current
    const liveEpoch = liveWorkbenchEpochRef.current
    const isCurrent = (tenantId?: string | null) => (
      requestId === loadRequestRef.current
      && expectedTargetEpoch === targetEpochRef.current
      && (tenantId === undefined || tenantRef.current === tenantId)
    )
    setLoading(true)
    setError('')
    try {
      let tenantId = requestedTenant ?? tenantRef.current
      if (platform) {
        const roster = await api.request<{ tenants: TenantSummary[] }>('/tenants')
        if (!isCurrent()) return
        setTenants(roster.tenants)
        tenantId = roster.tenants.some(tenant => tenant.tenant_id === tenantId)
          ? tenantId
          : roster.tenants.find(tenant => tenant.kind === 'personal')?.tenant_id ?? roster.tenants[0]?.tenant_id
        replaceTenant(tenantId ?? null)
        if (!tenantId) {
          api.clearTenant()
          configurationRequestRef.current += 1
          configurationTargetRef.current = null
          setSnapshot(emptyState)
          setSessionActivity({})
          setCatalog(null)
          setPresets(emptyPresets)
          replaceSelection(null, null)
          setError('')
          return
        }
        api.setTenant(tenantId)
        localStorage.setItem(storage.tenant, tenantId)
      }
      const nextSnapshot = await readWorkbenchState()
      if (!isCurrent(platform ? tenantId : undefined)) return
      const requestedSelection = selectionRef.current
      const initialSelection = liveEpoch !== liveWorkbenchEpochRef.current ? requestedSelection : chooseSelection(nextSnapshot, requestedSelection.workspaceId, requestedSelection.sessionId)
      const target = selectionTarget(initialSelection.workspaceId, initialSelection.sessionId)
      const targetKey = executionTargetKey(target)
      const configurationRequestId = ++configurationRequestRef.current
      const configuration = await readAvailableTargetConfiguration(target)
      if (!isCurrent(platform ? tenantId : undefined)) return
      const latestSelection = selectionRef.current
      const liveChanged = liveEpoch !== liveWorkbenchEpochRef.current
      const selection = liveChanged ? latestSelection : chooseSelection(nextSnapshot, latestSelection.workspaceId, latestSelection.sessionId)
      const finalTargetKey = executionTargetKey(selectionTarget(selection.workspaceId, selection.sessionId))
      if (!liveChanged) setSnapshot(nextSnapshot)
      if (configuration && configurationRequestId === configurationRequestRef.current && targetKey === finalTargetKey) {
        configurationTargetRef.current = targetKey
        setCatalog(configuration.catalog)
        setPresets(configuration.presets)
      } else {
        configurationTargetRef.current = targetKey === finalTargetKey ? targetKey : null
        setCatalog(null)
        setPresets(emptyPresets)
      }
      if (!liveChanged) replaceSelection(selection.workspaceId, selection.sessionId)
      setAuthRequired(false)
    } catch (cause) {
      if (!isCurrent()) return
      if (cause instanceof ApiError && cause.status === 401 && remote) setAuthRequired(true)
      else setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (isCurrent()) setLoading(false)
    }
  }, [authRequired, platform, remote, replaceSelection, replaceTenant])

  React.useEffect(() => {
    if (authReady) void load()
    // Initial boot is intentionally independent from future selection changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authReady, authRequired, serverIdentity?.user.user_id])

  React.useEffect(() => {
    if (currentWorkspaceId) localStorage.setItem(storage.workspace, currentWorkspaceId)
    else localStorage.removeItem(storage.workspace)
    if (currentSessionId) localStorage.setItem(storage.session, currentSessionId)
    else localStorage.removeItem(storage.session)
  }, [currentSessionId, currentWorkspaceId])

  React.useEffect(() => {
    if (loading || authRequired || (platform && !tenantRef.current)) return
    const target = selectionTarget(currentWorkspaceId, currentSessionId)
    const targetKey = executionTargetKey(target)
    if (configurationTargetRef.current === targetKey) return
    const requestId = ++configurationRequestRef.current
    configurationTargetRef.current = null
    setCatalog(null)
    setPresets(emptyPresets)
    void readTargetConfiguration(target)
      .then(configuration => {
        if (requestId !== configurationRequestRef.current) return
        const current = selectionRef.current
        if (executionTargetKey(selectionTarget(current.workspaceId, current.sessionId)) !== targetKey) return
        configurationTargetRef.current = targetKey
        setCatalog(configuration.catalog)
        setPresets(configuration.presets)
        setError('')
      })
      .catch(() => {
        if (requestId !== configurationRequestRef.current) return
        const current = selectionRef.current
        if (executionTargetKey(selectionTarget(current.workspaceId, current.sessionId)) !== targetKey) return
        configurationTargetRef.current = targetKey
        setCatalog(null)
        setPresets(emptyPresets)
      })
  }, [authRequired, currentSessionId, currentWorkspaceId, loading, platform])

  const refresh = React.useCallback(async () => {
    const requestId = ++refreshRequestRef.current
    const liveEpoch = liveWorkbenchEpochRef.current
    const targetEpoch = targetEpochRef.current
    const tenantId = tenantRef.current
    const isCurrent = () => (
      requestId === refreshRequestRef.current
      && targetEpoch === targetEpochRef.current
      && tenantId === tenantRef.current
    )
    try {
      const nextSnapshot = await readWorkbenchState()
      if (!isCurrent()) return
      const requestedSelection = selectionRef.current
      const initialSelection = liveEpoch !== liveWorkbenchEpochRef.current ? requestedSelection : chooseSelection(nextSnapshot, requestedSelection.workspaceId, requestedSelection.sessionId)
      const target = selectionTarget(initialSelection.workspaceId, initialSelection.sessionId)
      const targetKey = executionTargetKey(target)
      const configurationRequestId = ++configurationRequestRef.current
      const configuration = await readAvailableTargetConfiguration(target)
      if (!isCurrent()) return
      const latestSelection = selectionRef.current
      const liveChanged = liveEpoch !== liveWorkbenchEpochRef.current
      const selection = liveChanged ? latestSelection : chooseSelection(nextSnapshot, latestSelection.workspaceId, latestSelection.sessionId)
      const finalTargetKey = executionTargetKey(selectionTarget(selection.workspaceId, selection.sessionId))
      if (!liveChanged) setSnapshot(nextSnapshot)
      if (configurationRequestId === configurationRequestRef.current) {
        if (configuration && targetKey === finalTargetKey) {
          configurationTargetRef.current = targetKey
          setCatalog(configuration.catalog)
          setPresets(configuration.presets)
        } else {
          configurationTargetRef.current = targetKey === finalTargetKey ? targetKey : null
          setCatalog(null)
          setPresets(emptyPresets)
        }
      }
      if (!liveChanged) replaceSelection(selection.workspaceId, selection.sessionId)
      setError('')
    } catch (cause) {
      if (!isCurrent()) return
      setError(cause instanceof Error ? cause.message : String(cause))
      throw cause
    }
  }, [replaceSelection])

  const setOnlineComputersOnly = React.useCallback(async (onlyOnline: boolean) => {
    const effective = platform && onlyOnline
    if (onlineComputersOnlyRef.current === effective) return
    onlineComputersOnlyRef.current = effective
    computerFilterEpoch.current += 1
    const epoch = invalidateTarget()
    setOnlineComputersOnlyState(effective)
    if (authRequired) return
    computerFilterLoading.current = true
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    setLoading(true)
    try { await refresh() } catch { /* Refresh exposes the current load error. */ }
    finally { if (targetEpochRef.current === epoch) { computerFilterLoading.current = false; setLoading(false) } }
  }, [authRequired, invalidateTarget, platform, refresh])

  const acceptLiveWorkbench = React.useCallback((
    nextSnapshot: ApplicationState,
    _revision: number,
    activity: SessionLiveActivity[],
  ) => {
    if (computerFilterLoading.current) return
    liveWorkbenchEpochRef.current += 1
    const requested = selectionRef.current
    const selection = chooseSelection(nextSnapshot, requested.workspaceId, requested.sessionId)
    setSnapshot(nextSnapshot)
    setSessionActivity(Object.fromEntries(activity.map(item => [item.session_id, item])))
    replaceSelection(selection.workspaceId, selection.sessionId)
    setError('')
  }, [replaceSelection])

  const acceptLiveActivity = React.useCallback((activity: SessionLiveActivity) => {
    if (computerFilterLoading.current) return
    setSessionActivity(current => ({ ...current, [activity.session_id]: activity }))
  }, [])

  const login = React.useCallback(async (token: string, remember = false) => {
    if (platform) {
      setError('')
      await beginOidcLogin()
      return
    }
    const normalized = token.trim()
    api.setToken(normalized)
    try {
      await api.request(remote ? '/nodes' : '/health')
      if (remote) storeRemoteToken(normalized, remember)
      setToasts([])
      setAuthRequired(false)
      setError('')
    } catch (cause) {
      api.clearToken()
      if (remote) clearRemoteToken()
      throw cause
    }
  }, [platform, remote])

  const logout = React.useCallback(() => {
    authenticationRequestRef.current += 1
    const nativeToken = readNativeToken()
    const browserToken = nativeToken || readOidcToken()
    if (platform && browserToken) void api.request('/auth/logout', { method: 'POST', headers: { authorization: `Bearer ${browserToken}` } }).catch(() => undefined)
    setAccessPaused(false)
    setOidcRegistrationRequired(false)
    api.setRefreshHandler(null)
    invalidateTarget()
    api.clearToken()
    api.clearTenant()
    if (platform) {
      clearOidcSession()
      clearNativeSession()
      setServerIdentity(null)
      retryAuthentication()
    }
    if (remote && !platform) clearRemoteToken()
    localStorage.removeItem(storage.tenant)
    localStorage.removeItem(storage.workspace)
    localStorage.removeItem(storage.session)
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    setTenants([])
    setToasts([])
    setError('')
    replaceTenant(null)
    replaceSelection(null, null)
    setAuthRequired(platform || remote)
  }, [invalidateTarget, platform, remote, replaceSelection, replaceTenant])

  const selectTenant = React.useCallback(async (id: string) => {
    if (!platform || id === tenantRef.current) return
    const targetEpoch = invalidateTarget()
    replaceTenant(id)
    api.setTenant(id)
    replaceSelection(null, null)
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    localStorage.setItem(storage.tenant, id)
    await load(id, targetEpoch)
  }, [invalidateTarget, load, platform, replaceSelection, replaceTenant])

  const createTenant = React.useCallback(async (displayName: string, slug: string) => {
    if (!platform) throw new Error(t('error.tenantPlatformOnly'))
    const response = await api.request<{ tenant: TenantSummary }>('/tenants', {
      method: 'POST',
      body: { display_name: displayName, slug },
    })
    const targetEpoch = invalidateTarget()
    replaceTenant(response.tenant.tenant_id)
    api.setTenant(response.tenant.tenant_id)
    replaceSelection(null, null)
    setSnapshot(emptyState)
    setSessionActivity({})
    setCatalog(null)
    setPresets(emptyPresets)
    localStorage.setItem(storage.tenant, response.tenant.tenant_id)
    await load(response.tenant.tenant_id, targetEpoch)
  }, [invalidateTarget, load, platform, replaceSelection, replaceTenant, t])

  const selectWorkspace = React.useCallback((id: string) => {
    const first = [...snapshot.sessions]
      .filter(session => session.workspace_id === id && session.archived_at_ms == null)
      .sort((a, b) => b.updated_at_ms - a.updated_at_ms)[0]
    replaceSelection(id, first?.identity.session_id ?? null)
  }, [replaceSelection, snapshot.sessions])

  const selectSession = React.useCallback((id: string) => {
    const session = snapshot.sessions.find(item => item.identity.session_id === id && item.archived_at_ms == null)
    if (!session) return
    replaceSelection(
      snapshot.workspaces.some(workspace => workspace.workspace_id === session.workspace_id) ? session.workspace_id : null,
      id,
    )
  }, [replaceSelection, snapshot.sessions, snapshot.workspaces])

  const createSession = React.useCallback(async (workspaceId = currentWorkspaceId ?? undefined, agentPreset?: string) => {
    if (!workspaceId) throw new Error(t('error.workspaceRequired'))
    const placement = snapshot.workspaces.find(workspace => workspace.workspace_id === workspaceId)?.placement
    const selectedPreset = agentPreset ?? (
      await api.request<AgentPresetRoster>(executionTargetPath('/agent-presets', { workspaceId }))
    ).default_id
    const created = await api.request<LocalSession>('/sessions', {
      method: 'POST',
      body: {
        workspace_id: workspaceId,
        agent_preset: selectedPreset,
        permissions: permissionForPlacement(readDefaultPermission(localStorage), placement),
      },
    })
    await refresh()
    replaceSelection(workspaceId, created.identity.session_id)
    return created
  }, [currentWorkspaceId, refresh, replaceSelection, snapshot.workspaces, t])

  const createWorkspace = React.useCallback(async (input: string | PlatformWorkspaceCreateInput) => {
    const response = await api.request<Workspace | { workspace: { workspace_id: string } }>('/workspaces', {
      method: 'POST',
      body: typeof input === 'string' ? { path: input } : input,
    })
    const workspaceId = 'workspace' in response ? response.workspace.workspace_id : response.workspace_id
    const nextSnapshot = await readWorkbenchState()
    setSnapshot(nextSnapshot)
    const created = nextSnapshot.workspaces.find(workspace => workspace.workspace_id === workspaceId)
      ?? ('workspace' in response ? null : response)
    if (!created) throw new Error(t('error.workspaceRefreshMissing'))
    const related = [...nextSnapshot.sessions]
      .filter(session => session.workspace_id === created.workspace_id)
      .sort((left, right) => right.updated_at_ms - left.updated_at_ms)[0]
    replaceSelection(created.workspace_id, related?.identity.session_id ?? null)
    return created
  }, [replaceSelection, t])

  const renameWorkspace = React.useCallback(async (id: string, title: string) => {
    const renamed = await api.request<Workspace>(`/workspaces/${encodeURIComponent(id)}`, {
      method: 'PATCH',
      body: { title },
    })
    const nextSnapshot = await readWorkbenchState()
    setSnapshot(nextSnapshot)
    return renamed
  }, [])

  const unregisterWorkspace = React.useCallback(async (id: string) => {
    await api.request(`/workspaces/${encodeURIComponent(id)}`, { method: 'DELETE' })
    const nextSnapshot = await readWorkbenchState()
    setSnapshot(nextSnapshot)
    const selectedSession = activeSessions(nextSnapshot)
      .find(session => session.identity.session_id === currentSessionId)
    if (selectedSession) {
      replaceSelection(
        nextSnapshot.workspaces.some(workspace => workspace.workspace_id === selectedSession.workspace_id)
          ? selectedSession.workspace_id
          : null,
        selectedSession.identity.session_id,
      )
      return
    }
    const selection = chooseSelection(nextSnapshot, currentWorkspaceId === id ? null : currentWorkspaceId, currentSessionId)
    replaceSelection(selection.workspaceId, selection.sessionId)
  }, [currentSessionId, currentWorkspaceId, replaceSelection])

  const updateSession = React.useCallback(async (id: string, update: Record<string, unknown>) => {
    const updated = await api.request<LocalSession>(`/sessions/${encodeURIComponent(id)}`, { method: 'PATCH', body: update })
    await refresh()
    return updated
  }, [refresh])

  const forkSession = React.useCallback(async (id: string, atSeq?: number) => {
    if (forkOperationRef.current) throw new Error(t('error.sessionForkBusy'))
    replaceForkOperation({ sourceSessionId: id, childSessionId: null, phase: 'creating' })
    try {
      const child = await api.request<LocalSession>(`/sessions/${encodeURIComponent(id)}/fork`, {
        method: 'POST',
        ...(atSeq == null ? {} : { body: { at_seq: atSeq } }),
      })
      const nextSnapshot = await readWorkbenchState()
      setSnapshot(nextSnapshot)
      replaceForkOperation({
        sourceSessionId: id,
        childSessionId: child.identity.session_id,
        phase: 'hydrating',
      })
      replaceSelection(
        nextSnapshot.workspaces.some(workspace => workspace.workspace_id === child.workspace_id) ? child.workspace_id : null,
        child.identity.session_id,
      )
      return child
    } catch (cause) {
      replaceForkOperation(null)
      throw cause
    }
  }, [replaceForkOperation, replaceSelection, t])

  const completeForkHydration = React.useCallback((sessionId: string) => {
    if (forkOperationRef.current?.phase !== 'hydrating'
      || forkOperationRef.current.childSessionId !== sessionId) return
    replaceForkOperation(null)
  }, [replaceForkOperation])

  const archiveSession = React.useCallback(async (id: string) => {
    const archived = await api.request<LocalSession>(`/sessions/${encodeURIComponent(id)}/archive`, { method: 'POST' })
    const nextSnapshot = await readWorkbenchState()
    setSnapshot(nextSnapshot)
    if (id === currentSessionId) {
      const replacement = activeSessions(nextSnapshot)
        .filter(session => session.workspace_id === archived.workspace_id)
        .sort((left, right) => right.updated_at_ms - left.updated_at_ms)[0]
        ?? activeSessions(nextSnapshot).sort((left, right) => right.updated_at_ms - left.updated_at_ms)[0]
      replaceSelection(
        replacement && nextSnapshot.workspaces.some(workspace => workspace.workspace_id === replacement.workspace_id)
          ? replacement.workspace_id
          : null,
        replacement?.identity.session_id ?? null,
      )
    }
    return archived
  }, [currentSessionId, replaceSelection])

  const deleteSession = React.useCallback(async (id: string) => {
    if (id === currentSessionId) replaceSelection(currentWorkspaceId, null)
    setSnapshot(current => ({
      ...current,
      sessions: current.sessions.filter(session => session.identity.session_id !== id),
    }))
    try {
      await api.request(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' })
      await refresh()
    } catch (cause) {
      await refresh()
      throw cause
    }
  }, [currentSessionId, currentWorkspaceId, refresh, replaceSelection])

  const currentWorkspace = snapshot.workspaces.find(item => item.workspace_id === currentWorkspaceId) ?? null
  const currentSession = snapshot.sessions.find(item => item.identity.session_id === currentSessionId && item.archived_at_ms == null) ?? null
  const currentTenantRole = tenants.find(tenant => tenant.tenant_id === currentTenantId)?.role ?? null

  return (
    <WorkbenchContext.Provider value={{
      snapshot,
      sessionActivity,
      catalog,
      presets,
      loading,
      error,
      remote,
      platform,
      authRequired,
      accessPaused,
      oidcRegistrationRequired,
      registerOidcUsername,
      pauseAccess,
      serverAuthConfig,
      serverIdentity,
      accountScope: platform && serverIdentity ? JSON.stringify([serverIdentity.user.user_id, currentTenantId]) : undefined,
      authenticate,
      retryAuthentication,
      acceptInstance,
      tenants,
      currentTenantId,
      currentTenantRole,
      currentWorkspace,
      currentSession,
      currentWorkspaceId,
      currentSessionId,
      forkOperation,
      toasts,
      login,
      logout,
      selectTenant,
      createTenant,
      refresh,
      onlineComputersOnly,
      setOnlineComputersOnly,
      acceptLiveWorkbench,
      acceptLiveActivity,
      selectWorkspace,
      selectSession,
      createSession,
      createWorkspace,
      renameWorkspace,
      unregisterWorkspace,
      updateSession,
      forkSession,
      completeForkHydration,
      archiveSession,
      deleteSession,
      notify,
      dismissToast,
    }}>
      <InputViewerProvider local={!platform && !remote} user={platform ? serverIdentity?.user ?? null : null}>{children}</InputViewerProvider>
    </WorkbenchContext.Provider>
  )
}

export function useWorkbench() {
  const value = React.useContext(WorkbenchContext)
  if (!value) throw new Error('useWorkbench must be used inside WorkbenchProvider')
  return value
}

export { storage }
