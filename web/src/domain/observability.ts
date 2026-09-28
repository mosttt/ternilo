import type { LocalSession, SessionEvent } from '@/types'
import { buildToolTraces } from './events'
import { executionActivityPhase, type ExecutionActivityPhase, type ExecutionPhase } from './turn-activity'

export type PendingInteraction = 'approval' | 'plan-review' | 'question'
export type ActivityState = 'warning' | 'waiting' | 'running' | 'completed' | 'idle'

export interface SubagentView {
  id: string
  provider: string
  label: string
  task: string
  supportsFollowup?: boolean
  sessionId?: string
  transcriptKind: 'conversation' | 'process_lifecycle'
  status: 'running' | 'idle' | 'completed' | 'failed' | 'cancelled'
  output?: string
  error?: string
  createdAt: number
  updatedAt: number
}

export interface SessionObservation {
  latestSeq: number
  activeRuns: string[]
  workspaceWaitingRuns: string[]
  executionPhases: Record<string, ExecutionActivityPhase>
  pending: Record<string, PendingInteraction>
  subagents: Record<string, SubagentView>
  lastTerminalSeq?: number
}

export interface SessionStatusView {
  state: ActivityState
  kind: PendingInteraction | 'workspace-wait' | 'subagent-wait' | 'capacity-wait' | 'running' | 'subagents' | 'completed' | 'idle'
  count?: number
}

export const emptySessionObservation: SessionObservation = {
  latestSeq: -1,
  activeRuns: [],
  workspaceWaitingRuns: [],
  executionPhases: {},
  pending: {},
  subagents: {},
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined
}

function finite(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

function parseJson(value: string): unknown {
  try { return JSON.parse(value) } catch { return null }
}

function interactionKind(question: Record<string, unknown>): PendingInteraction {
  if (record(question.tool_approval)) return 'approval'
  if (record(question.presentation)?.kind === 'plan_review') return 'plan-review'
  return 'question'
}

function parseSubagent(value: unknown): SubagentView | null {
  const item = record(value)
  const id = text(item?.subagent_id)
  const status = text(item?.status)
  if (!item || !id || !['running', 'idle', 'completed', 'failed', 'cancelled'].includes(status ?? '')) return null
  return {
    id,
    provider: text(item.provider) ?? '',
    label: text(item.label) ?? id,
    task: text(item.task) ?? '',
    supportsFollowup: typeof item.supports_followup === 'boolean'
      ? item.supports_followup
      : undefined,
    sessionId: text(item.session_id),
    transcriptKind: item.transcript_kind === 'conversation'
      ? 'conversation'
      : 'process_lifecycle',
    status: status as SubagentView['status'],
    output: text(item.output),
    error: text(item.error),
    createdAt: finite(item.created_at_ms) ?? 0,
    updatedAt: finite(item.updated_at_ms) ?? 0,
  }
}

/** Fold only durable canonical events into the status facts used by the sidebar. */
export function reduceSessionObservation(
  previous: SessionObservation,
  incoming: readonly SessionEvent[],
): SessionObservation {
  if (!incoming.length) return previous
  const activeRuns = new Set(previous.activeRuns)
  const workspaceWaitingRuns = new Set(previous.workspaceWaitingRuns)
  const executionPhases = { ...previous.executionPhases }
  const pending = { ...previous.pending }
  const subagents = { ...previous.subagents }
  let latestSeq = previous.latestSeq
  let lastTerminalSeq = previous.lastTerminalSeq

  for (const event of [...incoming].sort((left, right) => left.seq - right.seq)) {
    if (event.seq <= latestSeq) continue
    latestSeq = event.seq
    if (event.type === 'turn_started') activeRuns.add(event.run_id)
    if (event.type === 'workspace_execution_waiting' && activeRuns.has(event.run_id)) workspaceWaitingRuns.add(event.run_id)
    if (event.type === 'workspace_execution_acquired') workspaceWaitingRuns.delete(event.run_id)
    const executionPhase = executionActivityPhase(event)
    if (executionPhase && activeRuns.has(event.run_id)) executionPhases[event.run_id] = executionPhase
    if (event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') {
      activeRuns.delete(event.run_id)
      workspaceWaitingRuns.delete(event.run_id)
      delete executionPhases[event.run_id]
      lastTerminalSeq = event.seq
    }
    if (event.type === 'user_question_asked') {
      const question = record(event.question)
      const id = text(question?.id)
      if (question && id) pending[id] = interactionKind(question)
    }
    if (event.type === 'user_question_answered') {
      const id = text(record(event.answer)?.question_id)
      if (id) delete pending[id]
    }
    if (event.type === 'subagent_updated') {
      const subagent = parseSubagent(event.subagent)
      if (subagent) subagents[subagent.id] = subagent
    }
  }
  return {
    latestSeq,
    activeRuns: [...activeRuns],
    workspaceWaitingRuns: [...workspaceWaitingRuns],
    executionPhases,
    pending,
    subagents,
    ...(lastTerminalSeq === undefined ? {} : { lastTerminalSeq }),
  }
}

export function executionWaitingStatus(phase: ExecutionPhase | undefined): SessionStatusView | null {
  const kind = phase === 'waiting_for_workspace' ? 'workspace-wait'
    : phase === 'waiting_for_subagents' ? 'subagent-wait'
      : phase === 'waiting_for_capacity' ? 'capacity-wait' : null
  return kind ? { state: 'waiting', kind } : null
}

export function sessionStatuses(observation: SessionObservation, viewedThroughSeq = -1): readonly SessionStatusView[] {
  const pending = Object.values(observation.pending)
  const pendingKind = (['approval', 'plan-review', 'question'] as const).find(kind => pending.includes(kind))
  const runningSubagents = Object.values(observation.subagents).filter(item => item.status === 'running').length
  const secondary = runningSubagents > 0
    ? { state: 'running', kind: 'subagents', count: runningSubagents } as const
    : null
  if (pendingKind) return secondary
    ? [{ state: 'warning', kind: pendingKind }, secondary]
    : [{ state: 'warning', kind: pendingKind }]
  const currentRun = observation.activeRuns.at(-1)
  const waiting = currentRun ? executionWaitingStatus(observation.workspaceWaitingRuns.includes(currentRun)
    ? 'waiting_for_workspace' : observation.executionPhases[currentRun]) : null
  if (waiting) return secondary ? [waiting, secondary] : [waiting]
  if (observation.activeRuns.length > 0) return secondary
    ? [{ state: 'running', kind: 'running' }, secondary]
    : [{ state: 'running', kind: 'running' }]
  if (secondary) return [secondary]
  if (observation.lastTerminalSeq !== undefined && observation.lastTerminalSeq > viewedThroughSeq) {
    return [{ state: 'completed', kind: 'completed' }]
  }
  return [{ state: 'idle', kind: 'idle' }]
}

export function deriveSubagents(events: readonly SessionEvent[]): SubagentView[] {
  return Object.values(reduceSessionObservation(emptySessionObservation, events).subagents)
    .sort((left, right) => left.createdAt - right.createdAt || left.id.localeCompare(right.id))
}

function canonicalSubagentStatus(events: readonly SessionEvent[], supportsFollowup?: boolean) {
  const activeRuns = new Set<string>()
  let terminal: SubagentView['status'] | null = null
  let updatedAt = 0
  for (const event of [...events].sort((left, right) => left.seq - right.seq)) {
    if (event.type === 'turn_started') {
      activeRuns.add(event.run_id)
      updatedAt = event.occurred_at_ms
    }
    if (event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') {
      activeRuns.delete(event.run_id)
      terminal = event.type === 'turn_finished'
        ? supportsFollowup === true ? 'idle' : 'completed'
        : event.type === 'turn_failed' ? 'failed' : 'cancelled'
      updatedAt = event.occurred_at_ms
    }
  }
  return activeRuns.size > 0
    ? { status: 'running' as const, updatedAt }
    : terminal ? { status: terminal, updatedAt } : null
}

/** Use each canonical child Session as the durable status source for Team members. */
export function mergeCanonicalSubagentEvents(
  subagents: readonly SubagentView[],
  memberEvents: Readonly<Record<string, readonly SessionEvent[]>>,
): SubagentView[] {
  return subagents.map(subagent => {
    const status = canonicalSubagentStatus(memberEvents[subagent.id] ?? [], subagent.supportsFollowup)
    return status && status.updatedAt >= subagent.updatedAt
      ? { ...subagent, status: status.status, updatedAt: status.updatedAt }
      : subagent
  })
}

export interface JobView {
  id: string
  command: string
  status: 'running' | 'completed' | 'failed' | 'cancelled'
  detail?: string
  startedAt: number
  finishedAt?: number
}

function jobSnapshot(value: unknown): Omit<JobView, 'startedAt' | 'finishedAt'> | null {
  const item = record(value)
  const id = text(item?.job_id)
  const command = text(item?.command)
  const status = text(item?.status)
  if (!item || !id || command === undefined || !['running', 'completed', 'failed', 'cancelled'].includes(status ?? '')) return null
  const result = record(item.result)
  const exitCode = finite(result?.exit_code)
  const detail = text(item.error)
    ?? (result?.timed_out === true ? 'timed_out' : exitCode !== undefined ? `exit ${exitCode}` : undefined)
  return { id, command, status: status as JobView['status'], detail }
}

interface JobObservation {
  seq: number
  observedAt: number
  sourceStartedAt: number
  job: Omit<JobView, 'startedAt' | 'finishedAt'>
}

/** Project background jobs from canonical lifecycle events and legacy completed tool results. */
export function deriveJobs(events: readonly SessionEvent[]): JobView[] {
  const rows = new Map<string, JobView>()
  const observations: JobObservation[] = events
    .filter(event => event.type === 'job_updated')
    .flatMap(event => {
      const job = jobSnapshot(event.job)
      return job ? [{
        seq: event.seq,
        observedAt: event.occurred_at_ms,
        sourceStartedAt: event.occurred_at_ms,
        job,
      }] : []
    })
  const traces = [...buildToolTraces([...events]).values()]
    .filter(trace => ['job_start', 'job_output', 'job_list', 'job_kill'].includes(trace.name) && trace.output && !trace.output.is_error)
  for (const trace of traces) {
    const decoded = parseJson(trace.output!.content)
    const snapshots = Array.isArray(decoded) ? decoded : [decoded]
    for (const value of snapshots) {
      const job = jobSnapshot(value)
      if (job) observations.push({
        seq: trace.finished!.seq,
        observedAt: trace.finished!.occurred_at_ms,
        sourceStartedAt: trace.started.occurred_at_ms,
        job,
      })
    }
  }
  observations.sort((left, right) => left.seq - right.seq)
  for (const observation of observations) {
    const previous = rows.get(observation.job.id)
    rows.set(observation.job.id, {
      ...observation.job,
      startedAt: previous
        ? Math.min(previous.startedAt, observation.sourceStartedAt)
        : observation.sourceStartedAt,
      ...(observation.job.status === 'running'
        ? {}
        : { finishedAt: previous?.finishedAt ?? observation.observedAt }),
    })
  }
  return [...rows.values()].sort((left, right) => {
    if ((left.status === 'running') !== (right.status === 'running')) return left.status === 'running' ? -1 : 1
    return left.status === 'running'
      ? left.startedAt - right.startedAt
      : (right.finishedAt ?? 0) - (left.finishedAt ?? 0)
  })
}

export type WorkflowStatus = 'running' | 'completed' | 'failed' | 'cancelled'
export interface WorkflowMemberView {
  sequence: number
  label: string
  phase: string | null
  subagentId: string
  status: WorkflowStatus
}
export interface WorkflowPhaseView {
  key: string
  title: string | null
  detail?: string
  members: WorkflowMemberView[]
}
export interface WorkflowRunView {
  id: string
  anchor: SessionEvent
  name: string
  description: string
  status: WorkflowStatus
  currentPhase: string | null
  phases: WorkflowPhaseView[]
  logs: string[]
  error?: string
  startedAt: number
  finishedAt?: number
}

function workflowId(event: SessionEvent): string | undefined {
  return text(event.workflow_id)
}

function phaseKey(value: string | null): string {
  return value === null ? 'missing' : `value:${value.length}:${value}`
}

export function deriveWorkflowRuns(events: readonly SessionEvent[]): WorkflowRunView[] {
  const runs = new Map<string, WorkflowRunView>()
  for (const event of [...events].sort((left, right) => left.seq - right.seq)) {
    const id = workflowId(event)
    if (!id) continue
    if (event.type === 'workflow_run_started') {
      const meta = record(event.meta)
      if (!meta) continue
      const phases = Array.isArray(meta.phases) ? meta.phases.flatMap(value => {
        const phase = record(value)
        const title = text(phase?.title)
        if (title === undefined) return []
        return [{ key: phaseKey(title), title, detail: text(phase?.detail), members: [] }]
      }) : []
      runs.set(id, {
        id,
        anchor: event,
        name: text(meta.name) ?? id,
        description: text(meta.description) ?? '',
        status: 'running',
        currentPhase: null,
        phases,
        logs: [],
        startedAt: event.occurred_at_ms,
      })
      continue
    }
    const run = runs.get(id)
    if (!run) continue
    if (event.type === 'workflow_phase_changed') run.currentPhase = text(event.title) ?? null
    if (event.type === 'workflow_log_emitted') {
      const message = text(event.message)
      if (message) run.logs = [...run.logs, message].slice(-8)
    }
    if (event.type === 'workflow_agent_started') {
      const sequence = finite(event.sequence)
      const label = text(event.label)
      const subagentId = text(event.subagent_id)
      if (sequence === undefined || label === undefined || subagentId === undefined) continue
      const phase = text(event.phase) ?? null
      const key = phaseKey(phase)
      let group = run.phases.find(item => item.key === key)
      if (!group) {
        group = { key, title: phase, members: [] }
        run.phases.push(group)
      }
      group.members.push({ sequence, label, phase, subagentId, status: 'running' })
    }
    if (event.type === 'workflow_agent_finished') {
      const sequence = finite(event.sequence)
      const outcome = text(event.outcome)
      if (sequence === undefined || !['completed', 'failed', 'cancelled'].includes(outcome ?? '')) continue
      for (const phase of run.phases) {
        const member = phase.members.find(item => item.sequence === sequence)
        if (member) member.status = outcome as WorkflowStatus
      }
    }
    if (event.type === 'workflow_run_finished') {
      const reason = text(event.stop_reason)
      run.status = reason === 'error' ? 'failed'
        : reason === 'cancelled' ? 'cancelled'
          : 'completed'
      run.error = text(event.error)
      run.finishedAt = event.occurred_at_ms
    }
  }
  return [...runs.values()].sort((left, right) => left.anchor.seq - right.anchor.seq)
}

export interface ScheduleEventView {
  operation: 'create' | 'delete' | 'dispatch'
  id: string
  prompt?: string
  recurrence?: 'once' | 'every'
  everySeconds?: number
  scheduledAt?: number
  nextScheduledAt?: number
  acceptedAt?: number
}

export function scheduleEventView(event: SessionEvent): ScheduleEventView | null {
  if (event.type !== 'schedule_changed') return null
  const change = record(event.change)
  const operation = text(change?.operation)
  if (!change || !['create', 'delete', 'dispatch'].includes(operation ?? '')) return null
  const exactOperation = operation as ScheduleEventView['operation']
  if (operation === 'create') {
    const schedule = record(change.schedule)
    const id = text(schedule?.id)
    if (!schedule || !id) return null
    const rule = record(schedule.rule)
    const kind = text(rule?.kind)
    return {
      operation: exactOperation,
      id,
      prompt: text(schedule.prompt),
      recurrence: kind === 'every' ? 'every' : 'once',
      everySeconds: finite(rule?.every_seconds),
      scheduledAt: finite(schedule.scheduled_at_ms),
    }
  }
  const id = text(change.id)
  if (!id) return null
  return operation === 'delete'
    ? { operation: exactOperation, id }
    : {
        operation: exactOperation,
        id,
        acceptedAt: finite(change.accepted_at_ms),
        nextScheduledAt: finite(change.next_scheduled_at_ms),
      }
}

export function addressableSubagentSession(subagent: SubagentView, sessions: readonly LocalSession[]) {
  if (!subagent.sessionId) return null
  return sessions.find(session => (
    session.identity.session_id === subagent.sessionId
    && session.subagent?.subagent_id === subagent.id
  )) ?? null
}
