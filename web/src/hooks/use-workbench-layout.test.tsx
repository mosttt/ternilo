import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DETAILS_DEFAULT, SIDEBAR_COLLAPSED, SIDEBAR_DEFAULT } from '@/domain/layout'
import {
  LAYOUT_STORAGE_KEY,
  readLayoutPreferences,
  useWorkbenchLayout,
  type WorkbenchLayout,
} from './use-workbench-layout'

let root: Root
let host: HTMLDivElement
let width = 1920
let fireResize: (() => void) | null = null
let current: WorkbenchLayout

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

class ResizeObserverStub {
  readonly callback: ResizeObserverCallback
  constructor(callback: ResizeObserverCallback) { this.callback = callback }
  observe() { fireResize = () => this.callback([], this as unknown as ResizeObserver) }
  disconnect() { fireResize = null }
  unobserve() {}
}

function Harness({ detailsOpen = true }: { detailsOpen?: boolean }) {
  current = useWorkbenchLayout(detailsOpen)
  return <div ref={current.frameRef} />
}

function resize(next: number) {
  width = next
  act(() => {
    fireResize?.()
    vi.advanceTimersByTime(16)
  })
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage())
  localStorage.clear()
  width = 1920
  fireResize = null
  vi.useFakeTimers()
  vi.stubGlobal('ResizeObserver', ResizeObserverStub)
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => window.setTimeout(() => callback(0), 16))
  vi.stubGlobal('cancelAnimationFrame', (handle: number) => window.clearTimeout(handle))
  Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: width })
  Element.prototype.getBoundingClientRect = function () {
    return { width, height: 900, x: 0, y: 0, left: 0, top: 0, right: width, bottom: 900, toJSON: () => ({}) }
  }
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('useWorkbenchLayout', () => {
  it('auto-collapses below 1024, allows a manual reopen, and restores wide preferences', () => {
    act(() => root.render(<Harness />))
    expect(current.columns).toEqual({ sidebar: 280, center: 1280, details: 360 })

    resize(980)
    expect(current.narrow).toBe(true)
    expect(current.sidebarCollapsed).toBe(true)
    expect(current.columns.sidebar).toBe(SIDEBAR_COLLAPSED)
    expect(current.detailsOverlay).toBe(true)

    act(() => current.setSidebarCollapsed(false))
    expect(current.sidebarCollapsed).toBe(false)
    expect(current.columns.sidebar).toBe(SIDEBAR_DEFAULT)

    resize(1920)
    expect(current.narrow).toBe(false)
    expect(current.sidebarCollapsed).toBe(false)
    expect(current.columns.details).toBe(DETAILS_DEFAULT)
  })

  it('persists drag preferences and bases details drag on its conceded rendered width', () => {
    act(() => root.render(<Harness />))
    act(() => {
      current.beginSidebarResize()
      current.resizeSidebar(70)
      current.endResize()
    })
    expect(current.columns.sidebar).toBe(350)
    expect(readLayoutPreferences()).toMatchObject({ sidebarWidth: 350 })

    resize(1320)
    expect(current.columns.details).toBe(330)
    act(() => {
      current.beginDetailsResize()
      current.resizeDetails(10)
      current.endResize()
    })
    expect(readLayoutPreferences()).toMatchObject({ detailsWidth: 320 })
    resize(1920)
    expect(current.columns.details).toBe(320)
  })

  it('restores persisted widths in a new mount', () => {
    localStorage.setItem(LAYOUT_STORAGE_KEY, JSON.stringify({ sidebarWidth: 410, detailsWidth: 500, sidebarCollapsed: false }))
    act(() => root.render(<Harness />))
    expect(current.columns).toMatchObject({ sidebar: 410, details: 500 })
  })

  it('reopens a manually collapsed wide sidebar at the default width', () => {
    act(() => root.render(<Harness />))
    act(() => {
      current.beginSidebarResize()
      current.resizeSidebar(70)
      current.endResize()
      current.setSidebarCollapsed(true)
    })
    expect(current.sidebarCollapsed).toBe(true)

    act(() => current.setSidebarCollapsed(false))
    expect(current.sidebarCollapsed).toBe(false)
    expect(current.columns.sidebar).toBe(SIDEBAR_DEFAULT)
  })

  it('keeps the mobile overlay mode outside desktop track collapse semantics', () => {
    width = 760
    Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: width })
    act(() => root.render(<Harness />))
    expect(current.mobile).toBe(true)
    expect(current.sidebarCollapsed).toBe(false)
    expect(current.columns.details).toBe(0)
    expect(current.detailsOverlay).toBe(true)
  })

  it('projects details as an overlay when phone landscape cannot retain its minimum column', () => {
    width = 844
    Object.defineProperty(window, 'innerWidth', { configurable: true, writable: true, value: width })
    act(() => root.render(<Harness />))
    expect(current.mobile).toBe(false)
    expect(current.columns.details).toBe(0)
    expect(current.detailsOverlay).toBe(true)
  })
})
