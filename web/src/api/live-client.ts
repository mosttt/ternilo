import type {
  AgentTeamSnapshot,
  ApplicationState,
  PendingQuestion,
  Profile,
  SessionEvent,
  SessionInboxSnapshot,
  SessionProjection,
  SessionStats,
} from '@/types'
import type { ExecutionPhase } from '@/domain/turn-activity'

export const LIVE_PROTOCOL_VERSION = 1

export interface SessionLiveReadMask {
  inbox: boolean
  stats: boolean
  projection: boolean
  questions: boolean
  profile: boolean
  agent_team: boolean
}

export const ALL_SESSION_LIVE_READS: SessionLiveReadMask = {
  inbox: true,
  stats: true,
  projection: true,
  questions: true,
  profile: true,
  agent_team: true,
}

export interface SessionLiveMetadata {
  read: SessionLiveReadMask
  inbox?: SessionInboxSnapshot
  stats?: SessionStats
  projection?: SessionProjection
  questions?: PendingQuestion[]
  profile?: Profile
  agent_team?: AgentTeamSnapshot
}

export interface SessionLiveActivity {
  session_id: string
  running: boolean
  updated_at_ms: number
  execution?: { run_id: string; phase: ExecutionPhase }
}

export type LiveErrorCode =
  | 'invalid_input'
  | 'composition'
  | 'policy_denied'
  | 'execution'
  | 'unavailable'
  | 'cancelled'
  | 'conflict'

export type LiveClientFrame =
  | {
      type: 'hello'
      protocol_version: number
      bearer_token?: string
      tenant_id?: string
    }
  | {
      type: 'subscribe'
      subscription_id: number
      session_id: string
      /** Last accepted event sequence; the server starts at the following sequence. */
      after_seq?: number
      metadata: SessionLiveReadMask
    }
  | {
      type: 'unsubscribe'
      subscription_id: number
    }

export type LiveServerFrame =
  | { type: 'ready'; protocol_version: number }
  | { type: 'workbench'; revision: number; state: ApplicationState; activity: SessionLiveActivity[] }
  | { type: 'activity'; activity: SessionLiveActivity }
  | {
      type: 'event_batch'
      subscription_id: number
      session_id: string
      reset: boolean
      complete: boolean
      events: SessionEvent[]
      /** First event sequence not included in this or any preceding batch. */
      next_seq: number
    }
  | {
      type: 'session_metadata'
      subscription_id: number
      session_id: string
      metadata: SessionLiveMetadata
    }
  | {
      type: 'error'
      subscription_id?: number
      code: LiveErrorCode
      message: string
    }

export type LiveConnectionStatus =
  | 'idle'
  | 'connecting'
  | 'authenticating'
  | 'ready'
  | 'reconnecting'
  | 'stopped'

export interface LiveCredentials {
  bearerToken?: string | null
  tenantId?: string | null
  revision?: number
}

export interface LiveSubscriptionOptions {
  afterSeq?: number | null
  metadata?: SessionLiveReadMask
}

export interface LiveSocket {
  readonly readyState: number
  onopen: ((event: Event) => unknown) | null
  onmessage: ((event: MessageEvent) => unknown) | null
  onclose: ((event: CloseEvent) => unknown) | null
  onerror: ((event: Event) => unknown) | null
  send(data: string): void
  close(code?: number, reason?: string): void
}

type TimerHandle = ReturnType<typeof setTimeout>

interface LiveTimer {
  setTimeout(callback: () => void, delayMs: number): TimerHandle
  clearTimeout(handle: TimerHandle): void
}

export interface LiveNetworkEvents {
  addEventListener(type: 'online' | 'offline', listener: EventListener): void
  removeEventListener(type: 'online' | 'offline', listener: EventListener): void
}

export interface LiveClientOptions {
  url: string
  credentials: () => LiveCredentials
  createSocket?: (url: string) => LiveSocket
  timer?: LiveTimer
  reconnectInitialDelayMs?: number
  reconnectMaxDelayMs?: number
  reconnectStableAfterMs?: number
  networkEvents?: LiveNetworkEvents | null
}

interface ActiveSubscription {
  subscriptionId: number
  sessionId: string
  afterSeq: number | null
  metadata: SessionLiveReadMask
}

const SOCKET_OPEN = 1

const defaultTimer: LiveTimer = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: handle => clearTimeout(handle),
}

function isLiveServerFrame(value: unknown): value is LiveServerFrame {
  return typeof value === 'object'
    && value !== null
    && typeof (value as { type?: unknown }).type === 'string'
}

function sameReadMask(left: SessionLiveReadMask, right: SessionLiveReadMask) {
  return left.inbox === right.inbox
    && left.stats === right.stats
    && left.projection === right.projection
    && left.questions === right.questions
    && left.profile === right.profile
    && left.agent_team === right.agent_team
}

export class LiveClient {
  private readonly createSocket: (url: string) => LiveSocket
  private readonly timer: LiveTimer
  private readonly reconnectInitialDelayMs: number
  private readonly reconnectMaxDelayMs: number
  private readonly reconnectStableAfterMs: number
  private readonly networkEvents: LiveNetworkEvents | null
  private readonly frameListeners = new Set<(frame: LiveServerFrame) => void>()
  private readonly statusListeners = new Set<(status: LiveConnectionStatus) => void>()
  private socket: LiveSocket | null = null
  private reconnectTimer: TimerHandle | null = null
  private stableTimer: TimerHandle | null = null
  private reconnectAttempt = 0
  private socketGeneration = 0
  private nextSubscriptionId = 1
  private activeSubscription: ActiveSubscription | null = null
  private running = false
  private ready = false
  private currentStatus: LiveConnectionStatus = 'idle'
  private observingNetwork = false
  private readonly handleOffline: EventListener = () => {
    if (!this.running) return
    this.ready = false
    this.setStatus('reconnecting')
    if (this.socket) this.socket.close(1000, 'browser offline')
    else this.scheduleReconnect()
  }
  private readonly handleOnline: EventListener = () => {
    if (!this.running || this.ready) return
    if (this.reconnectTimer !== null) {
      this.timer.clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    const socket = this.socket
    if (socket) {
      this.socketGeneration += 1
      this.socket = null
      socket.onopen = null
      socket.onmessage = null
      socket.onclose = null
      socket.onerror = null
      socket.close(1000, 'browser online')
    }
    this.openSocket()
  }

  constructor(private readonly options: LiveClientOptions) {
    this.createSocket = options.createSocket
      ?? (url => new WebSocket(url) as LiveSocket)
    this.timer = options.timer ?? defaultTimer
    this.reconnectInitialDelayMs = Math.max(1, options.reconnectInitialDelayMs ?? 250)
    this.reconnectMaxDelayMs = Math.max(
      this.reconnectInitialDelayMs,
      options.reconnectMaxDelayMs ?? 10_000,
    )
    this.reconnectStableAfterMs = Math.max(1, options.reconnectStableAfterMs ?? 10_000)
    this.networkEvents = options.networkEvents === undefined
      ? typeof window === 'undefined' ? null : window
      : options.networkEvents
  }

  get status() {
    return this.currentStatus
  }

  get subscriptionId() {
    return this.activeSubscription?.subscriptionId ?? null
  }

  start() {
    if (this.running) return
    this.running = true
    this.reconnectAttempt = 0
    this.observeNetwork()
    this.openSocket()
  }

  stop() {
    if (!this.running && this.currentStatus === 'stopped') return
    this.running = false
    this.ready = false
    this.socketGeneration += 1
    if (this.reconnectTimer !== null) {
      this.timer.clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.clearStableTimer()
    this.stopObservingNetwork()
    const socket = this.socket
    this.socket = null
    if (socket) {
      socket.onopen = null
      socket.onmessage = null
      socket.onclose = null
      socket.onerror = null
      socket.close(1000, 'client stopped')
    }
    this.setStatus('stopped')
  }

  credentialsChanged() {
    if (!this.running) return
    this.ready = false
    this.socketGeneration += 1
    if (this.reconnectTimer !== null) {
      this.timer.clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.clearStableTimer()
    const socket = this.socket
    this.socket = null
    if (socket) {
      socket.onopen = null
      socket.onmessage = null
      socket.onclose = null
      socket.onerror = null
      socket.close(1000, 'credentials changed')
    }
    this.reconnectAttempt = 0
    if (this.options.credentials().bearerToken) this.openSocket()
    else this.setStatus('reconnecting')
  }

  setSession(sessionId: string | null, options: LiveSubscriptionOptions = {}) {
    const normalizedSessionId = sessionId?.trim() ?? ''
    if (this.activeSubscription?.sessionId === normalizedSessionId) {
      const afterSeq = options.afterSeq === undefined
        ? this.activeSubscription.afterSeq
        : options.afterSeq ?? null
      const metadata = options.metadata ?? this.activeSubscription.metadata
      if (afterSeq === this.activeSubscription.afterSeq
        && sameReadMask(metadata, this.activeSubscription.metadata)) {
        return this.activeSubscription.subscriptionId
      }
      return this.resubscribe({ afterSeq, metadata })
    }

    const previous = this.activeSubscription
    this.activeSubscription = normalizedSessionId
      ? {
          subscriptionId: this.nextSubscriptionId++,
          sessionId: normalizedSessionId,
          afterSeq: options.afterSeq ?? null,
          metadata: { ...(options.metadata ?? ALL_SESSION_LIVE_READS) },
        }
      : null

    if (this.ready) {
      if (previous) {
        this.send({ type: 'unsubscribe', subscription_id: previous.subscriptionId })
      }
      this.sendSubscription()
    }
    return this.activeSubscription?.subscriptionId ?? null
  }

  resubscribe(options: LiveSubscriptionOptions = {}) {
    const previous = this.activeSubscription
    if (!previous) return null
    this.activeSubscription = {
      subscriptionId: this.nextSubscriptionId++,
      sessionId: previous.sessionId,
      afterSeq: options.afterSeq ?? null,
      metadata: { ...(options.metadata ?? previous.metadata) },
    }
    if (this.ready) {
      this.send({ type: 'unsubscribe', subscription_id: previous.subscriptionId })
      this.sendSubscription()
    }
    return this.activeSubscription.subscriptionId
  }

  onFrame(listener: (frame: LiveServerFrame) => void) {
    this.frameListeners.add(listener)
    return () => { this.frameListeners.delete(listener) }
  }

  onStatus(listener: (status: LiveConnectionStatus) => void) {
    this.statusListeners.add(listener)
    listener(this.currentStatus)
    return () => { this.statusListeners.delete(listener) }
  }

  private openSocket() {
    if (!this.running) return
    if (this.reconnectTimer !== null) {
      this.timer.clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    const generation = ++this.socketGeneration
    this.ready = false
    this.setStatus(this.reconnectAttempt === 0 ? 'connecting' : 'reconnecting')

    let socket: LiveSocket
    let credentialRevision: number | undefined
    try {
      socket = this.createSocket(this.options.url)
    } catch {
      this.scheduleReconnect()
      return
    }
    this.socket = socket

    socket.onopen = () => {
      if (!this.isCurrentSocket(socket, generation)) return
      this.setStatus('authenticating')
      const credentials = this.options.credentials()
      credentialRevision = credentials.revision
      this.send({
        type: 'hello',
        protocol_version: LIVE_PROTOCOL_VERSION,
        ...(credentials.bearerToken ? { bearer_token: credentials.bearerToken } : {}),
        ...(credentials.tenantId ? { tenant_id: credentials.tenantId } : {}),
      })
    }
    socket.onmessage = event => {
      if (!this.isCurrentSocket(socket, generation) || typeof event.data !== 'string') return
      if (credentialRevision !== this.options.credentials().revision) return
      let parsed: unknown
      try {
        parsed = JSON.parse(event.data)
      } catch {
        return
      }
      if (!isLiveServerFrame(parsed)) return
      this.receiveFrame(parsed, socket)
    }
    socket.onerror = () => {
      if (this.isCurrentSocket(socket, generation)) socket.close()
    }
    socket.onclose = () => {
      if (!this.isCurrentSocket(socket, generation)) return
      this.socket = null
      this.ready = false
      this.scheduleReconnect()
    }
  }

  private receiveFrame(frame: LiveServerFrame, socket: LiveSocket) {
    if (frame.type === 'ready') {
      if (frame.protocol_version !== LIVE_PROTOCOL_VERSION) {
        this.running = false
        socket.close(4002, 'unsupported live protocol version')
        this.setStatus('stopped')
        return
      }
      this.ready = true
      this.clearStableTimer()
      if (this.reconnectAttempt > 0) {
        this.stableTimer = this.timer.setTimeout(() => {
          this.stableTimer = null
          this.reconnectAttempt = 0
        }, this.reconnectStableAfterMs)
      }
      this.setStatus('ready')
      this.emit(frame)
      this.sendSubscription()
      return
    }

    if (frame.type === 'error') {
      if (frame.subscription_id === undefined
        || frame.subscription_id === this.activeSubscription?.subscriptionId) {
        this.emit(frame)
      }
      return
    }
    if (!this.ready) return
    if (frame.type === 'workbench' || frame.type === 'activity') {
      this.emit(frame)
      return
    }

    const subscription = this.activeSubscription
    if (!subscription
      || frame.subscription_id !== subscription.subscriptionId
      || frame.session_id !== subscription.sessionId) {
      return
    }
    if (frame.type === 'event_batch') {
      // EventBatch.next_seq is the first sequence not yet delivered, while
      // Subscribe.after_seq is the last sequence already accepted.
      subscription.afterSeq = frame.next_seq === 0 ? null : frame.next_seq - 1
    }
    this.emit(frame)
  }

  private sendSubscription() {
    const subscription = this.activeSubscription
    if (!subscription) return
    this.send({
      type: 'subscribe',
      subscription_id: subscription.subscriptionId,
      session_id: subscription.sessionId,
      ...(subscription.afterSeq === null ? {} : { after_seq: subscription.afterSeq }),
      metadata: subscription.metadata,
    })
  }

  private send(frame: LiveClientFrame) {
    if (this.socket?.readyState === SOCKET_OPEN) {
      this.socket.send(JSON.stringify(frame))
    }
  }

  private scheduleReconnect() {
    if (!this.running || this.reconnectTimer !== null) return
    this.clearStableTimer()
    this.setStatus('reconnecting')
    const delay = Math.min(
      this.reconnectInitialDelayMs * 2 ** this.reconnectAttempt,
      this.reconnectMaxDelayMs,
    )
    this.reconnectAttempt += 1
    this.reconnectTimer = this.timer.setTimeout(() => {
      this.reconnectTimer = null
      this.openSocket()
    }, delay)
  }

  private isCurrentSocket(socket: LiveSocket, generation: number) {
    return this.running && this.socket === socket && this.socketGeneration === generation
  }

  private clearStableTimer() {
    if (this.stableTimer === null) return
    this.timer.clearTimeout(this.stableTimer)
    this.stableTimer = null
  }

  private observeNetwork() {
    if (!this.networkEvents || this.observingNetwork) return
    this.networkEvents.addEventListener('offline', this.handleOffline)
    this.networkEvents.addEventListener('online', this.handleOnline)
    this.observingNetwork = true
  }

  private stopObservingNetwork() {
    if (!this.networkEvents || !this.observingNetwork) return
    this.networkEvents.removeEventListener('offline', this.handleOffline)
    this.networkEvents.removeEventListener('online', this.handleOnline)
    this.observingNetwork = false
  }

  private emit(frame: LiveServerFrame) {
    this.frameListeners.forEach(listener => listener(frame))
  }

  private setStatus(status: LiveConnectionStatus) {
    if (status === this.currentStatus) return
    this.currentStatus = status
    this.statusListeners.forEach(listener => listener(status))
  }
}
