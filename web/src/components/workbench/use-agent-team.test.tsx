// @vitest-environment jsdom

import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { SessionLiveActivity } from '@/api/live-client'
import type { AgentTeamSnapshot, AgentTeamTaskDraft, LocalSession, SessionEvent } from '@/types'
import { agentTeamApi } from './agent-team-api'
import { useAgentTeam } from './use-agent-team'

function snapshot(teamId: string): AgentTeamSnapshot {
  return {
    team_id: teamId,
    current_member_id: `${teamId}-lead`,
    members: [
      { id: `${teamId}-lead`, label: `${teamId} lead`, role: 'lead' },
      {
        id: `${teamId}-worker`, parent_id: `${teamId}-lead`, subagent_id: `${teamId}-agent`,
        label: `${teamId} worker`, provider: 'in-process', role: 'subagent',
      },
    ],
    tasks: [],
    messages: [],
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (cause: unknown) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

type TeamHook = ReturnType<typeof useAgentTeam>
let current!: TeamHook
let host: HTMLDivElement
let root: Root

const NO_SESSIONS: readonly LocalSession[] = []
const NO_ACTIVITY: Readonly<Record<string, SessionLiveActivity>> = {}

function childSession(sessionId: string, subagentId: string, updatedAt = 1): LocalSession {
  return {
    identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: sessionId },
    workspace_id: 'workspace', workspace_path: '/tmp/workspace', title: sessionId,
    permissions: 'workspace_write', model: { provider: 'profile_default' },
    agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute',
    parent_session_id: 'owner',
    subagent: { subagent_id: subagentId, provider: 'in-process', transcript_kind: 'conversation' },
    created_at_ms: 1, updated_at_ms: updatedAt,
  }
}

function Probe({
  sessionId,
  sessions = NO_SESSIONS,
  liveSnapshot = null,
  liveActivity = NO_ACTIVITY,
}: {
  sessionId: string
  sessions?: readonly LocalSession[]
  liveSnapshot?: AgentTeamSnapshot | null
  liveActivity?: Readonly<Record<string, SessionLiveActivity>>
}) {
  current = useAgentTeam(sessionId, true, sessions, liveSnapshot, liveActivity)
  return <div data-team-id={current.snapshot?.team_id ?? ''} data-pending={current.pending ?? ''} />
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('useAgentTeam Session epochs', () => {
  it('keeps a live Team snapshot when an older initial REST read finishes later', async () => {
    const initialRead = deferred<AgentTeamSnapshot>()
    const live = snapshot('live-team')
    vi.spyOn(agentTeamApi, 'snapshot').mockImplementation(() => initialRead.promise as never)
    vi.spyOn(agentTeamApi, 'events').mockResolvedValue([])

    act(() => root.render(<Probe sessionId="team-a" />))
    expect(agentTeamApi.snapshot).toHaveBeenCalledWith('team-a', expect.any(AbortSignal))

    act(() => root.render(<Probe sessionId="team-a" liveSnapshot={live} />))
    await act(async () => { await Promise.resolve() })
    expect(current.snapshot?.team_id).toBe('live-team')

    await act(async () => {
      initialRead.resolve(snapshot('stale-rest-team'))
      await initialRead.promise
    })
    expect(current.snapshot?.team_id).toBe('live-team')
  })

  it('loads a canonical child history when the Session topology arrives after live Team metadata', async () => {
    const live = snapshot('team-a')
    const child = childSession('child-a', 'team-a-agent')
    const events: SessionEvent[] = [
      { seq: 0, type: 'turn_started', run_id: 'child-run', occurred_at_ms: 10 },
    ]
    vi.spyOn(agentTeamApi, 'snapshot').mockResolvedValue(live)
    vi.spyOn(agentTeamApi, 'events').mockResolvedValue(events)

    act(() => root.render(<Probe sessionId="team-a" liveSnapshot={live} />))
    await act(async () => { await Promise.resolve() })
    expect(agentTeamApi.snapshot).not.toHaveBeenCalled()
    expect(agentTeamApi.events).not.toHaveBeenCalled()

    act(() => root.render(<Probe sessionId="team-a" liveSnapshot={live} sessions={[child]} />))
    await act(async () => {
      await vi.waitFor(() => expect(agentTeamApi.events).toHaveBeenCalledWith('child-a', expect.any(AbortSignal)))
      expect(agentTeamApi.events).toHaveBeenCalledTimes(1)
    })
    expect(current.memberEvents['team-a-agent']).toEqual(events)
  })

  it('invalidates only the affected canonical child history on live activity edges', async () => {
    const live = snapshot('team-a')
    const child = childSession('child-a', 'team-a-agent')
    const started: SessionEvent[] = [
      { seq: 0, type: 'turn_started', run_id: 'child-run', occurred_at_ms: 10 },
    ]
    const finished: SessionEvent[] = [
      ...started,
      { seq: 1, type: 'turn_finished', run_id: 'child-run', occurred_at_ms: 20 },
    ]
    vi.spyOn(agentTeamApi, 'snapshot').mockResolvedValue(live)
    vi.spyOn(agentTeamApi, 'events')
      .mockResolvedValueOnce(started)
      .mockResolvedValueOnce(finished)
    const running = {
      'child-a': { session_id: 'child-a', running: true, updated_at_ms: 10 },
    }

    act(() => root.render(<Probe
      sessionId="team-a"
      liveSnapshot={live}
      sessions={[child]}
      liveActivity={running}
    />))
    await act(async () => {
      await vi.waitFor(() => expect(agentTeamApi.events).toHaveBeenCalledWith('child-a', expect.any(AbortSignal)))
    })
    expect(vi.mocked(agentTeamApi.events).mock.calls[0]?.[1]?.aborted).toBe(false)
    expect(current.memberEvents['team-a-agent']).toEqual(started)

    act(() => root.render(<Probe
      sessionId="team-a"
      liveSnapshot={live}
      sessions={[childSession('child-a', 'team-a-agent', 11)]}
      liveActivity={{
        'child-a': { session_id: 'child-a', running: true, updated_at_ms: 11 },
      }}
    />))
    await act(async () => { await Promise.resolve() })
    expect(agentTeamApi.events).toHaveBeenCalledTimes(1)

    const settled = {
      'child-a': { session_id: 'child-a', running: false, updated_at_ms: 20 },
    }
    act(() => root.render(<Probe
      sessionId="team-a"
      liveSnapshot={live}
      sessions={[child]}
      liveActivity={settled}
    />))
    await act(async () => {
      await vi.waitFor(() => expect(agentTeamApi.events).toHaveBeenCalledTimes(2))
    })
    expect(current.memberEvents['team-a-agent']).toEqual(finished)
    expect(agentTeamApi.events).toHaveBeenCalledTimes(2)

    act(() => root.render(<Probe
      sessionId="team-a"
      liveSnapshot={live}
      sessions={[child]}
      liveActivity={{
        ...settled,
        unrelated: { session_id: 'unrelated', running: true, updated_at_ms: 30 },
      }}
    />))
    await act(async () => { await Promise.resolve() })
    expect(agentTeamApi.events).toHaveBeenCalledTimes(2)
  })

  it('cannot publish an old mutation refresh or clear the new Session pending action', async () => {
    const createA = deferred<unknown>()
    const sendB = deferred<unknown>()
    vi.spyOn(agentTeamApi, 'snapshot').mockImplementation(async sessionId => snapshot(sessionId))
    vi.spyOn(agentTeamApi, 'createTask').mockImplementation(() => createA.promise as never)
    vi.spyOn(agentTeamApi, 'sendMessage').mockImplementation(() => sendB.promise as never)

    act(() => root.render(<Probe sessionId="team-a" />))
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(agentTeamApi.snapshot).toHaveBeenCalledWith('team-a', expect.any(AbortSignal))
    expect(current.snapshot?.team_id).toBe('team-a')
    const draft: AgentTeamTaskDraft = {
      subject: 'A task', description: '', status: 'pending', dependencies: [], owner: null,
    }
    let oldMutation!: Promise<boolean>
    act(() => { oldMutation = current.createTask(draft) })
    expect(current.pending).toBe('create')

    act(() => root.render(<Probe sessionId="team-b" />))
    await act(async () => {
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(current.snapshot?.team_id).toBe('team-b')
    let newMutation!: Promise<boolean>
    act(() => { newMutation = current.sendMessage('team-b-worker', 'B message') })
    expect(current.pending).toBe('send')

    await act(async () => {
      createA.resolve({})
      expect(await oldMutation).toBe(false)
    })
    expect(current.snapshot?.team_id).toBe('team-b')
    expect(current.pending).toBe('send')
    expect(agentTeamApi.snapshot).toHaveBeenCalledTimes(2)

    await act(async () => {
      sendB.resolve({})
      expect(await newMutation).toBe(true)
    })
    expect(current.snapshot?.team_id).toBe('team-b')
    expect(current.pending).toBeNull()
    expect(agentTeamApi.snapshot).toHaveBeenCalledTimes(3)
  })
})
