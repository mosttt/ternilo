import { afterEach, expect, it, vi } from 'vitest'
import { installDesktopContextMenu } from './desktop-context-menu'

let dispose: (() => void) | undefined

afterEach(() => {
  dispose?.()
  delete window.__TAURI__
})

it('preserves the default menu in a normal browser', () => {
  dispose = installDesktopContextMenu()
  const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
  document.body.dispatchEvent(event)
  expect(event.defaultPrevented).toBe(false)
})

it('suppresses only the desktop default menu without swallowing application events', () => {
  window.__TAURI__ = { core: { invoke: vi.fn() } }
  dispose = installDesktopContextMenu()
  const applicationMenu = vi.fn()
  document.body.addEventListener('contextmenu', applicationMenu, { once: true })
  const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
  document.body.dispatchEvent(event)
  expect(event.defaultPrevented).toBe(true)
  expect(applicationMenu).toHaveBeenCalledOnce()
  dispose()
  const restored = new MouseEvent('contextmenu', { bubbles: true, cancelable: true })
  document.body.dispatchEvent(restored)
  expect(restored.defaultPrevented).toBe(false)
})
