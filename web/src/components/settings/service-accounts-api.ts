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
