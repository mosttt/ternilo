import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import {
  addressableSubagentSession, deriveJobs, deriveSubagents, deriveWorkflowRuns, emptySessionObservation,
  mergeCanonicalSubagentEvents, reduceSessionObservation, scheduleEventView, sessionStatuses,
} from './observability'
import type { LocalSession } from '@/types'

function event(seq: number, type: string, values: Record<string, unknown> = {}): SessionEvent {
  return { seq, type, run_id: String(values.run_id ?? 'run-1'), occurred_at_ms: seq * 1_000, ...values }
}

describe('canonical observability projections', () => {
  it('projects dependency and capacity waits without treating either as completion or directory release', () => {
    let observation = reduceSessionObservation(emptySessionObservation, [event(0, 'turn_started'), event(1, 'execution_activity_changed', { phase: 'waiting_for_subagents' })])
    expect(sessionStatuses(observation)).toEqual([{ state: 'waiting', kind: 'subagent-wait' }])
    observation = reduceSessionObservation(observation, [event(2, 'execution_activity_changed', { phase: 'waiting_for_capacity' })])
    expect(sessionStatuses(observation)).toEqual([{ state: 'waiting', kind: 'capacity-wait' }])
    expect(reduceSessionObservation(observation, [event(1, 'execution_activity_changed', { phase: 'running' })])).toEqual(observation)
    observation = reduceSessionObservation(observation, [event(3, 'workspace_execution_waiting'), event(4, 'execution_activity_changed', { phase: 'running' })])
    expect(sessionStatuses(observation)).toEqual([{ state: 'waiting', kind: 'workspace-wait' }])
    observation = reduceSessionObservation(observation, [event(5, 'workspace_execution_acquired')])
    expect(sessionStatuses(observation)).toEqual([{ state: 'running', kind: 'running' }])
    observation = reduceSessionObservation(observation, [event(6, 'turn_cancelled'), event(7, 'execution_activity_changed', { phase: 'waiting_for_capacity' })])
    expect(observation.executionPhases).toEqual({})
    expect(sessionStatuses(observation, 7)).toEqual([{ state: 'idle', kind: 'idle' }])
  })

  it('keeps directory waiting distinct from model activity across replay, acquisition and cancellation', () => {
    const waiting = reduceSessionObservation(emptySessionObservation, [event(0, 'turn_started'), event(1, 'user_message'), event(2, 'workspace_execution_waiting')])
    expect(sessionStatuses(waiting)).toEqual([{ state: 'waiting', kind: 'workspace-wait' }])
    const acquired = reduceSessionObservation(waiting, [event(3, 'workspace_execution_acquired')])
    expect(sessionStatuses(acquired)).toEqual([{ state: 'running', kind: 'running' }])
    expect(reduceSessionObservation(acquired, [event(2, 'workspace_execution_waiting')])).toEqual(acquired)
    const cancelled = reduceSessionObservation(waiting, [event(3, 'turn_cancelled')])
    expect(cancelled.workspaceWaitingRuns).toEqual([])
    expect(sessionStatuses(cancelled, 3)).toEqual([{ state: 'idle', kind: 'idle' }])
  })

  it('projects canonical child Session terminals over stale parent Subagent status', () => {
    const running = deriveSubagents([event(0, 'subagent_updated', { subagent: {
      subagent_id: 'agent-1', provider: 'in-process', label: 'Research', task: 'inspect',
      supports_followup: true, status: 'running', created_at_ms: 1, updated_at_ms: 1,
    } })])
    expect(mergeCanonicalSubagentEvents(running, {
      'agent-1': [
        event(0, 'turn_started', { run_id: 'child-run' }),
        event(1, 'turn_finished', { run_id: 'child-run' }),
      ],
    })[0]).toMatchObject({ status: 'idle', updatedAt: 1_000 })
    expect(mergeCanonicalSubagentEvents(running, {
      'agent-1': [event(0, 'turn_started', { run_id: 'child-run' })],
    })[0]?.status).toBe('running')
    expect(mergeCanonicalSubagentEvents(running, {
      'agent-1': [
        event(0, 'turn_started', { run_id: 'child-run' }),
        event(1, 'turn_failed', { run_id: 'child-run' }),
      ],
    })[0]?.status).toBe('failed')

    const newerParent = [{ ...running[0]!, status: 'running' as const, updatedAt: 2_000 }]
    expect(mergeCanonicalSubagentEvents(newerParent, {
      'agent-1': [
        event(0, 'turn_started', { run_id: 'child-run' }),
        event(1, 'turn_finished', { run_id: 'child-run' }),
      ],
    })[0]).toMatchObject({ status: 'running', updatedAt: 2_000 })
  })

  it('navigates subagents only through the explicit canonical Session binding', () => {
    const [subagent] = deriveSubagents([event(1, 'subagent_updated', { subagent: {
      subagent_id: 'agent-1', session_id: 'child-session-1', transcript_kind: 'conversation',
      provider: 'in-process', label: 'Research', task: 'inspect', status: 'idle',
      created_at_ms: 1, updated_at_ms: 2,
    } })])
    const session = (id: string, subagentId?: string): LocalSession => ({
      identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: id },
      workspace_id: 'workspace', workspace_path: '/tmp/workspace', title: id,
      permissions: 'workspace_write', model: { provider: 'profile_default' }, agent_preset: 'standard',
      preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 1,
      ...(subagentId ? { subagent: { subagent_id: subagentId, provider: 'in-process', transcript_kind: 'conversation' } as const } : {}),
    })
    expect(subagent).toMatchObject({
      id: 'agent-1', sessionId: 'child-session-1', transcriptKind: 'conversation',
    })
    expect(addressableSubagentSession(subagent!, [session('agent-1', 'agent-1')])).toBeNull()
    expect(addressableSubagentSession(subagent!, [session('child-session-1', 'wrong-agent')])).toBeNull()
    expect(addressableSubagentSession(subagent!, [session('child-session-1', 'agent-1')])?.identity.session_id)
      .toBe('child-session-1')
  })

  it('orders pending interaction above own and descendant activity, then exposes unviewed completion', () => {
    const observation = reduceSessionObservation(emptySessionObservation, [
      event(0, 'turn_started'),
      event(1, 'subagent_updated', { subagent: {
        subagent_id: 'agent-1', provider: 'in-process', label: 'Research', task: 'inspect',
        supports_followup: true, status: 'running', output: null, error: null, created_at_ms: 1, updated_at_ms: 2,
      } }),
      event(2, 'user_question_asked', { question: {
        id: 'q-1', question: 'Run?', options: [{ label: 'Allow once' }, { label: 'Deny' }], multi_select: false,
        tool_approval: { tool_name: 'shell', call_id: 'c', reason: 'test', arguments: {} },
      } }),
    ])
    expect(sessionStatuses(observation)).toEqual([
      { state: 'warning', kind: 'approval' },
      { state: 'running', kind: 'subagents', count: 1 },
    ])
    expect(deriveSubagents([event(1, 'subagent_updated', { subagent: {
      subagent_id: 'agent-1', provider: 'in-process', label: 'Research', task: 'inspect',
      supports_followup: true, status: 'idle', output: null, error: null,
      created_at_ms: 1, updated_at_ms: 2,
    } })])[0]?.supportsFollowup).toBe(true)

    const settled = reduceSessionObservation(observation, [
      event(3, 'user_question_answered', { answer: { question_id: 'q-1', selected: ['Allow once'] } }),
      event(4, 'subagent_updated', { subagent: {
        subagent_id: 'agent-1', provider: 'in-process', label: 'Research', task: 'inspect',
        supports_followup: true, status: 'idle', output: 'done', error: null, created_at_ms: 1, updated_at_ms: 4,
      } }),
      event(5, 'turn_finished'),
    ])
    expect(sessionStatuses(settled, 4)).toEqual([{ state: 'completed', kind: 'completed' }])
    expect(sessionStatuses(settled, 5)).toEqual([{ state: 'idle', kind: 'idle' }])
  })

  it('lets canonical job lifecycle events settle old tool observations without /jobs', () => {
    const events = [
      event(0, 'tool_call_started', { call: { id: 'call-1', name: 'job_start', arguments: { command: 'sleep 1' } } }),
      event(1, 'tool_call_finished', { call_id: 'call-1', name: 'job_start', output: { content: JSON.stringify({ job_id: 'job-1', command: 'sleep 1', status: 'running', result: null, error: null }), is_error: false } }),
      event(4, 'job_updated', { job: { job_id: 'job-1', command: 'sleep 1', status: 'completed', result: { exit_code: 0, stdout: '', stderr: '', timed_out: false }, error: null } }),
    ]
    expect(deriveJobs(events)).toEqual([{
      id: 'job-1', command: 'sleep 1', status: 'completed', detail: 'exit 0', startedAt: 0, finishedAt: 4_000,
    }])
    expect(deriveJobs([event(0, 'tool_call_finished', { call_id: 'x', name: 'shell', output: { content: '{}', is_error: false } })])).toEqual([])
  })

  it('projects failed and cancelled job terminals from canonical events', () => {
    const jobs = deriveJobs([
      event(1, 'job_updated', { job: { job_id: 'failed', command: 'exit 7', status: 'failed', result: { exit_code: 7, stdout: '', stderr: '', timed_out: false }, error: null } }),
      event(2, 'job_updated', { job: { job_id: 'cancelled', command: 'sleep 30', status: 'cancelled', result: null, error: null } }),
    ])
    expect(jobs.map(job => ({ id: job.id, status: job.status, detail: job.detail }))).toEqual([
      { id: 'cancelled', status: 'cancelled', detail: undefined },
      { id: 'failed', status: 'failed', detail: 'exit 7' },
    ])
  })

  it('folds workflow run, phase, member, logs, and terminal status by workflow id', () => {
    const events = [
      event(0, 'workflow_run_started', { workflow_id: 'wf-1', meta: { name: 'Review', description: 'Check code', phases: [{ title: 'Inspect', detail: 'Read files' }] } }),
      event(1, 'workflow_phase_changed', { workflow_id: 'wf-1', title: 'Inspect' }),
      event(2, 'workflow_agent_started', { workflow_id: 'wf-1', sequence: 1, label: 'Reviewer', phase: 'Inspect', subagent_id: 'agent-1' }),
      event(3, 'workflow_log_emitted', { workflow_id: 'wf-1', message: 'reading' }),
      event(4, 'workflow_agent_finished', { workflow_id: 'wf-1', sequence: 1, outcome: 'completed' }),
      event(5, 'workflow_run_finished', { workflow_id: 'wf-1', stop_reason: 'completed', agents_started: 1, error: null }),
    ]
    const [run] = deriveWorkflowRuns(events)
    expect(run).toMatchObject({ id: 'wf-1', name: 'Review', status: 'completed', currentPhase: 'Inspect', logs: ['reading'], finishedAt: 5_000 })
    expect(run?.phases[0]?.members).toEqual([{
      sequence: 1, label: 'Reviewer', phase: 'Inspect', subagentId: 'agent-1', status: 'completed',
    }])
  })

  it('preserves recurrence and next-dispatch facts from schedule events', () => {
    expect(scheduleEventView(event(1, 'schedule_changed', { change: {
      operation: 'create', schedule: {
        id: 'schedule-1', prompt: 'check status', rule: { kind: 'every', every_seconds: 300 },
        scheduled_at_ms: 10_000, created_at_ms: 1_000,
      },
    } }))).toEqual({
      operation: 'create', id: 'schedule-1', prompt: 'check status', recurrence: 'every', everySeconds: 300, scheduledAt: 10_000,
    })
    expect(scheduleEventView(event(2, 'schedule_changed', { change: {
      operation: 'dispatch', id: 'schedule-1', accepted_at_ms: 10_000, next_scheduled_at_ms: 310_000,
    } }))).toEqual({ operation: 'dispatch', id: 'schedule-1', acceptedAt: 10_000, nextScheduledAt: 310_000 })
  })
})
