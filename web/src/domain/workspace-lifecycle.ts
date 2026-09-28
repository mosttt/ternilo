import type { LocalSession, Workspace } from '@/types'

export const UNGROUPED_WORKSPACE_ACCOUNT = '__ungrouped__'

export type WorkspaceRenameIssue = 'blank' | 'unchanged' | 'duplicate' | null

export function workspaceRenameIssue(
  draft: string,
  target: Workspace,
  workspaces: readonly Workspace[],
): WorkspaceRenameIssue {
  const title = draft.trim()
  if (!title) return 'blank'
  if (title === target.title) return 'unchanged'
  return workspaces.some(workspace => workspace.workspace_id !== target.workspace_id && workspace.title === title)
    ? 'duplicate'
    : null
}

export function ungroupedSessions(
  workspaces: readonly Workspace[],
  sessions: readonly LocalSession[],
): LocalSession[] {
  const registered = new Set(workspaces.map(workspace => workspace.workspace_id))
  return sessions.filter(session => !registered.has(session.workspace_id))
}

export function abbreviateHomePath(path: string, home?: string): string {
  if (!home) return path
  const normalizedHome = home.replace(/[\\/]+$/, '')
  if (path === normalizedHome) return '~'
  const separator = normalizedHome.includes('\\') ? '\\' : '/'
  return path.startsWith(`${normalizedHome}${separator}`)
    ? `~${path.slice(normalizedHome.length)}`
    : path
}
