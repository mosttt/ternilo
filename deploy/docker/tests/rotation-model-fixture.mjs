import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'

const expectedKeyFile = process.argv[2]
if (!expectedKeyFile) throw new Error('usage: rotation-model-fixture.mjs EXPECTED_KEY_FILE')

let accepted = 0
let rejected = 0

const server = createServer(async (request, response) => {
  if (request.method === 'GET' && request.url === '/stats') {
    response.writeHead(200, { 'content-type': 'application/json' })
    response.end(JSON.stringify({ accepted, rejected }))
    return
  }
  if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
    response.writeHead(404)
    response.end()
    return
  }

  const expected = (await readFile(expectedKeyFile, 'utf8')).trim()
  if (request.headers.authorization !== `Bearer ${expected}`) {
    rejected += 1
    response.writeHead(401, { 'content-type': 'application/json' })
    response.end(JSON.stringify({ error: { message: 'invalid model credential' } }))
    return
  }

  for await (const _chunk of request) {
    // Drain the request before sending the streaming response.
  }
  accepted += 1
  response.writeHead(200, {
    'content-type': 'text/event-stream',
    'x-request-id': 'cloud-rotation-model-canary',
  })
  response.end([
    'data: {"choices":[{"delta":{"content":"credential rotation canary ready"}}]}\n\n',
    'data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
    'data: [DONE]\n\n',
  ].join(''))
})

server.listen(0, process.env.TERNILO_ACCEPTANCE_MODEL_BIND || '127.0.0.1', () => {
  const address = server.address()
  if (!address || typeof address === 'string') throw new Error('fixture did not bind a TCP port')
  process.stdout.write(`${address.port}\n`)
})

const stop = () => server.close(() => process.exit(0))
process.on('SIGINT', stop)
process.on('SIGTERM', stop)
