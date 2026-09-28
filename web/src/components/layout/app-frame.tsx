import * as React from 'react'
import type { WorkbenchLayout } from '@/hooks/use-workbench-layout'
import css from './app-frame.module.css'

export interface AppFrameLabels {
  closeMobileSidebar: string
  resizeSidebar: string
  resizeDetails: string
}

export interface AppFrameProps {
  layout: WorkbenchLayout
  mobileSidebarOpen: boolean
  detailsOpen: boolean
  detailsFullscreen?: boolean
  labels: AppFrameLabels
  sidebar: React.ReactNode
  conversation: React.ReactNode
  details: React.ReactNode
  overlay?: React.ReactNode
  onCloseMobileSidebar(): void
}

function CenterColumn({ children, inert }: { children: React.ReactNode; inert?: boolean }) {
  return <div className={css.centerCol} data-app-center-column="" inert={inert ? true : undefined}>{children}</div>
}

function DetailsColumn({ children, inert }: { children: React.ReactNode; inert?: boolean }) {
  return <div className={css.detailsCol} data-app-details-column="" inert={inert ? true : undefined}>{children}</div>
}

function DragHandle({
  side,
  left,
  label,
  onStart,
  onDrag,
  onEnd,
}: {
  side: 'sidebar' | 'details'
  left: number
  label: string
  onStart(): void
  onDrag(dx: number): void
  onEnd(): void
}) {
  const [dragging, setDragging] = React.useState(false)
  const origin = React.useRef(0)
  const latest = React.useRef(0)
  const frame = React.useRef<number | null>(null)
  const callbacks = React.useRef({ onStart, onDrag, onEnd })
  callbacks.current = { onStart, onDrag, onEnd }

  const onPointerDown = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)
    origin.current = event.clientX
    latest.current = event.clientX
    callbacks.current.onStart()
    setDragging(true)
  }, [])
  const onPointerMove = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
    latest.current = event.clientX
    frame.current ??= requestAnimationFrame(() => {
      frame.current = null
      callbacks.current.onDrag(latest.current - origin.current)
    })
  }, [])
  const onPointerUp = React.useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (!event.currentTarget.hasPointerCapture(event.pointerId)) return
    event.currentTarget.releasePointerCapture(event.pointerId)
    if (frame.current !== null) {
      cancelAnimationFrame(frame.current)
      frame.current = null
    }
    callbacks.current.onDrag(latest.current - origin.current)
    setDragging(false)
    callbacks.current.onEnd()
  }, [])

  return (
    <div
      className={css.handle}
      style={{ left }}
      role="separator"
      aria-label={label}
      aria-orientation="vertical"
      data-side={side}
      data-dragging={dragging || undefined}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    />
  )
}

export function AppFrame({
  layout,
  mobileSidebarOpen,
  detailsOpen,
  detailsFullscreen = false,
  labels,
  sidebar,
  conversation,
  details,
  overlay,
  onCloseMobileSidebar,
}: AppFrameProps) {
  const closeMobileSidebar = React.useRef(onCloseMobileSidebar)
  closeMobileSidebar.current = onCloseMobileSidebar

  React.useEffect(() => {
    if (!layout.mobile || !mobileSidebarOpen) return
    const frame = layout.frameRef.current
    const sidebar = frame?.querySelector<HTMLElement>('[data-app-sidebar-column]')
    if (!sidebar) return
    const previouslyFocused = document.activeElement instanceof HTMLElement ? document.activeElement : null
    const focusable = () => Array.from(sidebar.querySelectorAll<HTMLElement>(
      'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    )).filter(element => !element.hidden && element.getClientRects().length > 0)
    const entryFrame = requestAnimationFrame(() => {
      sidebar.querySelector<HTMLElement>('[data-mobile-sidebar-close]')?.focus()
    })
    const keydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return
      const externalDismissLayer = Array.from(document.querySelectorAll<HTMLElement>(
        '[data-ternilo-dismiss-layer][data-state="open"]',
      )).some(layer => !sidebar.contains(layer))
      if (event.key === 'Escape') {
        if (externalDismissLayer) return
        event.preventDefault()
        closeMobileSidebar.current()
        return
      }
      if (event.key !== 'Tab') return
      // Radix menus and dialogs are portalled outside the drawer. Their own
      // focus scopes must win while open; otherwise this capture listener
      // pulls every Tab press back into the sidebar behind the portal.
      if (externalDismissLayer) return
      const candidates = focusable()
      if (candidates.length === 0) return
      const first = candidates[0]
      const last = candidates[candidates.length - 1]
      if (event.shiftKey && (document.activeElement === first || !sidebar.contains(document.activeElement))) {
        event.preventDefault()
        last.focus()
      } else if (!event.shiftKey && (document.activeElement === last || !sidebar.contains(document.activeElement))) {
        event.preventDefault()
        first.focus()
      }
    }
    document.addEventListener('keydown', keydown, true)
    return () => {
      cancelAnimationFrame(entryFrame)
      document.removeEventListener('keydown', keydown, true)
      requestAnimationFrame(() => {
        if (previouslyFocused?.isConnected) previouslyFocused.focus()
      })
    }
  }, [layout.mobile, layout.frameRef, mobileSidebarOpen])

  const mobileSidebarModal = layout.mobile && mobileSidebarOpen
  const detailsModal = detailsOpen && (layout.detailsOverlay || detailsFullscreen)
  return (
    <div
      ref={layout.frameRef}
      className={css.frame}
      style={{
        gridTemplateColumns: `${layout.columns.sidebar}px minmax(0, 1fr) ${layout.columns.details}px`,
      }}
      data-app-frame=""
      data-mobile={layout.mobile || undefined}
      data-mobile-sidebar-open={mobileSidebarOpen || undefined}
      data-sidebar-collapsed={layout.sidebarCollapsed || undefined}
      data-details-open={detailsOpen || undefined}
      data-details-overlay={detailsModal || undefined}
      data-details-collapsed={layout.columns.details === 0 || undefined}
      data-dragging={layout.dragging || undefined}
    >
      <div
        className={css.sidebarCol}
        data-app-sidebar-column=""
        inert={detailsModal || (layout.mobile && !mobileSidebarOpen) ? true : undefined}
      >{sidebar}</div>
      <CenterColumn inert={mobileSidebarModal || detailsModal}>{conversation}</CenterColumn>
      <DetailsColumn inert={mobileSidebarModal}>{details}</DetailsColumn>
      {mobileSidebarOpen && (
        <button
          type="button"
          tabIndex={-1}
          aria-label={labels.closeMobileSidebar}
          className={css.mobileBackdrop}
          data-mobile-sidebar-backdrop=""
          onClick={onCloseMobileSidebar}
        />
      )}
      <div className={css.overlayLayer} data-shell-overlay="">
        {overlay}
      </div>
      {!layout.mobile && !layout.sidebarCollapsed && (
        <DragHandle
          side="sidebar"
          left={layout.columns.sidebar}
          label={labels.resizeSidebar}
          onStart={layout.beginSidebarResize}
          onDrag={layout.resizeSidebar}
          onEnd={layout.endResize}
        />
      )}
      {!layout.mobile && layout.columns.details > 0 && (
        <DragHandle
          side="details"
          left={layout.viewport - layout.columns.details}
          label={labels.resizeDetails}
          onStart={layout.beginDetailsResize}
          onDrag={layout.resizeDetails}
          onEnd={layout.endResize}
        />
      )}
    </div>
  )
}
