import { api } from '@/api/client'
import type { PlatformRole, RegistrationSettings } from '@/auth/server'

export type AccountStatus = 'active' | 'pending' | 'rejected' | 'banned' | 'removed'

export interface PlatformAccount {
  user_id: string
  username: string
  email: string | null
  platform_role: PlatformRole
  role_revision: number
  status: AccountStatus
  status_revision: number
  created_at_ms: number
  personal_tenant_id: string
}

export interface AccountPage {
  accounts: PlatformAccount[]
  next_cursor: string | null
}

export function listAccounts(input: { query: string; role: PlatformRole | ''; status?: AccountStatus | ''; cursor: string | null }, signal?: AbortSignal) {
  const query = new URLSearchParams({ limit: '25' })
  if (input.query) query.set('query', input.query)
  if (input.role) query.set('role', input.role)
  if (input.status) query.set('status', input.status)
  if (input.cursor) query.set('cursor', input.cursor)
  return api.request<AccountPage>(`/admin/accounts?${query}`, { signal })
}

export function getRegistrationSettings(signal?: AbortSignal) {
  return api.request<RegistrationSettings>('/admin/registration', { signal })
}

export function setRegistrationSettings(settings: RegistrationSettings) {
  return api.request<RegistrationSettings>('/admin/registration', { method: 'PATCH', body: settings })
}

export function reviewAccount(account: PlatformAccount, decision: 'approve' | 'reject') {
  return api.request<PlatformAccount>(`/admin/accounts/${encodeURIComponent(account.user_id)}/review`, {
    method: 'POST', body: { decision, status_revision: account.status_revision },
  })
}

export type AccountStatusAction = 'ban' | 'unban' | 'remove'

export function setAccountStatus(account: PlatformAccount, action: AccountStatusAction) {
  return api.request<PlatformAccount>(`/admin/accounts/${encodeURIComponent(account.user_id)}/status`, {
    method: 'POST', body: { action, status_revision: account.status_revision },
  })
}

export function setAccountRole(account: PlatformAccount, role: Exclude<PlatformRole, 'owner'>) {
  return api.request<PlatformAccount>(`/admin/accounts/${encodeURIComponent(account.user_id)}/role`, {
    method: 'PATCH', body: { role, role_revision: account.role_revision },
  })
}

export interface ExecutionStatus {
  claims_paused: boolean
  active_runs: number
  active_commands: number
}

export function getExecutionStatus(signal?: AbortSignal) {
  return api.request<ExecutionStatus>('/admin/execution', { signal })
}

export function setExecutionPaused(claimsPaused: boolean) {
  return api.request<ExecutionStatus>('/admin/execution', { method: 'PATCH', body: { claims_paused: claimsPaused } })
}

export function canReadAccounts(role?: PlatformRole) {
  return role === 'owner' || role === 'admin' || role === 'auditor'
}

export function canManageWorkers(role?: PlatformRole) {
  return role === 'owner' || role === 'admin' || role === 'operator'
}

export function isPlatformStaff(role?: PlatformRole) {
  return role === 'owner' || role === 'admin' || role === 'operator' || role === 'auditor'
}

export interface AccountNodeCleanup {
  tenant_id: string
  executor_id: string
  request: {
    request_id: string
    status_revision: number
    created_at_ms: number
    state: 'pending' | 'confirmed'
    detail: string | null
    confirmed_at_ms: number | null
  }
}

export function getAccountNodeCleanup(userId: string, signal?: AbortSignal) {
  return api.request<AccountNodeCleanup[]>(`/admin/accounts/${encodeURIComponent(userId)}/node-cleanup`, { signal })
}
