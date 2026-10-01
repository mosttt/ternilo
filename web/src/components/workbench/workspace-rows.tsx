import * as React from 'react'
import {
  Archive, ArrowDown, ArrowUp, ChevronDown, ChevronRight, Cloud, Ellipsis,
  Folder, GitFork, Laptop, MapPin, Pencil, Plus, Share2, Trash2,
} from 'lucide-react'
import { HoverCard } from 'radix-ui'
import type { LocalSession, SessionSearchHit, Workspace } from '@/types'
import type { SessionStatusView } from '@/domain/observability'
import { ownsResource, resourcePermissions } from '@/domain/resource-access'
import { sessionAbsoluteTime, sessionRelativeTime } from '@/domain/session-time'
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuSeparator, DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useLocale, useTranslate } from '@/i18n/provider'
import { cn } from '@/lib/utils'
import { Dialog, DialogContent, DialogTitle } from '@/components/ui/dialog'
import { WorkspaceLocationDetails, workspacePlacementLabel } from './workspace-location'
import css from './workspace-rows.module.css'

export type DropMarker = 'before' | 'after' | null

export interface RowDragHandlers {
  draggable: boolean
  marker: DropMarker
  onDragStart(event: React.DragEvent<HTMLElement>): void
  onDragOver(event: React.DragEvent<HTMLElement>): void
  onDrop(event: React.DragEvent<HTMLElement>): void
  onDragEnd(): void
}

export interface RowOrderActions {
  kind: 'workspace' | 'session-group' | 'session-all'
  canMoveUp: boolean
  canMoveDown: boolean
  moveUp(): void
  moveDown(): void
}

function ManualOrderMenu({ order }: { order: RowOrderActions }) {
  const t = useTranslate('workspace')
  const label = order.kind === 'workspace'
    ? t('order.workspace.label')
    : order.kind === 'session-group'
      ? t('order.session.group.label')
      : t('order.session.all.label')
  const up = order.kind === 'workspace'
    ? t('order.workspace.up')
    : order.kind === 'session-group'
      ? t('order.session.group.up')
      : t('order.session.all.up')
  const down = order.kind === 'workspace'
    ? t('order.workspace.down')
    : order.kind === 'session-group'
      ? t('order.session.group.down')
      : t('order.session.all.down')
  return (
    <>
      <DropdownMenuSeparator />
      <DropdownMenuLabel>{label}</DropdownMenuLabel>
      <DropdownMenuItem
        className={css.reorderMenuItem}
        data-sidebar-reorder="up"
        disabled={!order.canMoveUp}
        onSelect={order.moveUp}
      >
        <ArrowUp />{up}
      </DropdownMenuItem>
      <DropdownMenuItem
        className={css.reorderMenuItem}
        data-sidebar-reorder="down"
        disabled={!order.canMoveDown}
        onSelect={order.moveDown}
      >
        <ArrowDown />{down}
      </DropdownMenuItem>
    </>
  )
}

export function WorkspaceRow({ workspace, expanded, active, drag, order, readOnly = false, platform = false, showPlacement = false, computerGrouped = false, onToggle, onNewSession, onRename, onUnregister, onShare }: {
  workspace: Workspace
  platform?: boolean
  showPlacement?: boolean
  computerGrouped?: boolean
  expanded: boolean
  active: boolean
  drag: RowDragHandlers
  order?: RowOrderActions
  readOnly?: boolean
  onToggle(): void
  onNewSession(): void
  onRename(): void
  onUnregister(): void
  onShare?(): void
}) {
  const t = useTranslate('workspace')
  const [menuOpen, setMenuOpen] = React.useState(false)
  const [hoverOpen, setHoverOpen] = React.useState(false)
  const [locationOpen, setLocationOpen] = React.useState(false)
  const permissions = resourcePermissions(workspace.access, !readOnly)
  const isOwner = ownsResource(workspace.access, !readOnly)
  const PlacementIcon = computerGrouped ? Folder : workspace.placement === 'cloud' ? Cloud : Laptop
  const online = workspace.status !== 'offline' && workspace.status !== 'error'
  const placementLabel = workspacePlacementLabel(workspace, t)
  const statusLabel = online ? t('status.online') : t('status.offline')
  const locationKey = `${workspace.workspace_id}:${workspace.access?.is_owner !== false}:${online}`
  const openLocation = () => { setHoverOpen(false); setLocationOpen(true) }

  const row = (
    <div
      className={cn(
        css.workspaceRow,
        menuOpen && css.menuOpen,
        drag.marker === 'before' && css.dropBefore,
        drag.marker === 'after' && css.dropAfter,
      )}
      data-sidebar-workspace-row=""
      draggable={readOnly ? false : drag.draggable}
      onDragStart={readOnly ? undefined : drag.onDragStart}
      onDragOver={readOnly ? undefined : drag.onDragOver}
      onDrop={readOnly ? undefined : drag.onDrop}
      onDragEnd={readOnly ? undefined : drag.onDragEnd}
    >
      <button
        type="button"
        className={css.workspaceButton}
        data-sidebar-workspace-button=""
        data-active={active || undefined}
        aria-expanded={expanded}
        onClick={onToggle}
      >
        <span className={css.workspaceLeading} aria-hidden="true">
          <PlacementIcon className={css.placementIcon} size={15} />
          {expanded ? <ChevronDown className={css.chevron} size={14} /> : <ChevronRight className={css.chevron} size={14} />}
        </span>
        <span className={css.workspaceTitle} data-sidebar-workspace-title="">{workspace.title}</span>
        {showPlacement && <span className={css.machineLabel} data-sidebar-workspace-machine="" title={placementLabel}>{workspace.node_id ?? placementLabel}</span>}
        <span
          className={css.statusDot}
          data-offline={online ? undefined : ''}
          title={`${placementLabel} · ${statusLabel}`}
          aria-label={`${placementLabel} · ${statusLabel}`}
        />
      </button>
      {(permissions.configure || permissions.submit || isOwner || onShare) && <span className={css.rowActions}>
        <DropdownMenu open={menuOpen} onOpenChange={open => { setMenuOpen(open); if (open) setHoverOpen(false) }}>
          <DropdownMenuTrigger asChild>
            <button type="button" className={css.iconButton} aria-label={t('actions.workspace.aria', { name: workspace.title })}>
              <Ellipsis size={16} />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuItem onSelect={openLocation}><MapPin />{t('location.view')}</DropdownMenuItem>
            <DropdownMenuItem disabled={!permissions.configure} onSelect={onRename}><Pencil />{t('rename')}</DropdownMenuItem>
            <DropdownMenuItem disabled={!isOwner} className="text-destructive focus:text-destructive" onSelect={onUnregister}><Trash2 />{t('delete.workspace')}</DropdownMenuItem>
            {onShare && <DropdownMenuItem onSelect={onShare}><Share2 />{t(isOwner && permissions.configure ? 'sharing.action' : 'sharing.viewAccess')}</DropdownMenuItem>}
            {order && <ManualOrderMenu order={order} />}
          </DropdownMenuContent>
        </DropdownMenu>
        <button type="button" disabled={!permissions.submit} className={css.iconButton} aria-label={t('actions.newSession.aria', { name: workspace.title })} onClick={onNewSession}>
          <Plus size={16} />
        </button>
      </span>}
      {!(permissions.configure || permissions.submit || isOwner || onShare) && <span className={css.rowActions}>
        <button type="button" className={css.iconButton} aria-label={t('location.view')} onClick={openLocation}><MapPin size={15} /></button>
      </span>}
    </div>
  )

  return <>
    <HoverCard.Root
      open={menuOpen || locationOpen ? false : hoverOpen}
      onOpenChange={open => {
        if (menuOpen || locationOpen) return
        setHoverOpen(open)
      }}
      openDelay={500}
      closeDelay={200}
    >
      <HoverCard.Trigger asChild>{row}</HoverCard.Trigger>
      <HoverCard.Portal>
        <HoverCard.Content className={css.hoverCard} side="right" align="start" sideOffset={8}>
          {hoverOpen && !menuOpen && !locationOpen && <WorkspaceLocationDetails key={locationKey} workspace={workspace} platform={platform} />}
          <HoverCard.Arrow className="fill-popover" />
        </HoverCard.Content>
      </HoverCard.Portal>
    </HoverCard.Root>
    <Dialog open={locationOpen} onOpenChange={setLocationOpen}>
      <DialogContent className={css.locationDialog} aria-describedby={undefined}>
        <DialogTitle className="sr-only">{t('location.title', { name: workspace.title })}</DialogTitle>
        {locationOpen && <WorkspaceLocationDetails key={locationKey} workspace={workspace} platform={platform} />}
      </DialogContent>
    </Dialog>
  </>
}

export function UngroupedWorkspaceRow({ expanded, active, sessionCount, readOnly = false, onToggle, onDeleteAll }: {
  expanded: boolean
  active: boolean
  sessionCount: number
  readOnly?: boolean
  onToggle(): void
  onDeleteAll(): void
}) {
  const t = useTranslate('workspace')
  const [menuOpen, setMenuOpen] = React.useState(false)
  return (
    <div
      className={cn(css.workspaceRow, menuOpen && css.menuOpen)}
      data-sidebar-workspace-row=""
    >
      <button type="button" className={css.workspaceButton} data-sidebar-workspace-button="" data-active={active || undefined} aria-expanded={expanded} onClick={onToggle}>
        <span className={css.workspaceLeading} aria-hidden="true">
          <Laptop className={css.placementIcon} size={15} />
          {expanded ? <ChevronDown className={css.chevron} size={14} /> : <ChevronRight className={css.chevron} size={14} />}
        </span>
        <span className={css.workspaceTitle}>{t('group.ungrouped')}</span>
      </button>
      {!readOnly && <span className={css.rowActions}>
        <DropdownMenu open={menuOpen} onOpenChange={setMenuOpen}>
          <DropdownMenuTrigger asChild>
            <button type="button" className={css.iconButton} aria-label={t('actions.ungrouped.aria')}><Ellipsis size={16} /></button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            <DropdownMenuItem className="text-destructive focus:text-destructive" onSelect={onDeleteAll}><Trash2 />{t('delete.all', { count: sessionCount })}</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </span>}
    </div>
  )
}

function statusLabel(status: SessionStatusView, t: ReturnType<typeof useTranslate<'observability'>>) {
  if (status.kind === 'subagents') return t(status.count === 1 ? 'status.subagents.one' : 'status.subagents.other', { count: status.count ?? 0 })
  const keys = {
    approval: 'status.waitingApproval',
    'plan-review': 'status.planReview',
    question: 'status.waitingAnswer',
    running: 'status.running',
    'workspace-wait': 'status.waitingForWorkspace',
    'subagent-wait': 'status.waitingForSubagents',
    'capacity-wait': 'status.waitingForCapacity',
    completed: 'status.completed',
    idle: 'status.idle',
  } as const
  return t(keys[status.kind])
}

export function SessionRow({ session, searchHit, statuses = [{ state: 'idle', kind: 'idle' }], now, active, drag, order, readOnly = false, forkLocked = false, depth = 0, levelOffset = 0, childCount = 0, childrenExpanded = true, onToggleChildren, onSelect, onRename, onFork, onArchive, onShare }: {
  session: LocalSession
  searchHit?: SessionSearchHit
  statuses?: readonly SessionStatusView[]
  now: number
  active: boolean
  drag: RowDragHandlers
  order?: RowOrderActions
  readOnly?: boolean
  forkLocked?: boolean
  depth?: number
  levelOffset?: number
  childCount?: number
  childrenExpanded?: boolean
  onToggleChildren?(): void
  onSelect(id: string): void
  onRename(session: LocalSession): void
  onFork(session: LocalSession): Promise<void>
  onArchive(session: LocalSession): Promise<void>
  onShare?(session: LocalSession): void
}) {
  const t = useTranslate('workspace')
  const permissions = resourcePermissions(session.access, !readOnly)
  const activityT = useTranslate('observability')
  const { locale } = useLocale()
  const [menuOpen, setMenuOpen] = React.useState(false)
  const [hoverOpen, setHoverOpen] = React.useState(false)
  const id = session.identity.session_id
  const timestamp = searchHit?.occurred_at_ms ?? session.updated_at_ms
  const primary = statuses[0] ?? { state: 'idle', kind: 'idle' } as const
  const title = session.blank || session.title === 'New session'
    ? t('session.new')
    : session.title || t('session.new')
  const absoluteTime = sessionAbsoluteTime(timestamp, locale)
  const rowStyle = { '--session-indent': `${depth * 14}px` } as React.CSSProperties
  const row = (
    <div
      className={cn(
        css.sessionRow,
        active && css.selected,
        menuOpen && css.menuOpen,
        searchHit && css.searchResult,
        drag.marker === 'before' && css.dropBefore,
        drag.marker === 'after' && css.dropAfter,
        depth > 0 && css.nestedSession,
        childCount > 0 && css.sessionParent,
      )}
      style={rowStyle}
      data-sidebar-session-row=""
      data-session-id={id}
      data-session-depth={depth}
      data-subagent-session={session.subagent ? '' : undefined}
      role="treeitem"
      aria-level={depth + 2 + levelOffset}
      aria-selected={active}
      draggable={readOnly ? false : drag.draggable}
      onDragStart={readOnly ? undefined : drag.onDragStart}
      onDragOver={readOnly ? undefined : drag.onDragOver}
      onDrop={readOnly ? undefined : drag.onDrop}
      onDragEnd={readOnly ? undefined : drag.onDragEnd}
    >
      {childCount > 0 && onToggleChildren && (
        <button
          type="button"
          className={css.childToggle}
          data-sidebar-session-children-toggle=""
          data-state={primary.state}
          aria-expanded={childrenExpanded}
          aria-label={t(childrenExpanded ? 'session.children.collapse' : 'session.children.expand', { name: title })}
          onClick={onToggleChildren}
        >
          {childrenExpanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        </button>
      )}
      <button
        type="button"
        className={css.sessionButton}
        data-sidebar-session-button=""
        data-sidebar-session-active={active || undefined}
        data-search-result={searchHit ? '' : undefined}
        onClick={() => onSelect(id)}
      >
        <span className={css.sessionStatusSlot} aria-hidden="true">
          {primary.kind !== 'idle' && <span className={css.sessionStatusDot} data-state={primary.state} />}
        </span>
        <span className={css.sessionCopy}>
          <span className={css.title} data-sidebar-session-title="">{title}</span>
          {searchHit?.excerpt && searchHit.excerpt !== session.title && <span className={css.snippet} data-sidebar-session-snippet="">{searchHit.excerpt}</span>}
        </span>
        {!session.blank && (
          <time
            className={css.time}
            data-sidebar-session-time=""
            dateTime={new Date(timestamp).toISOString()}
            title={t('time.updated', { time: absoluteTime })}
            aria-label={t('time.updated', { time: absoluteTime })}
          >
            {sessionRelativeTime(timestamp, now, locale)}
          </time>
        )}
      </button>
      {(permissions.configure || permissions.submit || onShare) && !session.blank && (
        <span className={css.rowActions}>
          <DropdownMenu open={menuOpen} onOpenChange={open => { setMenuOpen(open); if (open) setHoverOpen(false) }}>
            <DropdownMenuTrigger asChild>
              <button type="button" className={css.iconButton} aria-label={t('actions.session.aria', { name: title })}><Ellipsis size={16} /></button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">
              <DropdownMenuItem disabled={!permissions.configure} onSelect={() => onRename(session)}><Pencil />{t('rename')}</DropdownMenuItem>
              <DropdownMenuItem disabled={forkLocked || !permissions.submit} onSelect={() => void onFork(session)}><GitFork />{t(forkLocked ? 'menu.forking' : 'menu.fork')}</DropdownMenuItem>
              <DropdownMenuItem disabled={!permissions.configure} onSelect={() => void onArchive(session)}><Archive />{t('menu.archiveSession')}</DropdownMenuItem>
              {onShare && <DropdownMenuItem onSelect={() => onShare(session)}><Share2 />{t(ownsResource(session.access, !readOnly) && permissions.configure ? 'sharing.action' : 'sharing.viewAccess')}</DropdownMenuItem>}
              {order && <ManualOrderMenu order={order} />}
            </DropdownMenuContent>
          </DropdownMenu>
        </span>
      )}
    </div>
  )
  return (
    <HoverCard.Root open={menuOpen ? false : hoverOpen} onOpenChange={open => { if (!menuOpen) setHoverOpen(open) }} openDelay={500} closeDelay={200}>
      <HoverCard.Trigger asChild>{row}</HoverCard.Trigger>
      <HoverCard.Portal>
        <HoverCard.Content className={css.hoverCard} side="right" align="start" sideOffset={8}>
          <div className={css.hoverContent}>
            <strong className={css.hoverTitle}>{title}</strong>
            {!session.blank && <span className={css.hoverMeta}>{t('time.updated', { time: absoluteTime })}</span>}
            {statuses.map((status, index) => (
              <span className={css.hoverStatus} key={`${status.kind}-${index}`}>
                <span className={css.sessionStatusDot} data-state={status.state} aria-hidden="true" />
                {statusLabel(status, activityT)}
              </span>
            ))}
          </div>
          <HoverCard.Arrow className="fill-popover" />
        </HoverCard.Content>
      </HoverCard.Portal>
    </HoverCard.Root>
  )
}
