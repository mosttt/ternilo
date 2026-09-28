// @vitest-environment jsdom

import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { TrajectoryRecord, TrajectoryTurn } from '@/domain/trajectory'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/trajectory'
import { zh as chatZh } from '@/i18n/resources/chat'
import type { SessionEvent } from '@/types'
import { TrajectoryLedger } from './trajectory-ledger'

const t = ((key: keyof typeof zh, values?: Record<string, string | number>) => {
  let result = zh[key]
  for (const [name, value] of Object.entries(values ?? {})) result = result.replace(`{${name}}`, String(value))
  return result
}) as Translate<'trajectory'>
const errorT = ((key: keyof typeof chatZh) => chatZh[key]) as Translate<'chat'>

function turn(number: number, recordCount = 1): TrajectoryTurn {
  const records = Array.from({ length: recordCount }, (_, index): TrajectoryRecord => {
    const seq = number * 1_000 + index
    const event = { seq, type: 'user_message', run_id: `run-${number}`, occurred_at_ms: seq } as SessionEvent
    return {
      key: `record-${seq}`, event, relatedEvents: [event], kind: 'user', tag: 'USER', title: 'User', summary: `message ${seq}`,
      step: 0, depth: 0, running: false, error: false,
    }
  })
  return {
    runId: `run-${number}`, number, startedAt: number, endedAt: number + 1, durationMs: 1, status: 'complete',
  records, groups: [{ key: 'message', title: 'Message', records }], boundaryEvents: [],
  }
}

describe('TrajectoryLedger', () => {
  let host: HTMLDivElement
  let root: Root

  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
    vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
    vi.stubGlobal('matchMedia', () => ({ matches: false, addEventListener() {}, removeEventListener() {} }))
    host = document.createElement('div')
    host.className = 'conversation-scroll'
    document.body.append(host)
    root = createRoot(host)
  })

  afterEach(() => {
    act(() => root.unmount())
    host.remove()
    vi.unstubAllGlobals()
  })

  it('renders direct rows for a small ledger and wires selection and turn folding', () => {
    const onSelect = vi.fn()
    const onToggleTurn = vi.fn()
    act(() => root.render(<TrajectoryLedger turns={[turn(1)]} collapsed={new Set()} rangeKeys={null} selection={null} onToggleTurn={onToggleTurn} onSelect={onSelect} t={t} errorT={errorT} />))
    expect(host.querySelector('[data-trajectory-ledger]')?.hasAttribute('data-virtualized')).toBe(false)
    act(() => (host.querySelector('[data-trajectory-record]') as HTMLButtonElement).click())
    expect(onSelect).toHaveBeenCalledOnce()
    act(() => (host.querySelector('[aria-expanded="true"]') as HTMLButtonElement).click())
    expect(onToggleTurn).toHaveBeenCalledWith('run-1')
  })

  it('switches large histories to virtual rows', () => {
    act(() => root.render(<TrajectoryLedger turns={Array.from({ length: 55 }, (_, index) => turn(index + 1))} collapsed={new Set()} rangeKeys={null} selection={null} onToggleTurn={() => {}} onSelect={() => {}} t={t} errorT={errorT} />))
    expect(host.querySelector('[data-trajectory-ledger]')?.getAttribute('data-virtualized')).toBe('true')
  })
})
