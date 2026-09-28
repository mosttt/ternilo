import type { LocalSession, SessionSearchHit, Workspace } from '@/types'

export const MAX_SESSION_SEARCH_LENGTH = 500
export const MAX_SESSION_SEARCH_RESULTS = 100

export function normalizeSessionSearchQuery(query: string): string {
  return query.slice(0, MAX_SESSION_SEARCH_LENGTH).trim().toLocaleLowerCase()
}

export function deriveSessionSearchResults(
  sessions: readonly LocalSession[],
  workspaces: readonly Workspace[],
  hits: readonly SessionSearchHit[],
  query: string,
  limit = MAX_SESSION_SEARCH_RESULTS,
): { items: LocalSession[]; hasMore: boolean } {
  const normalized = normalizeSessionSearchQuery(query)
  if (!normalized) return { items: [...sessions], hasMore: false }
  const workspaceTitles = new Map(
    workspaces.map(workspace => [workspace.workspace_id, workspace.title.toLocaleLowerCase()]),
  )
  const visible = sessions.filter(session => session.archived_at_ms == null && !session.blank)
  const byId = new Map(visible.map(session => [session.identity.session_id, session]))
  const local = visible.filter(session =>
    session.title.toLocaleLowerCase().includes(normalized)
    || workspaceTitles.get(session.workspace_id)?.includes(normalized),
  ).sort((left, right) => right.updated_at_ms - left.updated_at_ms)

  const ordered = [...local]
  const included = new Set(local.map(session => session.identity.session_id))
  for (const hit of hits) {
    const session = byId.get(hit.session_id)
    if (!session || included.has(hit.session_id)) continue
    included.add(hit.session_id)
    ordered.push(session)
  }
  return {
    items: ordered.slice(0, limit),
    hasMore: hits.length >= limit || ordered.length > limit,
  }
}
