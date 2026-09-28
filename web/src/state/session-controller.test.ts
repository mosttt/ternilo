import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/client'
import type {
  LiveConnectionStatus,
  LiveServerFrame,
  LiveSubscriptionOptions,
} from '@/api/live-client'
import type {
  AgentTeamSnapshot,
  ApplicationState,
  PendingQuestion,
  Profile,
  SessionEvent,
  SessionInboxSnapshot,
  SessionProjection,
  SessionStats,
  SessionSubmission,
} from '@/types'
import {
  SessionController,
  type SessionApi,
  type SessionControllerDependencies,
  type SessionLiveTransport,
  type SessionRequestOptions,
} from './session-controller'

class FakeApi implements SessionApi {
  readonly calls: Array<{ path: string; options?: SessionRequestOptions }> = []

  constructor(readonly handler: (path: string, options?: SessionRequestOptions) => Promise<unknown>) {}

  request<Value>(path: string, options?: SessionRequestOptions): Promise<Value> {
    this.calls.push({ path, options })
    return this.handler(path, options) as Promise<Value>
  }
}

class FakeLive implements SessionLiveTransport {
  readonly targets: Array<{ sessionId: string | null; options?: LiveSubscriptionOptions }> = []
  readonly retries: LiveSubscriptionOptions[] = []
  private readonly frameListeners = new Set<(frame: LiveServerFrame) => void>()
  private readonly statusListeners = new Set<(status: LiveConnectionStatus) => void>()

  setSession(sessionId: string | null, options?: LiveSubscriptionOptions) {
    this.targets.push({ sessionId, options })
    return sessionId ? this.targets.length : null
  }

  resubscribe(options: LiveSubscriptionOptions = {}) {
    this.retries.push(options)
    return this.retries.length
  }

  onFrame(listener: (frame: LiveServerFrame) => void) {
    this.frameListeners.add(listener)
    return () => { this.frameListeners.delete(listener) }
  }

  onStatus(listener: (status: LiveConnectionStatus) => void) {
    this.statusListeners.add(listener)
    return () => { this.statusListeners.delete(listener) }
  }

  emit(frame: LiveServerFrame) {
    this.frameListeners.forEach(listener => listener(frame))
  }

  status(status: LiveConnectionStatus) {
    this.statusListeners.forEach(listener => listener(status))
  }

  get listenerCount() {
    return this.frameListeners.size + this.statusListeners.size
  }
}

const emptyInbox = (sessionId: string): SessionInboxSnapshot => ({
  session_id: sessionId,
  active_run_id: null,
  paused: false,
  items: [],
})

const event = (seq: number, values: Partial<SessionEvent> = {}): SessionEvent => ({
  seq,
  occurred_at_ms: seq,
  run_id: 'run-1',
  type: 'assistant_message',
  ...values,
})

const stats = { events: 2 } as SessionStats
const projection: SessionProjection = { session_id: 'session', as_of_seq: 1, values: {} }
const profile: Profile = { plugins: [] }
const team: AgentTeamSnapshot = {
  team_id: 'team-1',
  current_member_id: 'lead',
  members: [{ id: 'lead', label: 'Lead', role: 'lead' }],
  tasks: [],
  messages: [],
}

function metadata(path: string): unknown {
  if (path.includes('/history?')) return { events: [], next_before_seq: null }
  if (path.endsWith('/stats')) return stats
  if (path.endsWith('/projection')) return projection
  if (path.startsWith('/questions?')) return [] as PendingQuestion[]
  if (path.endsWith('/plugins')) return profile
  if (path.endsWith('/queue')) return emptyInbox('session')
  throw new Error(`unhandled path ${path}`)
}

function dependencies(api: SessionApi, live = new FakeLive()) {
  const deps: SessionControllerDependencies = {
    api,
    live,
    acceptWorkbench: vi.fn(),
    acceptActivity: vi.fn(),
    refresh: vi.fn(async () => undefined),
    notify: vi.fn(),
    labels: () => ({
      metadata: ['statistics', 'projection', 'questions', 'plugins'],
      skillNameError: 'invalid skill',
      runStopping: 'stopping',
    }),
    inputAuthor: () => ({ kind: 'local' }),
    randomId: () => 'client-run',
    now: () => 100,
  }
  return { deps, live }
}

async function flush() {
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
}

describe('session controller live transport', async () => {
  it('publishes a long history once and discards unfinished history when switching sessions', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('source', true)
    await flush()
    live.emit({ type: 'event_batch', subscription_id: 1, session_id: 'source', reset: true,
      complete: false, events: [event(99)], next_seq: 100 })
    controller.setTarget('session', true)
    await flush()
    const snapshots: number[] = []
    controller.subscribe(() => { snapshots.push(controller.getSnapshot().events.length) })
    for (let offset = 0; offset < 30_000; offset += 250) {
      live.emit({ type: 'event_batch', subscription_id: 2, session_id: 'session', reset: offset === 0,
        complete: offset === 29_750,
        events: Array.from({ length: 250 }, (_, index) => event(offset + index)), next_seq: offset + 250 })
    }
    expect(snapshots.filter(count => count > 0 && count < 30_000)).toEqual([])
    expect(controller.getSnapshot().events.map(item => item.seq)).toEqual(Array.from({ length: 30_000 }, (_, index) => index))
    expect(controller.getSnapshot().loading).toBe(false)
    controller.dispose()
  })

  it('retains received history when loading fails and can retry from a clean baseline', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    live.emit({ type: 'event_batch', subscription_id: 1, session_id: 'session', reset: true,
      complete: false, events: [event(0)], next_seq: 1 })
    live.emit({ type: 'error', subscription_id: 1, code: 'unavailable', message: 'Disconnected' })
    expect(controller.getSnapshot()).toMatchObject({ loading: false, historyError: 'Disconnected', events: [{ seq: 0 }] })
    controller.retryHistory()
    live.emit({ type: 'event_batch', subscription_id: 2, session_id: 'session', reset: true,
      complete: true, events: [event(2)], next_seq: 3 })
    expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([2])
    controller.dispose()
  })

  it('keeps the pending submission snapshot stable while streaming unrelated events', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    const pending = controller.getSnapshot().pendingSubmissions
    const subscriber = vi.fn()
    const unsubscribe = controller.subscribe(subscriber)

    for (let seq = 0; seq < 100; seq += 1) {
      live.emit({
        type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
        complete: false, events: [event(seq)], next_seq: seq + 1,
      })
      expect(controller.getSnapshot().pendingSubmissions).toBe(pending)
    }
    expect(controller.getSnapshot().events).toHaveLength(100)
    expect(subscriber).toHaveBeenCalledTimes(100)
    unsubscribe()
    controller.dispose()
  })

  it('hydrates chunked history and all metadata without REST polling', async () => {
    const api = new FakeApi(async path => metadata(path))
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()

    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: true,
      complete: false, events: [event(0)], next_seq: 1,
    })
    expect(controller.getSnapshot()).toMatchObject({ loading: true, events: [] })

    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
      complete: true, events: [event(1, { type: 'turn_finished' })], next_seq: 2,
    })
    live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: true, projection: true, questions: true, profile: true, agent_team: true },
        inbox: { ...emptyInbox('session'), active_run_id: 'run-1' },
        stats,
        projection,
        questions: [],
        profile,
        agent_team: team,
      },
    })

    expect(controller.getSnapshot()).toMatchObject({
      loadedSessionId: 'session', loading: false, busy: true, activeRunId: 'run-1',
      stats, projection, questions: [], effectiveProfile: profile, agentTeam: team,
    })
    expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([0, 1])
    expect(api.calls.map(call => call.path)).toEqual(['/sessions/session/history?limit=200'])
    controller.dispose()
  })

  it('routes Workbench baselines and ignores stale Session frames', async () => {
    const api = new FakeApi(async path => metadata(path))
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    live.status('ready')
    controller.setTarget('source', true)
    await flush()
    controller.setTarget('fork', true)
    await flush()

    const state: ApplicationState = { workspaces: [], sessions: [] }
    live.emit({ type: 'workbench', revision: 7, state, activity: [] })
    const backgroundActivity = { session_id: 'background', running: true, updated_at_ms: 12 }
    live.emit({ type: 'activity', activity: backgroundActivity })
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'source', reset: true,
      complete: true, events: [event(99)], next_seq: 100,
    })
    live.emit({
      type: 'event_batch', subscription_id: 2, session_id: 'fork', reset: true,
      complete: true, events: [event(2), event(4)], next_seq: 5,
    })

    expect(deps.acceptWorkbench).toHaveBeenCalledWith(state, 7, [])
    expect(deps.acceptActivity).toHaveBeenCalledWith(backgroundActivity)
    expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([2, 4])
    expect(controller.getSnapshot().liveStatus).toBe('ready')
    expect(live.targets.map(item => item.sessionId)).toEqual([null, 'source', null, 'fork'])
    controller.dispose()
  })

  it('replaces an old baseline and explicitly resubscribes on history retry', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    live.status('ready')
    controller.setTarget('session', true)
    await flush()
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: true,
      complete: true, events: [event(7)], next_seq: 8,
    })

    controller.retryHistory()
    expect(controller.getSnapshot()).toMatchObject({
      liveStatus: 'ready', loading: true, events: [], historyError: '',
    })
    await flush()
    expect(live.targets.at(-1)).toEqual({ sessionId: 'session', options: { afterSeq: null } })

    live.emit({
      type: 'event_batch', subscription_id: 2, session_id: 'session', reset: true,
      complete: true, events: [event(0)], next_seq: 1,
    })
    expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([0])
    controller.dispose()
  })

  it('surfaces a live subscription error and keeps it target-scoped', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    live.emit({ type: 'error', subscription_id: 1, code: 'unavailable', message: 'history unavailable' })
    expect(controller.getSnapshot()).toMatchObject({
      loadedSessionId: 'session', loading: false, historyError: 'history unavailable',
    })
    live.status('reconnecting')
    expect(controller.getSnapshot().liveStatus).toBe('reconnecting')
    live.status('ready')
    expect(controller.getSnapshot().liveStatus).toBe('ready')
    controller.dispose()
  })

  it('keeps optimistic identity out of requests and replaces it with the admitted author', async () => {
    let admit!: (submission: SessionSubmission) => void
    const api = new FakeApi(async (path, options) => {
      if (path.endsWith('/queue') && options?.method === 'POST') return new Promise(resolve => { admit = resolve })
      return metadata(path)
    })
    const { deps } = dependencies(api)
    deps.inputAuthor = () => ({ kind: 'account', user_id: 'current-user', username: 'current-user' })
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', false, 'account-a')
    await flush()
    const sending = controller.submit('hello')
    expect(controller.getSnapshot().pendingSubmissions[0]?.author).toEqual({ kind: 'account', user_id: 'current-user', username: 'current-user' })
    expect(api.calls[0]?.options?.body).toEqual({ delivery: 'queue', run_id: 'client-run', content: { kind: 'prompt', input: 'hello' }, references: [], attachments: [] })
    admit({ id: 'accepted', run_id: 'client-run', content: { kind: 'prompt', input: 'hello' }, references: [], attachments: [], placement: 'running', created_at_ms: 1, updated_at_ms: 1,
      provenance: { input_id: 'accepted', author: { kind: 'account', user_id: 'canonical-user', username: 'canonical-user' } },
    })
    await sending
    expect(controller.getSnapshot().pendingSubmissions[0]?.author).toEqual({ kind: 'account', user_id: 'canonical-user', username: 'canonical-user' })
    controller.setTarget('session', false, 'account-b')
    await flush()
    expect(controller.getSnapshot().pendingSubmissions).toEqual([])
    controller.dispose()
  })

  it('keeps an optimistic submission until its exact durable live event arrives', async () => {
    const submission: SessionSubmission = {
      id: 'submission-1', run_id: 'client-run', content: { kind: 'prompt', input: 'hello' },
      references: [], attachments: [], placement: 'running', created_at_ms: 100, updated_at_ms: 100,
    }
    const runningInbox = { ...emptyInbox('session'), active_run_id: 'client-run', items: [submission] }
    const api = new FakeApi(async (path, options) => {
      if (path.endsWith('/queue') && options?.method === 'POST') return submission
      if (path.endsWith('/queue')) return runningInbox
      return metadata(path)
    })
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', false)
    await flush()

    await controller.submit('hello')
    expect(controller.getSnapshot().pendingSubmissions).toMatchObject([{
      request_id: 'client-run', submission_id: 'submission-1', input: 'hello',
    }])

    controller.setTarget('session', true)
    await flush()
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: true,
      complete: true,
      events: [event(0, {
        type: 'user_message',
        source: { kind: 'submission', submission_id: 'submission-1', created_at_ms: 1, delivery: 'queue' },
      })],
      next_seq: 1,
    })
    expect(controller.getSnapshot().pendingSubmissions).toEqual([])
    controller.dispose()
  })

  it('drops optimistic submissions when the tenant scope changes', async () => {
    const submission: SessionSubmission = {
      id: 'submission-1', run_id: 'client-run', content: { kind: 'prompt', input: 'hello' },
      references: [], attachments: [], placement: 'running', created_at_ms: 100, updated_at_ms: 100,
    }
    const api = new FakeApi(async (path, options) => {
      if (path.endsWith('/queue') && options?.method === 'POST') return submission
      if (path.endsWith('/queue')) return emptyInbox('session')
      return metadata(path)
    })
    const { deps } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', false, 'tenant-a')
    await flush()

    await controller.submit('hello')
    expect(controller.getSnapshot().pendingSubmissions).toHaveLength(1)

    controller.setTarget('session', false, 'tenant-b')
    await flush()
    expect(controller.getSnapshot().pendingSubmissions).toEqual([])
    controller.dispose()
  })

  it('keeps explicit queue, cancellation, and question mutations functional', async () => {
    let questionBody: unknown
    const api = new FakeApi(async (path, options) => {
      if (path.includes('/answer?')) questionBody = options?.body
      if (path.endsWith('/queue/item/steer')) return {
        id: 'item', run_id: 'queued-run', content: { kind: 'prompt', input: 'queued' },
        references: [], attachments: [], placement: 'queued', created_at_ms: 1, updated_at_ms: 1,
      } as SessionSubmission
      if (path === '/sessions/session/queue') return { ...emptyInbox('session'), active_run_id: 'active-run' }
      if (path.includes('/queue/item') || path.includes('/turns/active-run') || path.includes('/answer?')) return undefined
      return metadata(path)
    })
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: false, projection: false, questions: false, profile: false, agent_team: false },
        inbox: { ...emptyInbox('session'), active_run_id: 'active-run' },
      },
    })

    await controller.editQueueItem('item', 'updated', 1)
    await controller.removeQueueItem('item')
    await controller.steerQueueItem('item')
    await controller.cancel()
    await controller.answerQuestion('question-1', { selected: ['yes'] })

    expect(api.calls.map(call => [call.path, call.options?.method ?? 'GET'])).toEqual(expect.arrayContaining([
      ['/sessions/session/queue/item', 'PATCH'],
      ['/sessions/session/queue/item', 'DELETE'],
      ['/sessions/session/queue/item/steer', 'POST'],
      ['/sessions/session/turns/active-run', 'DELETE'],
      ['/questions/question-1/answer?session_id=session', 'POST'],
    ]))
    expect(deps.notify).toHaveBeenCalledWith('stopping', 'info')
    expect(questionBody).toEqual({ selected: ['yes'] })
    controller.dispose()
  })

  it.each([false, true])('refreshes a rejected edit without retrying or replacing its error when refresh fails: %s', async refreshFails => {
    const conflict = new ApiError('queue conflict', 409, 'conflict')
    const latest: SessionSubmission = {
      id: 'item', run_id: 'run', content: { kind: 'prompt', input: 'other account' },
      references: [], attachments: [], placement: 'queued', created_at_ms: 1, updated_at_ms: 2,
    }
    const api = new FakeApi(async (path, options) => {
      if (options?.method === 'PATCH') throw conflict
      if (path.endsWith('/queue')) {
        if (refreshFails) throw new Error('refresh failed')
        return { ...emptyInbox('session'), items: [latest] }
      }
      return metadata(path)
    })
    const { deps } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    await expect(controller.editQueueItem('item', 'my draft', 1)).rejects.toBe(conflict)
    const writes = api.calls.filter(call => call.options?.method === 'PATCH')
    expect(writes).toHaveLength(1)
    expect(writes[0]!.options!.body).toEqual({ input: 'my draft', expected_updated_at_ms: 1 })
    expect(api.calls.some(call => call.path.endsWith('/queue') && !call.options?.method)).toBe(true)
    if (!refreshFails) expect(controller.getSnapshot().inbox?.items).toEqual([latest])
    controller.dispose()
  })

  it('repairs a stale cancellation inbox after the matching terminal event arrives', async () => {
    const staleInbox = { ...emptyInbox('session'), active_run_id: 'active-run' }
    let resolveFirstQueue!: (inbox: SessionInboxSnapshot) => void
    const firstQueue = new Promise<SessionInboxSnapshot>(resolve => {
      resolveFirstQueue = resolve
    })
    let queueReads = 0
    const api = new FakeApi(async (path, options) => {
      if (path.includes('/turns/active-run') && options?.method === 'DELETE') return undefined
      if (path === '/sessions/session/queue') {
        queueReads += 1
        return queueReads === 1 ? firstQueue : emptyInbox('session')
      }
      return metadata(path)
    })
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: false, projection: false, questions: false, profile: false, agent_team: false },
        inbox: staleInbox,
      },
    })

    const cancellation = controller.cancel()
    await flush()
    expect(queueReads).toBe(1)
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
      complete: false, events: [event(2, { run_id: 'active-run', type: 'turn_cancelled' })], next_seq: 3,
    })
    resolveFirstQueue(staleInbox)
    await cancellation

    expect(queueReads).toBe(2)
    expect(controller.getSnapshot()).toMatchObject({ activeRunId: null, busy: false })
    controller.dispose()
  })

  it('does not let a late HTTP snapshot undo a newly started live batch', async () => {
    let resolveRead!: (inbox: SessionInboxSnapshot) => void
    const read = new Promise<SessionInboxSnapshot>(resolve => { resolveRead = resolve })
    const api = new FakeApi(async path => path.endsWith('/queue') ? read : metadata(path))
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    const publish = (runId: string) => live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: false, projection: false, questions: false, profile: false, agent_team: false },
        inbox: { ...emptyInbox('session'), active_run_id: runId },
      },
    })
    publish('A')
    live.emit({ type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
      complete: true, events: [event(0, { run_id: 'A', type: 'turn_finished' })], next_seq: 1,
    })
    expect(api.calls.filter(call => call.path.endsWith('/queue'))).toHaveLength(1)
    publish('BC')
    resolveRead(emptyInbox('session'))
    await flush()
    expect(controller.getSnapshot()).toMatchObject({ activeRunId: 'BC', busy: true })
    controller.dispose()
  })

  it.each([false, true])('recovers an offline inbox read when live metadata returns (live first: %s)', async liveFirst => {
    let rejectRead!: (error: Error) => void
    const pendingRead = new Promise<never>((_, reject) => { rejectRead = reject })
    const api = new FakeApi(async path => path.endsWith('/queue') ? pendingRead : metadata(path))
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    const publishInbox = (activeRunId: string | null) => live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: false, projection: false, questions: false, profile: false, agent_team: false },
        inbox: { ...emptyInbox('session'), active_run_id: activeRunId },
      },
    })
    publishInbox('active-run')
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
      complete: true, events: [event(2, { run_id: 'active-run', type: 'turn_cancelled' })], next_seq: 3,
    })
    expect(api.calls.map(call => call.path)).toEqual(['/sessions/session/history?limit=200', '/sessions/session/queue'])
    if (liveFirst) publishInbox(null)
    rejectRead(new Error('selected Ternilo node is offline'))
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(controller.getSnapshot().error).toBe('')
    expect(controller.getSnapshot().metadataWarning).toBe(liveFirst ? '' : 'selected Ternilo node is offline')
    if (!liveFirst) publishInbox(null)
    expect(controller.getSnapshot()).toMatchObject({
      error: '', metadataWarning: '', busy: false, activeRunId: null,
      events: [{ seq: 2, type: 'turn_cancelled' }],
    })
    controller.dispose()
  })

  it('does not refresh the inbox for another run terminal event', async () => {
    const api = new FakeApi(async path => metadata(path))
    const { deps, live } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    live.emit({
      type: 'session_metadata', subscription_id: 1, session_id: 'session',
      metadata: {
        read: { inbox: true, stats: false, projection: false, questions: false, profile: false, agent_team: false },
        inbox: { ...emptyInbox('session'), active_run_id: 'active-run' },
      },
    })
    live.emit({
      type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
      complete: false, events: [event(2, { run_id: 'other-run', type: 'turn_finished' })], next_seq: 3,
    })
    await flush()

    expect(api.calls.map(call => call.path)).toEqual(['/sessions/session/history?limit=200'])
    expect(controller.getSnapshot()).toMatchObject({ activeRunId: 'active-run', busy: true })
    controller.dispose()
  })

  it('reports partial failures for a user-requested metadata reload', async () => {
    const api = new FakeApi(async path => {
      if (path.endsWith('/stats')) throw new Error('statistics unavailable')
      return metadata(path)
    })
    const { deps } = dependencies(api)
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', false)
    await flush()

    await controller.reloadMetadata()
    expect(controller.getSnapshot()).toMatchObject({
      metadataWarning: 'statistics：statistics unavailable',
      projection,
      questions: [],
      effectiveProfile: profile,
    })
    controller.dispose()
  })

  it('unsubscribes and removes live listeners on disposal', async () => {
    const { deps, live } = dependencies(new FakeApi(async path => metadata(path)))
    const controller = new SessionController(deps)
    controller.start()
    controller.setTarget('session', true)
    await flush()
    expect(live.listenerCount).toBe(2)
    controller.dispose()
    expect(live.listenerCount).toBe(0)
    expect(live.targets.at(-1)?.sessionId).toBeNull()
    controller.start()
    expect(live.listenerCount).toBe(2)
    controller.dispose()
    expect(live.listenerCount).toBe(0)
    await flush()
  })
})

it('loads a bounded baseline, prepends older pages while Live advances, and keeps the live cursor independent', async () => {
  let resolveOlder!: (value: unknown) => void
  const api = new FakeApi(async path => path.includes('before_seq=')
    ? new Promise(resolve => { resolveOlder = resolve })
    : { events: [event(200), event(201)], next_before_seq: 200 })
  const { deps, live } = dependencies(api)
  const controller = new SessionController(deps)
  controller.start()
  controller.setTarget('session', true)
  expect(live.targets).toEqual([{ sessionId: null, options: undefined }])
  await flush()
  expect(live.targets.at(-1)).toEqual({ sessionId: 'session', options: { afterSeq: 201 } })
  const older = controller.loadOlderHistory()
  await controller.loadOlderHistory()
  expect(api.calls).toHaveLength(2)
  live.emit({ type: 'event_batch', subscription_id: 1, session_id: 'session', reset: false,
    complete: true, events: [event(202)], next_seq: 203 })
  resolveOlder({ events: [event(198), event(199)], next_before_seq: 198 })
  await older
  expect(controller.getSnapshot()).toMatchObject({ nextBeforeSeq: 198, loadingOlder: false })
  expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([198, 199, 200, 201, 202])
  expect(live.targets).toHaveLength(2)
  controller.dispose()
})

it('cancels stale initial and older pages across account scopes and keeps failed page cursors retryable', async () => {
  const pending: Array<{ resolve(value: unknown): void; reject(error: Error): void }> = []
  const api = new FakeApi(() => new Promise((resolve, reject) => pending.push({ resolve, reject })))
  const { deps, live } = dependencies(api)
  const controller = new SessionController(deps)
  controller.start()
  controller.setTarget('same', true, 'first-account')
  controller.setTarget('same', true, 'second-account')
  pending[0]!.resolve({ events: [event(999)], next_before_seq: 999 })
  pending[1]!.resolve({ events: [event(5)], next_before_seq: 5 })
  await flush()
  expect(controller.getSnapshot().events.map(item => item.seq)).toEqual([5])
  expect(api.calls[0]!.options?.signal?.aborted).toBe(true)
  const failed = controller.loadOlderHistory()
  pending[2]!.reject(new Error('offline'))
  await failed
  expect(controller.getSnapshot()).toMatchObject({ nextBeforeSeq: 5, olderHistoryError: 'offline', historyError: '', loadingOlder: false })
  const old = controller.loadOlderHistory()
  controller.setTarget('same', true, 'third-account')
  pending[3]!.resolve({ events: [event(1)], next_before_seq: null })
  pending[4]!.resolve({ events: [], next_before_seq: null })
  await old
  await flush()
  expect(controller.getSnapshot().events).toEqual([])
  expect(live.targets.at(-1)?.options?.afterSeq).toBe(null)
  controller.dispose()
})
