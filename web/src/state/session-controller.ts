import { createSessionEventBuffer, mergeSessionEventBuffer, type SessionEventBuffer } from '@/domain/session-event-buffer'
import { visibleSubmissionEchoes } from '@/domain/submission-echo'
import { conversationEvents } from '@/domain/conversation-events'
import type {
  LiveConnectionStatus,
  LiveServerFrame,
  SessionLiveActivity,
  LiveSubscriptionOptions,
} from '@/api/live-client'
import type {
  AgentTeamSnapshot,
  ApplicationState,
  PendingQuestion,
  PendingSubmissionEcho,
  Profile,
  SessionCommandReceipt,
  SessionEvent,
  SessionEventPage,
  SessionInboxSnapshot,
  SessionProjection,
  SessionStats,
  SessionSubmission,
  InputAuthor,
  SubmissionDelivery,
  SubmissionReference,
  ToastMessage,
  UserQuestionAnswer,
} from '@/types'
import { createActionStore } from './action-store'

export interface SessionRequestOptions {
  method?: string
  body?: unknown
  signal?: AbortSignal
}

export interface SessionApi {
  request<T>(path: string, options?: SessionRequestOptions): Promise<T>
}

export interface SessionLiveTransport {
  setSession(sessionId: string | null, options?: LiveSubscriptionOptions): number | null
  resubscribe(options?: LiveSubscriptionOptions): number | null
  onFrame(listener: (frame: LiveServerFrame) => void): () => void
  onStatus(listener: (status: LiveConnectionStatus) => void): () => void
}

export interface SessionControllerLabels {
  metadata: readonly [string, string, string, string]
  skillNameError: string
  runStopping: string
}

export interface SessionControllerDependencies {
  api: SessionApi
  live: SessionLiveTransport
  acceptWorkbench(state: ApplicationState, revision: number, activity: SessionLiveActivity[]): void
  acceptActivity(activity: SessionLiveActivity): void
  refresh(): Promise<void>
  notify(message: string, kind?: ToastMessage['kind']): void
  labels(): SessionControllerLabels
  inputAuthor(): InputAuthor | undefined
  randomId(): string
  now(): number
}

export interface SessionControllerSnapshot {
  liveStatus: LiveConnectionStatus
  loadedSessionId: string | null
  events: SessionEvent[]
  pendingSubmissions: PendingSubmissionEcho[]
  inbox: SessionInboxSnapshot | null
  stats: SessionStats | null
  projection: SessionProjection | null
  questions: PendingQuestion[]
  effectiveProfile: Profile | null
  agentTeam: AgentTeamSnapshot | null
  loading: boolean
  loadingHistory: boolean
  busy: boolean
  activeRunId: string | null
  error: string
  historyError: string
  nextBeforeSeq: number | null
  loadingOlder: boolean
  olderHistoryError: string
  metadataWarning: string
}

export interface SessionRuntimeActions {
  submit(
    input: string,
    attachments?: Array<{ name: string; media_type: string; content: string }>,
    delivery?: SubmissionDelivery,
    references?: SubmissionReference[],
    regenerateFrom?: number,
  ): Promise<void>
  submitFeedback(text: string): Promise<void>
  editQueueItem(id: string, input: string, expectedUpdatedAtMs: number): Promise<void>
  loadQueueItem(id: string): Promise<SessionSubmission | undefined>
  removeQueueItem(id: string): Promise<void>
  steerQueueItem(id: string): Promise<void>
  cancel(): Promise<void>
  answerQuestion(questionId: string, answer: UserQuestionAnswer): Promise<void>
  reloadMetadata(): Promise<void>
  retryHistory(): void
  loadOlderHistory(): Promise<void>
}

export type SessionRuntime = SessionControllerSnapshot & SessionRuntimeActions

function initialSnapshot(pendingSubmissions: PendingSubmissionEcho[] = []): SessionControllerSnapshot {
  return {
    liveStatus: 'idle',
    loadedSessionId: null,
    events: [],
    pendingSubmissions,
    inbox: null,
    stats: null,
    projection: null,
    questions: [],
    effectiveProfile: null,
    agentTeam: null,
    loading: false,
    loadingHistory: false,
    busy: false,
    activeRunId: null,
    error: '',
    historyError: '',
    nextBeforeSeq: null,
    loadingOlder: false,
    olderHistoryError: '',
    metadataWarning: '',
  }
}

function errorMessage(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause)
}

function terminalTurn(event: SessionEvent) {
  return event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled'
}

interface MetadataFlight {
  sessionId: string
  generation: number
  signal?: AbortSignal
  pending: MetadataRefresh
  promise: Promise<void>
}

interface InboxFlight {
  sessionId: string
  generation: number
  signal?: AbortSignal
  dirty: boolean
  promise: Promise<void>
}

interface CachedSession {
  snapshot: SessionControllerSnapshot
  buffer: SessionEventBuffer
  bytes: number
}

function containsOldestRunStart(events: SessionEvent[]) {
  const first = events[0]
  return !first || first.seq === 0 || events.some(event => event.run_id === first.run_id && event.type === 'turn_started')
}

interface MetadataRefresh {
  stats: boolean
  projection: boolean
  questions: boolean
  profile: boolean
}

const allMetadata: MetadataRefresh = {
  stats: true,
  projection: true,
  questions: true,
  profile: true,
}

const noMetadata = (): MetadataRefresh => ({
  stats: false,
  projection: false,
  questions: false,
  profile: false,
})

function hasMetadata(refresh: MetadataRefresh) {
  return refresh.stats || refresh.projection || refresh.questions || refresh.profile
}

function mergeMetadata(left: MetadataRefresh, right: MetadataRefresh): MetadataRefresh {
  return {
    stats: left.stats || right.stats,
    projection: left.projection || right.projection,
    questions: left.questions || right.questions,
    profile: left.profile || right.profile,
  }
}

function updatesInbox(event: SessionEvent) {
  return event.type === 'turn_started'
    || event.type === 'user_message'
    || event.type === 'user_question_asked'
    || event.type === 'user_question_answered'
    || terminalTurn(event)
}

function updatesStats(event: SessionEvent) {
  return event.type === 'turn_started'
    || event.type === 'user_message'
    || event.type === 'step_started'
    || event.type === 'assistant_message'
    || event.type === 'tool_call_started'
    || event.type === 'tool_call_finished'
    || event.type === 'code_dispatch_started'
    || event.type === 'code_dispatch_finished'
    || terminalTurn(event)
}

function updatesProjection(event: SessionEvent) {
  return event.type === 'feedback_recorded'
    || event.type === 'plan_updated'
    || event.type === 'plan_review_completed'
    || event.type === 'todo_updated'
    || event.type === 'goal_updated'
    || event.type === 'context_compacted'
    || event.type === 'workflow_run_started'
    || event.type === 'workflow_phase_changed'
    || event.type === 'workflow_log_emitted'
    || event.type === 'workflow_agent_started'
    || event.type === 'workflow_agent_finished'
    || event.type === 'workflow_run_finished'
}

function metadataForEvents(events: readonly SessionEvent[]): MetadataRefresh {
  return {
    stats: events.some(updatesStats),
    projection: events.some(updatesProjection),
    questions: events.some(event => event.type === 'user_question_asked' || event.type === 'user_question_answered'),
    profile: events.some(event => event.type === 'runtime_extension_changed'),
  }
}

/** Owns one selected session's transport loop and commands without React or browser globals. */
export class SessionController implements SessionRuntimeActions {
  private readonly store = createActionStore(initialSnapshot())
  private disposeLiveFrame: (() => void) | null = null
  private disposeLiveStatus: (() => void) | null = null
  private scopeKey = ''
  private sessionId: string | null = null
  private enabled = false
  private generation = 0
  private eventBuffer: SessionEventBuffer = createSessionEventBuffer(null)
  private historyEvents: SessionEvent[] | null = null
  private pendingBySession = new Map<string, PendingSubmissionEcho[]>()
  private abortController: AbortController | null = null
  private metadataFlight: MetadataFlight | null = null
  private inboxFlight: InboxFlight | null = null
  private inboxRevision = 0
  private historyRevision = 0
  private readonly cachedSessions = new Map<string, CachedSession>()
  private streamEvents: SessionEvent[] = []
  private streamTimer: ReturnType<typeof setTimeout> | null = null

  constructor(private readonly dependencies: SessionControllerDependencies) {}

  readonly getSnapshot = () => this.store.getSnapshot()
  readonly subscribe = (listener: () => void) => this.store.subscribe(listener)

  start() {
    if (this.disposeLiveFrame || this.disposeLiveStatus) return
    this.disposeLiveFrame = this.dependencies.live.onFrame(frame => this.handleLiveFrame(frame))
    this.disposeLiveStatus = this.dependencies.live.onStatus(status => this.handleLiveStatus(status))
  }

  setTarget(sessionId: string | null, enabled: boolean, scopeKey = 'host') {
    if (this.sessionId === sessionId && this.enabled === enabled && this.scopeKey === scopeKey) return
    const scopeChanged = this.scopeKey !== scopeKey
    const liveStatus = this.getSnapshot().liveStatus
    if (!scopeChanged) this.rememberTarget()
    this.stopTarget()
    if (scopeChanged) {
      this.pendingBySession.clear()
      this.cachedSessions.clear()
    }
    this.scopeKey = scopeKey
    this.sessionId = sessionId
    this.enabled = enabled
    this.eventBuffer = createSessionEventBuffer(sessionId)
    const pending = sessionId === null ? [] : this.pendingBySession.get(sessionId) ?? []
    const cached = sessionId && enabled ? this.cachedSessions.get(sessionId) : undefined
    if (cached && sessionId) {
      this.cachedSessions.delete(sessionId)
      this.eventBuffer = cached.buffer
      this.replaceSnapshot({ ...cached.snapshot, liveStatus, pendingSubmissions: pending, loading: false, loadingHistory: false, loadingOlder: false, olderHistoryError: '', historyError: '', error: '' })
      this.reconcilePending(sessionId, cached.buffer.events, cached.snapshot.inbox)
      this.abortController = new AbortController()
      this.dependencies.live.setSession(sessionId, { afterSeq: cached.buffer.events.at(-1)?.seq ?? null })
    } else {
      this.replaceSnapshot({ ...initialSnapshot(pending), liveStatus })
      if (sessionId && enabled) this.startTarget(sessionId)
    }
  }

  dispose() {
    this.stopTarget()
    this.disposeLiveFrame?.()
    this.disposeLiveStatus?.()
    this.disposeLiveFrame = null
    this.disposeLiveStatus = null
    this.sessionId = null
    this.enabled = false
    this.cachedSessions.clear()
    this.pendingBySession.clear()
  }

  private rememberTarget() {
    const snapshot = this.getSnapshot()
    if (!this.sessionId || !this.enabled || snapshot.loadingHistory || this.historyEvents !== null
      || snapshot.loadedSessionId !== this.sessionId || snapshot.historyError) return
    // Keep recent conversations in memory only; cap both large events and session count.
    const bytes = JSON.stringify(this.eventBuffer.events).length * 2
    this.cachedSessions.delete(this.sessionId)
    this.cachedSessions.set(this.sessionId, { snapshot, buffer: this.eventBuffer, bytes })
    let totalBytes = 0
    for (const cached of this.cachedSessions.values()) {
      totalBytes += cached.bytes
    }
    while (this.cachedSessions.size > 8 || totalBytes > 24 * 1024 * 1024) {
      const oldest = this.cachedSessions.keys().next().value!
      const cached = this.cachedSessions.get(oldest)!
      totalBytes -= cached.bytes
      this.cachedSessions.delete(oldest)
    }
  }

  private replaceSnapshot(snapshot: SessionControllerSnapshot) {
    this.store.dispatch(() => snapshot)
  }

  private updateSnapshot(patch: Partial<SessionControllerSnapshot>) {
    this.store.dispatch(current => {
      const entries = Object.entries(patch) as Array<[keyof SessionControllerSnapshot, SessionControllerSnapshot[keyof SessionControllerSnapshot]]>
      if (entries.every(([key, value]) => Object.is(current[key], value))) return current
      return { ...current, ...patch }
    })
  }

  private setRuntimeError(error: string) {
    this.updateSnapshot({ error })
  }

  private clearRuntimeError() {
    this.updateSnapshot({ error: '' })
  }

  private isCurrent(sessionId: string, generation: number, signal?: AbortSignal) {
    return this.enabled && this.matchesTarget(sessionId, generation, signal)
  }

  private matchesTarget(sessionId: string, generation: number, signal?: AbortSignal) {
    return this.sessionId === sessionId && this.generation === generation && !signal?.aborted
  }

  private stopTarget() {
    if (this.streamTimer !== null) clearTimeout(this.streamTimer)
    this.streamTimer = null
    this.streamEvents = []
    this.generation += 1
    this.historyEvents = null
    this.dependencies.live.setSession(null)
    this.abortController?.abort()
    this.abortController = null
    this.metadataFlight = null
    this.inboxFlight = null
  }

  private startTarget(sessionId: string) {
    const controller = new AbortController()
    this.abortController = controller
    this.updateSnapshot({ loading: true, loadingHistory: true })
    void this.loadInitialHistory(sessionId, this.generation, controller.signal)
  }

  private async readHistoryBatch(sessionId: string, signal?: AbortSignal, before?: number, acceptPage?: (page: SessionEventPage) => boolean): Promise<SessionEventPage> {
    const pageSize = 1_000
    const read = (cursor?: number) => this.dependencies.api.request<SessionEventPage>(
      `/sessions/${encodeURIComponent(sessionId)}/history?limit=${pageSize}${cursor === undefined ? '' : `&before_seq=${cursor}`}`, { signal },
    )
    let page = await read(before)
    if (acceptPage?.(page) === false) return page
    const events = [...page.events]
    const latest = page.events.at(-1)?.seq ?? 0
    const budget = before === undefined && latest < 10_000 ? 10_000 : 5_000
    while (page.next_before_seq !== null && page.events.length === pageSize
      && (events.length < budget || !containsOldestRunStart(events))) {
      if (signal?.aborted) break
      page = await read(page.next_before_seq)
      events.unshift(...page.events)
      if (acceptPage?.(page) === false) break
    }
    return { events, next_before_seq: page.next_before_seq }
  }

  private async loadInitialHistory(sessionId: string, generation: number, signal: AbortSignal) {
    const revision = this.historyRevision
    const accepted = { page: null as SessionEventPage | null }
    let subscribed = false
    try {
      const page = await this.readHistoryBatch(sessionId, signal, undefined, page => {
        if (!this.isCurrent(sessionId, generation, signal) || revision !== this.historyRevision) return false
        accepted.page = page
        this.mergeEvents(sessionId, page.events)
        this.updateSnapshot({ loadedSessionId: sessionId, loading: false })
        if (!subscribed) {
          subscribed = true
          this.dependencies.live.setSession(sessionId, { afterSeq: page.events.at(-1)?.seq ?? null })
        }
        return true
      })
      if (!this.isCurrent(sessionId, generation, signal) || revision !== this.historyRevision) return
      this.updateSnapshot({ nextBeforeSeq: page.next_before_seq })
    } catch (cause) {
      if (!this.isCurrent(sessionId, generation, signal) || revision !== this.historyRevision) return
      this.updateSnapshot({
        loadedSessionId: sessionId,
        loading: false,
        ...(accepted.page
          ? { nextBeforeSeq: accepted.page.next_before_seq, olderHistoryError: errorMessage(cause) }
          : { historyError: errorMessage(cause) }),
      })
    } finally {
      if (this.isCurrent(sessionId, generation, signal) && revision === this.historyRevision) {
        this.updateSnapshot({ loadingHistory: false })
      }
    }
  }

  readonly loadOlderHistory = async () => {
    const sessionId = this.sessionId
    const { nextBeforeSeq, loadingOlder } = this.getSnapshot()
    if (!sessionId || !this.enabled || nextBeforeSeq === null || loadingOlder) return
    const generation = this.generation
    const revision = this.historyRevision
    const signal = this.abortController?.signal
    this.updateSnapshot({ loadingOlder: true, olderHistoryError: '' })
    try {
      const page = await this.readHistoryBatch(sessionId, signal, nextBeforeSeq)
      if (!this.isCurrent(sessionId, generation, signal) || revision !== this.historyRevision) return
      this.mergeEvents(sessionId, page.events)
      this.updateSnapshot({ nextBeforeSeq: page.next_before_seq })
    } catch (cause) {
      if (this.isCurrent(sessionId, generation, signal) && revision === this.historyRevision) {
        this.updateSnapshot({ olderHistoryError: errorMessage(cause) })
      }
    } finally {
      if (this.isCurrent(sessionId, generation, signal) && revision === this.historyRevision) this.updateSnapshot({ loadingOlder: false })
    }
  }

  private handleLiveStatus(status: LiveConnectionStatus) {
    this.updateSnapshot({ liveStatus: status })
  }

  private handleLiveFrame(frame: LiveServerFrame) {
    if (frame.type === 'workbench') {
      this.dependencies.acceptWorkbench(frame.state, frame.revision, frame.activity)
      return
    }
    if (frame.type === 'activity') {
      this.dependencies.acceptActivity(frame.activity)
      return
    }
    const sessionId = this.sessionId
    if (!sessionId || !this.enabled) return
    if (frame.type === 'event_batch' && frame.session_id === sessionId && !frame.reset
      && this.historyEvents === null && frame.events.length > 0
      && frame.events.every(event => event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta')) {
      this.streamEvents.push(...frame.events)
      if (frame.complete) this.updateSnapshot({ loadedSessionId: sessionId, loading: false })
      if (this.streamTimer === null) this.streamTimer = setTimeout(() => this.flushStreamEvents(), 100)
      return
    }
    this.flushStreamEvents()
    if (frame.type === 'error') {
      if (frame.code === 'policy_denied' || frame.code === 'invalid_input') {
        this.historyRevision += 1
        this.abortController?.abort()
        this.cachedSessions.delete(sessionId)
        this.eventBuffer = createSessionEventBuffer(sessionId)
        this.historyEvents = null
        this.replaceSnapshot({ ...initialSnapshot(), liveStatus: this.getSnapshot().liveStatus })
      }
      if (this.historyEvents !== null) {
        this.mergeEvents(sessionId, this.historyEvents)
        this.historyEvents = null
      }
      this.updateSnapshot({
        loadedSessionId: sessionId,
        loading: false,
        loadingHistory: false,
        historyError: frame.message,
      })
      return
    }
    if (frame.type === 'event_batch') {
      if (frame.session_id !== sessionId) return
      if (frame.reset) {
        this.historyRevision += 1
        this.updateSnapshot({ nextBeforeSeq: null, loadingHistory: false, loadingOlder: false, olderHistoryError: '' })
        this.eventBuffer = createSessionEventBuffer(sessionId)
        this.historyEvents = []
        this.updateSnapshot({ events: [], loading: true, historyError: '' })
      }
      if (this.historyEvents !== null) {
        for (const event of frame.events) this.historyEvents.push(event)
        if (!frame.complete) return
        const history = this.historyEvents
        this.historyEvents = null
        this.mergeEvents(sessionId, history)
      } else {
        this.mergeEvents(sessionId, frame.events)
      }
      if (frame.complete) {
        this.updateSnapshot({ loadedSessionId: sessionId, loading: false, historyError: '' })
      }
      return
    }
    if (frame.type !== 'session_metadata' || frame.session_id !== sessionId) return

    const { metadata } = frame
    const patch: Partial<SessionControllerSnapshot> = { metadataWarning: '' }
    if (metadata.read.inbox) {
      this.inboxRevision += 1
      const inbox = metadata.inbox ?? null
      patch.inbox = inbox
      patch.activeRunId = inbox?.active_run_id ?? null
      patch.busy = patch.activeRunId !== null
      if (inbox?.error) this.setRuntimeError(inbox.error)
      if (inbox) this.reconcilePending(sessionId, this.eventBuffer.events, inbox)
    }
    if (metadata.read.stats) patch.stats = metadata.stats ?? null
    if (metadata.read.projection) patch.projection = metadata.projection ?? null
    if (metadata.read.questions) patch.questions = metadata.questions ?? []
    if (metadata.read.profile) patch.effectiveProfile = metadata.profile ?? null
    if (metadata.read.agent_team) patch.agentTeam = metadata.agent_team ?? null
    this.updateSnapshot(patch)
  }

  private flushStreamEvents() {
    if (this.streamTimer !== null) clearTimeout(this.streamTimer)
    this.streamTimer = null
    const events = this.streamEvents
    this.streamEvents = []
    if (this.sessionId && this.enabled && events.length) this.mergeEvents(this.sessionId, events)
  }

  private mergeEvents(sessionId: string, incoming: SessionEvent[]) {
    if (!incoming.length || this.sessionId !== sessionId) return
    const activeRunId = this.getSnapshot().activeRunId
    const refreshInbox = activeRunId !== null
      && incoming.some(event => event.run_id === activeRunId && terminalTurn(event))
    const next = mergeSessionEventBuffer(this.eventBuffer, sessionId, incoming)
    if (next !== this.eventBuffer) {
      this.eventBuffer = next
      this.reconcilePending(sessionId, next.events, this.getSnapshot().inbox)
      this.updateSnapshot({ events: conversationEvents(next.events) })
    }
    if (refreshInbox) {
      const generation = this.generation
      const inbox = this.getSnapshot().inbox
      void this.reloadInboxFor(sessionId).catch(cause => {
        if (this.isCurrent(sessionId, generation) && this.getSnapshot().inbox === inbox) {
          this.updateSnapshot({ metadataWarning: errorMessage(cause) })
        }
      })
    }
  }

  private setPending(sessionId: string, update: (current: PendingSubmissionEcho[]) => PendingSubmissionEcho[]) {
    const current = this.pendingBySession.get(sessionId) ?? []
    const next = update(current)
    if (next.length) this.pendingBySession.set(sessionId, next)
    else this.pendingBySession.delete(sessionId)
    if (this.sessionId === sessionId) {
      this.updateSnapshot({
        pendingSubmissions: visibleSubmissionEchoes(next, this.eventBuffer.events, this.getSnapshot().inbox),
      })
    }
  }

  private reconcilePending(sessionId: string, events: SessionEvent[], inbox: SessionInboxSnapshot | null) {
    const current = this.pendingBySession.get(sessionId) ?? []
    const visible = visibleSubmissionEchoes(current, events, inbox)
    if (visible.length !== current.length) {
      if (visible.length) this.pendingBySession.set(sessionId, visible)
      else this.pendingBySession.delete(sessionId)
    }
    if (this.sessionId === sessionId) {
      const previous = this.getSnapshot().pendingSubmissions
      if (previous.length !== visible.length || visible.some((item, index) => item !== previous[index])) {
        this.updateSnapshot({ pendingSubmissions: visible })
      }
    }
  }

  private reloadMetadataFor(sessionId: string, requested: MetadataRefresh = allMetadata): Promise<void> {
    if (this.sessionId !== sessionId) return Promise.resolve()
    const generation = this.generation
    const current = this.metadataFlight
    if (current?.sessionId === sessionId && current.generation === generation) {
      current.pending = mergeMetadata(current.pending, requested)
      return current.promise
    }
    const flight: MetadataFlight = {
      sessionId,
      generation,
      signal: this.abortController?.signal,
      pending: requested,
      promise: Promise.resolve(),
    }
    flight.promise = this.runMetadataFlight(flight)
    this.metadataFlight = flight
    return flight.promise
  }

  private async runMetadataFlight(flight: MetadataFlight) {
    try {
      do {
        const requested = flight.pending
        flight.pending = noMetadata()
        const encoded = encodeURIComponent(flight.sessionId)
        const options = flight.signal ? { signal: flight.signal } : undefined
        const results = await Promise.allSettled([
          requested.stats
            ? this.dependencies.api.request<SessionStats>(`/sessions/${encoded}/stats`, options)
            : Promise.resolve(undefined),
          requested.projection
            ? this.dependencies.api.request<SessionProjection>(`/sessions/${encoded}/projection`, options)
            : Promise.resolve(undefined),
          requested.questions
            ? this.dependencies.api.request<PendingQuestion[]>(`/questions?session_id=${encoded}`, options)
            : Promise.resolve(undefined),
          requested.profile
            ? this.dependencies.api.request<Profile>(`/sessions/${encoded}/plugins`, options)
            : Promise.resolve(undefined),
        ])
        if (!this.matchesTarget(flight.sessionId, flight.generation, flight.signal)) return
        const labels = this.dependencies.labels().metadata
        const failures: string[] = []
        const included = [requested.stats, requested.projection, requested.questions, requested.profile]
        results.forEach((result, index) => {
          if (included[index] && result.status === 'rejected') failures.push(`${labels[index]}：${errorMessage(result.reason)}`)
        })
        const [stats, projection, questions, profile] = results
        this.updateSnapshot({
          ...(requested.stats && stats.status === 'fulfilled' ? { stats: stats.value } : {}),
          ...(requested.projection && projection.status === 'fulfilled' ? { projection: projection.value } : {}),
          ...(requested.questions && questions.status === 'fulfilled' ? { questions: questions.value } : {}),
          ...(requested.profile && profile.status === 'fulfilled' ? { effectiveProfile: profile.value } : {}),
          metadataWarning: failures.join('；'),
        })
      } while (hasMetadata(flight.pending) && this.matchesTarget(flight.sessionId, flight.generation, flight.signal))
    } finally {
      if (this.metadataFlight === flight) this.metadataFlight = null
    }
  }

  private reloadInboxFor(sessionId: string): Promise<void> {
    if (this.sessionId !== sessionId) return Promise.resolve()
    const generation = this.generation
    const current = this.inboxFlight
    if (current?.sessionId === sessionId && current.generation === generation) {
      current.dirty = true
      return current.promise
    }
    const flight: InboxFlight = {
      sessionId,
      generation,
      signal: this.abortController?.signal,
      dirty: false,
      promise: Promise.resolve(),
    }
    flight.promise = this.runInboxFlight(flight)
    this.inboxFlight = flight
    return flight.promise
  }

  private async runInboxFlight(flight: InboxFlight) {
    try {
      do {
        flight.dirty = false
        const revision = this.inboxRevision
        const next = await this.dependencies.api.request<SessionInboxSnapshot>(
          `/sessions/${encodeURIComponent(flight.sessionId)}/queue`,
          flight.signal ? { signal: flight.signal } : undefined,
        )
        if (!this.matchesTarget(flight.sessionId, flight.generation, flight.signal)) return
        if (revision !== this.inboxRevision) continue
        const activeRunId = next.active_run_id ?? null
        this.updateSnapshot({
          inbox: next,
          activeRunId,
          busy: activeRunId !== null,
        })
        if (next.error) this.setRuntimeError(next.error)
        this.reconcilePending(flight.sessionId, this.eventBuffer.events, next)
      } while (flight.dirty && this.matchesTarget(flight.sessionId, flight.generation, flight.signal))
    } finally {
      if (this.inboxFlight === flight) this.inboxFlight = null
    }
  }

  readonly reloadMetadata = async () => {
    if (this.sessionId) await this.reloadMetadataFor(this.sessionId)
  }

  readonly retryHistory = () => {
    if (!this.sessionId || !this.enabled) return
    const sessionId = this.sessionId
    const liveStatus = this.getSnapshot().liveStatus
    this.eventBuffer = createSessionEventBuffer(sessionId)
    this.historyEvents = null
    const pending = this.pendingBySession.get(sessionId) ?? []
    this.replaceSnapshot({ ...initialSnapshot(pending), liveStatus })
    this.stopTarget()
    this.startTarget(sessionId)
  }

  readonly submit = async (
    input: string,
    attachments: Array<{ name: string; media_type: string; content: string }> = [],
    delivery: SubmissionDelivery = 'queue',
    references: SubmissionReference[] = [],
    regenerateFrom?: number,
  ) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    const runId = this.dependencies.randomId()
    this.clearRuntimeError()
    let content: { kind: 'prompt'; input: string } | { kind: 'skill'; name: string; input: string } = { kind: 'prompt', input }
    if (input.startsWith('/skill ')) {
      const invocation = input.slice('/skill '.length).trim()
      const separator = invocation.search(/\s/)
      const name = separator === -1 ? invocation : invocation.slice(0, separator)
      if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(name)) throw new Error(this.dependencies.labels().skillNameError)
      content = { kind: 'skill', name, input: separator === -1 ? '' : invocation.slice(separator).trim() }
    }
    const echo: PendingSubmissionEcho = {
      author: this.dependencies.inputAuthor(),
      request_id: runId,
      session_id: sessionId,
      run_id: runId,
      delivery,
      input,
      references,
      attachments,
      created_at_ms: this.dependencies.now(),
    }
    if (regenerateFrom === undefined) this.setPending(sessionId, current => [...current, echo])
    let submission: SessionSubmission
    try {
      submission = await this.dependencies.api.request<SessionSubmission>(
        `/sessions/${encodeURIComponent(sessionId)}/queue`,
        { method: 'POST', body: { delivery, run_id: runId,
          content: regenerateFrom === undefined ? content : {
            kind: 'regenerate', target_seq: regenerateFrom, input: content.input,
            skill_name: content.kind === 'skill' ? content.name : undefined,
          }, references, attachments } },
      )
    } catch (cause) {
      this.setPending(sessionId, current => current.filter(item => item.request_id !== echo.request_id))
      const message = errorMessage(cause)
      if (this.sessionId === sessionId) this.setRuntimeError(message)
      throw cause instanceof Error ? cause : new Error(message)
    }
    this.setPending(sessionId, current => current.map(item => item.request_id === echo.request_id
      ? { ...item, submission_id: submission.id, author: submission.provenance?.author }
      : item))
    void Promise.allSettled([
      this.reloadInboxFor(sessionId),
      this.dependencies.refresh(),
    ]).then(synchronized => {
      const failure = synchronized.find(result => result.status === 'rejected')
      if (failure?.status === 'rejected' && this.sessionId === sessionId) {
        this.setRuntimeError(errorMessage(failure.reason))
      }
    })
  }

  readonly submitFeedback = async (text: string) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    this.clearRuntimeError()
    const receipt = await this.dependencies.api.request<SessionCommandReceipt>(
      `/sessions/${encodeURIComponent(sessionId)}/commands/feedback`,
      { method: 'POST', body: { text } },
    )
    this.mergeEvents(sessionId, receipt.events)
    const metadata = metadataForEvents(receipt.events)
    await Promise.allSettled([
      ...(hasMetadata(metadata) ? [this.reloadMetadataFor(sessionId, metadata)] : []),
      ...(receipt.events.some(updatesInbox) ? [this.reloadInboxFor(sessionId)] : []),
      this.dependencies.refresh(),
    ])
  }

  readonly editQueueItem = async (id: string, input: string, expectedUpdatedAtMs: number) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    try {
      await this.dependencies.api.request(`/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(id)}`, {
        method: 'PATCH', body: { input, expected_updated_at_ms: expectedUpdatedAtMs },
      })
    } catch (cause) {
      await Promise.allSettled([this.reloadInboxFor(sessionId)])
      throw cause
    }
    await this.reloadInboxFor(sessionId)
  }

  readonly removeQueueItem = async (id: string) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    await this.dependencies.api.request(`/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(id)}`, { method: 'DELETE' })
    await this.reloadInboxFor(sessionId)
  }

  readonly loadQueueItem = async (id: string) => {
    if (!this.sessionId) return undefined
    const sessionId = this.sessionId
    const generation = this.generation
    await this.reloadInboxFor(sessionId)
    if (sessionId !== this.sessionId || generation !== this.generation) return undefined
    return this.getSnapshot().inbox?.items.find(item => item.id === id && item.placement === 'queued')
  }

  readonly steerQueueItem = async (id: string) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    await this.dependencies.api.request<SessionSubmission>(
      `/sessions/${encodeURIComponent(sessionId)}/queue/${encodeURIComponent(id)}/steer`,
      { method: 'POST' },
    )
    await this.reloadInboxFor(sessionId)
  }

  readonly cancel = async () => {
    const activeRunId = this.getSnapshot().activeRunId
    if (!this.sessionId || !activeRunId) return
    const sessionId = this.sessionId
    await this.dependencies.api.request(
      `/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(activeRunId)}`,
      { method: 'DELETE' },
    )
    this.dependencies.notify(this.dependencies.labels().runStopping, 'info')
    await this.reloadInboxFor(sessionId)
  }

  readonly answerQuestion = async (questionId: string, answer: UserQuestionAnswer) => {
    if (!this.sessionId) return
    const sessionId = this.sessionId
    await this.dependencies.api.request(
      `/questions/${encodeURIComponent(questionId)}/answer?session_id=${encodeURIComponent(sessionId)}`,
      { method: 'POST', body: answer },
    )
    await this.reloadMetadataFor(sessionId)
  }
}
