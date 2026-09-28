// @vitest-environment jsdom

import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SessionLiveActivity } from '@/api/live-client'
import type { LocalSession, SessionEvent } from '@/types'
import { useSessionActivityRoster } from './session-activity-roster'

function session(id: string, parentSessionId?: string): LocalSession {
  return {
    identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: id },
    workspace_id: 'workspace', workspace_path: '/workspace', title: id,
    parent_session_id: parentSessionId,
    permissions: 'workspace_write', model: { provider: 'profile_default' }, agent_preset: 'standard',
    preset_plugins: [], profile_plugins: [], mode: 'execute', blank: false,
    created_at_ms: 1, updated_at_ms: 1,
  }
}

function event(seq: number, type: string, runId = 'run'): SessionEvent {
  return { seq, type, run_id: runId, occurred_at_ms: seq }
}

function Probe({ currentEvents, liveActivity, currentSessionId = 'current' }: {
  currentEvents: readonly SessionEvent[]
  liveActivity: Record<string, SessionLiveActivity>
  currentSessionId?: string
}) {
  const activity = useSessionActivityRoster(
    [session('current'), session('background'), session('child', 'current')],
    currentSessionId,
    currentEvents,
    liveActivity,
  )
  const kinds = (id: string) => activity.statuses(id).map(status => status.kind).join(',')
  return <output>{kinds('current')}:{kinds('background')}:{kinds('child')}:{activity.updatedAt('background', 1)}</output>
}

let root: Root
let host: HTMLDivElement

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

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('localStorage', memoryStorage())
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.unstubAllGlobals()
})

describe('session activity roster live projection', () => {
  it.each([
    ['waiting_for_subagents', 'subagent-wait'],
    ['waiting_for_capacity', 'capacity-wait'],
    ['waiting_for_workspace', 'workspace-wait'],
  ] as const)('reads optional %s live state for an unselected session without loading its history', (phase, kind) => {
    act(() => root.render(<Probe currentEvents={[]} liveActivity={{ background: {
      session_id: 'background', running: true, updated_at_ms: 50,
      execution: { run_id: 'background-run', phase },
    } }} />))
    expect(host.textContent).toBe(`idle:${kind}:idle:50`)
  })

  it('does not apply a previous run live wait to the selected new run', () => {
    act(() => root.render(<Probe currentEvents={[event(5, 'turn_started', 'new-run')]} liveActivity={{ current: {
      session_id: 'current', running: true, updated_at_ms: 3,
      execution: { run_id: 'old-run', phase: 'waiting_for_capacity' },
    } }} />))
    expect(host.textContent).toBe('running:idle:idle:1')
  })

  it('keeps canonical dependency waiting above a generic live running signal', () => {
    act(() => root.render(<Probe currentEvents={[event(0, 'turn_started'), { ...event(1, 'execution_activity_changed'), phase: 'waiting_for_subagents' }]} liveActivity={{ current: {
      session_id: 'current', running: true, updated_at_ms: 50,
    } }} />))
    expect(host.textContent).toBe('subagent-wait:idle:idle:1')
  })

  it('retains the selected session directory-wait label when the live roster reports an active run', () => {
    const events = [event(0, 'turn_started'), event(1, 'workspace_execution_waiting')]
    const liveActivity = { current: { session_id: 'current', running: true, updated_at_ms: 50 } }
    act(() => root.render(<Probe currentEvents={events} liveActivity={liveActivity} />))
    expect(host.textContent).toBe('workspace-wait:idle:idle:1')
    act(() => root.render(<Probe currentEvents={[...events, event(2, 'workspace_execution_acquired')]} liveActivity={liveActivity} />))
    expect(host.textContent).toBe('running:idle:idle:1')
  })

  it('combines selected canonical events with connection-wide activity without issuing requests', () => {
    act(() => root.render(<Probe
      currentEvents={[event(1, 'turn_started')]}
      liveActivity={{ background: { session_id: 'background', running: true, updated_at_ms: 50 } }}
    />))

    expect(host.textContent).toBe('running:running:idle:50')
  })

  it('projects a running child onto its parent and follows live deltas', () => {
    act(() => root.render(<Probe
      currentEvents={[]}
      liveActivity={{ child: { session_id: 'child', running: true, updated_at_ms: 60 } }}
    />))
    expect(host.textContent).toBe('subagents:idle:running:1')

    act(() => root.render(<Probe
      currentEvents={[]}
      liveActivity={{ child: { session_id: 'child', running: false, updated_at_ms: 70 } }}
    />))
    expect(host.textContent).toBe('idle:idle:completed:1')
  })

  it('keeps a background completion visible until that session is opened', () => {
    const render = (running: boolean, currentSessionId = 'current') => act(() => root.render(<Probe
      currentEvents={[]}
      currentSessionId={currentSessionId}
      liveActivity={{ background: { session_id: 'background', running, updated_at_ms: running ? 50 : 60 } }}
    />))
    render(true)
    expect(host.textContent).toBe('idle:running:idle:50')
    render(false)
    expect(host.textContent).toBe('idle:completed:idle:60')
    render(false)
    expect(host.textContent).toBe('idle:completed:idle:60')
    render(false, 'background')
    expect(host.textContent).toBe('idle:idle:idle:60')
    render(false)
    expect(host.textContent).toBe('idle:idle:idle:60')
  })

  it('does not mark an initially idle or currently viewed session as unseen', () => {
    const render = (running: boolean) => act(() => root.render(<Probe
      currentEvents={[]}
      liveActivity={{ current: { session_id: 'current', running, updated_at_ms: 70 } }}
    />))
    render(false)
    expect(host.textContent).toBe('idle:idle:idle:1')
    render(true)
    render(false)
    expect(host.textContent).toBe('idle:idle:idle:1')
  })

  it('records viewed completion once without writing storage for every streamed token', () => {
    const write = vi.spyOn(localStorage, 'setItem')
    const events = [event(0, 'turn_started')]
    for (let seq = 1; seq <= 100; seq += 1) {
      events.push(event(seq, 'assistant_message_delta'))
      act(() => root.render(<Probe currentEvents={[...events]} liveActivity={{}} />))
    }
    expect(write).not.toHaveBeenCalled()
    events.push(event(101, 'turn_finished'))
    act(() => root.render(<Probe currentEvents={events} liveActivity={{}} />))
    expect(host.textContent).toBe('idle:idle:idle:1')
    expect(write).toHaveBeenCalledExactlyOnceWith('ternilo.session-viewed-seq-v1', JSON.stringify({ current: 101 }))
  })
})
