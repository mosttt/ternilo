import * as React from 'react'
import type { SessionLiveActivity } from '@/api/live-client'
import {
  emptySessionObservation, executionWaitingStatus, reduceSessionObservation, sessionStatuses,
  type SessionObservation, type SessionStatusView,
} from '@/domain/observability'
import type { LocalSession, SessionEvent } from '@/types'

const VIEWED_STORAGE = 'ternilo.session-viewed-seq-v1'

function readViewed(): Record<string, number> {
  try {
    const value = JSON.parse(localStorage.getItem(VIEWED_STORAGE) ?? '{}') as unknown
    if (typeof value !== 'object' || value === null || Array.isArray(value)) return {}
    return Object.fromEntries(Object.entries(value).flatMap(([key, seq]) =>
      typeof seq === 'number' && Number.isFinite(seq) ? [[key, seq]] : []))
  } catch { return {} }
}

function writeViewed(value: Record<string, number>) {
  localStorage.setItem(VIEWED_STORAGE, JSON.stringify(value))
}

function runningDescendants(
  sessions: readonly LocalSession[],
  activity: Readonly<Record<string, SessionLiveActivity>>,
) {
  const parentById = new Map(sessions.map(session => [
    session.identity.session_id,
    session.parent_session_id ?? null,
  ]))
  const counts: Record<string, number> = {}
  for (const session of sessions) {
    const id = session.identity.session_id
    if (!activity[id]?.running) continue
    const visited = new Set<string>()
    let parent = parentById.get(id) ?? null
    while (parent && !visited.has(parent)) {
      visited.add(parent)
      counts[parent] = (counts[parent] ?? 0) + 1
      parent = parentById.get(parent) ?? null
    }
  }
  return counts
}

export interface SessionActivityRoster {
  statuses(id: string): readonly SessionStatusView[]
  updatedAt(id: string, fallback: number): number
}

/**
 * Project sidebar activity from the selected Session events and the shared live
 * workbench stream. This hook performs no network requests of its own.
 */
export function useSessionActivityRoster(
  sessions: readonly LocalSession[],
  currentSessionId: string | null,
  currentSessionEvents: readonly SessionEvent[] = [],
  liveActivity: Readonly<Record<string, SessionLiveActivity>> = {},
): SessionActivityRoster {
  const viewed = React.useRef<Record<string, number> | null>(null)
  if (viewed.current === null) viewed.current = readViewed()
  const previousActivity = React.useRef(liveActivity)
  const [unviewedCompletions, setUnviewedCompletions] = React.useState<ReadonlySet<string>>(() => new Set())
  const currentObservation = React.useMemo(
    () => reduceSessionObservation(emptySessionObservation, currentSessionEvents),
    [currentSessionEvents],
  )
  const observations = React.useMemo<Record<string, SessionObservation>>(
    () => currentSessionId ? { [currentSessionId]: currentObservation } : {},
    [currentObservation, currentSessionId],
  )
  const descendantCounts = React.useMemo(
    () => runningDescendants(sessions, liveActivity),
    [liveActivity, sessions],
  )

  React.useEffect(() => {
    const previous = previousActivity.current
    previousActivity.current = liveActivity
    const next = new Set(unviewedCompletions)
    for (const [id, activity] of Object.entries(liveActivity)) {
      if (previous[id]?.running && !activity.running && id !== currentSessionId) next.add(id)
    }
    if (currentSessionId) next.delete(currentSessionId)
    for (const id of next) {
      if (!sessions.some(session => session.identity.session_id === id)) next.delete(id)
    }
    if (next.size !== unviewedCompletions.size || [...next].some(id => !unviewedCompletions.has(id))) {
      setUnviewedCompletions(next)
    }
  }, [currentSessionId, liveActivity, sessions, unviewedCompletions])

  React.useEffect(() => {
    const terminalSeq = currentObservation.lastTerminalSeq
    if (!currentSessionId || terminalSeq === undefined || (viewed.current?.[currentSessionId] ?? -1) >= terminalSeq) return
    viewed.current = { ...viewed.current, [currentSessionId]: terminalSeq }
    writeViewed(viewed.current)
  }, [currentObservation.lastTerminalSeq, currentSessionId])

  return React.useMemo(() => ({
    statuses(id: string) {
      const observation = observations[id] ?? emptySessionObservation
      const viewedThrough = id === currentSessionId ? observation.latestSeq : viewed.current?.[id] ?? -1
      const canonical = sessionStatuses(observation, viewedThrough)
      const live = liveActivity[id]
      const currentRun = observation.activeRuns.at(-1)
      const liveWaiting = live?.running && live.execution && (!currentRun || currentRun === live.execution.run_id)
        ? executionWaitingStatus(live.execution.phase) : null
      const pending = canonical[0]?.state === 'warning' || canonical[0]?.state === 'waiting' ? canonical[0] : liveWaiting
      const ownRunning = liveActivity[id]?.running || canonical.some(status => status.kind === 'running')
      const descendants = descendantCounts[id] ?? 0
      const secondary = descendants > 0
        ? { state: 'running', kind: 'subagents', count: descendants } as const
        : null
      if (pending) return secondary ? [pending, secondary] : [pending]
      if (ownRunning) {
        const primary = { state: 'running', kind: 'running' } as const
        return secondary ? [primary, secondary] : [primary]
      }
      if (secondary) return [secondary]
      if (unviewedCompletions.has(id)) return [{ state: 'completed', kind: 'completed' }]
      return canonical
    },
    updatedAt(id: string, fallback: number) {
      return Math.max(fallback, liveActivity[id]?.updated_at_ms ?? 0)
    },
  }), [currentSessionId, descendantCounts, liveActivity, observations, unviewedCompletions])
}
