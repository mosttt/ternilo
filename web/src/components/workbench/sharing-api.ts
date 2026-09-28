import { api } from '@/api/client'
import { pageQuery, type PageQuery } from '@/components/settings/platform-admin-api'
import type { ResourceAccess, ResourcePermissions, ResourceShare, ShareSubject } from '@/types'

export interface SharingTarget {
  kind: 'project' | 'workspace' | 'session'
  id: string
  title: string
  tenantId?: string
}

export interface SharingSnapshot {
  access: ResourceAccess
  shares: ResourceShare[]
  next_cursor: string | null
  project_inheritance?: ProjectSharingInheritance
}

export interface ProjectSharingInheritance {
  project_id: string
  project_name: string
  enabled: boolean
  can_change: boolean
}

export interface SharingCandidates {
  candidates: ShareSubject[]
  next_cursor: string | null
}

function sharingEndpoint(target: SharingTarget) {
  const collection = { project: 'projects', workspace: 'workspaces', session: 'sessions' }[target.kind]
  return `/${collection}/${encodeURIComponent(target.id)}/sharing`
}

const scope = (target: SharingTarget) => target.tenantId ? { 'x-ternilo-tenant': target.tenantId } : undefined

export const subjectId = (subject: ShareSubject) => subject.kind === 'user' ? subject.user.user_id : subject.group.group_id
export const subjectLabel = (subject: ShareSubject) => subject.kind === 'user' ? subject.user.username : subject.group.name
export const subjectKey = (subject: ShareSubject) => `${subject.kind}:${subjectId(subject)}`

export function getSharing(target: SharingTarget, input: PageQuery = {}, signal?: AbortSignal) {
  return api.request<SharingSnapshot>(`${sharingEndpoint(target)}?${pageQuery(input)}`, { signal, headers: scope(target) })
}

export function listSharingCandidates(target: SharingTarget, kind: ShareSubject['kind'], input: PageQuery = {}, signal?: AbortSignal) {
  const query = new URLSearchParams(pageQuery(input))
  query.set('kind', kind)
  return api.request<SharingCandidates>(`${sharingEndpoint(target)}/candidates?${query}`, { signal, headers: scope(target) })
}

export function setSharing(target: SharingTarget, subject: ShareSubject, permissions: ResourcePermissions | null) {
  return api.request<void>(`${sharingEndpoint(target)}/${subject.kind}/${encodeURIComponent(subjectId(subject))}`, permissions
    ? { method: 'PUT', body: permissions, headers: scope(target) }
    : { method: 'DELETE', headers: scope(target) })
}

export function setProjectInheritance(target: SharingTarget, enabled: boolean) {
  return api.request<ProjectSharingInheritance>(`/workspaces/${encodeURIComponent(target.id)}/project-sharing`, {
    method: 'PUT', headers: scope(target), body: { enabled },
  })
}
