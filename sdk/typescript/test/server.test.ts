import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { once } from 'node:events'
import test from 'node:test'
import { WebSocketServer } from 'ws'
import { ServerClient, ServerError } from '../src/client.ts'

test('Live reconnect resumes accepted sequences, deduplicates replay, exposes reset and stops on revocation', async () => {
  const server = createServer()
  const sockets = new WebSocketServer({ server })
  server.listen(0, '127.0.0.1'); await once(server, 'listening')
  const address = server.address(); assert.ok(address && typeof address !== 'string')
  const receipts: unknown[] = []
  let connections = 0
  sockets.on('connection', socket => {
    const connection = ++connections
    socket.on('message', data => {
      const frame = JSON.parse(data.toString())
      receipts.push(frame)
      if (frame.type === 'hello') socket.send(JSON.stringify({ type: 'ready', protocol_version: 1 }))
      if (frame.type !== 'subscribe') return
      const batch = (seqs: number[], reset = false) => ({ type: 'event_batch', subscription_id: 1, session_id: 'shared', reset,
        complete: true, next_seq: Math.max(...seqs) + 1, events: seqs.map(seq => ({ seq, run_id: 'run', type: 'turn_started' })) })
      if (connection === 1) { socket.send(JSON.stringify(batch([1]))); socket.close() }
      else {
        socket.send(JSON.stringify(batch([1, 2])))
        socket.send(JSON.stringify(batch([0], true)))
        socket.send(JSON.stringify({ type: 'error', subscription_id: 1, code: 'policy_denied', message: 'share revoked' }))
      }
    })
  })
  const client = new ServerClient({ baseUrl: `http://127.0.0.1:${address.port}`, accessToken: 'private-token', tenantId: 'team' })
  try {
    const stream = client.watch('shared', { afterSeq: 0, timeoutMs: 5000 })
    assert.deepEqual((await stream.next()).value?.events.map(event => event.seq), [1])
    assert.deepEqual((await stream.next()).value?.events.map(event => event.seq), [2])
    const reset = (await stream.next()).value
    assert.equal(reset?.reset, true)
    assert.deepEqual(reset?.events.map(event => event.seq), [0])
    await assert.rejects(stream.next(), error => error instanceof ServerError && error.code === 'policy_denied')
    assert.equal(connections, 2)
    assert.deepEqual(receipts.filter((frame: any) => frame.type === 'hello'), Array(2).fill({ type: 'hello', protocol_version: 1, bearer_token: 'private-token', tenant_id: 'team' }))
    assert.deepEqual(receipts.filter((frame: any) => frame.type === 'subscribe').map((frame: any) => frame.after_seq), [0, 1])
  } finally {
    client.close(); for (const socket of sockets.clients) socket.terminate()
    sockets.close(); server.close(); await once(server, 'close')
  }
})

test('HTTP never replays a submitted mutation or forwards credentials through redirects', async () => {
  let submissions = 0, leaked = false
  const destination = createServer((request, response) => { leaked = true; response.end('{}') })
  destination.listen(0, '127.0.0.1'); await once(destination, 'listening')
  const target = destination.address(); assert.ok(target && typeof target !== 'string')
  const server = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, 'Bearer private-token')
    assert.equal(request.headers['x-ternilo-tenant'], 'team')
    if (request.url === '/api/v1/mutate') {
      for await (const _ of request) {}
      submissions++; response.destroy()
    } else response.writeHead(302, { location: `http://127.0.0.1:${target.port}/secret` }).end()
  })
  server.listen(0, '127.0.0.1'); await once(server, 'listening')
  const address = server.address(); assert.ok(address && typeof address !== 'string')
  const client = new ServerClient({ baseUrl: `http://127.0.0.1:${address.port}`, accessToken: 'private-token', tenantId: 'team' })
  try {
    await assert.rejects(client.request('/mutate', { method: 'POST', body: { input: 'one task' } }))
    assert.equal(submissions, 1)
    await assert.rejects(client.request('/redirect'))
    assert.equal(leaked, false)
  } finally {
    client.close(); server.close(); destination.close()
    await Promise.all([once(server, 'close'), once(destination, 'close')])
  }
})

test('watch timeout interrupts a silent handshake and releases the socket', async () => {
  const server = createServer(), sockets = new WebSocketServer({ server })
  let flap = false
  sockets.on('connection', socket => {
    socket.on('message', () => {
      if (flap) { socket.send(JSON.stringify({ type: 'ready', protocol_version: 1 })); socket.close() }
    })
  })
  server.listen(0, '127.0.0.1'); await once(server, 'listening')
  const address = server.address(); assert.ok(address && typeof address !== 'string')
  const client = new ServerClient({ baseUrl: `http://127.0.0.1:${address.port}`, accessToken: 'token', tenantId: 'team' })
  try {
    await assert.rejects(client.watch('session', { timeoutMs: 100 }).next(), { name: 'TimeoutError' })
    flap = true
    await assert.rejects(client.watch('session', { timeoutMs: 2000, reconnectTimeoutMs: 200 }).next(),
      error => error instanceof ServerError && error.code === 'unavailable')
  } finally {
    client.close(); for (const socket of sockets.clients) socket.terminate()
    sockets.close(); server.close(); await once(server, 'close')
  }
})
