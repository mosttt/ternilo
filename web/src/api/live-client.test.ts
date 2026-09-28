import { afterEach, describe, expect, it, vi } from 'vitest'

import {
  ALL_SESSION_LIVE_READS,
  LIVE_PROTOCOL_VERSION,
  LiveClient,
  type LiveClientFrame,
  type LiveNetworkEvents,
  type LiveServerFrame,
  type LiveSocket,
} from './live-client'

class FakeNetworkEvents implements LiveNetworkEvents {
  private readonly listeners = new Map<'online' | 'offline', Set<EventListener>>()

  addEventListener(type: 'online' | 'offline', listener: EventListener) {
    const listeners = this.listeners.get(type) ?? new Set<EventListener>()
    listeners.add(listener)
    this.listeners.set(type, listeners)
  }

  removeEventListener(type: 'online' | 'offline', listener: EventListener) {
    this.listeners.get(type)?.delete(listener)
  }

  dispatch(type: 'online' | 'offline') {
    this.listeners.get(type)?.forEach(listener => listener(new Event(type)))
  }
}

class FakeSocket implements LiveSocket {
  readyState = 0
  onopen: ((event: Event) => unknown) | null = null
  onmessage: ((event: MessageEvent) => unknown) | null = null
  onclose: ((event: CloseEvent) => unknown) | null = null
  onerror: ((event: Event) => unknown) | null = null
  readonly sent: LiveClientFrame[] = []
  closedWith: { code: number; reason: string } | null = null

  send(data: string) {
    this.sent.push(JSON.parse(data) as LiveClientFrame)
  }

  close(code = 1000, reason = '') {
    if (this.readyState === 3) return
    this.closedWith = { code, reason }
    this.readyState = 3
    this.onclose?.(new CloseEvent('close', { code, reason }))
  }

  open() {
    this.readyState = 1
    this.onopen?.(new Event('open'))
  }

  receive(frame: LiveServerFrame) {
    this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(frame) }))
  }

  serverClose() {
    this.close(1006, 'connection lost')
  }
}

function createHarness(reconnectInitialDelayMs = 100, reconnectStableAfterMs = 1_000) {
  const sockets: FakeSocket[] = []
  const network = new FakeNetworkEvents()
  const credentials = {
    bearerToken: 'secret',
    tenantId: 'tenant-a',
    revision: 0,
  }
  const client = new LiveClient({
    url: 'ws://example.test/api/v1/live',
    credentials: () => credentials,
    createSocket: () => {
      const socket = new FakeSocket()
      sockets.push(socket)
      return socket
    },
    reconnectInitialDelayMs,
    reconnectMaxDelayMs: 400,
    reconnectStableAfterMs,
    networkEvents: network,
  })
  return { client, sockets, credentials, network }
}

function ready(socket: FakeSocket) {
  socket.receive({ type: 'ready', protocol_version: LIVE_PROTOCOL_VERSION })
}

afterEach(() => {
  vi.useRealTimers()
})

describe('LiveClient', () => {
  it('handshakes once and switches logical subscriptions on one socket', () => {
    const { client, sockets } = createHarness()
    const firstSubscription = client.setSession('session-a', { afterSeq: 4 })

    client.start()
    expect(sockets).toHaveLength(1)
    sockets[0].open()
    expect(sockets[0].sent).toEqual([{
      type: 'hello',
      protocol_version: LIVE_PROTOCOL_VERSION,
      bearer_token: 'secret',
      tenant_id: 'tenant-a',
    }])

    ready(sockets[0])
    expect(sockets[0].sent[1]).toEqual({
      type: 'subscribe',
      subscription_id: firstSubscription,
      session_id: 'session-a',
      after_seq: 4,
      metadata: ALL_SESSION_LIVE_READS,
    })

    const secondSubscription = client.setSession('session-b')
    expect(secondSubscription).not.toBe(firstSubscription)
    expect(sockets[0].sent.slice(2)).toEqual([
      { type: 'unsubscribe', subscription_id: firstSubscription },
      {
        type: 'subscribe',
        subscription_id: secondSubscription,
        session_id: 'session-b',
        metadata: ALL_SESSION_LIVE_READS,
      },
    ])
  })

  it('delivers event chunks and rejects stale subscription frames', () => {
    const { client, sockets } = createHarness()
    const received: LiveServerFrame[] = []
    const firstSubscription = client.setSession('session-a') as number
    client.onFrame(frame => received.push(frame))
    client.start()
    sockets[0].open()
    ready(sockets[0])
    received.length = 0

    sockets[0].receive({
      type: 'event_batch',
      subscription_id: firstSubscription,
      session_id: 'session-a',
      reset: true,
      complete: false,
      events: [{ seq: 0, occurred_at_ms: 1, run_id: 'run-a', type: 'turn_started' }],
      next_seq: 1,
    })
    sockets[0].receive({
      type: 'event_batch',
      subscription_id: firstSubscription,
      session_id: 'session-a',
      reset: false,
      complete: true,
      events: [{ seq: 1, occurred_at_ms: 2, run_id: 'run-a', type: 'turn_finished' }],
      next_seq: 2,
    })
    sockets[0].receive({
      type: 'session_metadata',
      subscription_id: firstSubscription,
      session_id: 'session-a',
      metadata: {
        read: {
          ...ALL_SESSION_LIVE_READS,
          inbox: false,
          projection: false,
          questions: false,
          profile: false,
          agent_team: false,
        },
        stats: {
          events: 2,
          turns: 1,
          completed_turns: 1,
          failed_turns: 0,
          cancelled_turns: 0,
          steps: 0,
          tool_calls: 0,
          user_messages: 1,
          assistant_messages: 1,
          estimated_logged_tokens: 0,
          exact_input_tokens: 0,
          exact_output_tokens: 0,
          exact_reasoning_tokens: 0,
          cached_input_tokens: 0,
          model_attempts: 0,
          measured_model_responses: 0,
          model_duration_ms: 0,
          tool_duration_ms: 0,
          first_token_duration_ms: 0,
          measured_first_tokens: 0,
          generation_duration_ms: 0,
          generation_output_tokens: 0,
        },
      },
    })
    expect(received.map(frame => frame.type)).toEqual([
      'event_batch',
      'event_batch',
      'session_metadata',
    ])

    const secondSubscription = client.setSession('session-b') as number
    sockets[0].receive({
      type: 'event_batch',
      subscription_id: firstSubscription,
      session_id: 'session-a',
      reset: false,
      complete: true,
      events: [],
      next_seq: 3,
    })
    sockets[0].receive({
      type: 'error',
      subscription_id: firstSubscription,
      code: 'unavailable',
      message: 'stale',
    })
    sockets[0].receive({
      type: 'workbench',
      revision: 8,
      state: { workspaces: [], sessions: [] },
      activity: [],
    })
    sockets[0].receive({
      type: 'activity',
      activity: { session_id: 'background', running: true, updated_at_ms: 9 },
    })
    sockets[0].receive({
      type: 'error',
      subscription_id: secondSubscription,
      code: 'unavailable',
      message: 'current',
    })
    sockets[0].receive({
      type: 'error',
      code: 'unavailable',
      message: 'connection-wide',
    })

    expect(received.slice(3)).toEqual([
      { type: 'workbench', revision: 8, state: { workspaces: [], sessions: [] }, activity: [] },
      {
        type: 'activity',
        activity: { session_id: 'background', running: true, updated_at_ms: 9 },
      },
      {
        type: 'error',
        subscription_id: secondSubscription,
        code: 'unavailable',
        message: 'current',
      },
      { type: 'error', code: 'unavailable', message: 'connection-wide' },
    ])
  })

  it('delivers Agent Team through the shared metadata slice', () => {
    const { client, sockets } = createHarness()
    const subscriptionId = client.setSession('session-a') as number
    const received: LiveServerFrame[] = []
    client.onFrame(frame => received.push(frame))
    client.start()
    sockets[0].open()
    ready(sockets[0])
    received.length = 0

    sockets[0].receive({
      type: 'session_metadata',
      subscription_id: subscriptionId,
      session_id: 'session-a',
      metadata: {
        read: { ...ALL_SESSION_LIVE_READS, agent_team: true },
        agent_team: {
          team_id: 'team-a',
          current_member_id: 'lead',
          members: [{
            id: 'lead',
            label: 'Lead',
            role: 'lead',
          }],
          tasks: [],
          messages: [],
        },
      },
    })

    expect(received).toHaveLength(1)
    expect(received[0]).toMatchObject({
      type: 'session_metadata',
      metadata: {
        read: { agent_team: true },
        agent_team: { team_id: 'team-a' },
      },
    })
  })

  it('applies changed options when the same Session is selected again', () => {
    const { client, sockets } = createHarness()
    const firstSubscription = client.setSession('session-a', { afterSeq: 9 }) as number
    client.start()
    sockets[0].open()
    ready(sockets[0])

    const secondSubscription = client.setSession('session-a', { afterSeq: null }) as number
    expect(secondSubscription).not.toBe(firstSubscription)
    expect(sockets[0].sent.slice(-2)).toEqual([
      { type: 'unsubscribe', subscription_id: firstSubscription },
      {
        type: 'subscribe',
        subscription_id: secondSubscription,
        session_id: 'session-a',
        metadata: ALL_SESSION_LIVE_READS,
      },
    ])
  })

  it('replaces the socket immediately when credentials change and rejects old frames', () => {
    const { client, sockets, credentials } = createHarness()
    const received: LiveServerFrame[] = []
    client.onFrame(frame => received.push(frame))
    client.start()
    sockets[0].open()
    ready(sockets[0])
    received.length = 0

    credentials.tenantId = 'tenant-b'
    credentials.revision += 1
    client.credentialsChanged()

    expect(sockets[0].readyState).toBe(3)
    expect(sockets).toHaveLength(2)
    sockets[0].receive({
      type: 'workbench',
      revision: 99,
      state: { workspaces: [], sessions: [] },
      activity: [],
    })
    expect(received).toEqual([])

    sockets[1].open()
    expect(sockets[1].sent[0]).toEqual({
      type: 'hello',
      protocol_version: LIVE_PROTOCOL_VERSION,
      bearer_token: 'secret',
      tenant_id: 'tenant-b',
    })
    ready(sockets[1])
    received.length = 0
    sockets[1].receive({
      type: 'workbench',
      revision: 1,
      state: { workspaces: [], sessions: [] },
      activity: [],
    })
    expect(received.map(frame => frame.type)).toEqual(['workbench'])
  })

  it('reconnects exponentially and converts next_seq to the last accepted after_seq', () => {
    vi.useFakeTimers()
    const { client, sockets } = createHarness(100)
    const subscriptionId = client.setSession('session-a') as number
    client.start()
    sockets[0].open()
    ready(sockets[0])
    sockets[0].receive({
      type: 'event_batch',
      subscription_id: subscriptionId,
      session_id: 'session-a',
      reset: false,
      complete: true,
      events: [],
      next_seq: 11,
    })

    sockets[0].serverClose()
    expect(client.status).toBe('reconnecting')
    vi.advanceTimersByTime(99)
    expect(sockets).toHaveLength(1)
    vi.advanceTimersByTime(1)
    expect(sockets).toHaveLength(2)

    sockets[1].open()
    expect(sockets[1].sent[0].type).toBe('hello')
    sockets[1].serverClose()
    vi.advanceTimersByTime(199)
    expect(sockets).toHaveLength(2)
    vi.advanceTimersByTime(1)
    expect(sockets).toHaveLength(3)

    sockets[2].open()
    ready(sockets[2])
    expect(sockets[2].sent[1]).toEqual({
      type: 'subscribe',
      subscription_id: subscriptionId,
      session_id: 'session-a',
      after_seq: 10,
      metadata: ALL_SESSION_LIVE_READS,
    })

    sockets[2].serverClose()
    vi.advanceTimersByTime(399)
    expect(sockets).toHaveLength(3)
    vi.advanceTimersByTime(1)
    expect(sockets).toHaveLength(4)

    sockets[3].open()
    ready(sockets[3])
    vi.advanceTimersByTime(1_000)
    sockets[3].serverClose()
    vi.advanceTimersByTime(99)
    expect(sockets).toHaveLength(4)
    vi.advanceTimersByTime(1)
    expect(sockets).toHaveLength(5)
    client.stop()
  })

  it('leaves ready immediately when the browser goes offline and reconnects on online', () => {
    const { client, sockets, network } = createHarness()
    client.start()
    sockets[0].open()
    ready(sockets[0])

    network.dispatch('offline')
    expect(client.status).toBe('reconnecting')
    expect(sockets[0].readyState).toBe(3)
    expect(sockets[0].closedWith).toEqual({ code: 1000, reason: 'browser offline' })

    network.dispatch('online')
    expect(sockets).toHaveLength(2)
    sockets[1].open()
    ready(sockets[1])
    expect(client.status).toBe('ready')
    client.stop()
  })

  it('uses an allowed application close code for an unsupported live protocol', () => {
    const { client, sockets } = createHarness()
    client.start()
    sockets[0].open()
    sockets[0].receive({
      type: 'ready',
      protocol_version: LIVE_PROTOCOL_VERSION + 1,
    })

    expect(client.status).toBe('stopped')
    expect(sockets[0].closedWith).toEqual({
      code: 4002,
      reason: 'unsupported live protocol version',
    })
  })
})
