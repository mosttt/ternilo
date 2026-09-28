import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { currentTurnActivity } from './turn-activity'

function event(seq: number, type: string, runId = 'run-1'): SessionEvent {
  return { seq, type, run_id: runId, occurred_at_ms: seq * 1_000 }
}

const waiting = [event(0, 'turn_started'), event(1, 'user_message'), event(2, 'workspace_execution_waiting')]

describe('directory waiting turn activity', () => {
  it('restores dependency and capacity waits and only resets timing when the phase changes', () => {
    const events = [event(0, 'turn_started'), { ...event(2, 'execution_activity_changed'), phase: 'waiting_for_subagents' }]
    expect(currentTurnActivity(events)).toMatchObject({ phase: 'waiting_for_subagents', phaseStartedAt: 2_000 })
    events.push({ ...event(3, 'execution_activity_changed'), phase: 'waiting_for_subagents' })
    expect(currentTurnActivity(events)?.phaseStartedAt).toBe(2_000)
    events.push({ ...event(4, 'execution_activity_changed'), phase: 'waiting_for_capacity' })
    expect(currentTurnActivity(events)).toMatchObject({ phase: 'waiting_for_capacity', phaseStartedAt: 4_000 })
    events.push({ ...event(5, 'execution_activity_changed'), phase: 'running' })
    expect(currentTurnActivity(events)).toMatchObject({ phase: 'running', phaseStartedAt: 5_000 })
  })

  it('does not mistake foreground admission for acquiring the physical directory', () => {
    const events = [...waiting, { ...event(3, 'execution_activity_changed'), phase: 'running' }]
    expect(currentTurnActivity(events)?.phase).toBe('waiting_for_workspace')
    expect(currentTurnActivity([...events, event(4, 'workspace_execution_acquired')])?.phase).toBe('running')
  })

  it.each(['turn_finished', 'turn_failed', 'turn_cancelled'])('ignores old execution phases after %s and leaves the next run independent', type => {
    const events = [event(0, 'turn_started'), { ...event(1, 'execution_activity_changed'), phase: 'waiting_for_capacity' }, event(2, type)]
    expect(currentTurnActivity(events)).toBeNull()
    expect(currentTurnActivity([...events, event(3, 'turn_started', 'run-2'), { ...event(4, 'execution_activity_changed'), phase: 'waiting_for_subagents' }]))
      .toMatchObject({ runId: 'run-2', phase: 'running', phaseStartedAt: 3_000 })
  })

  it('restores waiting from history and starts response timing only after acquiring the directory', () => {
    expect(currentTurnActivity(waiting)).toEqual({ runId: 'run-1', startedAt: 0, phaseStartedAt: 2_000, phase: 'waiting_for_workspace' })
    expect(currentTurnActivity([...waiting, event(40, 'workspace_execution_acquired')])).toEqual({ runId: 'run-1', startedAt: 0, phaseStartedAt: 40_000, phase: 'running' })
  })

  it.each(['turn_finished', 'turn_failed', 'turn_cancelled'])('clears waiting on %s without carrying it into the next run', type => {
    const completed = [...waiting, event(3, type)]
    expect(currentTurnActivity(completed)).toBeNull()
    expect(currentTurnActivity([...completed, event(4, 'turn_started', 'run-2')])).toMatchObject({ runId: 'run-2', phase: 'running' })
  })

  it('does not let an older run change the current run phase', () => {
    const events = [...waiting, event(3, 'turn_started', 'run-2'), event(4, 'workspace_execution_acquired'), event(5, 'turn_cancelled')]
    expect(currentTurnActivity(events)).toMatchObject({ runId: 'run-2', phase: 'running', phaseStartedAt: 3_000 })
  })
})
