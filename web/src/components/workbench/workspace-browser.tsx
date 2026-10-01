import { ownsResource } from '@/domain/resource-access'
import { ResourceSharingDialog, type SharingTarget } from './resource-sharing-dialog'
import * as React from 'react'
import {
  Archive, Check, ChevronDown, ChevronRight, Cloud, FolderPlus, Laptop, ListFilter, Search, X,
} from 'lucide-react'
import { api } from '@/api/client'
import {
  canonicalSessionTree, FLAT_SESSION_ORDER, foldSessionWindow, moveItem, moveItemByStep,
  groupWorkspacesByComputer, orderIds, readSidebarView, replaceOrderedSubset, workspaceComputerId, type SidebarSessionNode,
} from '@/domain/sidebar-view'
import {
  deriveSessionSearchResults, MAX_SESSION_SEARCH_LENGTH, normalizeSessionSearchQuery,
} from '@/domain/session-search'
import { UNGROUPED_WORKSPACE_ACCOUNT, ungroupedSessions } from '@/domain/workspace-lifecycle'
import { storage, useWorkbench } from '@/state/workbench'
import type { LocalSession, SessionEvent, SessionSearchHit, SidebarOrdering, Workspace } from '@/types'
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuCheckboxItem, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuSeparator, DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { cn } from '@/lib/utils'
import { useSessionActivityRoster } from './session-activity-roster'
import { SessionRenameDialog } from './session-rename-dialog'
import { SessionArchiveDialog } from './session-archive-dialog'
import { WorkspaceRenameDialog, WorkspaceUnregisterDialog } from './workspace-lifecycle-dialogs'
import {
  type DropMarker, type RowDragHandlers, type RowOrderActions, SessionRow,
  UngroupedWorkspaceRow, WorkspaceRow,
} from './workspace-rows'
import css from './workspace-browser.module.css'

const SESSION_DRAG = 'application/x-ternilo-session'
const WORKSPACE_DRAG = 'application/x-ternilo-workspace'
const EMPTY_ORDERING: SidebarOrdering = { workspace_order: [], session_order_by_account: {} }

function ordered<T>(items: readonly T[], ids: readonly string[], idOf: (item: T) => string): T[] {
  const byId = new Map(items.map(item => [idOf(item), item]))
  return ids.flatMap(id => {
    const item = byId.get(id)
    return item === undefined ? [] : [item]
  })
}

function dropAfter(event: React.DragEvent<HTMLElement>) {
  const bounds = event.currentTarget.getBoundingClientRect()
  return event.clientY >= bounds.top + bounds.height / 2
}

type DragState =
  | { kind: 'workspace'; source: string; over: { id: string; marker: Exclude<DropMarker, null> } | null }
  | { kind: 'session'; source: string; account: string; over: { id: string; marker: Exclude<DropMarker, null> } | null }

function blankDragHandlers(): RowDragHandlers {
  return {
    draggable: false,
    marker: null,
    onDragStart: () => undefined,
    onDragOver: () => undefined,
    onDrop: () => undefined,
    onDragEnd: () => undefined,
  }
}

export function WorkspaceBrowser({ wide, readOnly = false, currentSessionEvents = [], expandSidebar, onChooseWorkspace, onSessionActivated }: {
  wide: boolean
  readOnly?: boolean
  currentSessionEvents?: readonly SessionEvent[]
  expandSidebar(): void
  onChooseWorkspace(createSession?: boolean): void
  onSessionActivated(): void
}) {
  const {
    snapshot, sessionActivity, currentSessionId, currentSession, currentWorkspaceId, currentTenantId,
    platform, authRequired, loading, accountScope, serverIdentity, refresh,
    setOnlineComputersOnly,
    selectWorkspace, selectSession, createSession, renameWorkspace, unregisterWorkspace,
    updateSession, forkSession, forkOperation, archiveSession, deleteSession, notify,
  } = useWorkbench()
  const t = useTranslate('sidebar')
  const workspaceT = useTranslate('workspace')
  const archiveT = useTranslate('sessionArchive')
  const archiveScope = JSON.stringify([accountScope, currentTenantId])
  const [openArchiveScope, setOpenArchiveScope] = React.useState<string | null>(null)
  React.useEffect(() => setOpenArchiveScope(null), [archiveScope])
  const common = useTranslate('common')
  const [view, setView] = React.useState(() => readSidebarView(localStorage.getItem(storage.sidebarView)))
  const changeView = (next: typeof view) => {
    setView(next)
    void setOnlineComputersOnly(next.groupBy === 'computer' && next.onlineComputersOnly)
  }
  const computerCollapseKey = (id: string) => JSON.stringify([accountScope, currentTenantId, id])
  const activeWorkspace = snapshot.workspaces.find(workspace => workspace.workspace_id === currentWorkspaceId)
  const activeComputerKey = activeWorkspace ? computerCollapseKey(workspaceComputerId(activeWorkspace)) : null
  React.useEffect(() => {
    if (!activeComputerKey || view.groupBy !== 'computer') return
    setView(current => current.collapsedComputers.includes(activeComputerKey)
      ? { ...current, collapsedComputers: current.collapsedComputers.filter(id => id !== activeComputerKey) } : current)
  }, [activeComputerKey, currentSessionId, currentWorkspaceId, view.groupBy])
  const [ordering, setOrdering] = React.useState<SidebarOrdering>(EMPTY_ORDERING)
  const orderingRef = React.useRef(ordering)
  const confirmedOrderingRef = React.useRef(ordering)
  const orderingTouched = React.useRef(false)
  const orderingSaveChain = React.useRef<Promise<void>>(Promise.resolve())
  const orderingScopeRef = React.useRef<string | null>(accountScope ?? currentTenantId ?? '')
  const [showAll, setShowAll] = React.useState<Set<string>>(new Set())
  const [query, setQuery] = React.useState('')
  const [hits, setHits] = React.useState<SessionSearchHit[]>([])
  const [hitQuery, setHitQuery] = React.useState('')
  const [searching, setSearching] = React.useState(false)
  const [searchError, setSearchError] = React.useState('')
  const [searchRetry, setSearchRetry] = React.useState(0)
  const [searchOpen, setSearchOpen] = React.useState(false)
  const [sharingTarget, setSharingTarget] = React.useState<SharingTarget | null>(null)
  const sharingEnabled = platform && serverIdentity?.instance.mode === 'multi_user'
  const [renameTarget, setRenameTarget] = React.useState<LocalSession | null>(null)
  const [workspaceRenameTarget, setWorkspaceRenameTarget] = React.useState<Workspace | null>(null)
  const [workspaceRemoveTarget, setWorkspaceRemoveTarget] = React.useState<Workspace | null>(null)
  const [ungroupedDeleteTarget, setUngroupedDeleteTarget] = React.useState<LocalSession[] | null>(null)
  const [deletingUngrouped, setDeletingUngrouped] = React.useState(false)
  const [ungroupedDeleteError, setUngroupedDeleteError] = React.useState('')
  const [deleteCancelled, setDeleteCancelled] = React.useState(false)
  const deleteOperation = React.useRef<{ cancelled: boolean } | null>(null)
  React.useEffect(() => () => {
    if (deleteOperation.current) deleteOperation.current.cancelled = true
    deleteOperation.current = null
  }, [accountScope, currentTenantId])
  React.useEffect(() => {
    setUngroupedDeleteTarget(null)
    setDeletingUngrouped(false)
    setUngroupedDeleteError('')
    setDeleteCancelled(false)
  }, [accountScope, currentTenantId])
  const [drag, setDrag] = React.useState<DragState | null>(null)
  const dragRef = React.useRef<DragState | null>(null)
  const [now, setNow] = React.useState(() => Date.now())
  const searchInput = React.useRef<HTMLInputElement>(null)

  const updateDrag = (next: DragState | null) => {
    dragRef.current = next
    setDrag(next)
  }

  React.useEffect(() => {
    localStorage.setItem(storage.sidebarView, JSON.stringify(view))
  }, [view])

  React.useEffect(() => {
    if (authRequired || loading || (platform && !currentTenantId)) return
    let active = true
    const scope = accountScope ?? currentTenantId ?? ''
    orderingScopeRef.current = scope
    orderingTouched.current = false
    orderingRef.current = EMPTY_ORDERING
    confirmedOrderingRef.current = EMPTY_ORDERING
    setOrdering(EMPTY_ORDERING)
    void api.request<SidebarOrdering>('/sidebar-ordering', {
      headers: currentTenantId ? { 'x-ternilo-tenant': currentTenantId } : undefined,
    })
      .then(next => {
        if (!active || orderingScopeRef.current !== scope || orderingTouched.current) return
        orderingRef.current = next
        confirmedOrderingRef.current = next
        setOrdering(next)
      })
      .catch(cause => {
        if (!active) return
        notify(t('toast.orderingLoadFailed', { error: cause instanceof Error ? cause.message : String(cause) }), 'error')
      })
    return () => { active = false; orderingScopeRef.current = null }
  }, [accountScope, authRequired, currentTenantId, loading, notify, platform, t])

  const persistOrdering = (mutate: (current: SidebarOrdering) => SidebarOrdering) => {
    const next = mutate(orderingRef.current)
    if (next === orderingRef.current) return
    orderingTouched.current = true
    orderingRef.current = next
    setOrdering(next)
    const scope = orderingScopeRef.current
    const headers = currentTenantId ? { 'x-ternilo-tenant': currentTenantId } : undefined
    orderingSaveChain.current = orderingSaveChain.current.then(async () => {
      if (orderingScopeRef.current !== scope) return
      try {
        const saved = await api.request<SidebarOrdering>('/sidebar-ordering', { method: 'PUT', body: next, headers })
        if (orderingScopeRef.current !== scope) return
        confirmedOrderingRef.current = saved
        if (orderingRef.current !== next) return
        orderingRef.current = saved
        setOrdering(saved)
      } catch (cause) {
        if (orderingScopeRef.current !== scope) return
        if (orderingRef.current === next) {
          orderingRef.current = confirmedOrderingRef.current
          setOrdering(confirmedOrderingRef.current)
        }
        notify(t('toast.orderingSaveFailed', { error: cause instanceof Error ? cause.message : String(cause) }), 'error')
      }
    })
  }

  React.useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000)
    return () => window.clearInterval(timer)
  }, [])

  const activeAccount = currentSession && !snapshot.workspaces.some(workspace => workspace.workspace_id === currentSession.workspace_id)
    ? UNGROUPED_WORKSPACE_ACCOUNT
    : currentWorkspaceId
  const lastAutoExpandedAccount = React.useRef<string | null>(null)
  React.useEffect(() => {
    const previous = lastAutoExpandedAccount.current
    lastAutoExpandedAccount.current = activeAccount
    if (!activeAccount || previous === activeAccount) return
    setView(current => current.expandedWorkspaces.includes(activeAccount)
      ? current
      : { ...current, expandedWorkspaces: [...current.expandedWorkspaces, activeAccount] })
  }, [activeAccount])

  React.useEffect(() => {
    if (!currentSession?.subagent || !currentSession.parent_session_id) return
    const byId = new Map(snapshot.sessions.map(session => [session.identity.session_id, session]))
    const ancestors = new Set<string>()
    let child: LocalSession | undefined = currentSession
    while (child?.subagent && child.parent_session_id) {
      ancestors.add(child.parent_session_id)
      child = byId.get(child.parent_session_id)
    }
    setView(current => {
      const collapsedSessions = current.collapsedSessions.filter(id => !ancestors.has(id))
      return collapsedSessions.length === current.collapsedSessions.length
        ? current
        : { ...current, collapsedSessions }
    })
  }, [currentSession, snapshot.sessions])

  React.useEffect(() => {
    const normalizedQuery = normalizeSessionSearchQuery(query)
    if (!normalizedQuery) {
      setHits([])
      setHitQuery('')
      setSearching(false)
      setSearchError('')
      return
    }
    const controller = new AbortController()
    const timer = window.setTimeout(() => {
      setSearching(true)
      setSearchError('')
      const params = new URLSearchParams({ query: normalizedQuery, limit: '100' })
      if (platform && view.groupBy === 'computer' && view.onlineComputersOnly) params.set('online_computers_only', 'true')
      void api.request<SessionSearchHit[]>(`/session-search?${params}`, { signal: controller.signal })
        .then(next => {
          if (controller.signal.aborted) return
          setHits(next)
          setHitQuery(normalizedQuery)
        })
        .catch(cause => {
          if (controller.signal.aborted || (cause instanceof Error && cause.name === 'AbortError')) return
          setSearchError(cause instanceof Error ? cause.message : String(cause))
        })
        .finally(() => { if (!controller.signal.aborted) setSearching(false) })
    }, 180)
    return () => {
      window.clearTimeout(timer)
      controller.abort()
    }
  }, [query, searchRetry, platform, view.groupBy, view.onlineComputersOnly])

  React.useEffect(() => {
    if (!wide || !searchOpen) return
    const timer = window.setTimeout(() => searchInput.current?.focus({ preventScroll: true }), 0)
    return () => window.clearTimeout(timer)
  }, [searchOpen, wide])

  const workspaces = snapshot.workspaces
  const showPlacement = new Set(workspaces.map(workspace => workspace.placement === 'local_node'
    ? `node:${workspace.node_id ?? ''}` : workspace.placement ?? 'local')).size > 1
  const sessions = snapshot.sessions.filter(session => session.archived_at_ms == null)
  const activity = useSessionActivityRoster(sessions, currentSessionId, currentSessionEvents, sessionActivity)
  const normalizedQuery = normalizeSessionSearchQuery(query)
  const activeHits = hitQuery === normalizedQuery ? hits : []
  const searchResults = deriveSessionSearchResults(sessions, workspaces, activeHits, normalizedQuery)
  const dataState = searching
    ? 'loading'
    : searchError
      ? 'error'
      : normalizedQuery && searchResults.items.length === 0
        ? 'empty'
        : !normalizedQuery && !workspaces.length && !sessions.length
          ? 'empty'
          : 'ready'
  const expanded = new Set(view.expandedWorkspaces)
  const hitBySession = new Map<string, SessionSearchHit>()
  for (const hit of activeHits) if (!hitBySession.has(hit.session_id)) hitBySession.set(hit.session_id, hit)

  const setExpanded = (account: string, value: boolean) => {
    setView(current => {
      const accounts = new Set(current.expandedWorkspaces)
      if (value) accounts.add(account)
      else accounts.delete(account)
      return { ...current, expandedWorkspaces: [...accounts] }
    })
    if (!value) setShowAll(current => {
      const next = new Set(current)
      next.delete(account)
      return next
    })
  }
  const setSessionChildrenExpanded = (sessionId: string, value: boolean) => {
    setView(current => {
      const collapsed = new Set(current.collapsedSessions)
      if (value) collapsed.delete(sessionId)
      else collapsed.add(sessionId)
      return { ...current, collapsedSessions: [...collapsed] }
    })
  }
  const setSessionOrder = (account: string, order: string[]) => {
    const established = new Set(sessions.filter(session => !session.blank).map(session => session.identity.session_id))
    persistOrdering(current => ({
      ...current,
      session_order_by_account: {
        ...current.session_order_by_account,
        [account]: order.filter(id => established.has(id)),
      },
    }))
  }
  const sessionOrder = (items: LocalSession[], account: string, mode = view.orderBy) => orderIds(
    items,
    item => item.identity.session_id,
    item => item.updated_at_ms,
    ordering.session_order_by_account[account] ?? [],
    mode,
  )

  const activateSession = (id: string) => {
    selectSession(id)
    onSessionActivated()
  }
  const startSession = async (workspaceId?: string) => {
    if (!workspaceId && !currentWorkspaceId) {
      onChooseWorkspace(true)
      return
    }
    try {
      await createSession(workspaceId)
      onSessionActivated()
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const confirmSessionRename = async (session: LocalSession, title: string) => {
    await updateSession(session.identity.session_id, { title })
    notify(t('toast.sessionRenamed'))
  }
  const fork = async (session: LocalSession) => {
    if (forkOperation) return
    try {
      await forkSession(session.identity.session_id)
      notify(t('toast.sessionForked'))
      onSessionActivated()
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const archive = async (session: LocalSession) => {
    try {
      await archiveSession(session.identity.session_id)
      notify(t('toast.sessionArchived'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }
  const deleteUngrouped = async (items: LocalSession[]) => {
    if (!items.length || deleteOperation.current) return
    const operation = { cancelled: false }
    deleteOperation.current = operation
    let deleted = 0
    setDeletingUngrouped(true)
    setDeleteCancelled(false)
    setUngroupedDeleteError('')
    try {
      for (const session of items) {
        if (operation.cancelled) break
        await deleteSession(session.identity.session_id)
        deleted += 1
        if (deleteOperation.current !== operation) return
        setUngroupedDeleteTarget(current => current?.filter(item => item.identity.session_id !== session.identity.session_id) ?? null)
      }
      setUngroupedDeleteTarget(null)
      notify(operation.cancelled ? t('toast.cancelledUngrouped', { deleted, remaining: items.length - deleted }) : t('toast.deletedUngrouped', { count: deleted }))
    } catch (cause) {
      if (deleteOperation.current === operation) setUngroupedDeleteError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (deleteOperation.current === operation) {
        deleteOperation.current = null
        setDeletingUngrouped(false)
        setDeleteCancelled(false)
      }
    }
  }

  const sessionDrag = (account: string, id: string, order: string[]): RowDragHandlers => {
    const marker = drag?.kind === 'session' && drag.account === account && drag.over?.id === id
      ? drag.over.marker
      : null
    return {
      draggable: view.orderBy === 'manual',
      marker,
      onDragStart: event => {
        event.dataTransfer.effectAllowed = 'move'
        event.dataTransfer.setData(SESSION_DRAG, `${account}\n${id}`)
        updateDrag({ kind: 'session', source: id, account, over: null })
      },
      onDragOver: event => {
        const active = dragRef.current
        if (active?.kind !== 'session' || active.account !== account) return
        event.preventDefault()
        event.dataTransfer.dropEffect = 'move'
        updateDrag({ ...active, over: { id, marker: dropAfter(event) ? 'after' : 'before' } })
      },
      onDrop: event => {
        const active = dragRef.current
        if (active?.kind !== 'session' || active.account !== account) return
        event.preventDefault()
        setSessionOrder(account, moveItem(order, active.source, id, dropAfter(event)))
        updateDrag(null)
      },
      onDragEnd: () => updateDrag(null),
    }
  }

  const workspaceIds = orderIds(workspaces, item => item.workspace_id, item => item.updated_at_ms, ordering.workspace_order, view.orderBy)
  const workspaceIdsInGroup = (id: string, ids = workspaceIds) => {
    if (view.groupBy !== 'computer') return ids
    const workspace = workspaces.find(item => item.workspace_id === id)
    if (!workspace) return []
    const computer = workspaceComputerId(workspace)
    return ids.filter(candidate => workspaceComputerId(workspaces.find(item => item.workspace_id === candidate)!) === computer)
  }
  const sameComputerGroup = (source: string, target: string) => workspaceIdsInGroup(source).includes(target)
  const workspaceOrderActions = (id: string): RowOrderActions | undefined => {
    if (view.orderBy !== 'manual') return undefined
    const groupIds = workspaceIdsInGroup(id)
    const index = groupIds.indexOf(id)
    const move = (step: -1 | 1) => persistOrdering(current => {
      const currentOrder = orderIds(
        workspaces,
        item => item.workspace_id,
        item => item.updated_at_ms,
        current.workspace_order,
        'manual',
      )
      return { ...current, workspace_order: replaceOrderedSubset(currentOrder, moveItemByStep(workspaceIdsInGroup(id, currentOrder), id, step)) }
    })
    return {
      kind: 'workspace',
      canMoveUp: index > 0,
      canMoveDown: index >= 0 && index < groupIds.length - 1,
      moveUp: () => move(-1),
      moveDown: () => move(1),
    }
  }
  const workspaceDrag = (id: string): RowDragHandlers => {
    const marker = drag?.kind === 'workspace' && drag.over?.id === id ? drag.over.marker : null
    return {
      draggable: view.orderBy === 'manual',
      marker,
      onDragStart: event => {
        event.dataTransfer.effectAllowed = 'move'
        event.dataTransfer.setData(WORKSPACE_DRAG, id)
        updateDrag({ kind: 'workspace', source: id, over: null })
      },
      onDragOver: event => {
        const active = dragRef.current
        if (active?.kind !== 'workspace') return
        if (!sameComputerGroup(active.source, id)) {
          event.dataTransfer.dropEffect = 'none'
          updateDrag({ ...active, over: null })
          return
        }
        event.preventDefault()
        event.dataTransfer.dropEffect = 'move'
        updateDrag({ ...active, over: { id, marker: dropAfter(event) ? 'after' : 'before' } })
      },
      onDrop: event => {
        const active = dragRef.current
        if (active?.kind !== 'workspace' || !sameComputerGroup(active.source, id)) return
        event.preventDefault()
        const after = dropAfter(event)
        persistOrdering(current => ({ ...current, workspace_order: replaceOrderedSubset(workspaceIds, moveItem(workspaceIdsInGroup(id), active.source, id, after)) }))
        updateDrag(null)
      },
      onDragEnd: () => updateDrag(null),
    }
  }

  const renderSession = (
    session: LocalSession,
    account: string,
    order: string[],
    searchHit?: SessionSearchHit,
    depth = 0,
    childCount = 0,
  ) => {
    const sortableOrder = order.filter(id => !sessions.find(item => item.identity.session_id === id)?.blank)
    const index = sortableOrder.indexOf(session.identity.session_id)
    const nested = depth > 0
    const childrenExpanded = !view.collapsedSessions.includes(session.identity.session_id)
    const orderActions: RowOrderActions | undefined = view.orderBy === 'manual' && !searchHit && !session.blank && !nested
      ? {
          kind: view.groupBy === 'flat' ? 'session-all' : 'session-group',
          canMoveUp: index > 0,
          canMoveDown: index >= 0 && index < sortableOrder.length - 1,
          moveUp: () => setSessionOrder(account, moveItemByStep(sortableOrder, session.identity.session_id, -1)),
          moveDown: () => setSessionOrder(account, moveItemByStep(sortableOrder, session.identity.session_id, 1)),
        }
      : undefined
    const updatedAt = activity.updatedAt(session.identity.session_id, session.updated_at_ms)
    const displayedSession = updatedAt === session.updated_at_ms
      ? session
      : { ...session, updated_at_ms: updatedAt }
    return <SessionRow
      key={session.identity.session_id}
      session={displayedSession}
      searchHit={searchHit}
      statuses={activity.statuses(session.identity.session_id)}
      now={now}
      active={currentSessionId === session.identity.session_id}
      drag={readOnly || searchHit || session.blank || nested ? blankDragHandlers() : sessionDrag(account, session.identity.session_id, sortableOrder)}
      order={readOnly ? undefined : orderActions}
      readOnly={readOnly}
      depth={depth}
      levelOffset={view.groupBy === 'computer' && !normalizedQuery && account !== UNGROUPED_WORKSPACE_ACCOUNT ? 1 : 0}
      childCount={childCount}
      childrenExpanded={childrenExpanded}
      onToggleChildren={childCount > 0
        ? () => setSessionChildrenExpanded(session.identity.session_id, !childrenExpanded)
        : undefined}
      onSelect={activateSession}
      onRename={setRenameTarget}
      onFork={fork}
      forkLocked={forkOperation !== null}
      onArchive={archive}
      onShare={sharingEnabled && (session.access?.permissions.view ?? ownsResource(session.access, !readOnly && session.identity.user_id === serverIdentity?.user.user_id)) ? session => setSharingTarget({ kind: 'session', id: session.identity.session_id, title: session.title }) : undefined}
    />
  }

  const renderSessionNode = (
    node: SidebarSessionNode,
    account: string,
    order: string[],
    depth = 0,
    search = false,
  ): React.ReactNode => {
    const id = node.session.identity.session_id
    const expanded = !view.collapsedSessions.includes(id)
    return <React.Fragment key={id}>
      {renderSession(node.session, account, order, search ? hitBySession.get(id) : undefined, depth, node.children.length)}
      {expanded && node.children.length > 0 && (
        <div className={css.subagentGroup} role="group" data-sidebar-subagent-group={id}>
          {node.children.map(child => renderSessionNode(child, account, order, depth + 1, search))}
        </div>
      )}
    </React.Fragment>
  }

  const pinSessionNode = (node: SidebarSessionNode): boolean => (
    Boolean(node.session.blank)
    || node.session.identity.session_id === currentSessionId
    || activity.statuses(node.session.identity.session_id).some(status => status.state === 'running')
    || node.children.some(pinSessionNode)
  )

  const renderList = () => {
    if (normalizedQuery) {
      const ids = searchResults.items.map(item => item.identity.session_id)
      return canonicalSessionTree(searchResults.items)
        .map(node => renderSessionNode(node, FLAT_SESSION_ORDER, ids, 0, true))
    }
    if (view.groupBy === 'flat') {
      const ids = sessionOrder(sessions, FLAT_SESSION_ORDER)
      const tree = canonicalSessionTree(ordered(sessions, ids, item => item.identity.session_id))
      const rootIds = tree.map(node => node.session.identity.session_id)
      return tree.map(node => renderSessionNode(node, FLAT_SESSION_ORDER, rootIds))
    }

    const renderWorkspace = (workspace: Workspace) => {
      const items = sessions.filter(session => session.workspace_id === workspace.workspace_id)
      const ids = sessionOrder(items, workspace.workspace_id)
      const all = ordered(items, ids, item => item.identity.session_id)
      const tree = canonicalSessionTree(all)
      const rootIds = tree.map(node => node.session.identity.session_id)
      const isExpanded = expanded.has(workspace.workspace_id)
      const showEvery = showAll.has(workspace.workspace_id)
      const folded = foldSessionWindow(tree, node => Boolean(node.session.blank), pinSessionNode)
      const visible = showEvery ? tree : folded
      const hidden = tree.length - folded.length
      return (
        <section
          className={css.group}
          data-sidebar-workspace-group=""
          role="treeitem"
          aria-level={view.groupBy === 'computer' ? 2 : 1}
          aria-expanded={isExpanded}
          key={workspace.workspace_id}
        >
          <WorkspaceRow
            workspace={workspace}
            platform={platform}
            showPlacement={showPlacement && view.groupBy !== 'computer'}
            computerGrouped={view.groupBy === 'computer'}
            expanded={isExpanded}
            active={currentWorkspaceId === workspace.workspace_id}
            drag={readOnly ? blankDragHandlers() : workspaceDrag(workspace.workspace_id)}
            order={readOnly ? undefined : workspaceOrderActions(workspace.workspace_id)}
            readOnly={readOnly}
            onToggle={() => {
              setExpanded(workspace.workspace_id, !isExpanded)
              if (currentWorkspaceId !== workspace.workspace_id) selectWorkspace(workspace.workspace_id)
            }}
            onNewSession={() => {
              setExpanded(workspace.workspace_id, true)
              void startSession(workspace.workspace_id)
            }}
            onRename={() => setWorkspaceRenameTarget(workspace)}
            onUnregister={() => setWorkspaceRemoveTarget(workspace)}
            onShare={sharingEnabled && (workspace.access?.permissions.view ?? ownsResource(workspace.access, !readOnly && workspace.owner_user_id === serverIdentity?.user.user_id)) ? () => setSharingTarget({ kind: 'workspace', id: workspace.workspace_id, title: workspace.title }) : undefined}
          />
          {isExpanded && (
            <div className={css.groupBody} role="group">
              {visible.map(node => renderSessionNode(node, workspace.workspace_id, rootIds))}
              {hidden > 0 && (
                <div role="treeitem">
                  <button
                    type="button"
                    className={css.more}
                    aria-expanded={showEvery}
                    onClick={() => setShowAll(current => {
                      const next = new Set(current)
                      if (showEvery) next.delete(workspace.workspace_id)
                      else next.add(workspace.workspace_id)
                      return next
                    })}
                  >
                    {showEvery ? workspaceT('sessions.collapse') : workspaceT('sessions.more', { n: hidden })}
                  </button>
                </div>
              )}
            </div>
          )}
        </section>
      )
    }
    const orderedWorkspaces = ordered(workspaces, workspaceIds, item => item.workspace_id)
    const groups: React.ReactNode[] = view.groupBy !== 'computer' ? orderedWorkspaces.map(renderWorkspace)
      : groupWorkspacesByComputer(orderedWorkspaces).map(computer => {
        const collapseKey = computerCollapseKey(computer.id)
        const isExpanded = !view.collapsedComputers.includes(collapseKey)
        const label = computer.kind === 'node' ? computer.nodeId ? t('computer.name', { name: computer.nodeId }) : t('computer.unknown')
          : t(computer.kind === 'cloud' ? 'computer.cloud' : 'computer.local')
        const online = computer.workspaces.some(workspace => workspace.status !== 'offline' && workspace.status !== 'error')
        const Icon = computer.kind === 'cloud' ? Cloud : Laptop
        return <section key={computer.id} className={css.computerGroup} data-sidebar-computer-group={computer.id} role="treeitem" aria-level={1} aria-expanded={isExpanded}>
          <button className={css.computerHeader} type="button" data-sidebar-computer-button="" aria-expanded={isExpanded} title={label}
            onClick={() => setView(current => ({ ...current, collapsedComputers: isExpanded ? [...current.collapsedComputers, collapseKey] : current.collapsedComputers.filter(id => id !== collapseKey) }))}>
            {isExpanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}<Icon size={14} />
            <span className={css.computerTitle} data-sidebar-computer-title="">{label}</span>
            {!online && <span className={css.computerOffline}>{workspaceT('status.offline')}</span>}
            <span className={css.computerCount} aria-label={t('computer.workspaces', { n: computer.workspaces.length })}>{computer.workspaces.length}</span>
          </button>
          {isExpanded && <div className={css.computerBody} role="group">{computer.workspaces.map(renderWorkspace)}</div>}
        </section>
      })
    const ungrouped = ungroupedSessions(workspaces, sessions)
    if (ungrouped.length) {
      const ids = sessionOrder(ungrouped, UNGROUPED_WORKSPACE_ACCOUNT)
      const all = ordered(ungrouped, ids, item => item.identity.session_id)
      const tree = canonicalSessionTree(all)
      const rootIds = tree.map(node => node.session.identity.session_id)
      const isExpanded = expanded.has(UNGROUPED_WORKSPACE_ACCOUNT)
      const showEvery = showAll.has(UNGROUPED_WORKSPACE_ACCOUNT)
      const folded = foldSessionWindow(tree, node => Boolean(node.session.blank), pinSessionNode)
      const visible = showEvery ? tree : folded
      const hidden = tree.length - folded.length
      groups.push(
        <section
          className={css.group}
          data-sidebar-workspace-group=""
          role="treeitem"
          aria-expanded={isExpanded}
          key={UNGROUPED_WORKSPACE_ACCOUNT}
        >
          <UngroupedWorkspaceRow
            expanded={isExpanded}
            active={currentSession !== null && ungrouped.some(session => session.identity.session_id === currentSession.identity.session_id)}
            sessionCount={ungrouped.length}
            readOnly={readOnly || ungrouped.some(session => !ownsResource(session.access, !readOnly))}
            onToggle={() => setExpanded(UNGROUPED_WORKSPACE_ACCOUNT, !isExpanded)}
            onDeleteAll={() => {
              setUngroupedDeleteError('')
              setUngroupedDeleteTarget(ungrouped)
            }}
          />
          {isExpanded && (
            <div className={css.groupBody} role="group">
              {visible.map(node => renderSessionNode(node, UNGROUPED_WORKSPACE_ACCOUNT, rootIds))}
              {hidden > 0 && (
                <div role="treeitem">
                  <button
                    type="button"
                    className={css.more}
                    aria-expanded={showEvery}
                    onClick={() => setShowAll(current => {
                      const next = new Set(current)
                      if (showEvery) next.delete(UNGROUPED_WORKSPACE_ACCOUNT)
                      else next.add(UNGROUPED_WORKSPACE_ACCOUNT)
                      return next
                    })}
                  >
                    {showEvery ? workspaceT('sessions.collapse') : workspaceT('sessions.more', { n: hidden })}
                  </button>
                </div>
              )}
            </div>
          )}
        </section>,
      )
    }
    return groups
  }

  return (
    <div className={cn(css.root, !wide && css.rail)} data-workspace-browser-state={dataState}>
      {wide ? (
        <div className={css.header} data-sidebar-workspace-header="">
          {!searchOpen && <span className={css.sectionLabel}>{view.groupBy === 'flat' ? t('section.sessions') : t('section.workspaces')}</span>}
          <div className={cn(css.searchSlot, searchOpen && css.searchSlotExpanded)}>
            <div className={cn(css.search, searchOpen && css.searchExpanded)} onClick={() => setSearchOpen(true)}>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button type="button" className={css.searchButton} aria-label={t('search.open')} aria-expanded={searchOpen}>
                    <Search size={searchOpen ? 12 : 14} />
                  </button>
                </TooltipTrigger>
                <TooltipContent side="bottom" hidden={searchOpen}>{t('search.open')}</TooltipContent>
              </Tooltip>
              {searchOpen && (
                <>
                  <input
                    ref={searchInput}
                    className={css.searchInput}
                    value={query}
                    maxLength={MAX_SESSION_SEARCH_LENGTH}
                    placeholder={t('search.placeholder')}
                    aria-label={t('search.open')}
                    onChange={event => setQuery(event.target.value.slice(0, MAX_SESSION_SEARCH_LENGTH))}
                    onKeyDown={event => {
                      if (event.key !== 'Escape') return
                      setQuery('')
                      setSearchOpen(false)
                    }}
                  />
                  <button type="button" className={css.clearButton} aria-label={t('search.close')} onClick={event => {
                    event.stopPropagation()
                    setQuery('')
                    setSearchOpen(false)
                  }}><X size={14} /></button>
                </>
              )}
            </div>
          </div>
          <div className={cn(css.headerActions, searchOpen && css.headerActionsHidden)}>
            <Tooltip>
              <TooltipTrigger asChild>
                <button type="button" className={css.iconButton} aria-label={archiveT('title')} disabled={authRequired || loading || (platform && !currentTenantId)} onClick={() => setOpenArchiveScope(archiveScope)}><Archive size={16} /></button>
              </TooltipTrigger>
              <TooltipContent side="bottom">{archiveT('title')}</TooltipContent>
            </Tooltip>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button type="button" className={css.iconButton} aria-label={t('viewOptions.label')}><ListFilter size={16} /></button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-56 p-2">
                <DropdownMenuLabel>{t('groupBy.label')}</DropdownMenuLabel>
                <DropdownMenuItem onSelect={() => changeView({ ...view, groupBy: 'computer' })}>
                  <span className="flex-1">{t('groupBy.computer')}</span>{view.groupBy === 'computer' && <Check />}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => changeView({ ...view, groupBy: 'workspace' })}>
                  <span className="flex-1">{t('groupBy.workspace')}</span>{view.groupBy === 'workspace' && <Check />}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => changeView({ ...view, groupBy: 'flat' })}>
                  <span className="flex-1">{t('groupBy.flat')}</span>{view.groupBy === 'flat' && <Check />}
                </DropdownMenuItem>
                {view.groupBy === 'computer' && <DropdownMenuCheckboxItem checked={view.onlineComputersOnly}
                  onCheckedChange={checked => changeView({ ...view, onlineComputersOnly: checked === true })}>{t('computer.onlyOnline')}</DropdownMenuCheckboxItem>}
                <DropdownMenuSeparator />
                <DropdownMenuLabel>{t('orderBy.label')}</DropdownMenuLabel>
                <DropdownMenuItem onSelect={() => setView(current => ({ ...current, orderBy: 'manual' }))}>
                  <span className="flex-1">{t('orderBy.manual')}</span>{view.orderBy === 'manual' && <Check />}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setView(current => ({ ...current, orderBy: 'updated' }))}>
                  <span className="flex-1">{t('orderBy.updated')}</span>{view.orderBy === 'updated' && <Check />}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            {!readOnly && <Tooltip>
              <TooltipTrigger asChild>
                <button type="button" className={css.iconButton} aria-label={t('workspace.add')} onClick={() => onChooseWorkspace(true)}><FolderPlus size={16} /></button>
              </TooltipTrigger>
              <TooltipContent side="bottom">{t('workspace.add')}</TooltipContent>
            </Tooltip>}
          </div>
        </div>
      ) : (
        <div className={css.railControls}>
          <Tooltip>
            <TooltipTrigger asChild>
              <button type="button" className={css.iconButton} aria-label={archiveT('title')} disabled={authRequired || loading || (platform && !currentTenantId)} onClick={() => setOpenArchiveScope(archiveScope)}><Archive size={18} /></button>
            </TooltipTrigger>
            <TooltipContent side="right">{archiveT('title')}</TooltipContent>
          </Tooltip>
          <Tooltip>
            <TooltipTrigger asChild>
              <button type="button" className={css.searchButton} aria-label={t('search.open')} onClick={() => {
                setSearchOpen(true)
                expandSidebar()
              }}><Search size={18} /></button>
            </TooltipTrigger>
            <TooltipContent side="right">{t('search.open')}</TooltipContent>
          </Tooltip>
          {!readOnly && <Tooltip>
            <TooltipTrigger asChild>
              <button type="button" className={css.iconButton} aria-label={t('workspace.add')} onClick={() => onChooseWorkspace(true)}><FolderPlus size={18} /></button>
            </TooltipTrigger>
            <TooltipContent side="right">{t('workspace.add')}</TooltipContent>
          </Tooltip>}
        </div>
      )}

      <div className={css.listArea}>
        {wide && (
          <div className={css.list} role="tree" aria-label={normalizedQuery ? workspaceT('search.results.aria') : workspaceT('tree.aria')}>
            {renderList()}
            {searching && <p className={css.searchStatus}>{t('search.pending')}</p>}
            {searchError && <div role="alert" className={css.searchError}><span>{t('search.failed', { error: searchError })}</span><button type="button" onClick={() => setSearchRetry(value => value + 1)}>{common('retry')}</button></div>}
            {!searchError && searchResults.hasMore && <p className={css.searchLimit}>{t('search.limit')}</p>}
            {normalizedQuery && !searching && !searchError && searchResults.items.length === 0 && <p className={css.empty}>{t('search.empty')}</p>}
            {!normalizedQuery && !workspaces.length && !sessions.length && <div className={css.empty}>{t(view.groupBy === 'computer' && view.onlineComputersOnly ? 'empty.online' : readOnly ? 'empty.viewer' : 'empty.workspaces')}{!readOnly && <><br /><button type="button" className={css.emptyButton} onClick={() => onChooseWorkspace(true)}>{t('empty.choose')}</button></>}</div>}
          </div>
        )}
        {wide && <span className={css.fade} />}
      </div>

      {openArchiveScope === archiveScope && !authRequired && <SessionArchiveDialog key={`${archiveScope}:${view.groupBy}:${view.onlineComputersOnly}`} tenantId={currentTenantId} platform={platform} readOnly={readOnly} workspaces={workspaces} onlineComputersOnly={view.groupBy === 'computer' && view.onlineComputersOnly} onClose={() => setOpenArchiveScope(null)} onRestored={refresh} />}
      {sharingTarget && <ResourceSharingDialog key={`${sharingTarget.kind}:${sharingTarget.id}`} target={sharingTarget} onClose={() => setSharingTarget(null)} onChanged={refresh} />}
      {!readOnly && <SessionRenameDialog session={renameTarget} onOpenChange={open => { if (!open) setRenameTarget(null) }} onRename={confirmSessionRename} />}
      {!readOnly && <WorkspaceRenameDialog
        workspace={workspaceRenameTarget}
        workspaces={workspaces}
        onOpenChange={open => { if (!open) setWorkspaceRenameTarget(null) }}
        onRename={async (workspace, title) => {
          await renameWorkspace(workspace.workspace_id, title)
          notify(t('toast.workspaceRenamed'))
        }}
      />}
      {!readOnly && <WorkspaceUnregisterDialog
        workspace={workspaceRemoveTarget}
        onOpenChange={open => { if (!open) setWorkspaceRemoveTarget(null) }}
        onUnregister={async workspace => {
          await unregisterWorkspace(workspace.workspace_id)
          notify(t('toast.workspaceRemoved'))
        }}
      />}
      {!readOnly && <ActionDialog
        open={Boolean(ungroupedDeleteTarget)}
        title={t('deleteUngrouped.title')}
        description={t('confirm.deleteUngrouped', { count: ungroupedDeleteTarget?.length ?? 0 })}
        cancelLabel={deletingUngrouped ? t(deleteCancelled ? 'deleteUngrouped.cancelling' : 'deleteUngrouped.cancelRemaining') : common('cancel')}
        confirmLabel={t('deleteUngrouped.confirm')}
        busyLabel={t('deleteUngrouped.busy')}
        busy={deletingUngrouped}
        destructive
        error={ungroupedDeleteError}
        onOpenChange={open => {
          if (open) return
          setUngroupedDeleteTarget(null)
          setUngroupedDeleteError('')
        }}
        onConfirm={() => void deleteUngrouped(ungroupedDeleteTarget ?? [])}
        onCancelWhileBusy={deleteCancelled ? undefined : () => {
          if (deleteOperation.current) deleteOperation.current.cancelled = true
          setDeleteCancelled(true)
        }}
      />}
    </div>
  )
}
