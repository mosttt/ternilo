import type { SessionEvent } from '@/types'
import { regenerationTarget } from './conversation-events'

export type ExecutionActivityPhase = 'running' | 'waiting_for_subagents' | 'waiting_for_capacity'
export type ExecutionPhase = ExecutionActivityPhase | 'waiting_for_workspace'

export function executionActivityPhase(event: SessionEvent): ExecutionActivityPhase | undefined {
  if (event.type !== 'execution_activity_changed') return undefined
  return event.phase === 'running' || event.phase === 'waiting_for_subagents' || event.phase === 'waiting_for_capacity'
    ? event.phase : undefined
}

export interface TurnActivity {
  runId: string
  startedAt: number
  phaseStartedAt: number
  phase: ExecutionPhase
}

/** Project the current open turn from its ordered, canonical lifecycle events. */
export function currentTurnActivity(events: readonly SessionEvent[]): TurnActivity | null {
  const open = new Map<string, TurnActivity & { workspaceWaiting: boolean; executionPhase: ExecutionActivityPhase }>()
  for (const event of events) {
    if (regenerationTarget(event) !== undefined) {
      for (const runId of open.keys()) if (runId !== event.run_id) open.delete(runId)
    }
    if (event.type === 'turn_started') {
      open.set(event.run_id, { runId: event.run_id, startedAt: event.occurred_at_ms, phaseStartedAt: event.occurred_at_ms, phase: 'running', workspaceWaiting: false, executionPhase: 'running' })
    }
    const current = open.get(event.run_id)
    const executionPhase = executionActivityPhase(event)
    if (current && (executionPhase || event.type === 'workspace_execution_waiting' || event.type === 'workspace_execution_acquired')) {
      const workspaceWaiting = event.type === 'workspace_execution_waiting' ? true
        : event.type === 'workspace_execution_acquired' ? false : current.workspaceWaiting
      const nextExecutionPhase = executionPhase ?? current.executionPhase
      const phase = workspaceWaiting ? 'waiting_for_workspace' : nextExecutionPhase
      open.set(event.run_id, { ...current, workspaceWaiting, executionPhase: nextExecutionPhase, phase,
        phaseStartedAt: phase === current.phase ? current.phaseStartedAt : event.occurred_at_ms })
    }
    if (event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') open.delete(event.run_id)
  }
  const current = [...open.values()].at(-1)
  return current ? { runId: current.runId, startedAt: current.startedAt, phaseStartedAt: current.phaseStartedAt, phase: current.phase } : null
}
