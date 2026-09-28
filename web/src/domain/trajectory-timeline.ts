import type { TrajectoryRecord } from './trajectory'

export type TrajectoryTimelineMode = 'duration' | 'actual'

export interface TrajectoryTimeRange {
  start: number
  end: number
}

export interface TrajectoryTimelineSpan extends TrajectoryTimeRange {
  key: string
  record: TrajectoryRecord
  lane: 0 | 1 | 2
}

export interface TrajectoryTimelineModel {
  mode: TrajectoryTimelineMode
  start: number
  end: number
  duration: number
  spans: TrajectoryTimelineSpan[]
  removedIdleMs: number
}

function lane(record: TrajectoryRecord): 0 | 1 | 2 | null {
  if (record.kind === 'user') return 0
  if (record.kind === 'assistant') return 1
  if (record.kind === 'tool' || record.kind === 'code') return 2
  return null
}

function rawSpan(record: TrajectoryRecord): TrajectoryTimelineSpan | null {
  const recordLane = lane(record)
  if (recordLane === null) return null
  const start = record.timing?.startedAt ?? record.event.occurred_at_ms
  const explicitEnd = record.timing?.completedAt
  const durationEnd = record.timing?.durationMs == null ? undefined : start + record.timing.durationMs
  const end = Math.max(start, explicitEnd ?? durationEnd ?? start)
  return { key: record.key, record, lane: recordLane, start, end }
}

/**
 * Projects real spans onto a duration axis by removing only uncovered idle gaps.
 * Overlapping spans extend the covered frontier and never remove the same gap twice.
 */
export function compressTrajectoryIdle(spans: TrajectoryTimelineSpan[]): {
  spans: TrajectoryTimelineSpan[]
  removedIdleMs: number
} {
  if (!spans.length) return { spans: [], removedIdleMs: 0 }
  const offsets = new Map<string, number>()
  let coveredUntil: number | undefined
  let removedIdleMs = 0
  for (const span of [...spans].sort((left, right) => left.start - right.start || left.end - right.end)) {
    if (coveredUntil !== undefined && span.start > coveredUntil) {
      removedIdleMs += span.start - coveredUntil
    }
    offsets.set(span.key, removedIdleMs)
    coveredUntil = coveredUntil === undefined ? span.end : Math.max(coveredUntil, span.end)
  }
  return {
    spans: spans.map(span => {
      const offset = offsets.get(span.key) ?? 0
      return { ...span, start: span.start - offset, end: span.end - offset }
    }),
    removedIdleMs,
  }
}

export function buildTrajectoryTimeline(
  records: TrajectoryRecord[],
  mode: TrajectoryTimelineMode,
): TrajectoryTimelineModel | null {
  const raw = records.flatMap(record => {
    const span = rawSpan(record)
    return span === null ? [] : [span]
  })
  if (!raw.length) return null
  const projection = mode === 'duration'
    ? compressTrajectoryIdle(raw)
    : { spans: raw, removedIdleMs: 0 }
  const start = Math.min(...projection.spans.map(span => span.start))
  const measuredEnd = Math.max(...projection.spans.map(span => span.end))
  const end = measuredEnd > start ? measuredEnd : start + 1
  return {
    mode,
    start,
    end,
    duration: end - start,
    spans: projection.spans,
    removedIdleMs: projection.removedIdleMs,
  }
}

export function clampTimelineRange(range: TrajectoryTimeRange, domain: TrajectoryTimeRange): TrajectoryTimeRange {
  const domainDuration = Math.max(1, domain.end - domain.start)
  const requestedDuration = Math.min(domainDuration, Math.max(1, range.end - range.start))
  const start = Math.min(domain.end - requestedDuration, Math.max(domain.start, range.start))
  return { start, end: start + requestedDuration }
}

export function zoomTimelineRange(
  viewport: TrajectoryTimeRange,
  anchor: number,
  scale: number,
  domain: TrajectoryTimeRange,
  minimumDuration: number,
): TrajectoryTimeRange {
  const domainDuration = Math.max(1, domain.end - domain.start)
  const duration = Math.min(domainDuration, Math.max(Math.min(domainDuration, minimumDuration), (viewport.end - viewport.start) * scale))
  const ratio = (anchor - viewport.start) / Math.max(1, viewport.end - viewport.start)
  return clampTimelineRange({ start: anchor - duration * ratio, end: anchor + duration * (1 - ratio) }, domain)
}

export function panTimelineRange(
  viewport: TrajectoryTimeRange,
  delta: number,
  domain: TrajectoryTimeRange,
): TrajectoryTimeRange {
  return clampTimelineRange({ start: viewport.start + delta, end: viewport.end + delta }, domain)
}

export function normalizeTimelineSelection(
  first: number,
  second: number,
  domain: TrajectoryTimeRange,
  minimumDuration: number,
): TrajectoryTimeRange {
  const start = Math.min(first, second)
  const end = Math.max(first, second)
  if (end - start >= minimumDuration) return clampTimelineRange({ start, end }, domain)
  const middle = (start + end) / 2
  return clampTimelineRange({ start: middle - minimumDuration / 2, end: middle + minimumDuration / 2 }, domain)
}

export function trajectoryKeysInRange(model: TrajectoryTimelineModel | null, range: TrajectoryTimeRange | null) {
  if (model === null || range === null) return null
  return new Set(model.spans.filter(span => span.start <= range.end && span.end >= range.start).map(span => span.key))
}
