import * as React from 'react'
import { ApiError } from '@/api/client'
import type { SessionLiveActivity } from '@/api/live-client'
import type {
  AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskDraft, LocalSession, SessionEvent,
} from '@/types'
import { agentTeamApi } from './agent-team-api'

export type AgentTeamMutation = 'create' | 'replace' | 'delete' | 'send' | 'read'

const EMPTY_SESSIONS: readonly LocalSession[] = []
const EMPTY_ACTIVITY: Readonly<Record<string, SessionLiveActivity>> = {}

function canonicalMemberSessions(snapshot: AgentTeamSnapshot, sessions: readonly LocalSession[]) {
  const sessionBySubagent = new Map(sessions.flatMap(session => (
    session.subagent ? [[session.subagent.subagent_id, session] as const] : []
  )))
  return snapshot.members.flatMap(member => {
    if (!member.subagent_id) return []
    const session = sessionBySubagent.get(member.subagent_id)
    return session ? [[member.subagent_id, session] as const] : []
  })
}

function canonicalMemberHistoryKey(
  snapshot: AgentTeamSnapshot,
  sessions: readonly LocalSession[],
  activity: Readonly<Record<string, SessionLiveActivity>>,
) {
  const members = canonicalMemberSessions(snapshot, sessions)
  return [snapshot.team_id, ...members.map(([subagentId, session]) => {
    const sessionId = session.identity.session_id
    const live = activity[sessionId]
    return [
      subagentId,
      sessionId,
      live?.running ? 1 : 0,
    ].join('\0')
  })].join('\n')
}

export function useAgentTeam(
  sessionId: string | null,
  active: boolean,
  sessions: readonly LocalSession[] = EMPTY_SESSIONS,
  liveSnapshot: AgentTeamSnapshot | null = null,
  liveActivity: Readonly<Record<string, SessionLiveActivity>> = EMPTY_ACTIVITY,
) {
  const [snapshot, setSnapshot] = React.useState<AgentTeamSnapshot | null>(null)
  const [memberEvents, setMemberEvents] = React.useState<Record<string, SessionEvent[]>>({})
  const [loading, setLoading] = React.useState(false)
  const [loadError, setLoadError] = React.useState('')
  const [actionError, setActionError] = React.useState('')
  const [conflict, setConflict] = React.useState(false)
  const [pending, setPending] = React.useState<AgentTeamMutation | null>(null)
  const [stateSessionId, setStateSessionId] = React.useState(sessionId)
  const requestRevision = React.useRef(0)
  const pendingToken = React.useRef<symbol | null>(null)
  const sessionEpoch = React.useRef({ id: sessionId, value: 0 })
  const sessionsRef = React.useRef(sessions)
  const liveActivityRef = React.useRef(liveActivity)
  const liveSnapshotRef = React.useRef(liveSnapshot)
  const memberHistoryKey = React.useRef('')
  const historyRevision = React.useRef(0)
  sessionsRef.current = sessions
  liveActivityRef.current = liveActivity
  liveSnapshotRef.current = liveSnapshot
  const liveHistoryKey = liveSnapshot
    ? canonicalMemberHistoryKey(liveSnapshot, sessions, liveActivity)
    : ''

  if (sessionEpoch.current.id !== sessionId) {
    sessionEpoch.current = { id: sessionId, value: sessionEpoch.current.value + 1 }
    requestRevision.current += 1
    historyRevision.current += 1
    pendingToken.current = null
    memberHistoryKey.current = ''
  }

  const refresh = React.useCallback(async (signal?: AbortSignal, quiet = false) => {
    const targetSessionId = sessionId
    const targetEpoch = sessionEpoch.current.value
    const isCurrent = () => sessionEpoch.current.id === targetSessionId
      && sessionEpoch.current.value === targetEpoch
    if (!targetSessionId || !isCurrent()) return null
    const revision = ++requestRevision.current
    const memberRevision = ++historyRevision.current
    setStateSessionId(targetSessionId)
    if (!quiet) setLoading(true)
    setLoadError('')
    try {
      const next = await agentTeamApi.snapshot(targetSessionId, signal)
      const members = canonicalMemberSessions(next, sessionsRef.current)
      const histories = await Promise.all(members.map(async ([subagentId, childSession]) => (
        [subagentId, await agentTeamApi.events(childSession.identity.session_id, signal)] as const
      )))
      if (isCurrent() && revision === requestRevision.current) {
        setSnapshot(next)
        if (memberRevision === historyRevision.current) {
          setMemberEvents(Object.fromEntries(histories))
          memberHistoryKey.current = canonicalMemberHistoryKey(
            next,
            sessionsRef.current,
            liveActivityRef.current,
          )
        }
      }
      return next
    } catch (cause) {
      if (signal?.aborted) return null
      if (isCurrent() && revision === requestRevision.current) {
        setLoadError(cause instanceof Error ? cause.message : String(cause))
      }
      return null
    } finally {
      if (!quiet && isCurrent() && revision === requestRevision.current) setLoading(false)
    }
  }, [sessionId])

  const loadMemberHistories = React.useCallback(async (
    next: AgentTeamSnapshot,
    nextHistoryKey: string,
    signal?: AbortSignal,
  ) => {
    const targetSessionId = sessionId
    const targetEpoch = sessionEpoch.current.value
    const isCurrent = () => sessionEpoch.current.id === targetSessionId
      && sessionEpoch.current.value === targetEpoch
    if (!targetSessionId || !isCurrent()) return
    const revision = ++historyRevision.current
    const members = canonicalMemberSessions(next, sessionsRef.current)
    try {
      const histories = await Promise.all(members.map(async ([subagentId, childSession]) => (
        [subagentId, await agentTeamApi.events(childSession.identity.session_id, signal)] as const
      )))
      if (!signal?.aborted && isCurrent() && revision === historyRevision.current) {
        setMemberEvents(Object.fromEntries(histories))
        memberHistoryKey.current = nextHistoryKey
      }
    } catch (cause) {
      if (signal?.aborted) return
      if (isCurrent() && revision === historyRevision.current) {
        setLoadError(cause instanceof Error ? cause.message : String(cause))
      }
    }
  }, [sessionId])

  React.useLayoutEffect(() => {
    requestRevision.current += 1
    pendingToken.current = null
    memberHistoryKey.current = ''
    setStateSessionId(sessionId)
    setSnapshot(null)
    setMemberEvents({})
    setLoadError('')
    setActionError('')
    setConflict(false)
    setPending(null)
    if (!active || !sessionId) return
    const controller = new AbortController()
    const initialLiveSnapshot = liveSnapshotRef.current
    if (initialLiveSnapshot) setSnapshot(initialLiveSnapshot)
    else void refresh(controller.signal)
    return () => {
      controller.abort()
    }
  }, [active, refresh, sessionId])

  React.useEffect(() => {
    if (!active || !sessionId || !liveSnapshot) return
    requestRevision.current += 1
    setStateSessionId(sessionId)
    setSnapshot(liveSnapshot)
    setLoading(false)
    setLoadError('')
  }, [active, liveSnapshot, sessionId])

  React.useEffect(() => {
    if (!active || !sessionId || !liveHistoryKey || memberHistoryKey.current === liveHistoryKey) return
    const next = liveSnapshotRef.current
    if (!next) return
    const controller = new AbortController()
    void loadMemberHistories(next, liveHistoryKey, controller.signal)
    return () => controller.abort()
  }, [active, liveHistoryKey, loadMemberHistories, sessionId])

  const mutate = React.useCallback(async (
    kind: AgentTeamMutation,
    operation: (id: string) => Promise<unknown>,
  ) => {
    const targetSessionId = sessionId
    const targetEpoch = sessionEpoch.current.value
    const isCurrent = () => sessionEpoch.current.id === targetSessionId
      && sessionEpoch.current.value === targetEpoch
    if (!targetSessionId || !isCurrent() || pendingToken.current !== null) return false
    const token = Symbol(kind)
    pendingToken.current = token
    setStateSessionId(targetSessionId)
    setPending(kind)
    setActionError('')
    setConflict(false)
    try {
      await operation(targetSessionId)
      if (!isCurrent() || pendingToken.current !== token) return false
      await refresh(undefined, true)
      return isCurrent() && pendingToken.current === token
    } catch (cause) {
      if (!isCurrent() || pendingToken.current !== token) return false
      if (cause instanceof ApiError && cause.status === 409) {
        setConflict(true)
        await refresh(undefined, true)
      } else {
        setActionError(cause instanceof Error ? cause.message : String(cause))
      }
      return false
    } finally {
      if (isCurrent() && pendingToken.current === token) {
        pendingToken.current = null
        setPending(null)
      }
    }
  }, [refresh, sessionId])

  const stateIsCurrent = stateSessionId === sessionId

  return {
    snapshot: stateIsCurrent ? snapshot : null,
    memberEvents: stateIsCurrent ? memberEvents : {},
    loading: stateIsCurrent ? loading : Boolean(active && sessionId),
    loadError: stateIsCurrent ? loadError : '',
    actionError: stateIsCurrent ? actionError : '',
    conflict: stateIsCurrent ? conflict : false,
    pending: stateIsCurrent ? pending : null,
    clearNotice() {
      setActionError('')
      setConflict(false)
    },
    refresh: () => refresh(),
    createTask: (draft: AgentTeamTaskDraft) => mutate('create', id => agentTeamApi.createTask(id, draft)),
    replaceTask: (task: AgentTeamTask, draft: AgentTeamTaskDraft) => mutate('replace', id => agentTeamApi.replaceTask(id, task, draft)),
    deleteTask: (task: AgentTeamTask) => mutate('delete', id => agentTeamApi.deleteTask(id, task)),
    sendMessage: (to: string, content: string) => mutate('send', id => agentTeamApi.sendMessage(id, to, content)),
    markMessageRead: (messageId: string) => mutate('read', id => agentTeamApi.markMessageRead(id, messageId)),
  }
}
