import { describe, expect, it } from 'vitest'
import type { TrajectoryRecord, TrajectoryTurn } from '@/domain/trajectory'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/trajectory'
import type { SessionEvent } from '@/types'
import { filterTrajectoryTurns } from './trajectory-view'

const t = ((key: keyof typeof zh) => zh[key]) as Translate<'trajectory'>

function record(key: string, kind: TrajectoryRecord['kind'], summary: string): TrajectoryRecord {
  return {
    key,
    event: { seq: key.length, type: 'custom', run_id: 'run-1', occurred_at_ms: 1 } as SessionEvent,
    relatedEvents: [], kind, tag: kind, title: kind, summary, step: 0, depth: 0, running: false, error: false,
  }
}

function turn(records: TrajectoryRecord[]): TrajectoryTurn {
  return {
    runId: 'run-1', number: 1, startedAt: 1, endedAt: 2, durationMs: 1, status: 'complete',
    records, groups: [{ key: 'message', title: 'Message', records }], boundaryEvents: [],
  }
}

describe('trajectory selected-record projection', () => {
  it('keeps the selected record visible without discarding the active search', () => {
    const selected = record('selected', 'assistant', 'selected output')
    const match = record('match', 'user', 'needle')
    const result = filterTrajectoryTurns([turn([selected, match])], 'needle', false, selected.key, t)
    expect(result[0]?.records.map(item => item.key)).toEqual(['selected', 'match'])
  })

  it('keeps a selected call visible while other calls are hidden', () => {
    const selected = record('selected-tool', 'tool', 'selected call')
    const other = record('other-tool', 'tool', 'other call')
    const result = filterTrajectoryTurns([turn([selected, other])], '', true, selected.key, t)
    expect(result[0]?.records.map(item => item.key)).toEqual(['selected-tool'])
  })
})
