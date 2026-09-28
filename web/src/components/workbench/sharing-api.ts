import { api } from '@/api/client'
import { pageQuery, type PageQuery } from '@/components/settings/platform-admin-api'
import type { ResourceAccess, ResourcePermissions, ResourceShare, ShareSubject } from '@/types'

export interface SharingTarget {
  kind: 'workspace' | 'session'
  id: string
  title: string
}

export interface SharingSnapshot {
  access: ResourceAccess
  shares: ResourceShare[]
  next_cursor: string | null
}

export interface SharingCandidates {
  candidates: ShareSubject[]
  next_cursor: string | null
}

function sharingEndpoint(target: SharingTarget) {
  return `/${target.kind === 'workspace' ? 'workspaces' : 'sessions'}/${encodeURIComponent(target.id)}/sharing`
}

export const subjectId = (subject: ShareSubject) => subject.kind === 'user' ? subject.user.user_id : subject.group.group_id
export const subjectLabel = (subject: ShareSubject) => subject.kind === 'user' ? subject.user.username : subject.group.name
export const subjectKey = (subject: ShareSubject) => `${subject.kind}:${subjectId(subject)}`

export function getSharing(target: SharingTarget, input: PageQuery = {}, signal?: AbortSignal) {
  return api.request<SharingSnapshot>(`${sharingEndpoint(target)}?${pageQuery(input)}`, { signal })
}

export function listSharingCandidates(target: SharingTarget, kind: ShareSubject['kind'], input: PageQuery = {}, signal?: AbortSignal) {
  const query = new URLSearchParams(pageQuery(input))
  query.set('kind', kind)
  return api.request<SharingCandidates>(`${sharingEndpoint(target)}/candidates?${query}`, { signal })
}

export function setSharing(target: SharingTarget, subject: ShareSubject, permissions: ResourcePermissions | null) {
  return api.request<void>(`${sharingEndpoint(target)}/${subject.kind}/${encodeURIComponent(subjectId(subject))}`, permissions
    ? { method: 'PUT', body: permissions }
    : { method: 'DELETE' })
}
