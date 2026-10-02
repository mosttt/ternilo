import { api } from '@/api/client'

export type ServiceScope = 'resource.read' | 'run.execute'
export interface ServiceAccount {
  service_account_id: string
  tenant_id: string
  name: string
  notes: string
  enabled: boolean
  revision: number
  created_by: string
  created_at_ms: number
}
export interface ServiceCredential {
  credential_id: string
  name: string
  scopes: ServiceScope[]
  issued_at_ms: number
  expires_at_ms: number
  last_used_at_ms: number | null
  revoked_at_ms: number | null
}
const accounts = (tenant: string) => `/tenants/${encodeURIComponent(tenant)}/service-accounts`
const credentials = (tenant: string, id: string) => `${accounts(tenant)}/${encodeURIComponent(id)}/credentials`
export async function listServiceAccounts(tenant: string, signal: AbortSignal) {
  return (await api.request<{ service_accounts: ServiceAccount[] }>(accounts(tenant), { signal })).service_accounts
}
export async function createServiceAccount(tenant: string, body: { name: string; notes: string }) {
  return (await api.request<{ service_account: ServiceAccount }>(accounts(tenant), { method: 'POST', body })).service_account
}
export async function updateServiceAccount(tenant: string, id: string, body: { name: string; notes: string; enabled: boolean; expected_revision: number }) {
  return (await api.request<{ service_account: ServiceAccount }>(`${accounts(tenant)}/${encodeURIComponent(id)}`, { method: 'PATCH', body })).service_account
}
export async function listServiceCredentials(tenant: string, id: string, signal: AbortSignal) {
  return (await api.request<{ credentials: ServiceCredential[] }>(credentials(tenant, id), { signal })).credentials
}
export function issueServiceCredential(tenant: string, id: string, body: { name: string; scopes: ServiceScope[]; expires_at_ms: number }) {
  return api.request<{ credential: ServiceCredential; access_token: string }>(credentials(tenant, id), { method: 'POST', body })
}
export function revokeServiceCredential(tenant: string, id: string, credential: string) {
  return api.request<void>(`${credentials(tenant, id)}/${encodeURIComponent(credential)}`, { method: 'DELETE' })
}

export interface ServiceWorkspace {
  workspace_id: string
  name: string
  placement: 'cloud' | 'local_node'
  computer_name: string | null
  permissions: import('@/types').ResourcePermissions | null
}
export interface ServiceWorkspacePage { workspaces: ServiceWorkspace[]; next_cursor: string | null }
export function listServiceWorkspaces(tenant: string, id: string, query: string, cursor: string | null, signal: AbortSignal) {
  const parameters = new URLSearchParams({ query, limit: '25' })
  if (cursor) parameters.set('cursor', cursor)
  return api.request<ServiceWorkspacePage>(`${accounts(tenant)}/${encodeURIComponent(id)}/workspaces?${parameters}`, { signal })
}
export function setServiceWorkspaceAccess(tenant: string, id: string, workspace: ServiceWorkspace, permissions: ServiceWorkspace['permissions']) {
  return api.request<void>(`${accounts(tenant)}/${encodeURIComponent(id)}/workspaces/${encodeURIComponent(workspace.workspace_id)}`, { method: 'PUT', body: { permissions, expected_permissions: workspace.permissions } })
}
