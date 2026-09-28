import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ReasoningRow } from './reasoning-row'

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.useFakeTimers()
  vi.setSystemTime(1_500)
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.useRealTimers()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('ReasoningRow', () => {
  it('follows the latest streaming line, reports elapsed time and expands on demand', () => {
    act(() => root.render(<ReasoningRow reasoning={{
      text: 'first thought\nlatest thought', running: true, startedAt: 1_000,
    }} />))

    const trigger = host.querySelector<HTMLButtonElement>('[data-reasoning-row] [data-disclosure-row]')
    expect(trigger?.textContent).toContain('思考')
    expect(host.querySelector('[data-reasoning-summary]')?.textContent).toBe('latest thought')
    expect(host.querySelector('[data-reasoning-duration]')?.textContent).toBe('500 ms')
    expect(host.querySelector('[data-reasoning-body]')).toBeNull()

    act(() => {
      vi.setSystemTime(2_200)
      vi.advanceTimersByTime(100)
    })
    expect(host.querySelector('[data-reasoning-duration]')?.textContent).toBe('1.3 s')

    act(() => trigger?.dispatchEvent(new MouseEvent('click', { bubbles: true })))
    expect(trigger?.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelector('[data-reasoning-body]')?.textContent).toBe('first thought\nlatest thought')
  })

  it('uses the first line and the fixed duration after reasoning settles', () => {
    act(() => root.render(<ReasoningRow reasoning={{
      text: 'first thought\nlast thought', running: false, startedAt: 1_000,
      completedAt: 2_800, durationMs: 1_800,
    }} />))
    expect(host.querySelector('[data-reasoning-summary]')?.textContent).toBe('first thought')
    expect(host.querySelector('[data-reasoning-duration]')?.textContent).toBe('1.8 s')
    expect(host.querySelector('[data-reasoning-row]')?.getAttribute('data-state')).toBe('complete')
  })
})
