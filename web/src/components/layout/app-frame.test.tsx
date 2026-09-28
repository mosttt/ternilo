import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkbenchLayout } from '@/hooks/use-workbench-layout'
import { AppFrame } from './app-frame'

let host: HTMLDivElement
let root: Root

function pointer(type: string, pointerId: number, clientX: number): Event {
  const event = new Event(type, { bubbles: true, cancelable: true })
  Object.defineProperties(event, {
    pointerId: { value: pointerId },
    clientX: { value: clientX },
  })
  return event
}

function createLayout(overrides: Partial<WorkbenchLayout> = {}): WorkbenchLayout {
  return {
    frameRef: React.createRef<HTMLDivElement>(),
    viewport: 1440,
    mobile: false,
    narrow: false,
    detailsOverlay: false,
    sidebarCollapsed: false,
    columns: { sidebar: 280, center: 800, details: 360 },
    dragging: false,
    setSidebarCollapsed: vi.fn(),
    beginSidebarResize: vi.fn(),
    resizeSidebar: vi.fn(),
    beginDetailsResize: vi.fn(),
    resizeDetails: vi.fn(),
    endResize: vi.fn(),
    ...overrides,
  }
}

function renderFrame(
  layout: WorkbenchLayout,
  mobileSidebarOpen = false,
  sidebar: React.ReactNode = 'Sidebar',
  detailsOpen = layout.columns.details > 0 || layout.detailsOverlay,
) {
  const onCloseMobileSidebar = vi.fn()
  act(() => root.render(
    <AppFrame
      layout={layout}
      mobileSidebarOpen={mobileSidebarOpen}
      detailsOpen={detailsOpen}
      labels={{
        closeMobileSidebar: 'Close sidebar',
        resizeSidebar: 'Resize sidebar',
        resizeDetails: 'Resize details',
      }}
      sidebar={<aside className="app-sidebar">{sidebar}</aside>}
      conversation={<main className="conversation-column">Conversation</main>}
      details={<aside className="details-panel">Details</aside>}
      onCloseMobileSidebar={onCloseMobileSidebar}
    />,
  ))
  return onCloseMobileSidebar
}

beforeEach(() => {
  vi.useFakeTimers()
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => window.setTimeout(() => callback(0), 16))
  vi.stubGlobal('cancelAnimationFrame', (handle: number) => window.clearTimeout(handle))
  const captured = new WeakMap<Element, number>()
  Element.prototype.setPointerCapture = function (pointerId: number) { captured.set(this, pointerId) }
  Element.prototype.releasePointerCapture = function () { captured.delete(this) }
  Element.prototype.hasPointerCapture = function (pointerId: number) { return captured.get(this) === pointerId }
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

describe('AppFrame', () => {
  it('mounts the three domain columns and wires pointer-captured resizing', () => {
    const layout = createLayout()
    renderFrame(layout)

    const frame = host.querySelector('[data-app-frame]') as HTMLDivElement
    expect(frame.style.gridTemplateColumns).toBe('280px minmax(0, 1fr) 360px')
    expect(frame.textContent).toContain('Sidebar')
    expect(frame.textContent).toContain('Conversation')
    expect(frame.textContent).toContain('Details')
    expect(host.querySelectorAll('[role="separator"]')).toHaveLength(2)

    const sidebarHandle = host.querySelector('[aria-label="Resize sidebar"]')!
    act(() => sidebarHandle.dispatchEvent(pointer('pointerdown', 7, 280)))
    act(() => {
      sidebarHandle.dispatchEvent(pointer('pointermove', 7, 315))
      sidebarHandle.dispatchEvent(pointer('pointermove', 7, 340))
      vi.advanceTimersByTime(16)
    })
    expect(layout.beginSidebarResize).toHaveBeenCalledOnce()
    expect(layout.resizeSidebar).toHaveBeenLastCalledWith(60)

    act(() => sidebarHandle.dispatchEvent(pointer('pointerup', 7, 340)))
    expect(layout.endResize).toHaveBeenCalledOnce()
    expect(sidebarHandle.hasPointerCapture(7)).toBe(false)
  })

  it('removes desktop handles and exposes one dismissible sidebar backdrop on mobile', () => {
    const layout = createLayout({
      viewport: 390,
      mobile: true,
      narrow: true,
      columns: { sidebar: 280, center: 110, details: 0 },
    })
    const onClose = renderFrame(layout, true)

    const frame = host.querySelector('[data-app-frame]')!
    expect(frame.hasAttribute('data-mobile')).toBe(true)
    expect(frame.hasAttribute('data-mobile-sidebar-open')).toBe(true)
    expect(host.querySelectorAll('[role="separator"]')).toHaveLength(0)
    const backdrop = host.querySelector('[aria-label="Close sidebar"]') as HTMLButtonElement
    act(() => backdrop.click())
    expect(onClose).toHaveBeenCalledOnce()
  })

  it('lets a nested topmost overlay consume Escape before closing the mobile sidebar', () => {
    const layout = createLayout({
      viewport: 390,
      mobile: true,
      narrow: true,
      columns: { sidebar: 280, center: 110, details: 0 },
    })
    const onClose = renderFrame(layout, true)
    const topmostLayer = document.body.appendChild(document.createElement('div'))
    topmostLayer.setAttribute('data-ternilo-dismiss-layer', '')
    topmostLayer.setAttribute('data-state', 'open')
    act(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    expect(onClose).not.toHaveBeenCalled()

    topmostLayer.remove()
    act(() => document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    expect(onClose).toHaveBeenCalledOnce()
  })

  it('suspends the drawer Tab trap while an external portal owns focus', () => {
    const layout = createLayout({
      viewport: 390,
      mobile: true,
      narrow: true,
      columns: { sidebar: 280, center: 110, details: 0 },
    })
    renderFrame(layout, true, <button type="button">Inside drawer</button>)
    const drawerButton = host.querySelector('[data-app-sidebar-column] button') as HTMLButtonElement
    drawerButton.getClientRects = () => [drawerButton.getBoundingClientRect()] as unknown as DOMRectList
    act(() => vi.advanceTimersByTime(16))

    const portal = document.body.appendChild(document.createElement('div'))
    portal.setAttribute('data-ternilo-dismiss-layer', '')
    portal.setAttribute('data-state', 'open')
    const portalButton = portal.appendChild(document.createElement('button'))
    portalButton.focus()
    const portalTab = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })
    act(() => portalButton.dispatchEvent(portalTab))
    expect(portalTab.defaultPrevented).toBe(false)
    expect(document.activeElement).toBe(portalButton)

    portal.remove()
    const outside = document.body.appendChild(document.createElement('button'))
    outside.focus()
    const drawerTab = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })
    act(() => outside.dispatchEvent(drawerTab))
    expect(drawerTab.defaultPrevented).toBe(true)
    expect(host.querySelector('[data-app-sidebar-column]')?.contains(document.activeElement)).toBe(true)
    outside.remove()
  })

  it('marks a zero-column open inspector as an overlay', () => {
    const layout = createLayout({
      viewport: 844,
      narrow: true,
      detailsOverlay: true,
      columns: { sidebar: 56, center: 788, details: 0 },
    })
    renderFrame(layout)
    const frame = host.querySelector('[data-app-frame]')!
    expect(frame.hasAttribute('data-details-open')).toBe(true)
    expect(frame.hasAttribute('data-details-overlay')).toBe(true)
    expect(host.querySelectorAll('[aria-label="Resize details"]')).toHaveLength(0)
  })
})
