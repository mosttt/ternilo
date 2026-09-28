import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { installScrollbarActivity } from './scrollbar'

let surface: HTMLDivElement
let dispose: () => void

beforeEach(() => {
  vi.useFakeTimers()
  surface = document.createElement('div')
  surface.style.overflowY = 'auto'
  Object.defineProperties(surface, { clientHeight: { value: 100 }, scrollHeight: { value: 500 } })
  surface.innerHTML = '<button>item</button>'
  document.body.append(surface)
  dispose = installScrollbarActivity()
})

afterEach(() => {
  dispose()
  surface.remove()
  vi.useRealTimers()
})

it('does not reveal scrollbars on mount, restoration, or pointer entry without movement', () => {
  surface.dispatchEvent(new Event('scroll'))
  surface.dispatchEvent(new MouseEvent('pointerenter', { bubbles: true }))
  surface.dispatchEvent(new MouseEvent('pointermove', { movementX: 0, movementY: 0 } as MouseEventInit))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
})

it('reveals scrollable ancestors on user input and keeps them visible during momentum', () => {
  surface.firstElementChild!.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: 30 }))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(true)
  vi.advanceTimersByTime(1000)
  surface.dispatchEvent(new Event('scroll'))
  vi.advanceTimersByTime(1000)
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(true)
  vi.advanceTimersByTime(300)
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
  surface.dispatchEvent(new Event('scroll'))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
})

it('supports touch and keyboard without activating clipped or non-scrolling containers', () => {
  surface.dispatchEvent(new KeyboardEvent('keydown', { key: 'a' }))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
  surface.dispatchEvent(new KeyboardEvent('keydown', { key: 'PageDown' }))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(true)
  window.dispatchEvent(new Event('blur'))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
  surface.dispatchEvent(new Event('touchmove'))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(true)
  window.dispatchEvent(new Event('blur'))
  surface.style.overflowY = 'hidden'
  surface.dispatchEvent(new WheelEvent('wheel'))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
})

it('releases attributes, timers, and event listeners on disposal', () => {
  surface.dispatchEvent(new WheelEvent('wheel'))
  dispose()
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
  expect(vi.getTimerCount()).toBe(0)
  surface.dispatchEvent(new WheelEvent('wheel'))
  expect(surface.hasAttribute('data-scrollbar-active')).toBe(false)
})
