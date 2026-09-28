import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createInterface, type Interface as ReadLineInterface } from 'node:readline'
import { resolve } from 'node:path'

export { ServerClient, ServerError } from './server.ts'
export type { ServerClientOptions, ServerEvent, EventBatch, WatchOptions } from './server.ts'

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue }
export type JsonObject = { [key: string]: JsonValue }

export interface HarnessClientOptions {
  command?: string
  args?: string[]
  cwd?: string
  env?: NodeJS.ProcessEnv
  requestTimeoutMs?: number
  shutdownTimeoutMs?: number
}

export interface RunOptions {
  workspacePath: string
  sessionId?: string
  attachments?: JsonObject[]
  timeoutMs?: number
  onNotification?: (notification: JsonObject) => void
}

export interface RunResult {
  sessionId: string
  runId: string
  status: 'idle' | 'cancelled' | 'failed'
  answer: string
  events: JsonObject[]
  notifications: JsonObject[]
}

export class TerniloError extends Error {}

export class ProtocolError extends TerniloError {}

export class RemoteError extends TerniloError {
  readonly code: number
  readonly data: JsonValue | undefined

  constructor(code: number, message: string, data?: JsonValue) {
    super(`Ternilo RPC ${code}: ${message}`)
    this.code = code
    this.data = data
  }
}

export class TransportClosedError extends TerniloError {}

interface PendingRequest {
  resolve: (value: JsonObject) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

class AsyncQueue<T> {
  private readonly values: T[] = []
  private readonly waiters: Array<(value: T) => void> = []

  push(value: T): void {
    const waiter = this.waiters.shift()
    if (waiter === undefined) this.values.push(value)
    else waiter(value)
  }

  next(timeoutMs: number): Promise<T> {
    const value = this.values.shift()
    if (value !== undefined) return Promise.resolve(value)
    return new Promise<T>((resolveValue, reject) => {
      let waiter: (value: T) => void
      const timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter)
        if (index >= 0) this.waiters.splice(index, 1)
        reject(new Error(`notification wait timed out after ${timeoutMs}ms`))
      }, timeoutMs)
      waiter = (nextValue) => {
        clearTimeout(timer)
        resolveValue(nextValue)
      }
      this.waiters.push(waiter)
    })
  }
}

export class HarnessClient {
  private readonly options: Required<Pick<HarnessClientOptions, 'command' | 'args' | 'requestTimeoutMs' | 'shutdownTimeoutMs'>> & HarnessClientOptions
  private process?: ChildProcessWithoutNullStreams
  private lines?: ReadLineInterface
  private requestId = 0
  private subscriberId = 0
  private closed = false
  private initialized?: Promise<JsonObject>
  private readonly pending = new Map<number, PendingRequest>()
  private readonly subscribers = new Map<number, { predicate: (frame: JsonObject) => boolean; queue: AsyncQueue<JsonObject> }>()
  private readonly stderrTail: string[] = []

  constructor(options: HarnessClientOptions = {}) {
    this.options = {
      ...options,
      command: options.command ?? 'ternilo',
      args: options.args ?? ['rpc'],
      requestTimeoutMs: options.requestTimeoutMs ?? 30_000,
      shutdownTimeoutMs: options.shutdownTimeoutMs ?? 5_000,
    }
  }

  start(): Promise<JsonObject> {
    if (this.closed) return Promise.reject(new TransportClosedError('client is closed'))
    if (this.initialized !== undefined) return this.initialized
    this.process = spawn(this.options.command, this.options.args, {
      cwd: this.options.cwd,
      env: this.options.env,
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    this.process.stdout.setEncoding('utf8')
    this.process.stderr.setEncoding('utf8')
    this.lines = createInterface({ input: this.process.stdout })
    this.lines.on('line', (line) => this.acceptLine(line))
    this.process.stderr.on('data', (chunk: string) => {
      this.stderrTail.push(...chunk.split('\n').filter(Boolean))
      if (this.stderrTail.length > 200) this.stderrTail.splice(0, this.stderrTail.length - 200)
    })
    this.process.once('exit', () => this.failAll(this.closedError('runtime exited')))
    this.initialized = this.request('initialize', {
      protocol_version: 1,
      client: { name: 'ternilo-typescript', version: '0.1.0' },
    })
    return this.initialized
  }

  request(method: string, params?: JsonObject, timeoutMs = this.options.requestTimeoutMs): Promise<JsonObject> {
    const process = this.process
    if (process === undefined || process.exitCode !== null) {
      return Promise.reject(this.closedError('runtime is not running'))
    }
    const id = ++this.requestId
    const frame: JsonObject = { jsonrpc: '2.0', id, method }
    if (params !== undefined) frame.params = params
    return new Promise<JsonObject>((resolveRequest, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`Ternilo request ${JSON.stringify(method)} timed out after ${timeoutMs}ms`))
      }, timeoutMs)
      this.pending.set(id, { resolve: resolveRequest, reject, timer })
      process.stdin.write(`${JSON.stringify(frame)}\n`, (error) => {
        if (error === null || error === undefined) return
        const pending = this.pending.get(id)
        if (pending === undefined) return
        clearTimeout(pending.timer)
        this.pending.delete(id)
        pending.reject(this.closedError(`write failed: ${error.message}`))
      })
    })
  }

  async newSession(workspacePath: string, options: { sessionId?: string; agentId?: string; parentSessionId?: string } = {}): Promise<JsonObject> {
    const params: JsonObject = { workspace_path: resolve(workspacePath) }
    if (options.sessionId !== undefined) params.session_id = options.sessionId
    if (options.agentId !== undefined) params.agent_id = options.agentId
    if (options.parentSessionId !== undefined) params.parent_session_id = options.parentSessionId
    return this.request('session/new', params)
  }

  prompt(sessionId: string, prompt: string, options: { runId?: string; attachments?: JsonObject[] } = {}): Promise<JsonObject> {
    const params: JsonObject = {
      session_id: sessionId,
      prompt,
      attachments: options.attachments ?? [],
    }
    if (options.runId !== undefined) params.run_id = options.runId
    return this.request('session/prompt', params)
  }

  cancel(sessionId: string, runId: string): Promise<JsonObject> {
    return this.request('session/cancel', { session_id: sessionId, run_id: runId })
  }

  async run(prompt: string, options: RunOptions): Promise<RunResult> {
    await this.start()
    let sessionId = options.sessionId
    if (sessionId === undefined) {
      const created = await this.newSession(options.workspacePath)
      sessionId = ((created.identity as JsonObject).session_id as string)
    }
    const subscription = this.subscribe((frame) => {
      const params = frame.params as JsonObject | undefined
      return params?.session_id === sessionId
    })
    try {
      const receipt = await this.prompt(sessionId, prompt, { attachments: options.attachments })
      const runId = receipt.run_id as string
      const timeoutMs = options.timeoutMs ?? 300_000
      const deadline = Date.now() + timeoutMs
      const notifications: JsonObject[] = []
      const events: JsonObject[] = []
      while (true) {
        const remaining = deadline - Date.now()
        if (remaining <= 0) throw new Error(`Ternilo run ${JSON.stringify(runId)} timed out after ${timeoutMs}ms`)
        const frame = await subscription.queue.next(remaining)
        const params = frame.params as JsonObject
        if (params.run_id !== runId) continue
        notifications.push(frame)
        options.onNotification?.(frame)
        if (frame.method === 'session.event' && typeof params.event === 'object' && params.event !== null && !Array.isArray(params.event)) {
          events.push(params.event as JsonObject)
        }
        if (frame.method === 'session.status' && ['idle', 'cancelled', 'failed'].includes(params.status as string)) {
          const result = (params.result ?? {}) as JsonObject
          return {
            sessionId,
            runId,
            status: params.status as RunResult['status'],
            answer: (result.answer as string | undefined) ?? '',
            events,
            notifications,
          }
        }
      }
    } finally {
      this.subscribers.delete(subscription.id)
    }
  }

  async close(): Promise<void> {
    if (this.closed) return
    const process = this.process
    if (process === undefined) {
      this.closed = true
      return
    }
    try {
      if (process.exitCode === null) await this.request('shutdown', undefined, this.options.shutdownTimeoutMs)
    } catch {
      // Continue through the bounded process-reaping ladder.
    }
    process.stdin.end()
    await this.waitForExit(this.options.shutdownTimeoutMs).catch(() => undefined)
    if (process.exitCode === null) {
      process.kill('SIGTERM')
      await this.waitForExit(this.options.shutdownTimeoutMs).catch(() => undefined)
    }
    if (process.exitCode === null) {
      process.kill('SIGKILL')
      await this.waitForExit(this.options.shutdownTimeoutMs)
    }
    this.lines?.close()
    this.closed = true
    this.failAll(this.closedError('client closed'))
  }

  private subscribe(predicate: (frame: JsonObject) => boolean): { id: number; queue: AsyncQueue<JsonObject> } {
    const id = ++this.subscriberId
    const queue = new AsyncQueue<JsonObject>()
    this.subscribers.set(id, { predicate, queue })
    return { id, queue }
  }

  private acceptLine(line: string): void {
    let frame: JsonObject
    try {
      const parsed = JSON.parse(line) as unknown
      if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) throw new Error('frame is not an object')
      frame = parsed as JsonObject
    } catch {
      this.stderrTail.push(`non-JSON stdout: ${line}`)
      return
    }
    if (typeof frame.id === 'number' && frame.method === undefined) {
      const pending = this.pending.get(frame.id)
      if (pending === undefined) return
      clearTimeout(pending.timer)
      this.pending.delete(frame.id)
      if (typeof frame.error === 'object' && frame.error !== null && !Array.isArray(frame.error)) {
        const remote = frame.error as JsonObject
        pending.reject(new RemoteError(remote.code as number, remote.message as string, remote.data))
      } else if (typeof frame.result === 'object' && frame.result !== null && !Array.isArray(frame.result)) {
        pending.resolve(frame.result as JsonObject)
      } else {
        pending.reject(new ProtocolError('response has no object result'))
      }
      return
    }
    if (typeof frame.method === 'string' && frame.id === undefined) {
      for (const subscriber of this.subscribers.values()) {
        if (subscriber.predicate(frame)) subscriber.queue.push(frame)
      }
    }
  }

  private failAll(error: Error): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer)
      pending.reject(error)
    }
    this.pending.clear()
  }

  private waitForExit(timeoutMs: number): Promise<void> {
    const process = this.process
    if (process === undefined || process.exitCode !== null) return Promise.resolve()
    return new Promise<void>((resolveExit, reject) => {
      const timer = setTimeout(() => reject(new Error('runtime exit timed out')), timeoutMs)
      process.once('exit', () => {
        clearTimeout(timer)
        resolveExit()
      })
    })
  }

  private closedError(message: string): TransportClosedError {
    const suffix = this.stderrTail.length === 0 ? '' : `: ${this.stderrTail.join('\n')}`
    return new TransportClosedError(`${message}${suffix}`)
  }
}
