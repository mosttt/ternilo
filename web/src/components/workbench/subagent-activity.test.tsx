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
