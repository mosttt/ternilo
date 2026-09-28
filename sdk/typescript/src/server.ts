import { randomUUID } from 'node:crypto'
import { setTimeout as delay } from 'node:timers/promises'
import type { JsonObject, JsonValue, RunResult } from './client.ts'

export interface ServerClientOptions {
  baseUrl: string
  accessToken: string
  tenantId: string
  requestTimeoutMs?: number
}

export interface ServerEvent extends JsonObject {
  seq: number
  run_id: string
  type: string
}

export interface EventBatch {
  type: 'event_batch'
  session_id: string
  subscription_id: number
  reset: boolean
  complete: boolean
  next_seq: number
  events: ServerEvent[]
}

export interface WatchOptions {
  afterSeq?: number | null
  timeoutMs?: number
  reconnectTimeoutMs?: number
  signal?: AbortSignal
}

export class ServerError extends Error {
  readonly code: string
  readonly status?: number
  constructor(code: string, message: string, status?: number) {
    super(message)
    this.code = code
    this.status = status
  }
}

class Disconnected extends Error {}
const terminalStatus: Partial<Record<string, RunResult['status']>> = {
  turn_finished: 'idle', turn_failed: 'failed', turn_cancelled: 'cancelled',
}

/** HTTP and resumable Live access bound to one account and space. */
export class ServerClient {
  private readonly base: string
  private readonly options: ServerClientOptions
  private readonly lifetime = new AbortController()

  constructor(options: ServerClientOptions) {
    const url = new URL(options.baseUrl)
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
      throw new TypeError('baseUrl must be an HTTP(S) Server URL without credentials, query or fragment')
    }
    if (!options.accessToken || !options.tenantId || (options.requestTimeoutMs ?? 30000) <= 0) {
      throw new TypeError('accessToken, tenantId and a positive requestTimeoutMs are required')
    }
    this.base = url.href.replace(/\/$/, '')
    this.options = { ...options }
  }

  async request<T = JsonValue>(resource: string, options: { method?: string; body?: JsonValue; signal?: AbortSignal; timeoutMs?: number } = {}): Promise<T> {
    if (!resource.startsWith('/')) throw new TypeError('resource must start with /')
    const signal = AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(options.timeoutMs ?? this.options.requestTimeoutMs ?? 30000), ...(options.signal ? [options.signal] : [])])
    const response = await fetch(`${this.base}/api/v1${resource}`, {
      method: options.method ?? 'GET', redirect: 'error', signal,
      headers: { authorization: `Bearer ${this.options.accessToken}`, 'x-ternilo-tenant': this.options.tenantId,
        ...(options.body === undefined ? {} : { 'content-type': 'application/json' }) },
      ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
    })
    if (!response.ok) {
      const detail = await response.json().catch(() => ({})) as { error?: { code?: string; message?: string } }
      throw new ServerError(detail.error?.code ?? 'http_error', detail.error?.message ?? `Server HTTP ${response.status}`, response.status)
    }
    return response.status === 204 ? undefined as T : await response.json() as T
  }

  state(): Promise<JsonObject> { return this.request('/state') }

  createSession(workspaceId: string): Promise<JsonObject> {
    return this.request('/sessions', { method: 'POST', body: { workspace_id: workspaceId } })
  }

  history(sessionId: string, options: { beforeSeq?: number; limit?: number } = {}): Promise<{ events: ServerEvent[]; next_before_seq: number | null }> {
    const query = new URLSearchParams({ limit: String(options.limit ?? 200) })
    if (options.beforeSeq !== undefined) query.set('before_seq', String(options.beforeSeq))
    return this.request(`/sessions/${encodeURIComponent(sessionId)}/history?${query}`)
  }

  submit(sessionId: string, input: string, options: { runId?: string } = {}): Promise<JsonObject> {
    return this.request(`/sessions/${encodeURIComponent(sessionId)}/queue`, { method: 'POST', body: {
      run_id: options.runId ?? randomUUID(), content: { kind: 'prompt', input },
    } })
  }

  async cancel(sessionId: string, runId: string): Promise<void> {
    await this.request(`/sessions/${encodeURIComponent(sessionId)}/turns/${encodeURIComponent(runId)}`, { method: 'DELETE' })
  }

  async *watch(sessionId: string, options: WatchOptions = {}): AsyncGenerator<EventBatch, void, unknown> {
    const reconnectTimeout = options.reconnectTimeoutMs ?? 30000
    if (reconnectTimeout <= 0 || (options.timeoutMs !== undefined && options.timeoutMs <= 0)) throw new TypeError('timeouts must be positive')
    const signal = AbortSignal.any([this.lifetime.signal, ...(options.signal ? [options.signal] : []), ...(options.timeoutMs ? [AbortSignal.timeout(options.timeoutMs)] : [])])
    let cursor = options.afterSeq ?? null
    let outage = Date.now(), retryDelay = 100
    const url = this.base.replace(/^http/, 'ws') + '/api/v1/live'
    while (!this.lifetime.signal.aborted) {
      signal.throwIfAborted()
      if (Date.now() - outage >= reconnectTimeout) throw new ServerError('unavailable', 'Server Live reconnection timed out')
      let ready = false
      let received = false
      const stream = new SocketFrames(url, signal)
      try {
        const handshakeTimeout = Math.min(this.options.requestTimeoutMs ?? 30000, reconnectTimeout - (Date.now() - outage))
        const handshakeDeadline = Date.now() + handshakeTimeout
        await stream.open(handshakeTimeout)
        stream.send({ type: 'hello', protocol_version: 1, bearer_token: this.options.accessToken, tenant_id: this.options.tenantId })
        for (;;) {
          const frame = await stream.next(ready ? undefined : Math.max(0, handshakeDeadline - Date.now()))
          if (frame.type === 'error') throw new ServerError(String(frame.code), String(frame.message))
          if (frame.type === 'ready') {
            if (ready || frame.protocol_version !== 1) throw new ServerError('protocol', 'unsupported Server Live handshake')
            ready = true
            stream.send({ type: 'subscribe', subscription_id: 1, session_id: sessionId,
              ...(cursor === null ? {} : { after_seq: cursor }),
              metadata: { inbox: false, stats: false, projection: false, questions: false, profile: false, agent_team: false } })
          } else if (frame.type === 'event_batch') {
            if (!ready || frame.subscription_id !== 1 || frame.session_id !== sessionId) throw new ServerError('protocol', 'Server Live batch belongs to another subscription')
            const batch = eventBatch(frame, cursor)
            cursor = batch.next_seq === 0 ? null : batch.next_seq - 1
            received = true
            retryDelay = 100
            yield batch
          }
        }
      } catch (error) {
        if (this.lifetime.signal.aborted) return
        signal.throwIfAborted()
        if (!(error instanceof Disconnected)) throw error
      } finally {
        stream.close()
      }
      if (received) outage = Date.now()
      try {
        await delay(Math.max(0, Math.min(retryDelay, reconnectTimeout - (Date.now() - outage))), undefined, { signal })
      } catch (error) {
        if (this.lifetime.signal.aborted) return
        throw error
      }
      retryDelay = Math.min(retryDelay * 2, 2000)
    }
  }

  async run(sessionId: string, input: string, options: { timeoutMs?: number; signal?: AbortSignal } = {}): Promise<RunResult> {
    const history = await this.history(sessionId, { limit: 1 })
    const runId = randomUUID()
    await this.submit(sessionId, input, { runId })
    const events: ServerEvent[] = []
    for await (const batch of this.watch(sessionId, { afterSeq: history.events.at(-1)?.seq, timeoutMs: options.timeoutMs ?? 300000, signal: options.signal })) {
      if (batch.reset) events.length = 0
      for (const event of batch.events) {
        if (event.run_id !== runId) continue
        events.push(event)
        const status = terminalStatus[event.type]
        if (status) return { sessionId, runId, status, answer: typeof event.answer === 'string' ? event.answer : '', events, notifications: [] }
      }
    }
    throw new ServerError('closed', 'Server client closed before the run completed')
  }

  close(): void { this.lifetime.abort(new ServerError('closed', 'Server client is closed')) }
}

function eventBatch(frame: JsonObject, cursor: number | null): EventBatch {
  if (!Array.isArray(frame.events) || !Number.isSafeInteger(frame.next_seq) || Number(frame.next_seq) < 0 || typeof frame.reset !== 'boolean' || typeof frame.complete !== 'boolean') {
    throw new ServerError('protocol', 'invalid Server Live event batch')
  }
  let previous = -1
  for (const value of frame.events) {
    const seq = value && typeof value === 'object' && !Array.isArray(value) ? value.seq : undefined
    if (!Number.isSafeInteger(seq) || Number(seq) <= previous || Number(seq) >= Number(frame.next_seq)) throw new ServerError('protocol', 'invalid Server Live event sequence')
    previous = Number(seq)
  }
  if (!frame.reset && cursor !== null && Number(frame.next_seq) < cursor + 1) throw new ServerError('protocol', 'Server Live cursor moved backwards without a reset')
  const events = frame.events as ServerEvent[]
  return { ...frame, events: events.filter(event => frame.reset || cursor === null || event.seq > cursor) } as unknown as EventBatch
}

class SocketFrames {
  private readonly socket: WebSocket
  private readonly frames: JsonObject[] = []
  private failure?: Error
  private waiter?: () => void
  private readonly signal: AbortSignal
  private readonly abort: () => void

  constructor(url: string, signal: AbortSignal) {
    this.signal = signal
    this.socket = new WebSocket(url)
    this.abort = () => this.fail(signal.reason instanceof Error ? signal.reason : new Error('Server watch aborted'))
    signal.addEventListener('abort', this.abort, { once: true })
    this.socket.onopen = () => this.waiter?.()
    this.socket.onerror = () => this.fail(new Disconnected('Server Live connection failed'))
    this.socket.onclose = () => this.fail(new Disconnected('Server Live connection closed'))
    this.socket.onmessage = event => {
      try {
        const frame = JSON.parse(String(event.data)) as JsonObject
        if (!frame || typeof frame !== 'object' || typeof frame.type !== 'string') throw new Error('invalid Live frame')
        if (this.frames.length >= 128) { this.fail(new Disconnected('Server Live consumer is behind')); return }
        this.frames.push(frame)
        this.waiter?.()
      } catch { this.fail(new ServerError('protocol', 'invalid Server Live JSON frame')) }
    }
  }

  async open(timeout: number): Promise<void> {
    if (this.socket.readyState !== WebSocket.OPEN) await this.wait(timeout)
    if (this.failure) throw this.failure
  }

  async next(timeout?: number): Promise<JsonObject> {
    if (!this.frames.length) await this.wait(timeout)
    this.signal.throwIfAborted()
    const frame = this.frames.shift()
    if (frame) return frame
    throw this.failure ?? new Disconnected('Server Live connection closed')
  }

  send(frame: JsonObject): void {
    if (this.socket.readyState !== WebSocket.OPEN) throw new Disconnected('Server Live connection closed')
    this.socket.send(JSON.stringify(frame))
  }

  private wait(timeout?: number): Promise<void> {
    this.signal.throwIfAborted()
    if (this.failure) return Promise.reject(this.failure)
    return new Promise((resolve, reject) => {
      const timer = timeout === undefined ? undefined : setTimeout(() => {
        this.waiter = undefined
        reject(new Disconnected('Server Live handshake timed out'))
      }, timeout)
      this.waiter = () => { clearTimeout(timer); this.waiter = undefined; resolve() }
    })
  }

  private fail(error: Error): void {
    this.failure ??= error
    this.waiter?.()
    this.socket.close()
  }

  close(): void {
    this.signal.removeEventListener('abort', this.abort)
    this.socket.close()
  }
}
