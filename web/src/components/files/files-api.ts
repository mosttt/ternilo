import { api } from '@/api/client'

export type FileKind = 'upload' | 'generated'

export interface SessionFile {
  id: string
  session_id: string
  session_title: string
  session_archived: boolean
  workspace_id: string
  workspace_name: string
  kind: FileKind
  name: string
  media_type: string
  path: string | null
  occurred_at_ms: number
  event_seq: number | null
  attachment_index: number
  run_id: string
  source_status: 'online' | 'offline'
}

export interface FilePage {
  items: SessionFile[]
  next_cursor: string | null
  offline_sources: { workspace_id: string; executor_id: string }[]
}

export interface SessionFileContent {
  name: string
  media_type: string
  content_base64: string
}

export interface FileFilters {
  workspace_id: string
  session_id: string
  kind: FileKind | ''
  query: string
}

export function fileFilters(search: string): FileFilters {
  const params = new URLSearchParams(search)
  const kind = params.get('kind')
  return {
    workspace_id: params.get('workspace_id') ?? '',
    session_id: params.get('session_id') ?? '',
    kind: kind === 'upload' || kind === 'generated' ? kind : '',
    query: params.get('query') ?? '',
  }
}

export function filesLocation(filters: Partial<FileFilters> = {}): string {
  const query = new URLSearchParams()
  for (const [key, value] of Object.entries(filters)) if (value) query.set(key, value)
  return `/files${query.size ? `?${query}` : ''}`
}

export function listFiles(filters: FileFilters, cursor: string | null, signal: AbortSignal): Promise<FilePage> {
  const query = new URLSearchParams({ limit: '100' })
  for (const [key, value] of Object.entries(filters)) if (value) query.set(key, value)
  if (cursor) query.set('cursor', cursor)
  return api.request<FilePage>(`/files?${query}`, { signal })
}

export function readFileContent(file: Pick<SessionFile, 'session_id' | 'id'>, signal: AbortSignal): Promise<SessionFileContent> {
  return api.request<SessionFileContent>(`/sessions/${encodeURIComponent(file.session_id)}/files/${encodeURIComponent(file.id)}/content`, { signal })
}

export function fileKey(file: Pick<SessionFile, 'session_id' | 'id'>): string {
  return JSON.stringify([file.session_id, file.id])
}
