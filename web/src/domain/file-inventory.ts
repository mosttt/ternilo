import type { ApplicationState } from '@/types'
import type { FileFilters, FilePage } from '@/components/files/files-api'

interface CachedFiles {
  page: FilePage
  cursor: string | null
  loadedAt: number
}

const listings = new Map<string, CachedFiles>()
const listeners = new Set<() => void>()
let revision = 0
let workbenchKey = ''

export const fileInventoryRevision = () => revision
export function subscribeFileInventory(listener: () => void) {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

export function invalidateFileInventory() {
  listings.clear()
  revision += 1
  for (const listener of listeners) listener()
}

export function updateFileInventoryWorkbench(state: ApplicationState) {
  const key = JSON.stringify([
    state.workspaces.map(workspace => [workspace.workspace_id, workspace.updated_at_ms, workspace.status, workspace.access]),
    state.sessions.map(session => [session.identity.session_id, session.workspace_id, session.updated_at_ms, session.access]),
  ])
  if (key === workbenchKey) return
  workbenchKey = key
  invalidateFileInventory()
}

export function fileInventoryKey(scope: string, filters: FileFilters) {
  return JSON.stringify([scope, filters.workspace_id, filters.session_id, filters.kind, filters.query])
}

export function peekFileInventory(key: string) {
  const cached = listings.get(key)
  if (cached && Date.now() - cached.loadedAt < 30_000) return cached
  listings.delete(key)
  return undefined
}

export function rememberFileInventory(key: string, page: FilePage, cursor: string | null) {
  listings.delete(key)
  listings.set(key, { page, cursor, loadedAt: Date.now() })
  if (listings.size > 12) listings.delete(listings.keys().next().value!)
}
