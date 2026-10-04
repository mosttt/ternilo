import { loadServerAuthConfig, readNativeToken } from './server'

interface BrowserTokens {
  provider_id: string
  access_token: string
  expires_in: number
  refresh_token?: string
}

const failureKeys = {
  provider_denied: 'error.oidcProviderDenied',
  state_mismatch: 'error.oidcStateMismatch',
  expired: 'error.oidcExpired',
  secure_context_required: 'error.oidcSecureContext',
  origin_mismatch: 'error.oidcOriginMismatch',
  mfa_required: 'error.oidcMfaRequired',
} as const

export type OidcFailure = keyof typeof failureKeys

/** Machine-readable flow failures are localized at the React application shell. */
export class OidcFlowError extends Error {
  constructor(readonly failure: OidcFailure) {
    super(failure)
    this.name = 'OidcFlowError'
  }

  get translationKey() { return failureKeys[this.failure] }
}

let sessionRevision = 0

const keys = {
  access: 'ternilo.oidc.access',
  refresh: 'ternilo.oidc.refresh',
  expires: 'ternilo.oidc.expires',
  verifier: 'ternilo.oidc.verifier',
  state: 'ternilo.oidc.state',
  nonce: 'ternilo.oidc.nonce',
  linkNative: 'ternilo.oidc.link-native',
  returnPath: 'ternilo.oidc.return-path',
  mfa: 'ternilo.oidc.mfa',
  provider: 'ternilo.oidc.provider',
  attemptProvider: 'ternilo.oidc.attempt-provider',
  invitation: 'ternilo.oidc.invitation',
}

async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init)
  const body = await response.json().catch(() => ({})) as T & { error?: { message?: string } }
  if (!response.ok) throw new Error(body.error?.message ?? `HTTP ${response.status}`)
  return body
}

function randomBase64Url(bytes: number) {
  const value = crypto.getRandomValues(new Uint8Array(bytes))
  let binary = ''
  value.forEach(byte => { binary += String.fromCharCode(byte) })
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}

function base64Url(value: Uint8Array) {
  let binary = ''
  value.forEach(byte => { binary += String.fromCharCode(byte) })
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}

function storeTokens(tokens: BrowserTokens, fallbackRefresh = '') {
  sessionStorage.setItem(keys.provider, tokens.provider_id)
  sessionStorage.setItem(keys.access, tokens.access_token)
  const refresh = tokens.refresh_token || fallbackRefresh
  if (refresh) sessionStorage.setItem(keys.refresh, refresh)
  else sessionStorage.removeItem(keys.refresh)
  sessionStorage.setItem(keys.expires, String(Date.now() + tokens.expires_in * 1_000))
}

const returnPaths = new Set(['/auth/verify-email', '/', '/admin', '/admin/accounts', '/admin/workers', '/admin/instance', '/admin/models', '/models', '/spaces/current', '/files'])

function applicationReturnPath(value: string | null) {
  if (value?.startsWith('/files?')) {
    const query = new URLSearchParams(value.slice('/files?'.length).split('#', 1)[0])
    const filters = new URLSearchParams()
    for (const key of ['workspace_id', 'session_id', 'kind', 'query']) {
      const selected = query.get(key)
      if (selected) filters.set(key, selected)
    }
    return `/files${filters.size ? `?${filters}` : ''}`
  }
  return value && returnPaths.has(value) ? value : '/'
}

function restoreApplicationPath(path: string) {
  history.replaceState({}, '', path)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

function clearAttempt() {
  sessionStorage.removeItem(keys.verifier)
  sessionStorage.removeItem(keys.state)
  sessionStorage.removeItem(keys.nonce)
  sessionStorage.removeItem(keys.returnPath)
  sessionStorage.removeItem(keys.attemptProvider)
}

function isCallback() {
  const parameters = new URLSearchParams(location.search)
  return location.pathname === '/auth/callback' || parameters.has('code') || parameters.has('error')
}

async function finishCallback(accept: (tokens: BrowserTokens) => void | Promise<void>, allowMfa = true) {
  if (!isCallback()) return false
  const parameters = new URLSearchParams(location.search)
  const expectedState = sessionStorage.getItem(keys.state)
  const verifier = sessionStorage.getItem(keys.verifier)
  const nonce = sessionStorage.getItem(keys.nonce)
  const providerId = sessionStorage.getItem(keys.attemptProvider)
  const revision = sessionRevision
  const returnPath = applicationReturnPath(sessionStorage.getItem(keys.returnPath))
  try {
    if (parameters.has('error')) throw new OidcFlowError('provider_denied')
    const code = parameters.get('code')
    if (!code || !verifier || !nonce || !providerId || !expectedState || parameters.get('state') !== expectedState) {
      throw new OidcFlowError('state_mismatch')
    }
    const tokens = await fetchJson<BrowserTokens | PendingOidcMfa>('/auth/token', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ provider_id: providerId, code, code_verifier: verifier, nonce }),
    })
    if (revision !== sessionRevision) throw new OidcFlowError('expired')
    if ('mfa_challenge' in tokens) {
      if (!allowMfa) throw new OidcFlowError('mfa_required')
      sessionStorage.setItem(keys.mfa, JSON.stringify({ ...tokens, provider_id: providerId }))
    } else await accept({ ...tokens, provider_id: providerId })
    return true
  } finally {
    clearAttempt()
    restoreApplicationPath(returnPath)
  }
}

export async function completeOidcLink() {
  const nativeToken = sessionStorage.getItem(keys.linkNative)
  if (!nativeToken || !isCallback()) return false
  const returnPath = applicationReturnPath(sessionStorage.getItem(keys.returnPath))
  try {
    if (nativeToken !== readNativeToken()) throw new OidcFlowError('expired')
    return await finishCallback(async tokens => {
      if (nativeToken !== readNativeToken()) throw new OidcFlowError('expired')
      // Authenticate the existing account without provisioning a separate OIDC user.
      await fetchJson('/api/v1/auth/oidc-link', {
        method: 'POST',
        headers: { 'content-type': 'application/json', authorization: `Bearer ${nativeToken}` },
        body: JSON.stringify({ access_token: tokens.access_token }),
      })
      if (nativeToken !== readNativeToken()) throw new OidcFlowError('expired')
    }, false)
  } finally {
    sessionStorage.removeItem(keys.linkNative)
    clearAttempt()
    restoreApplicationPath(returnPath)
  }
}

export interface PendingOidcMfa { mfa_challenge: string; expires_at_ms: number; provider_id: string }
export function readPendingOidcMfa(): PendingOidcMfa | null {
  try {
    const value = JSON.parse(sessionStorage.getItem(keys.mfa) ?? 'null')
    if (typeof value?.mfa_challenge === 'string' && value.mfa_challenge.startsWith('ter_mc_') && value.expires_at_ms > Date.now()) return value
  } catch { /* Discard malformed pending authentication. */ }
  sessionStorage.removeItem(keys.mfa)
  return null
}
export async function completeOidcMfa(code: string) {
  const pending = readPendingOidcMfa(), revision = sessionRevision
  if (!pending) throw new OidcFlowError('expired')
  const tokens = await fetchJson<BrowserTokens>('/auth/mfa', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ provider_id: pending.provider_id, challenge: pending.mfa_challenge, code }) })
  if (revision !== sessionRevision || readPendingOidcMfa()?.mfa_challenge !== pending.mfa_challenge) throw new OidcFlowError('expired')
  sessionStorage.removeItem(keys.mfa)
  storeTokens({ ...tokens, provider_id: pending.provider_id })
}

export async function initializeOidcSession() {
  await finishCallback(tokens => storeTokens(tokens))
  if (readPendingOidcMfa()) return ''
  const access = sessionStorage.getItem(keys.access) ?? ''
  const expires = Number(sessionStorage.getItem(keys.expires) ?? 0)
  if (access && expires > Date.now() + 30_000) return access
  if (sessionStorage.getItem(keys.refresh)) {
    try { return await refreshOidcSession() } catch { clearOidcSession() }
  }
  return ''
}

export async function refreshOidcSession() {
  const revision = sessionRevision
  const refresh = sessionStorage.getItem(keys.refresh)
  const providerId = sessionStorage.getItem(keys.provider)
  if (!refresh || !providerId) throw new OidcFlowError('expired')
  const tokens = await fetchJson<BrowserTokens>('/auth/refresh', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ provider_id: providerId, refresh_token: refresh }),
  })
  if (revision !== sessionRevision) throw new OidcFlowError('expired')
  storeTokens({ ...tokens, provider_id: providerId }, refresh)
  return tokens.access_token
}

export function beginOidcLogin(providerId: string, invitationToken?: string) {
  return beginOidcFlow(providerId, undefined, invitationToken)
}

export function beginOidcLink(providerId: string) {
  const token = readNativeToken()
  if (!token) throw new OidcFlowError('expired')
  return beginOidcFlow(providerId, token)
}

async function beginOidcFlow(providerId: string, nativeToken?: string, invitationToken?: string) {
  if (!globalThis.crypto?.subtle) throw new OidcFlowError('secure_context_required')
  const revision = sessionRevision
  const returnPath = applicationReturnPath(location.pathname === '/files' ? `${location.pathname}${location.search}` : location.pathname)
  const { oidc_providers: providers } = await loadServerAuthConfig()
  const config = providers.find(provider => provider.id === providerId)?.config
  if (!config) throw new OidcFlowError('provider_denied')
  if (new URL(config.redirect_uri).origin !== location.origin) throw new OidcFlowError('origin_mismatch')
  const verifier = randomBase64Url(64)
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier))
  const state = randomBase64Url(24)
  const nonce = randomBase64Url(24)
  if (revision !== sessionRevision) throw new OidcFlowError('expired')
  if (nativeToken && nativeToken !== readNativeToken()) throw new OidcFlowError('expired')
  clearOidcSession()
  sessionStorage.setItem(keys.attemptProvider, providerId)
  if (invitationToken) sessionStorage.setItem(keys.invitation, invitationToken)
  if (nativeToken) {
    sessionStorage.setItem(keys.linkNative, nativeToken)
  }
  sessionStorage.setItem(keys.returnPath, returnPath)
  sessionStorage.setItem(keys.verifier, verifier)
  sessionStorage.setItem(keys.state, state)
  sessionStorage.setItem(keys.nonce, nonce)
  const url = new URL(config.authorization_endpoint)
  url.search = new URLSearchParams({
    response_type: 'code',
    client_id: config.client_id,
    redirect_uri: config.redirect_uri,
    scope: config.scope,
    state,
    nonce,
    code_challenge: base64Url(new Uint8Array(digest)),
    code_challenge_method: 'S256',
  }).toString()
  location.assign(url)
}

export function clearOidcSession() {
  sessionRevision += 1
  Object.values(keys).forEach(key => sessionStorage.removeItem(key))
}

export function readOidcToken() {
  return sessionStorage.getItem(keys.access) ?? ''
}

export function readOidcInvitation() { return sessionStorage.getItem(keys.invitation) ?? '' }
export function clearOidcInvitation() { sessionStorage.removeItem(keys.invitation) }
