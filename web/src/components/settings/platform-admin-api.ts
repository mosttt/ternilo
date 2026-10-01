import { api } from '@/api/client'
import type {
  ComputerDetailsResponse,
  ComputerManagement,
  AuditEntry,
  EnrollmentGrant,
  ManagedExecutionTarget,
  MemberPage,
  GroupPage,
  GroupRecord,
  ModelUsageReport,
  NodeCredentialGrant,
  ProjectRecord,
  TenantQuota,
  TenantRole,
} from '@/types'

function tenantResource(tenantId: string, resource: string) {
  return `/tenants/${encodeURIComponent(tenantId)}/${resource}`
}

export interface PageQuery {
  query?: string
  cursor?: string | null
  limit?: number
}

export function pageQuery(input: PageQuery = {}) {
  const query = new URLSearchParams({ limit: String(input.limit ?? 25) })
  if (input.query) query.set('query', input.query)
  if (input.cursor) query.set('cursor', input.cursor)
  return query.toString()
}

export function listMemberships(tenantId: string, input: PageQuery = {}, signal?: AbortSignal) {
  return api.request<MemberPage>(`${tenantResource(tenantId, 'members')}?${pageQuery(input)}`, { signal })
}

export function listGroups(tenantId: string, input: PageQuery = {}, signal?: AbortSignal) {
  return api.request<GroupPage>(`${tenantResource(tenantId, 'groups')}?${pageQuery(input)}`, { signal })
}

export function createGroup(tenantId: string, input: { name: string; description: string | null }) {
  return api.request<GroupRecord>(tenantResource(tenantId, 'groups'), { method: 'POST', body: input })
}

export function getGroup(tenantId: string, groupId: string) {
  return api.request<GroupRecord>(`${tenantResource(tenantId, 'groups')}/${encodeURIComponent(groupId)}`)
}

export function updateGroup(tenantId: string, groupId: string, input: { name: string; description: string | null }) {
  return api.request<GroupRecord>(`${tenantResource(tenantId, 'groups')}/${encodeURIComponent(groupId)}`, { method: 'PATCH', body: input })
}

export function removeGroup(tenantId: string, groupId: string) {
  return api.request<void>(`${tenantResource(tenantId, 'groups')}/${encodeURIComponent(groupId)}`, { method: 'DELETE' })
}

export function listGroupMembers(tenantId: string, groupId: string, input: PageQuery = {}, signal?: AbortSignal) {
  return api.request<MemberPage>(`${tenantResource(tenantId, 'groups')}/${encodeURIComponent(groupId)}/members?${pageQuery(input)}`, { signal })
}

export function setGroupMember(tenantId: string, groupId: string, userId: string, included: boolean) {
  return api.request<void>(`${tenantResource(tenantId, 'groups')}/${encodeURIComponent(groupId)}/members/${encodeURIComponent(userId)}`, { method: included ? 'PUT' : 'DELETE' })
}

export async function setMembership(tenantId: string, userId: string, role: TenantRole) {
  await api.request<void>(
    `${tenantResource(tenantId, 'members')}/${encodeURIComponent(userId)}`,
    { method: 'PUT', body: { role } },
  )
}

export async function removeMembership(tenantId: string, userId: string) {
  await api.request<void>(
    `${tenantResource(tenantId, 'members')}/${encodeURIComponent(userId)}`,
    { method: 'DELETE' },
  )
}

export async function listManagedComputers(tenantId: string) {
  const response = await api.request<{ executors: ManagedExecutionTarget[] }>(
    tenantResource(tenantId, 'executors'),
  )
  return response.executors
}

export async function listOwnedComputers(tenantId: string) {
  const response = await api.request<{ executors: ManagedExecutionTarget[] }>(
    tenantResource(tenantId, 'my-computers'),
  )
  return response.executors
}

export async function revokeComputer(tenantId: string, executorId: string) {
  await api.request<void>(
    `${tenantResource(tenantId, 'executors')}/${encodeURIComponent(executorId)}`,
    { method: 'DELETE' },
  )
}

export async function revokeOwnedComputer(tenantId: string, executorId: string) {
  await api.request<void>(
    `${tenantResource(tenantId, 'my-computers')}/${encodeURIComponent(executorId)}`,
    { method: 'DELETE' },
  )
}

function computerResource(tenantId: string, executorId: string, scope: 'owned' | 'managed') {
  return `${tenantResource(tenantId, scope === 'owned' ? 'my-computers' : 'executors')}/${encodeURIComponent(executorId)}`
}

export function getComputerDetails(tenantId: string, executorId: string, scope: 'owned' | 'managed', signal?: AbortSignal) {
  return api.request<ComputerDetailsResponse>(computerResource(tenantId, executorId, scope), { signal })
}

export function updateComputer(tenantId: string, executorId: string, scope: 'owned' | 'managed', body: { display_name: string | null; notes: string; expected_revision: number }) {
  return api.request<{ management: ComputerManagement }>(computerResource(tenantId, executorId, scope), { method: 'PATCH', body })
}

export function setComputerSuspended(tenantId: string, executorId: string, scope: 'owned' | 'managed', body: { suspended: boolean; expected_revision: number }) {
  return api.request<{ management: ComputerManagement }>(`${computerResource(tenantId, executorId, scope)}/suspension`, { method: 'PUT', body })
}

export function removeComputerRegistration(tenantId: string, executorId: string, scope: 'owned' | 'managed', expectedRevision: number) {
  return api.request<void>(`${computerResource(tenantId, executorId, scope)}/registration`, { method: 'DELETE', body: { expected_revision: expectedRevision } })
}

export async function getTenantQuota(tenantId: string) {
  const response = await api.request<{ quota: TenantQuota }>(
    tenantResource(tenantId, 'quota'),
  )
  return response.quota
}

export async function updateTenantQuota(tenantId: string, quota: TenantQuota) {
  await api.request<void>(tenantResource(tenantId, 'quota'), {
    method: 'PUT',
    body: quota,
  })
}

export async function getTenantModelUsage(tenantId: string, period: string, limit = 200) {
  const query = new URLSearchParams({ period, limit: String(limit) })
  return api.request<ModelUsageReport>(
    `${tenantResource(tenantId, 'model-usage')}?${query}`,
  )
}

export async function listTenantAudit(tenantId: string, limit = 200) {
  const query = new URLSearchParams({ limit: String(limit) })
  const response = await api.request<{ entries: AuditEntry[] }>(
    `${tenantResource(tenantId, 'audit')}?${query}`,
  )
  return response.entries
}

export async function listPlatformProjects() {
  const response = await api.request<{ projects: ProjectRecord[] }>('/projects')
  return response.projects
}

export interface NodeLaunchCommand {
  command: string
  executorId: string
}

function commandArgument(value: string) {
  // These generated arguments need no shell-specific interpolation or escaping.
  if (!/^[\p{L}\p{N}._:/\[\]-]+$/u.test(value)) throw new Error('Node launch arguments contain unsupported characters')
  return `"${value}"`
}

export function nodeLaunchCommand(
  origin: string,
  executorId: string,
  credentialToken: string,
) {
  const gateway = new URL('/api/v1/executors/connect', origin)
  gateway.protocol = gateway.protocol === 'https:' ? 'wss:' : 'ws:'
  const insecure = gateway.protocol === 'ws:' ? ' --allow-insecure-gateway' : ''
  return `ternilo serve --gateway-url ${commandArgument(gateway.toString())} --node-id=${commandArgument(executorId)} --token ${commandArgument(credentialToken)}${insecure}`
}

/**
 * Creates and immediately consumes the one-time enrollment. Only the resulting
 * long-lived, revocable credential is returned in a launch command held by the UI.
 */
export async function createNodeLaunch(
  tenantId: string,
  input: { executorId: string; projectId?: string; origin?: string },
): Promise<NodeLaunchCommand> {
  return createNodeLaunchForResource(tenantId, 'enrollments', input)
}

export async function createOwnedNodeLaunch(
  tenantId: string,
  input: { executorId: string; projectId?: string; origin?: string },
): Promise<NodeLaunchCommand> {
  return createNodeLaunchForResource(tenantId, 'my-computer-enrollments', input)
}

async function createNodeLaunchForResource(
  tenantId: string,
  enrollmentResource: 'enrollments' | 'my-computer-enrollments',
  input: { executorId: string; projectId?: string; origin?: string },
): Promise<NodeLaunchCommand> {
  commandArgument(input.executorId)
  const created = await api.request<{ enrollment: EnrollmentGrant }>(
    tenantResource(tenantId, enrollmentResource),
    {
      method: 'POST',
      body: {
        executor_id: input.executorId,
        project_id: input.projectId || null,
        ttl_seconds: 600,
      },
    },
  )
  const consumed = await api.request<{ credential: NodeCredentialGrant }>(
    '/enrollments/consume',
    {
      method: 'POST',
      body: {
        token: created.enrollment.token,
      },
    },
  )
  return {
    command: nodeLaunchCommand(
      input.origin ?? window.location.origin,
      input.executorId,
      consumed.credential.token,
    ),
    executorId: input.executorId,
  }
}
