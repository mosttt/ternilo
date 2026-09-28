import * as React from 'react'
import {
  computeColumns,
  CENTER_MIN,
  DETAILS_MAX,
  DETAILS_MIN,
  type LayoutPreferences,
  MOBILE_BREAKPOINT,
  normalizeLayoutPreferences,
  SIDEBAR_AUTO_COLLAPSE,
  SIDEBAR_COLLAPSED,
  SIDEBAR_DEFAULT,
  SIDEBAR_MAX,
  SIDEBAR_MIN,
  clampWidth,
} from '@/domain/layout'

export const LAYOUT_STORAGE_KEY = 'ternilo.layout-v1'

export function readLayoutPreferences(source: Storage = localStorage): LayoutPreferences {
  const stored = source.getItem(LAYOUT_STORAGE_KEY)
  if (!stored) return normalizeLayoutPreferences(undefined)
  try {
    return normalizeLayoutPreferences(JSON.parse(stored) as Partial<LayoutPreferences>)
  } catch {
    return normalizeLayoutPreferences(undefined)
  }
}

export function writeLayoutPreferences(value: LayoutPreferences, target: Storage = localStorage): void {
  target.setItem(LAYOUT_STORAGE_KEY, JSON.stringify(normalizeLayoutPreferences(value)))
}

export interface WorkbenchLayout {
  frameRef: React.RefObject<HTMLDivElement | null>
  viewport: number
  mobile: boolean
  narrow: boolean
  detailsOverlay: boolean
  sidebarCollapsed: boolean
  columns: ReturnType<typeof computeColumns>
  dragging: boolean
  setSidebarCollapsed(collapsed: boolean): void
  beginSidebarResize(): void
  resizeSidebar(dx: number): void
  beginDetailsResize(): void
  resizeDetails(dx: number): void
  endResize(): void
}

/** Owns persisted panel preferences and derives the currently renderable tracks. */
export function useWorkbenchLayout(detailsOpen: boolean, filesOpen = false): WorkbenchLayout {
  const frameRef = React.useRef<HTMLDivElement | null>(null)
  const [viewport, setViewport] = React.useState(() => window.innerWidth)
  const [preferences, setPreferences] = React.useState(readLayoutPreferences)
  const [filesWidth, setFilesWidth] = React.useState(() => {
    const stored = Number(localStorage.getItem('ternilo.files-panel-width'))
    return Number.isFinite(stored) && stored > 0 ? stored : null
  })
  const [narrowExpanded, setNarrowExpanded] = React.useState(false)
  const [dragging, setDragging] = React.useState(false)

  const mobile = viewport <= MOBILE_BREAKPOINT
  const narrow = viewport < SIDEBAR_AUTO_COLLAPSE
  const previousNarrow = React.useRef(narrow)

  React.useEffect(() => {
    writeLayoutPreferences(preferences)
  }, [preferences])
  React.useEffect(() => {
    if (filesWidth !== null) localStorage.setItem('ternilo.files-panel-width', String(filesWidth))
  }, [filesWidth])

  React.useEffect(() => {
    if (previousNarrow.current === narrow) return
    previousNarrow.current = narrow
    setNarrowExpanded(false)
  }, [narrow])

  React.useEffect(() => {
    const frame = frameRef.current
    if (!frame) return
    let animationFrame: number | null = null
    const observer = new ResizeObserver(() => {
      animationFrame ??= requestAnimationFrame(() => {
        animationFrame = null
        const width = frame.getBoundingClientRect().width
        if (width > 0) setViewport(width)
      })
    })
    observer.observe(frame)
    return () => {
      observer.disconnect()
      if (animationFrame !== null) cancelAnimationFrame(animationFrame)
    }
  }, [])

  const sidebarCollapsed = mobile
    ? false
    : narrow ? !narrowExpanded : preferences.sidebarCollapsed
  const detailsMaximum = filesOpen ? Math.max(DETAILS_MAX, viewport * .7) : DETAILS_MAX
  const preferredDetails = filesOpen ? filesWidth ?? (viewport - (sidebarCollapsed ? SIDEBAR_COLLAPSED : preferences.sidebarWidth)) / 2 : preferences.detailsWidth
  const columns = computeColumns(
    viewport,
    sidebarCollapsed ? 0 : preferences.sidebarWidth,
    detailsOpen && !mobile ? preferredDetails : 0,
    detailsMaximum,
    filesOpen ? 440 : CENTER_MIN,
  )
  const detailsOverlay = detailsOpen && columns.details === 0
  const columnsRef = React.useRef(columns)
  columnsRef.current = columns

  const sidebarBase = React.useRef(SIDEBAR_DEFAULT)
  const detailsBase = React.useRef(0)

  const setSidebarCollapsed = React.useCallback((collapsed: boolean) => {
    if (mobile) return
    if (narrow) {
      setNarrowExpanded(!collapsed)
      return
    }
    setPreferences(current => ({
      ...current,
      sidebarWidth: collapsed ? current.sidebarWidth : SIDEBAR_DEFAULT,
      sidebarCollapsed: collapsed,
    }))
  }, [mobile, narrow])

  const beginSidebarResize = React.useCallback(() => {
    sidebarBase.current = columnsRef.current.sidebar
    setDragging(true)
  }, [])
  const resizeSidebar = React.useCallback((dx: number) => {
    const sidebarWidth = clampWidth(sidebarBase.current + dx, SIDEBAR_MIN, SIDEBAR_MAX)
    setPreferences(current => ({ ...current, sidebarWidth, sidebarCollapsed: false }))
  }, [])
  const beginDetailsResize = React.useCallback(() => {
    detailsBase.current = columnsRef.current.details
    setDragging(true)
  }, [])
  const resizeDetails = React.useCallback((dx: number) => {
    const detailsWidth = clampWidth(detailsBase.current - dx, DETAILS_MIN, detailsMaximum)
    if (filesOpen) setFilesWidth(detailsWidth)
    else setPreferences(current => ({ ...current, detailsWidth }))
  }, [detailsMaximum, filesOpen])
  const endResize = React.useCallback(() => setDragging(false), [])

  return {
    frameRef,
    viewport,
    mobile,
    narrow,
    detailsOverlay,
    sidebarCollapsed,
    columns,
    dragging,
    setSidebarCollapsed,
    beginSidebarResize,
    resizeSidebar,
    beginDetailsResize,
    resizeDetails,
    endResize,
  }
}
