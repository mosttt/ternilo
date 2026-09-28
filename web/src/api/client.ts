export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
  ) {
    super(message)
    this.name = 'ApiError'
  }
}

type RequestOptions = Omit<RequestInit, 'body'> & { body?: unknown }

export class ApiClient {
  private token: string
  private accountRevision = 0
  private scopeRevision = 0
  private tokenRevision = 0
  private credentialsRevision = 0
  private tenantId = ''
  private refreshHandler: (() => Promise<string>) | null = null
  private refreshPromise: Promise<string> | null = null
  private unauthorizedListeners = new Set<() => void>()
  private forbiddenListeners = new Set<(error: ApiError) => void>()
  private credentialsListeners = new Set<() => void>()

  constructor(token = '') {
    this.token = token
  }

  setToken(token: string) {
    this.accountRevision += 1
    this.scopeRevision += 1
    this.replaceToken(token)
  }

  private replaceToken(token: string) {
    const next = token.trim()
    const changed = next !== this.token
    this.token = next
    this.tokenRevision += 1
    if (changed) this.notifyCredentialsChanged()
  }

  clearToken() {
    this.accountRevision += 1
    this.scopeRevision += 1
    const changed = this.token !== ''
    this.token = ''
    this.tokenRevision += 1
    if (changed) this.notifyCredentialsChanged()
  }

  hasToken() {
    return Boolean(this.token)
  }

  liveCredentials() {
    return {
      bearerToken: this.token || undefined,
      tenantId: this.tenantId || undefined,
      revision: this.credentialsRevision,
    }
  }

  setTenant(tenantId: string) {
    const next = tenantId.trim()
    if (next === this.tenantId) return
    this.scopeRevision += 1
    this.tenantId = next
    this.notifyCredentialsChanged()
  }

  clearTenant() {
    if (!this.tenantId) return
    this.scopeRevision += 1
    this.tenantId = ''
    this.notifyCredentialsChanged()
  }

  setRefreshHandler(handler: (() => Promise<string>) | null) {
    if (handler !== this.refreshHandler) this.refreshPromise = null
    this.refreshHandler = handler
  }

  onUnauthorized(listener: () => void) {
    this.unauthorizedListeners.add(listener)
    return () => { this.unauthorizedListeners.delete(listener) }
  }

  onForbidden(listener: (error: ApiError) => void) {
    this.forbiddenListeners.add(listener)
    return () => { this.forbiddenListeners.delete(listener) }
  }

  onCredentialsChanged(listener: () => void) {
    this.credentialsListeners.add(listener)
    return () => { this.credentialsListeners.delete(listener) }
  }

  private notifyCredentialsChanged() {
    this.credentialsRevision += 1
    this.credentialsListeners.forEach(listener => listener())
  }

  async request<T>(path: string, options: RequestOptions = {}): Promise<T> {
    return this.perform<T>(path, options, false)
  }

  private async perform<T>(path: string, options: RequestOptions, retried: boolean): Promise<T> {
    const tokenRevision = this.tokenRevision
    const accountRevision = this.accountRevision
    const platformRequest = path.startsWith('/admin/') || path.startsWith('/model-access/')
    const scopeRevision = this.scopeRevision
    const ensureCurrentScope = () => {
      if (accountRevision !== this.accountRevision || (!platformRequest && scopeRevision !== this.scopeRevision)) throw new ApiError(
        'Request belongs to a previous account or space', 409, 'request_scope_changed',
      )
    }
    const { body, headers, ...request } = options
    const response = await fetch(`/api/v1${path}`, {
      ...request,
      body: body === undefined ? undefined : JSON.stringify(body),
      headers: {
        ...(this.token ? { authorization: `Bearer ${this.token}` } : {}),
        ...(this.tenantId && !platformRequest ? { 'x-ternilo-tenant': this.tenantId } : {}),
        ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        ...headers,
      },
    })
    ensureCurrentScope()
    if (response.status === 204) {
      await response.arrayBuffer()
      ensureCurrentScope()
      return undefined as T
    }
    if (response.status === 401 && !retried && this.refreshHandler) {
      if (tokenRevision !== this.tokenRevision) {
        if (this.token) return this.perform<T>(path, options, true)
      } else {
        try {
          await this.refreshAccessToken()
          ensureCurrentScope()
          return this.perform<T>(path, options, true)
        } catch {
          ensureCurrentScope()
        }
      }
    }
    const payload = await response.json().catch(() => ({})) as {
      error?: { message?: string; code?: string }
    }
    ensureCurrentScope()
    if (!response.ok) {
      if (response.status === 401) {
        this.clearToken()
        this.unauthorizedListeners.forEach(listener => listener())
      }
      const error = new ApiError(
        payload.error?.message ?? `HTTP ${response.status}`,
        response.status,
        payload.error?.code ?? `http_${response.status}`,
      )
      if (response.status === 403) this.forbiddenListeners.forEach(listener => listener(error))
      throw error
    }
    return payload as T
  }

  private refreshAccessToken() {
    if (!this.refreshPromise) {
      const handler = this.refreshHandler
      if (!handler) return Promise.reject(new Error('token refresh is not configured'))
      const accountRevision = this.accountRevision
      const pending = handler().then(token => {
        if (accountRevision !== this.accountRevision) throw new ApiError(
          'Authentication changed during token refresh', 409, 'request_scope_changed',
        )
        this.replaceToken(token)
        return token
      })
      let wrapped: Promise<string>
      wrapped = pending.finally(() => {
        if (this.refreshPromise === wrapped) this.refreshPromise = null
      })
      this.refreshPromise = wrapped
    }
    return this.refreshPromise
  }
}

export const api = new ApiClient(window.__TERNILO_BOOT__?.apiToken ?? '')
