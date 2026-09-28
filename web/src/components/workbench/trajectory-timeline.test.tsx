// @vitest-environment jsdom

import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { TrajectoryRecord, TrajectoryTurn } from '@/domain/trajectory'
import type { SessionEvent } from '@/types'
import { TrajectoryTimeline } from './trajectory-timeline'

function record(key: string, kind: TrajectoryRecord['kind'], start: number, end: number): TrajectoryRecord {
  const event = { seq: start + 1, type: `${kind}_event`, run_id: 'run', occurred_at_ms: start } as SessionEvent
  return {
    key, event, relatedEvents: [event], kind, tag: kind.toUpperCase(), title: key, summary: key,
    step: 1, depth: 0, timing: { startedAt: start, completedAt: end, durationMs: end - start },
    running: false, error: false,
  }
}

const records = [record('user', 'user', 0, 0), record('model', 'assistant', 100, 500), record('tool', 'tool', 500, 1_000)]
const turns: TrajectoryTurn[] = [{
  runId: 'run', number: 1, startedAt: 0, endedAt: 1_000, durationMs: 1_000, status: 'complete',
  records, groups: [{ key: 'step', title: 'Step', records }], boundaryEvents: [],
}]

function pointer(type: string, x: number, button: number, pointerId = 1, y = 0) {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, clientX: x, clientY: y, button })
  Object.defineProperty(event, 'pointerId', { value: pointerId })
  return event
}

describe('TrajectoryTimeline interactions', () => {
  let host: HTMLDivElement
  let root: Root

  beforeEach(() => {
    host = document.createElement('div')
    document.body.append(host)
    root = createRoot(host)
  })

  afterEach(() => {
    act(() => root.unmount())
    host.remove()
  })

  function render(onRangeChange = vi.fn(), onSelect = vi.fn()) {
    act(() => root.render(
      <TrajectoryTimeline turns={turns} mode="actual" range={null} onRangeChange={onRangeChange} onSelect={onSelect} />,
    ))
    const plot = host.querySelector<HTMLElement>('[data-trajectory-timeline-plot]')!
    plot.getBoundingClientRect = () => ({ left: 0, right: 100, top: 0, bottom: 60, width: 100, height: 60, x: 0, y: 0, toJSON: () => ({}) })
    Object.assign(plot, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn(), hasPointerCapture: () => true })
    return { plot, onRangeChange, onSelect }
  }

  it('commits a drag range and exposes touch-sized viewport controls', () => {
    const { plot, onRangeChange } = render()
    expect(host.querySelector('[aria-label="放大时间轴"]')).not.toBeNull()
    expect(host.querySelector('[aria-label="复位时间轴"]')).not.toBeNull()
    act(() => {
      plot.dispatchEvent(pointer('pointerdown', 20, 0))
      plot.dispatchEvent(pointer('pointermove', 70, 0))
      plot.dispatchEvent(pointer('pointerup', 70, 0))
    })
    expect(onRangeChange).toHaveBeenLastCalledWith({ start: 200, end: 700 })
  })

  it('zooms at the cursor, pans with the right button, and resets on right click', () => {
    const { plot, onRangeChange } = render()
    const before = host.querySelector('[data-trajectory-timeline-scale]')?.textContent
    act(() => plot.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, clientX: 50, deltaY: -300 })))
    const zoomed = host.querySelector('[data-trajectory-timeline-scale]')?.textContent
    expect(zoomed).not.toBe(before)
    act(() => {
      plot.dispatchEvent(pointer('pointerdown', 80, 2))
      plot.dispatchEvent(pointer('pointermove', 60, 2))
      plot.dispatchEvent(pointer('pointerup', 60, 2))
    })
    const panned = host.querySelector('[data-trajectory-timeline-scale]')?.textContent
    expect(panned).not.toBe(zoomed)
    act(() => {
      plot.dispatchEvent(pointer('pointerdown', 50, 2, 2))
      plot.dispatchEvent(pointer('pointerup', 50, 2, 2))
    })
    expect(onRangeChange).toHaveBeenLastCalledWith(null)
    expect(host.querySelector('[data-trajectory-timeline-scale]')?.textContent).toBe(before)
  })

  it('selects a span without starting a background range', () => {
    const { plot, onRangeChange, onSelect } = render()
    act(() => {
      plot.dispatchEvent(pointer('pointerdown', 75, 0, 1, 50))
      plot.dispatchEvent(pointer('pointerup', 75, 0, 1, 50))
    })
    expect(onRangeChange).toHaveBeenCalledWith(null)
    expect(onSelect).toHaveBeenCalledWith(records[2])
  })
})
