import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import type { TrajectoryRecord } from './trajectory'
import {
  buildTrajectoryTimeline,
  normalizeTimelineSelection,
  panTimelineRange,
  trajectoryKeysInRange,
  zoomTimelineRange,
} from './trajectory-timeline'

function record(key: string, startedAt: number, completedAt: number, kind: TrajectoryRecord['kind'] = 'tool'): TrajectoryRecord {
  const event = { seq: Number(key.replace(/\D/g, '')) || 1, type: 'tool_call_started', run_id: 'run', occurred_at_ms: startedAt } as SessionEvent
  return {
    key, event, relatedEvents: [event], kind, tag: kind.toUpperCase(), title: key, summary: key,
    step: 1, depth: 0, timing: { startedAt, completedAt, durationMs: completedAt - startedAt },
    running: false, error: false,
  }
}

describe('trajectory timeline projection', () => {
  it('removes every uncovered idle gap while preserving operation durations', () => {
    const records = [record('tool-1', 1_000, 2_000), record('tool-2', 4_000, 5_000), record('tool-3', 40_000, 41_000)]
    const actual = buildTrajectoryTimeline(records, 'actual')
    const compressed = buildTrajectoryTimeline(records, 'duration')
    expect(actual?.spans.map(span => [span.start, span.end])).toEqual([[1_000, 2_000], [4_000, 5_000], [40_000, 41_000]])
    expect(compressed?.spans.map(span => [span.start, span.end])).toEqual([[1_000, 2_000], [2_000, 3_000], [3_000, 4_000]])
    expect(compressed?.removedIdleMs).toBe(37_000)
  })

  it('uses a covered frontier so overlapping calls do not compress a gap twice', () => {
    const compressed = buildTrajectoryTimeline([
      record('tool-1', 1_000, 5_000),
      record('tool-2', 2_000, 3_000),
      record('tool-3', 10_000, 11_000),
    ], 'duration')
    expect(compressed?.spans.map(span => [span.start, span.end])).toEqual([
      [1_000, 5_000], [2_000, 3_000], [5_000, 6_000],
    ])
    expect(compressed?.removedIdleMs).toBe(5_000)
  })

  it('keeps point events selectable and assigns semantic lanes', () => {
    const point = record('user-1', 100, 100, 'user')
    const model = buildTrajectoryTimeline([point, record('assistant-2', 100, 200, 'assistant')], 'actual')
    expect(model?.duration).toBe(100)
    expect(model?.spans.map(span => span.lane)).toEqual([0, 1])
    expect(trajectoryKeysInRange(model, { start: 99, end: 101 })).toEqual(new Set(['user-1', 'assistant-2']))
  })
})

describe('trajectory timeline viewport', () => {
  const domain = { start: 0, end: 1_000 }

  it('zooms around the cursor and clamps the viewport to the domain', () => {
    expect(zoomTimelineRange(domain, 250, 0.5, domain, 20)).toEqual({ start: 125, end: 625 })
    expect(zoomTimelineRange({ start: 0, end: 100 }, 0, 0.01, domain, 20)).toEqual({ start: 0, end: 20 })
  })

  it('pans without changing zoom and stops at both boundaries', () => {
    expect(panTimelineRange({ start: 200, end: 500 }, 200, domain)).toEqual({ start: 400, end: 700 })
    expect(panTimelineRange({ start: 200, end: 500 }, -500, domain)).toEqual({ start: 0, end: 300 })
    expect(panTimelineRange({ start: 800, end: 1_000 }, 500, domain)).toEqual({ start: 800, end: 1_000 })
  })

  it('expands a click-sized selection into a useful bounded interval', () => {
    expect(normalizeTimelineSelection(500, 500, domain, 40)).toEqual({ start: 480, end: 520 })
    expect(normalizeTimelineSelection(5, 5, domain, 40)).toEqual({ start: 0, end: 40 })
  })
})
