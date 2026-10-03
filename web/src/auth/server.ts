import { api, ApiError } from '@/api/client'

export type InstanceMode = 'single_user' | 'multi_user'
export type PlatformRole = 'owner' | 'admin' | 'operator' | 'auditor' | 'user'

export interface RegistrationSettings {
  mode: 'open' | 'invite'
  require_approval: boolean
  revision: number
}

export interface RegistrationResult {
  status: 'active' | 'pending'
  user_id: string
  session: NativeSession | null
}

export interface ServerAuthConfig {
  initialized: boolean
  mode: InstanceMode
  native_enabled: boolean
  oidc_enabled: boolean
  email_enabled?: boolean
  registration: RegistrationSettings
  turnstile?: { site_key: string }
  oidc?: {
    authorization_endpoint: string
    client_id: string
    redirect_uri: string
    scope: string
  }
}

export interface ServerInstance {
  managed_execution_enabled: boolean
  mode: InstanceMode
  owner_user_id: string
  revision: number
}

export interface ServerIdentity {
  email: string | null
  user: { user_id: string; username: string }
  instance: ServerInstance
  is_instance_owner: boolean
  platform_role: PlatformRole
  personal_tenant_id: string
  personal_project_id: string
  expires_at_ms?: number | null
}

export interface NativeSession extends ServerIdentity {
  access_token: string
  expires_at_ms: number
}

export type NativeLoginInput = (
  | { action: 'login'; username: string; password: string }
  | { action: 'register'; username: string; email: string; password: string }
  | { action: 'setup'; setup_token: string; username: string; email: string; password: string }
  | { action: 'accept'; token: string; username: string; email: string; password: string }
) & { turnstile_token?: string }

export interface ServerInvitation {
  invitation_id: string
  token: string
  expires_at_ms: number
  tenant_id: string | null
  role: 'viewer' | 'member' | 'admin'
}

export const NATIVE_SESSION_KEY = 'ternilo.native.session'

export function isServerAccessPaused(error: { code: string; message: string }) {
  return error.code === 'policy_denied'
    && error.message === 'this account is paused while the server is in single-user mode'
}

export function isOidcUsernameRequired(error: { code: string; message: string }) {
  return error.code === 'policy_denied'
    && error.message === 'choose a platform username to finish registration'
}

export async function publicRequest<T>(path: string, body?: unknown, bearer?: string): Promise<T> {
  const response = await fetch(path, body === undefined ? undefined : {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(bearer ? { authorization: `Bearer ${bearer}` } : {}) },
    body: JSON.stringify(body),
  })
  const payload = await response.json().catch(() => ({}))
  if (!response.ok) throw new ApiError(
    payload.error?.message ?? `HTTP ${response.status}`,
    response.status,
    payload.error?.code ?? `http_${response.status}`,
  )
  return payload as T
}

export function loadServerAuthConfig() {
  return publicRequest<ServerAuthConfig>('/auth/config')
}

export function signInNative(input: Exclude<NativeLoginInput, { action: 'register' }>) {
  const { action, ...body } = input
  return publicRequest<NativeSession>(`/api/v1/auth/${action === 'accept' ? 'invitations/accept' : action}`, body)
}

export function registerNative(input: Extract<NativeLoginInput, { action: 'register' }>) {
  const { action: _, ...body } = input
  return publicRequest<RegistrationResult>('/api/v1/auth/register', body)
}

export function registerOidcAccount(username: string, email: string, bearer: string, turnstileToken?: string) {
  return publicRequest<Pick<RegistrationResult, 'status' | 'user_id'>>('/api/v1/auth/oidc/register', { username, email, ...(turnstileToken ? { turnstile_token: turnstileToken } : {}) }, bearer)
}

export function loadServerIdentity() {
  return api.request<ServerIdentity>('/auth/session')
}

export function storeNativeSession(session: NativeSession) {
  sessionStorage.setItem(NATIVE_SESSION_KEY, JSON.stringify({
    access_token: session.access_token,
    expires_at_ms: session.expires_at_ms,
  }))
}

export function readNativeToken(): string {
  try {
    const session = JSON.parse(sessionStorage.getItem(NATIVE_SESSION_KEY) ?? 'null') as NativeSession | null
    if (session?.access_token && session.expires_at_ms > Date.now()) return session.access_token
  } catch { /* Discard incomplete browser storage. */ }
  clearNativeSession()
  return ''
}

export function clearNativeSession() {
  sessionStorage.removeItem(NATIVE_SESSION_KEY)
}

export function readAccountLink() {
  const parameters = new URLSearchParams(location.hash.slice(1))
  return {
    setupToken: parameters.get('setup_token') ?? '',
    invitationToken: parameters.get('invite') ?? '',
    teamInvitationToken: parameters.get('team_invite') ?? '',
  }
}

export function clearAccountLink() {
  const link = readAccountLink()
  if (link.setupToken || link.invitationToken || link.teamInvitationToken) {
    history.replaceState({}, '', `${location.pathname}${location.search}`)
  }
}

export function invitationUrl(token: string, team = false) {
  return `${location.origin}/#${new URLSearchParams({ [team ? 'team_invite' : 'invite']: token })}`
}
