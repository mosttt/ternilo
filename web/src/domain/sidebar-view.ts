import type { LocalSession } from '@/types'

export type SidebarGroupBy = 'workspace' | 'flat'
export type SidebarOrderBy = 'manual' | 'updated'

export const FLAT_SESSION_ORDER = '__flat_sessions__'

export interface SidebarViewState {
  groupBy: SidebarGroupBy
  orderBy: SidebarOrderBy
  expandedWorkspaces: string[]
  collapsedSessions: string[]
}

export const defaultSidebarView: SidebarViewState = {
  groupBy: 'workspace',
  orderBy: 'updated',
  expandedWorkspaces: [],
  collapsedSessions: [],
}

export function readSidebarView(serialized: string | null): SidebarViewState {
  if (!serialized) return defaultSidebarView
  try {
    const value = JSON.parse(serialized) as Partial<SidebarViewState>
    return {
      groupBy: value.groupBy === 'flat' ? 'flat' : 'workspace',
      orderBy: value.orderBy === 'manual' ? 'manual' : 'updated',
      expandedWorkspaces: Array.isArray(value.expandedWorkspaces) ? value.expandedWorkspaces.filter(item => typeof item === 'string') : [],
      collapsedSessions: Array.isArray(value.collapsedSessions) ? value.collapsedSessions.filter(item => typeof item === 'string') : [],
    }
  } catch {
    return defaultSidebarView
  }
}

export interface SidebarSessionNode {
  session: LocalSession
  children: SidebarSessionNode[]
}

/** Nest only canonical Subagent Sessions. Ordinary forks stay peer Sessions. */
export function canonicalSessionTree(orderedSessions: readonly LocalSession[]): SidebarSessionNode[] {
  const byId = new Map(orderedSessions.map(session => [session.identity.session_id, session]))
  const children = new Map<string, LocalSession[]>()
  const roots: LocalSession[] = []
  for (const session of orderedSessions) {
    const parentId = session.subagent ? session.parent_session_id : null
    if (!parentId || !byId.has(parentId)) {
      roots.push(session)
      continue
    }
    const siblings = children.get(parentId) ?? []
    siblings.push(session)
    children.set(parentId, siblings)
  }
  const node = (session: LocalSession): SidebarSessionNode => ({
    session,
    children: (children.get(session.identity.session_id) ?? []).map(node),
  })
  return roots.map(node)
}

export function orderIds<T>(
  items: T[],
  idOf: (item: T) => string,
  updatedAtOf: (item: T) => number,
  previous: string[],
  mode: SidebarOrderBy,
): string[] {
  if (mode === 'updated') {
    return [...items]
      .sort((left, right) => updatedAtOf(right) - updatedAtOf(left) || idOf(left).localeCompare(idOf(right)))
      .map(idOf)
  }
  const available = new Set(items.map(idOf))
  const retained = previous.filter(id => available.delete(id))
  const added = items
    .filter(item => available.has(idOf(item)))
    .sort((left, right) => updatedAtOf(right) - updatedAtOf(left) || idOf(left).localeCompare(idOf(right)))
    .map(idOf)
  return [...added, ...retained]
}

export function moveItem(order: string[], source: string, target: string, after: boolean): string[] {
  if (source === target || !order.includes(source) || !order.includes(target)) return order
  const next = order.filter(id => id !== source)
  const targetIndex = next.indexOf(target)
  next.splice(targetIndex + (after ? 1 : 0), 0, source)
  return next
}

export function moveItemByStep(order: string[], source: string, step: -1 | 1): string[] {
  const sourceIndex = order.indexOf(source)
  const targetIndex = sourceIndex + step
  if (sourceIndex < 0 || targetIndex < 0 || targetIndex >= order.length) return order
  return moveItem(order, source, order[targetIndex], step > 0)
}

export function foldSessionWindow<T>(
  items: T[],
  isProvisional: (item: T) => boolean,
  isPinned: (item: T) => boolean = () => false,
  establishedLimit = 5,
) {
  const pinnedEstablished = items.filter(item => !isProvisional(item) && isPinned(item)).length
  let unpinnedSlots = Math.max(0, establishedLimit - pinnedEstablished)
  return items.filter(item => {
    if (isProvisional(item) || isPinned(item)) return true
    if (unpinnedSlots === 0) return false
    unpinnedSlots -= 1
    return true
  })
}

/** Expand on account navigation, but retain a user's manual collapse inside the active account. */
export function expandActiveAccount(previous: string | null, active: string | null, expanded: string[]) {
  if (!active || previous === active || expanded.includes(active)) return expanded
  return [...expanded, active]
}
