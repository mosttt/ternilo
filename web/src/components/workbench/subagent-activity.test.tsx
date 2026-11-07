import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { expect, it, vi } from 'vitest'
import type { LocalSession, SessionEvent } from '@/types'
import { SubagentActivity } from './subagent-activity'
import { ExecutionAvailabilityContext } from './execution-availability'

it('announces startup before admission, opens only the bound child, and retires stale activity', () => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  const host = document.createElement('div')
  document.body.append(host)
  const root = createRoot(host)
  const open = vi.fn()
  const events: SessionEvent[] = [
    { seq: 1, occurred_at_ms: 1, run_id: 'run', type: 'turn_started' },
    { seq: 2, occurred_at_ms: 2, run_id: 'run', type: 'tool_call_started', call: { id: 'spawn', name: 'spawn_agent', arguments: { task: 'Review' } } },
  ]
  const child = { identity: { session_id: 'public-child' }, parent_session_id: 'parent', title: 'Reviewer',
    subagent: { subagent_id: 'runtime-child', transcript_kind: 'conversation' } } as LocalSession
  const render = (sessions: LocalSession[], running?: boolean, offline = false) => act(() => root.render(
    <ExecutionAvailabilityContext.Provider value={offline ? 'offline' : 'live'}>
      <SubagentActivity sessionId="parent" events={events} sessions={sessions} onOpenSession={open}
        activity={running == null ? {} : { 'public-child': { session_id: 'public-child', running, updated_at_ms: 5 } }} />
    </ExecutionAvailabilityContext.Provider>,
  ))
  try {
    render([])
    expect(host.textContent).toContain('正在启动子 Agent')
    render([child, { ...child, parent_session_id: 'unrelated', title: 'Other' }], true)
    expect(host.textContent).toContain('1 个子 Agent 正在协作')
    expect(host.textContent).not.toContain('Other')
    act(() => host.querySelector<HTMLButtonElement>('button')!.click())
    expect(open).toHaveBeenCalledWith('public-child')
    render([child], true, true)
    expect(host.textContent).toBe('')
    events.push({ seq: 3, occurred_at_ms: 3, run_id: 'run', type: 'turn_cancelled' })
    render([child], false)
    expect(host.textContent).toBe('')
  } finally {
    act(() => root.unmount())
    host.remove()
    delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
  }
})

for (const status of ['failed', 'completed', 'cancelled', 'idle']) {
  it(`prefers the newer ${status} snapshot but accepts a later restart`, () => {
    ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
    const host = document.createElement('div')
    const root = createRoot(host)
    const child = { identity: { session_id: 'child' }, parent_session_id: 'parent', title: 'Reviewer',
      subagent: { subagent_id: 'runtime', transcript_kind: 'conversation' } } as LocalSession
    const events: SessionEvent[] = [{ seq: 1, occurred_at_ms: 20, run_id: 'run', type: 'subagent_updated',
      subagent: { subagent_id: 'runtime', status, updated_at_ms: 20 } }]
    const render = (updated: number) => act(() => root.render(<SubagentActivity sessionId="parent"
      events={events} sessions={[child]} onOpenSession={vi.fn()}
      activity={{ child: { session_id: 'child', running: true, updated_at_ms: updated } }} />))
    try {
      render(5)
      expect(host.textContent).toBe('')
      render(20)
      expect(host.textContent).toBe('')
      render(21)
      expect(host.textContent).toContain('1 个子 Agent 正在协作')
      events[0] = { ...events[0], subagent: { subagent_id: 'runtime', status: 'running', updated_at_ms: 30 } }
      act(() => root.render(<SubagentActivity sessionId="parent" events={events} sessions={[child]}
        onOpenSession={vi.fn()} activity={{ child: { session_id: 'child', running: false, updated_at_ms: 25 } }} />))
      expect(host.textContent).toContain('1 个子 Agent 正在协作')
    } finally {
      act(() => root.unmount())
      delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
    }
  })
}
