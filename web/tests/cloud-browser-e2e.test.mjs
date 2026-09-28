import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { selectProject } from './platform-e2e-fixture.mjs'
import { createHash, createPrivateKey, createPublicKey, randomBytes, sign } from 'node:crypto'
import { execFile } from 'node:child_process'
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const execute = promisify(execFile)
const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const controlBinary = process.env.TERNILO_CLOUD_E2E_SERVER_BINARY || path.join(repository, 'target', 'debug', 'ternilo-server')
const workerBinary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY || path.join(repository, 'target', 'debug', 'ternilo-worker')
const signedFixtureBinary = path.join(repository, 'target', 'debug', 'examples', 'signed_web_fixture')
const containerMode = process.env.TERNILO_CLOUD_E2E_CONTAINER === '1'
const serverImage = process.env.TERNILO_CLOUD_E2E_SERVER_IMAGE || 'ternilo-server:local'
const workerImage = process.env.TERNILO_CLOUD_E2E_WORKER_IMAGE || 'ternilo-worker:local'
const cleanEnvironment = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('TERNILO_')))
const CLOUD_QUESTION_TRIGGER = 'exercise cloud structured question'
const CLOUD_QUESTION_ONE = '选择云端执行策略'
const CLOUD_QUESTION_TWO = '选择需要覆盖的云端表面'
const CLOUD_SKILL_TASK = 'validate typed cloud skill'
const CLOUD_PLAN_TASK = 'exercise cloud plan review'
const CLOUD_STEER_TASK = 'exercise cloud strict steer'
const CLOUD_STEER_MESSAGE = 'cloud steering injection'
const CLOUD_STOP_TASK = 'exercise cloud fast stop'
const CLOUD_AFTER_STOP_TASK = 'verify cloud after stop'
const CLOUD_TEAM_TASK = 'exercise cloud canonical agent team'
const CLOUD_CHILD_INITIAL_TASK = 'inspect cloud team workspace'
const CLOUD_CHILD_FOLLOWUP_TASK = 'verify cloud child addressed stop'
const CLOUD_TEAM_TASK_SUBJECT = 'Review cloud browser Team'
const CLOUD_TEAM_MESSAGE = 'Mailbox from Cloud root'
const CLOUD_SEARCH_TASK = 'exercise cloud canonical search'
const CLOUD_REASONING_SENTINEL = 'CLOUD_REASONING_SEARCH_SENTINEL_7F31'
const CLOUD_ASSISTANT_SENTINEL = 'CLOUD_ASSISTANT_SEARCH_SENTINEL_9C42'
const CLOUD_TOOL_SENTINEL = 'CLOUD_TOOL_SEARCH_SENTINEL_5A63'
const CLOUD_EXTENSION_PACKAGE_ID = 'dev.ternilo.browser-fixture'
const CLOUD_EXTENSION_VERSION = '1.0.0'
const CLOUD_EXTENSION_PUBLISHER_ID = 'ternilo-browser-fixture'
const CLOUD_EXTENSION_SOURCE = 'https://plugins.ternilo.dev/browser-fixture'
const CLOUD_EXTENSION_PROMPT_ID = 'fixture-guidance'
const CLOUD_EXTENSION_PROMPT_CONTENT = 'Use the signed WASM fixture only for deterministic acceptance checks.'
const CLOUD_EXTENSION_SKILL_NAME = 'signed-extension-fixture'
const CLOUD_EXTENSION_SKILL_CONTENT = 'Use the mounted signed Extension fixture only for deterministic browser acceptance checks. Verify its signature before invoking any contributed tool.'
const CLOUD_EXTENSION_TOOL_TASK = 'exercise cloud signed extension tool'
const CLOUD_EXTENSION_SKILL_TASK = 'exercise cloud signed extension skill'
const CLOUD_PARENT_STATS_AFTER_TEAM = { input: 149, output: 38, cached: 13, cachePercent: 9 }
const CLOUD_UNREGISTER_TASK = 'verify cloud session after workspace unregister'
const CLOUD_OFFLINE_FILE = 'cloud-offline-recovery.txt'
const CLOUD_OFFLINE_SENTINEL = 'CLOUD_OFFLINE_RECOVERY_SENTINEL_C2D8'
const LIFECYCLE_REQUEST_HEADER = 'x-ternilo-e2e-lifecycle-request'
const CLOUD_E2E_SECRETS = [
  'cloud-e2e-provider-secret',
  'cloud-browser-byok-secret',
  'cloud-browser-setting-secret',
]
const CLOUD_PLAN = `# Cloud rollout plan

## Prepare

- Inspect the canonical workspace and current profile.
- Confirm the Worker can read Skills and model settings.

## Execute

1. Run the focused Cloud workflow.
2. Verify the durable Session events and Agent Team state.
3. Review desktop and mobile layouts.

## Verify

- Confirm the approved plan switches the Session back to execute mode.
- Keep the final browser evidence attached to the process record.
`

function latestUserContent(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : []
  const latest = messages.at(-1)
  return latest?.role === 'user' ? JSON.stringify(latest.content ?? '') : ''
}

async function listen(server) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return server.address().port
}

async function freePort() {
  const server = createServer()
  const port = await listen(server)
  await closeServer(server)
  return port
}

async function closeServer(server) {
  if (!server.listening) return
  await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
}

function withTimeout(promise, label, timeoutMs = 30_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs)
    promise.then(
      value => { clearTimeout(timer); resolve(value) },
      error => { clearTimeout(timer); reject(error) },
    )
  })
}

function diagnosticResponseBody(body) {
  let redacted = body
    .replace(/Bearer\s+[A-Za-z0-9._~-]+/g, 'Bearer [redacted]')
    .replace(/("(?:api_key|secret|token)"\s*:\s*")[^"]*/gi, '$1[redacted]')
  for (const secret of CLOUD_E2E_SECRETS) redacted = redacted.replaceAll(secret, '[redacted]')
  return redacted.slice(0, 2_000)
}

function runChild(child, label) {
  return new Promise((resolve, reject) => {
    let diagnostics = ''
    child.stderr?.setEncoding('utf8')
    child.stderr?.on('data', chunk => { diagnostics += chunk })
    child.once('error', reject)
    child.once('exit', code => code === 0
      ? resolve()
      : reject(new Error(`${label} exited ${code}: ${diagnostics}`)))
  })
}

async function generateSignedFixture(directory) {
  await runChild(
    spawn(signedFixtureBinary, [directory], { cwd: repository, stdio: ['ignore', 'ignore', 'pipe'] }),
    'signed Extension v1 fixture generator',
  )
}

async function directoryUsage(root) {
  let bytes = 0
  let entries = 0
  const pending = [root]
  while (pending.length > 0) {
    const directory = pending.pop()
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      entries += 1
      const target = path.join(directory, entry.name)
      const metadata = await lstat(target)
      if (metadata.isDirectory()) pending.push(target)
      else bytes += metadata.size
    }
  }
  return { bytes, entries }
}

function base64Url(value) {
  return Buffer.from(value).toString('base64url')
}

async function startOidcServer() {
  const privatePem = await readFile(path.join(repository, 'crates/ternilo-control/tests/fixtures/oidc-private.pem'), 'utf8')
  const privateKey = createPrivateKey(privatePem)
  const jwk = createPublicKey(privateKey).export({ format: 'jwk' })
  const codes = new Map()
  let issuer
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, issuer)
    if (url.pathname === '/.well-known/openid-configuration') {
      return json(response, 200, {
        issuer,
        jwks_uri: `${issuer}/jwks`,
        authorization_endpoint: `${issuer}/authorize`,
        token_endpoint: `${issuer}/token`,
      })
    }
    if (url.pathname === '/jwks') {
      return json(response, 200, { keys: [{ ...jwk, kid: 'cloud-e2e', alg: 'RS256', use: 'sig' }] })
    }
    if (url.pathname === '/authorize') {
      assert.equal(url.searchParams.get('response_type'), 'code')
      assert.equal(url.searchParams.get('code_challenge_method'), 'S256')
      const code = base64Url(randomBytes(24))
      codes.set(code, url.searchParams.get('code_challenge'))
      const redirect = new URL(url.searchParams.get('redirect_uri'))
      redirect.searchParams.set('code', code)
      redirect.searchParams.set('state', url.searchParams.get('state'))
      response.writeHead(302, { location: redirect.toString() })
      return response.end()
    }
    if (url.pathname === '/token' && request.method === 'POST') {
      const form = new URLSearchParams(await body(request))
      if (form.get('grant_type') === 'authorization_code') {
        const challenge = codes.get(form.get('code'))
        const actual = base64Url(createHash('sha256').update(form.get('code_verifier') || '').digest())
        if (!challenge || challenge !== actual) return json(response, 400, { error: 'invalid_grant' })
        codes.delete(form.get('code'))
      } else if (form.get('grant_type') !== 'refresh_token' || form.get('refresh_token') !== 'cloud-refresh') {
        return json(response, 400, { error: 'invalid_grant' })
      }
      return json(response, 200, {
        access_token: jwt(privateKey, issuer),
        token_type: 'Bearer',
        expires_in: 3600,
        refresh_token: 'cloud-refresh',
        scope: 'openid profile email',
      })
    }
    response.writeHead(404)
    response.end()
  })
  const port = await listen(server)
  issuer = `http://127.0.0.1:${port}`
  return { issuer, accessToken: () => jwt(privateKey, issuer), close: () => closeServer(server) }
}

function jwt(privateKey, issuer) {
  const header = base64Url(JSON.stringify({ alg: 'RS256', typ: 'JWT', kid: 'cloud-e2e' }))
  const now = Math.floor(Date.now() / 1000)
  const payload = base64Url(JSON.stringify({
    iss: issuer,
    sub: 'cloud-browser-user',
    aud: 'ternilo-cloud-e2e',
    exp: now + 3600,
    nbf: now - 1,
    email: 'cloud@example.com',
    name: 'Cloud Browser',
  }))
  const signature = base64Url(sign('RSA-SHA256', Buffer.from(`${header}.${payload}`), privateKey))
  return `${header}.${payload}.${signature}`
}

async function startModelServer() {
  let resolveRequest
  let resolveQuestionResume
  let releaseSteerResponse
  let resolveStopClosed
  let resolveChildStopClosed
  const requests = []
  const waiters = new Set()
  const pendingResponses = new Set()
  const requestSeen = new Promise(resolve => { resolveRequest = resolve })
  const questionResumed = new Promise(resolve => { resolveQuestionResume = resolve })
  const steerRelease = new Promise(resolve => { releaseSteerResponse = resolve })
  const stopClosed = new Promise(resolve => { resolveStopClosed = resolve })
  const childStopClosed = new Promise(resolve => { resolveChildStopClosed = resolve })
  const publishRequest = observed => {
    requests.push(observed)
    resolveRequest(observed)
    for (const waiter of [...waiters]) {
      if (!waiter.predicate(observed)) continue
      clearTimeout(waiter.timeout)
      waiters.delete(waiter)
      waiter.resolve(observed)
    }
  }
  const waitForRequest = (predicate, description = 'matching model request', timeoutMs = 30_000, after = 0) => {
    const existing = requests.slice(after).find(predicate)
    if (existing) return Promise.resolve(existing)
    return new Promise((resolve, reject) => {
      const waiter = { predicate, resolve, timeout: undefined }
      waiter.timeout = setTimeout(() => {
        waiters.delete(waiter)
        reject(new Error(`timed out waiting for ${description}; modelRequests=${requests.length}; recent=${JSON.stringify(requests.slice(-5).map(entry => ({ model: entry.body.model, latestRole: entry.body.messages?.at(-1)?.role, latestUser: latestUserContent(entry.body).slice(-240) })))}`))
      }, timeoutMs)
      waiters.add(waiter)
    })
  }
  const server = createServer(async (request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
      response.writeHead(404)
      return response.end()
    }
    const parsed = JSON.parse(await body(request))
    const observed = { body: parsed, authorization: request.headers.authorization }
    publishRequest(observed)
    const messages = Array.isArray(parsed.messages) ? parsed.messages : []
    const latestMessage = messages.at(-1)
    const latestUser = latestUserContent(parsed)
    const isQuestionStart = latestUser.includes(CLOUD_QUESTION_TRIGGER)
      && parsed.tools?.some(tool => tool?.function?.name === 'ask_user')
    const isQuestionResume = latestMessage?.role === 'tool'
      && latestMessage?.tool_call_id === 'cloud-question-call'
    const isPlanStart = latestUser.includes(CLOUD_PLAN_TASK)
      && parsed.tools?.some(tool => tool?.function?.name === 'exit_plan_mode')
    const isPlanResume = latestMessage?.role === 'tool'
      && latestMessage?.tool_call_id === 'cloud-plan-call'
    const isSteerStart = latestUser.includes(CLOUD_STEER_TASK)
    const isSteerResume = latestUser.includes(CLOUD_STEER_MESSAGE)
    const isStopStart = latestUser.includes(CLOUD_STOP_TASK)
    const isTeamStart = latestUser.includes(CLOUD_TEAM_TASK)
      && parsed.tools?.some(tool => tool?.function?.name === 'spawn_agent')
    const isTeamResume = latestMessage?.role === 'tool'
      && latestMessage?.tool_call_id === 'cloud-team-spawn-call'
    const isChildInitial = latestUser.includes(CLOUD_CHILD_INITIAL_TASK)
    const isChildFollowup = latestUser.includes(CLOUD_CHILD_FOLLOWUP_TASK)
    const isSearchTurn = latestUser.includes(CLOUD_SEARCH_TASK)
    const isExtensionToolStart = latestUser.includes(CLOUD_EXTENSION_TOOL_TASK)
      && parsed.tools?.some(tool => tool?.function?.name === 'signed_fixture')
    const isExtensionToolResume = latestMessage?.role === 'tool'
      && latestMessage?.tool_call_id === 'cloud-extension-v1-tool-call'
    if (isQuestionStart) {
      const argumentsValue = JSON.stringify({ questions: [
        {
          id: 'strategy',
          header: '云端决策',
          question: CLOUD_QUESTION_ONE,
          detail: '这条问题由隔离 Worker 发出，并通过 **Control Cloud** 持久化。',
          options: [
            { label: '先验证 (Recommended)', description: '先完成真实链路验证。' },
            { label: '直接执行', description: '跳过额外核对。' },
          ],
        },
        {
          id: 'surfaces',
          question: CLOUD_QUESTION_TWO,
          options: [{ label: 'Web' }, { label: 'Worker' }, { label: 'PostgreSQL' }],
          multi_select: true,
        },
      ] })
      const stream = [
        `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: 'cloud-question-call', function: { name: 'ask_user', arguments: argumentsValue } }] } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":0}}}\n\n',
        'data: [DONE]\n\n',
      ]
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-question',
      })
      response.end(stream.join(''))
      return
    }
    if (isQuestionResume) {
      resolveQuestionResume(observed)
      const stream = [
        'data: {"choices":[{"delta":{"content":"cloud question resumed"}}]}\n\n',
        'data: {"choices":[],"usage":{"prompt_tokens":30,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":5}}}\n\n',
        'data: [DONE]\n\n',
      ]
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-question-resumed',
      })
      response.end(stream.join(''))
      return
    }
    if (isPlanStart) {
      const argumentsValue = JSON.stringify({ plan: CLOUD_PLAN })
      const stream = [
        `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: 'cloud-plan-call', function: { name: 'exit_plan_mode', arguments: argumentsValue } }] } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":18,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":0}}}\n\n',
        'data: [DONE]\n\n',
      ]
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-plan',
      })
      response.end(stream.join(''))
      return
    }
    if (isPlanResume) {
      const stream = [
        'data: {"choices":[{"delta":{"content":"cloud plan approved"}}]}\n\n',
        'data: {"choices":[],"usage":{"prompt_tokens":22,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":3}}}\n\n',
        'data: [DONE]\n\n',
      ]
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-plan-resumed',
      })
      response.end(stream.join(''))
      return
    }
    if (isSteerStart) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-steer-start',
      })
      response.flushHeaders()
      response.write('data: {"choices":[{"delta":{"content":"cloud steer boundary open"}}]}\n\n')
      pendingResponses.add(response)
      response.once('close', () => pendingResponses.delete(response))
      await steerRelease
      if (!response.destroyed) {
        response.write('data: {"choices":[],"usage":{"prompt_tokens":14,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}\n\n')
        response.end('data: [DONE]\n\n')
      }
      return
    }
    if (isSteerResume) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-steer-resumed',
      })
      response.end([
        'data: {"choices":[{"delta":{"content":"cloud steering applied"}}]}\n\n',
        'data: {"choices":[],"usage":{"prompt_tokens":19,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    if (isStopStart) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-stop',
      })
      response.flushHeaders()
      response.write('data: {"choices":[{"delta":{"content":"cloud stop stream remains active"}}]}\n\n')
      pendingResponses.add(response)
      response.once('close', () => {
        pendingResponses.delete(response)
        resolveStopClosed(Date.now())
      })
      return
    }
    if (isTeamStart) {
      const argumentsValue = JSON.stringify({
        task: CLOUD_CHILD_INITIAL_TASK,
        label: 'Cloud child',
        provider: 'in-process',
        background: true,
      })
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-team-spawn',
      })
      response.end([
        `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: 'cloud-team-spawn-call', function: { name: 'spawn_agent', arguments: argumentsValue } }] } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":18,"completion_tokens":6,"prompt_tokens_details":{"cached_tokens":0}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    if (isTeamResume) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-team-resume',
      })
      response.end([
        'data: {"choices":[{"delta":{"content":"cloud child spawned"}}]}\n\n',
        'data: {"choices":[],"usage":{"prompt_tokens":22,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    if (isChildInitial) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-child-initial',
      })
      response.end([
        'data: {"choices":[{"delta":{"content":"cloud child initial complete"}}]}\n\n',
        'data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    if (isChildFollowup) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-child-followup',
      })
      response.flushHeaders()
      response.write('data: {"choices":[{"delta":{"content":"cloud child follow-up remains active"}}]}\n\n')
      pendingResponses.add(response)
      response.once('close', () => {
        pendingResponses.delete(response)
        resolveChildStopClosed(Date.now())
      })
      return
    }
    if (isSearchTurn) {
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-search',
      })
      response.end([
        `data: ${JSON.stringify({ choices: [{ delta: { reasoning_content: CLOUD_REASONING_SENTINEL } }] })}\n\n`,
        `data: ${JSON.stringify({ choices: [{ delta: { content: CLOUD_ASSISTANT_SENTINEL } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":16,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":1}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    if (isExtensionToolStart) {
      const stream = [
        `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, id: 'cloud-extension-v1-tool-call', function: { name: 'signed_fixture', arguments: '{"subject":"cloud-browser"}' } }] } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":15,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":0}}}\n\n',
        'data: [DONE]\n\n',
      ]
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-extension-v1-tool-call',
      })
      response.end(stream.join(''))
      return
    }
    if (isExtensionToolResume) {
      const output = typeof latestMessage.content === 'string'
        ? latestMessage.content
        : JSON.stringify(latestMessage.content ?? '')
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'x-request-id': 'cloud-browser-extension-v1-tool-resume',
      })
      response.end([
        `data: ${JSON.stringify({ choices: [{ delta: { content: `cloud signed extension completed: ${output}` } }] })}\n\n`,
        'data: {"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":6,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
        'data: [DONE]\n\n',
      ].join(''))
      return
    }
    const stream = [
      'data: {"choices":[{"delta":{"content":"cloud broker "}}]}\n\n',
      'data: {"choices":[{"delta":{"content":"ready"}}]}\n\n',
      'data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
      'data: [DONE]\n\n',
    ]
    response.writeHead(200, {
      'content-type': 'text/event-stream',
      'x-request-id': 'cloud-browser-provider',
    })
    response.flushHeaders()
    response.write(stream[0])
    // Keep the run observable for more than the Web inbox polling interval. This
    // proves the running UI without turning a sub-millisecond fixture completion
    // into a false negative.
    await new Promise(resolve => setTimeout(resolve, 1_200))
    response.write(stream[1])
    await new Promise(resolve => setTimeout(resolve, 300))
    response.write(stream[2])
    response.end(stream[3])
  })
  const port = await listen(server)
  return {
    baseUrl: `http://127.0.0.1:${port}/v1`,
    requestSeen,
    questionResumed,
    waitForRequest,
    requestCursor: () => requests.length,
    releaseSteer: () => releaseSteerResponse(),
    stopClosed,
    childStopClosed,
    requests,
    close: async () => {
      releaseSteerResponse()
      for (const response of pendingResponses) response.destroy()
      await closeServer(server)
    },
  }
}

async function body(request) {
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  return Buffer.concat(chunks).toString('utf8')
}

function json(response, status, value) {
  const encoded = JSON.stringify(value)
  response.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(encoded) })
  response.end(encoded)
}

async function startPostgres() {
  const name = `ternilo-cloud-browser-${process.pid}-${Date.now()}`
  await execute('docker', [
    'run', '-d', '--rm', '--name', name,
    '-e', 'POSTGRES_PASSWORD=ternilo-test-password',
    '-e', 'POSTGRES_DB=ternilo_cloud_browser_test',
    '-p', '127.0.0.1::5432', 'postgres:17',
  ])
  for (let attempt = 0; attempt < 40; attempt++) {
    try {
      await execute('docker', ['exec', name, 'pg_isready', '-h', '127.0.0.1', '-U', 'postgres', '-d', 'ternilo_cloud_browser_test'])
      const { stdout } = await execute('docker', ['port', name, '5432/tcp'])
      const port = stdout.trim().split(':').at(-1)
      return {
        name,
        url: `postgres://postgres:ternilo-test-password@127.0.0.1:${port}/ternilo_cloud_browser_test`,
        stop: () => execute('docker', ['stop', name]).catch(() => {}),
      }
    } catch {
      await new Promise(resolve => setTimeout(resolve, 500))
    }
  }
  await execute('docker', ['stop', name]).catch(() => {})
  throw new Error('PostgreSQL test container did not become ready')
}

function startProcess(binary, args, environment = {}) {
  const child = spawn(binary, args, {
    cwd: repository,
    env: { ...cleanEnvironment, ...environment },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  child.stdout.setEncoding('utf8')
  child.stderr.setEncoding('utf8')
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  return { child, diagnostics: () => output }
}

async function stopProcess(process) {
  if (!process || process.child.exitCode !== null || process.child.signalCode !== null) return
  process.child.kill('SIGINT')
  const exited = new Promise(resolve => process.child.once('exit', resolve))
  let forceTimer
  const graceful = await Promise.race([
    exited.then(() => true),
    new Promise(resolve => { forceTimer = setTimeout(() => resolve(false), 5000) }),
  ])
  clearTimeout(forceTimer)
  if (!graceful && process.child.exitCode === null) process.child.kill('SIGKILL')
  if (process.child.exitCode === null) await exited
}

async function waitForHttp(url, process) {
  for (let attempt = 0; attempt < 80; attempt++) {
    if (process.child.exitCode !== null) throw new Error(`process exited early: ${process.diagnostics()}`)
    try {
      const response = await fetch(url)
      if (response.ok) return
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 250))
  }
  throw new Error(`HTTP service did not become ready: ${process.diagnostics()}`)
}

async function waitForOutput(process, pattern) {
  for (let attempt = 0; attempt < 120; attempt++) {
    if (process.child.exitCode !== null) throw new Error(`process exited early: ${process.diagnostics()}`)
    if (pattern.test(process.diagnostics())) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`process did not become ready: ${process.diagnostics()}`)
}

test('cloud web completes OIDC PKCE and executes through the host model broker', { timeout: 600_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-cloud-web-'))
  await chmod(directory, 0o700)
  if (process.env.TERNILO_E2E_ARTIFACT_DIR) await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
  const fixtureDirectory = path.join(directory, 'signed-extension-v1-fixture')
  await mkdir(fixtureDirectory)
  await generateSignedFixture(fixtureDirectory)
  const postgres = await startPostgres()
  const oidc = await startOidcServer()
  const model = await startModelServer()
  const port = await freePort()
  const origin = `http://127.0.0.1:${port}`
  const policyPath = path.join(directory, 'worker-policy.json')
  const workspaceRoot = path.join(directory, 'workspaces')
  await mkdir(workspaceRoot, { recursive: true })
  // The production named volume is root-owned and writable by the trusted
  // Worker parent. This fixture uses a bind mount owned by the host user while
  // deliberately dropping DAC_OVERRIDE, so expose the same writable root
  // without widening the mkdtemp parent's 0700 boundary.
  if (containerMode) await chmod(workspaceRoot, 0o777)
  await writeFile(policyPath, JSON.stringify({
    catalog_revision: 'ternilo-cloud-v2',
    policy_revision: 'cloud-browser-e2e-v2',
    maximum_limits: { max_steps: 4, max_tool_calls: 8 },
    max_run_attempts: 2,
    max_tenant_workspace_bytes: 1073741824,
    max_tenant_workspace_entries: 100000,
    minimum_workspace_free_bytes: 0,
    max_extension_packages_per_run: 1,
    extension_host_policy: {
      allowed_capabilities: ['log', 'workspace_read'],
      maximum_rhai_limits: {
        max_operations: 1000000,
        max_string_bytes: 1048576,
        max_collection_items: 100000,
        max_call_levels: 64,
        max_expr_depth: 64,
        max_variables: 4096,
        max_functions: 256,
        max_wall_ms: 60000,
        max_input_bytes: 1048576,
        max_output_bytes: 2097152,
        max_workspace_read_bytes: 2097152,
      },
      maximum_wasm_component_limits: {
        fuel: 20000000,
        max_memory_bytes: 67108864,
        max_input_bytes: 1048576,
        max_output_bytes: 2097152,
        max_workspace_read_bytes: 2097152,
      },
      max_payload_bytes: 16777216,
    },
    allowed_plugin_kinds: [
      'ternilo.session.log', 'ternilo.prompt.registry', 'ternilo.tools.registry',
      'ternilo.hooks.registry', 'ternilo.prompt.system', 'ternilo.prompt.identity',
      'ternilo.model.host_gateway', 'ternilo.session_title.llm', 'ternilo.context.compaction',
      'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local',
      'ternilo.prompt.workspace_instructions', 'ternilo.tools.files',
      'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry',
      'ternilo.skills.filesystem', 'ternilo.tools.skills',
      'ternilo.extension.package', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team',
      'ternilo.code_runtime.rhai', 'ternilo.tools.code_mode', 'ternilo.agent.react',
    ],
    denied_tools: [],

  }))

  const serverData = path.join(directory, 'server')
  const serverConfig = path.join(serverData, 'server.json')
  const workerData = path.join(directory, 'worker')
  const workerConfig = path.join(workerData, 'worker.json')
  const ownerPassword = randomBytes(24).toString('base64url')
  const serverEnvironment = {}
  const controlName = `ternilo-server-e2e-${process.pid}-${Date.now()}`
  const workerName = `ternilo-worker-e2e-${process.pid}-${Date.now()}`
  const controlArgs = [
    'serve', '--config', containerMode ? '/var/lib/ternilo/server.json' : serverConfig,
    '--listen', `127.0.0.1:${port}`,
    '--public-url', origin,
    '--oidc-issuer', oidc.issuer,
    '--oidc-audience', 'ternilo-cloud-e2e',
    '--oidc-client-id', 'ternilo-cloud-browser',
    '--allow-insecure-oidc', '--managed-execution-enabled',
    '--worker-policy', containerMode ? '/etc/ternilo/worker-policy.json' : policyPath,
  ]
  const controlProcesses = []
  const startControl = () => {
    const process = containerMode
      ? startProcess('docker', [
          'run', '--rm', '--name', controlName, '--network', 'host',
          '--user', `${globalThis.process.getuid?.() ?? 1000}:${globalThis.process.getgid?.() ?? 1000}`,
          '--read-only', '--tmpfs', '/tmp:size=64m,mode=1777',
          '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true',
          '--volume', `${serverData}:/var/lib/ternilo`,
          '--volume', `${policyPath}:/etc/ternilo/worker-policy.json:ro`,
          '--entrypoint', '/usr/local/bin/ternilo-server', serverImage,
          ...controlArgs,
        ], serverEnvironment)
      : startProcess(controlBinary, controlArgs, serverEnvironment)
    controlProcesses.push(process)
    return process
  }
  let control
  const workerArgs = [
    'serve', '--config', containerMode ? '/etc/ternilo/worker.json' : workerConfig,
    '--workspace-root', containerMode ? '/var/lib/ternilo/workspaces' : workspaceRoot,
  ]
  const startWorker = () => containerMode
    ? startProcess('docker', [
        'run', '--rm', '--name', workerName, '--network', 'host',
        '--user', '0:0', '--read-only', '--tmpfs', '/tmp:size=1g,mode=1777',
        '--cap-drop', 'ALL', '--cap-add', 'SYS_ADMIN', '--cap-add', 'NET_ADMIN',
        '--cap-add', 'SETUID', '--cap-add', 'SETGID',
        '--cap-add', 'CHOWN', '--cap-add', 'DAC_READ_SEARCH',
        '--cap-add', 'SETPCAP',
        '--security-opt', 'no-new-privileges:true', '--security-opt', 'seccomp=unconfined',
        '--volume', `${workerConfig}:/etc/ternilo/worker.json:ro`,
        '--volume', `${workspaceRoot}:/var/lib/ternilo/workspaces`,
        '--entrypoint', '/usr/bin/setpriv', workerImage,
        '--no-new-privs', '--inh-caps=-setpcap', '--ambient-caps=-setpcap', '--bounding-set=-setpcap',
        '/usr/local/bin/ternilo-worker', ...workerArgs,
      ])
    : startProcess(workerBinary, workerArgs)
  const workerProcesses = []
  const startTrackedWorker = () => {
    const process = startWorker()
    workerProcesses.push(process)
    return process
  }
  let worker
  let browser
  let page
  let currentPhase = 'native-bootstrap'
  let nativeToken
  const nativeRequest = async (endpoint, value, method = value === undefined ? 'GET' : 'POST', tenantId) => {
    const response = await fetch(`${origin}/api/v1${endpoint}`, {
      method,
      headers: { 'content-type': 'application/json', ...(nativeToken && { authorization: `Bearer ${nativeToken}` }), ...(tenantId && { 'x-ternilo-tenant': tenantId }) },
      ...(value !== undefined && { body: JSON.stringify(value) }),
    })
    const result = response.status === 204 ? null : await response.json()
    assert.ok(response.ok, `${method} ${endpoint}: ${response.status} ${diagnosticResponseBody(JSON.stringify(result))}`)
    return result
  }
  const restartWithPolicy = async policy => {
    await stopProcess(worker)
    if (containerMode) await execute('docker', ['rm', '-f', workerName]).catch(() => {})
    await stopProcess(control)
    if (containerMode) await execute('docker', ['rm', '-f', controlName]).catch(() => {})
    await writeFile(policyPath, JSON.stringify(policy))
    control = startControl()
    await waitForHttp(`${origin}/readyz`, control)
    worker = startTrackedWorker()
    await waitForOutput(worker, /Ternilo cloud worker .* ready/)
  }
  try {
    await mkdir(serverData, { mode: 0o700 })
    await mkdir(workerData, { mode: 0o700 })
    const fixtureOwner = `${process.getuid?.() ?? 1000}:${process.getgid?.() ?? 1000}`
    const serverInitArgs = [
      'init', '--config', containerMode ? '/var/lib/ternilo/server.json' : serverConfig, '--database-url', postgres.url,
      '--owner-username', 'cloud-browser', '--owner-email', 'cloud-browser@example.test', '--non-interactive',
    ]
    await execute(containerMode ? 'docker' : controlBinary, containerMode ? [
      'run', '--rm', '--network', 'host', '--user', fixtureOwner,
      '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true',
      '--read-only', '--tmpfs', '/tmp:size=64m,mode=1777',
      '--volume', `${serverData}:/var/lib/ternilo`, '--env', 'TERNILO_SERVER_OWNER_PASSWORD',
      '--entrypoint', '/usr/local/bin/ternilo-server', serverImage, ...serverInitArgs,
    ] : serverInitArgs, { cwd: repository, env: { ...cleanEnvironment, TERNILO_SERVER_OWNER_PASSWORD: ownerPassword } })
    control = startControl()
    await waitForHttp(`${origin}/readyz`, control)
    const login = await nativeRequest('/auth/login', { username: 'cloud-browser', password: ownerPassword })
    nativeToken = login.access_token
    const linked = await nativeRequest('/auth/oidc-link', { access_token: oidc.accessToken() })
    assert.equal(linked.native, true)
    assert.equal(linked.oidc?.issuer, oidc.issuer)
    const linkedSessionResponse = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${oidc.accessToken()}` } })
    assert.equal(linkedSessionResponse.status, 200)
    assert.equal((await linkedSessionResponse.json()).user.user_id, login.user.user_id)
    await nativeRequest('/admin/instance', { mode: 'multi_user', revision: login.instance.revision }, 'PATCH')
    const cloudTenant = await nativeRequest('/tenants', { slug: 'cloud-e2e', display_name: 'Cloud E2E' })
    assert.notEqual(cloudTenant.tenant.tenant_id, login.personal_tenant_id)
    const platformProfile = {
      id: 'primary', display_name: 'Cloud upstream', base_url: model.baseUrl,
      protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 128000, max_output_tokens: 2048, reasoning: { default_effort: 'medium', efforts: { medium: 'medium', high: 'ultra' } } },
      models: [{ id: 'cloud-model', display_name: 'Cloud Model', settings: { mode: 'inherit' } }],
      timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 100,
    }
    await nativeRequest('/admin/models/providers', { profile: platformProfile, enabled: true, api_key: 'cloud-e2e-provider-secret' })
    await nativeRequest('/admin/models/publications', { model_id: 'cloud-model', display_name: 'Cloud Model', provider_id: 'primary', upstream_model: 'cloud-model', enabled: true })
    const modelGrant = await nativeRequest('/admin/models/grants', { name: 'Cloud browser budget', subject: { kind: 'user', id: login.user.user_id }, model_ids: ['cloud-model'], monthly_tokens: 10_000_000, max_concurrent_requests: 8, allow_resource_sharing: true })
    const platformSelection = { provider: 'platform_model', grant_id: modelGrant.grant_id, model_id: 'cloud-model' }
    await nativeRequest('/default-model', platformSelection, 'PUT', cloudTenant.tenant.tenant_id)
    const grant = await nativeRequest('/admin/workers', { worker_id: 'cloud-browser-worker' })
    CLOUD_E2E_SECRETS.push(grant.token, ownerPassword)
    const workerInitArgs = [
      'init', '--config', containerMode ? '/var/lib/ternilo-config/worker.json' : workerConfig,
      '--server-url', origin, '--workspace-root', containerMode ? '/var/lib/ternilo/workspaces' : workspaceRoot,
      '--sandbox', containerMode ? 'container' : 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50',
    ]
    await execute(containerMode ? 'docker' : workerBinary, containerMode ? [
      'run', '--rm', '--user', fixtureOwner, '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true',
      '--read-only', '--tmpfs', '/tmp:size=64m,mode=1777',
      '--volume', `${workerData}:/var/lib/ternilo-config`, '--env', 'TERNILO_WORKER_TOKEN',
      '--entrypoint', '/usr/local/bin/ternilo-worker', workerImage, ...workerInitArgs,
    ] : workerInitArgs, { cwd: repository, env: { ...cleanEnvironment, TERNILO_WORKER_TOKEN: grant.token } })
    assert.doesNotMatch(await readFile(workerConfig, 'utf8'), /database_url|secret_master_key|api_key_env/)
    if (!containerMode) {
      const assetHashes = {}
      for (const asset of ['app.css', 'app.js']) {
        const expected = await readFile(path.join(webRoot, 'dist', 'assets', asset))
        const response = await fetch(`${origin}/assets/${asset}`)
        assert.equal(response.ok, true, `Control did not serve ${asset}`)
        const actual = Buffer.from(await response.arrayBuffer())
        const expectedSha256 = createHash('sha256').update(expected).digest('hex')
        const actualSha256 = createHash('sha256').update(actual).digest('hex')
        assert.equal(actualSha256, expectedSha256, `Control embedded stale ${asset}`)
        assetHashes[asset] = actualSha256
      }
      console.log(`Cloud current-source Web asset hashes: ${JSON.stringify(assetHashes)}`)
    }
    worker = startTrackedWorker()
    await waitForOutput(worker, /Ternilo cloud worker .* ready/)
    browser = await chromium.launch({ headless: true })
    page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    await page.addInitScript(header => {
      const nativeFetch = window.fetch.bind(window)
      window.fetch = (input, init) => {
        if (!init?.signal) return nativeFetch(input, init)
        const headers = new Headers(input instanceof Request ? input.headers : undefined)
        new Headers(init.headers).forEach((value, name) => headers.set(name, value))
        headers.set(header, '1')
        return nativeFetch(input, { ...init, headers })
      }
    }, LIFECYCLE_REQUEST_HEADER)
    await page.addInitScript(() => {
      const NativeWebSocket = window.WebSocket
      const liveSockets = []
      const closeCalls = []
      const nativeClose = NativeWebSocket.prototype.close
      NativeWebSocket.prototype.close = function trackedClose(code, reason) {
        const call = {
          url: this.url,
          code: code ?? null,
          reason: reason ?? null,
          before: this.readyState,
          after: null,
        }
        closeCalls.push(call)
        const result = code === undefined
          ? nativeClose.call(this)
          : nativeClose.call(this, code, reason)
        call.after = this.readyState
        return result
      }
      const networkEvents = {
        offlineAdds: 0,
        offlineRemoves: 0,
        onlineAdds: 0,
        onlineRemoves: 0,
      }
      const nativeAddEventListener = window.addEventListener.bind(window)
      const nativeRemoveEventListener = window.removeEventListener.bind(window)
      window.addEventListener = (type, listener, options) => {
        if (type === 'offline') networkEvents.offlineAdds += 1
        if (type === 'online') networkEvents.onlineAdds += 1
        nativeAddEventListener(type, listener, options)
      }
      window.removeEventListener = (type, listener, options) => {
        if (type === 'offline') networkEvents.offlineRemoves += 1
        if (type === 'online') networkEvents.onlineRemoves += 1
        nativeRemoveEventListener(type, listener, options)
      }
      const TrackedWebSocket = new Proxy(NativeWebSocket, {
        construct(target, argumentsList) {
          const socket = Reflect.construct(target, argumentsList, target)
          const [url] = argumentsList
          if (new URL(String(url), window.location.href).pathname === '/api/v1/live') {
            liveSockets.push(socket)
          }
          return socket
        },
      })
      Object.defineProperty(window, 'WebSocket', {
        configurable: true,
        writable: true,
        value: TrackedWebSocket,
      })
      Object.defineProperty(window, '__terniloCloudE2eLiveSockets', { value: liveSockets })
      Object.defineProperty(window, '__terniloCloudE2eCloseCalls', { value: closeCalls })
      Object.defineProperty(window, '__terniloCloudE2eNetworkEvents', { value: networkEvents })
    })
    const pageErrors = []
    const consoleErrors = []
    const consoleErrorDetails = []
    const failedRequests = []
    const failedRequestDetails = []
    const failedRequestTasks = []
    const failedResponses = []
    const failedResponseDetails = []
    const failedResponseTasks = []
    currentPhase = 'oidc-bootstrap'
    console.log(`Cloud phase: ${currentPhase}`)
    const liveSockets = []
    const backgroundLiveRequests = []
    const requestPhases = new WeakMap()
    const isLegacyLiveRequest = request => {
      const pathname = new URL(request.url()).pathname
      return pathname === '/api/v1/questions'
        || /\/api\/v1\/sessions\/[^/]+\/(?:event-delta|queue|stats|projection|plugins)$/.test(pathname)
    }
    page.on('request', request => {
      requestPhases.set(request, currentPhase)
      if (isLegacyLiveRequest(request)) {
        backgroundLiveRequests.push({ phase: currentPhase, method: request.method(), url: request.url(), at: Date.now() })
      }
    })
    page.on('websocket', socket => {
      if (new URL(socket.url()).pathname !== '/api/v1/live') return
      const record = {
        url: socket.url(),
        openedAt: Date.now(),
        closedAt: null,
        sent: [],
        received: [],
      }
      liveSockets.push(record)
      const capture = target => event => {
        if (typeof event.payload !== 'string') return
        try {
          target.push({ frame: JSON.parse(event.payload), at: Date.now(), phase: currentPhase })
        } catch {}
      }
      socket.on('framesent', capture(record.sent))
      socket.on('framereceived', capture(record.received))
      socket.on('close', () => { record.closedAt = Date.now() })
    })
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') {
        consoleErrors.push(message.text())
        consoleErrorDetails.push({ phase: currentPhase, text: message.text() })
      }
    })
    page.on('requestfailed', request => {
      const summary = `${request.method()} ${request.url()} ${request.failure()?.errorText ?? ''}`
      failedRequests.push(summary)
      failedRequestTasks.push(request.response()
        .then(response => failedRequestDetails.push({
          summary,
          phase: requestPhases.get(request) ?? currentPhase,
          method: request.method(),
          path: new URL(request.url()).pathname,
          resourceType: request.resourceType(),
          failure: request.failure()?.errorText ?? '',
          responseStatus: response?.status() ?? null,
          lifecycleRequest: request.headers()[LIFECYCLE_REQUEST_HEADER] === '1',
        }))
        .catch(error => failedRequestDetails.push({
          summary,
          phase: requestPhases.get(request) ?? currentPhase,
          method: request.method(),
          path: new URL(request.url()).pathname,
          resourceType: request.resourceType(),
          failure: request.failure()?.errorText ?? '',
          responseStatus: null,
          lifecycleRequest: request.headers()[LIFECYCLE_REQUEST_HEADER] === '1',
          diagnosticError: error instanceof Error ? error.message : String(error),
        })))
    })
    page.on('response', response => {
      if (response.status() < 400) return
      const method = response.request().method()
      const url = response.url()
      const phase = currentPhase
      failedResponses.push(`${response.status()} ${method} ${url}`)
      failedResponseTasks.push(response.text()
        .then(body => failedResponseDetails.push({
          phase,
          status: response.status(),
          method,
          path: new URL(url).pathname,
          body: diagnosticResponseBody(body),
        }))
        .catch(error => failedResponseDetails.push({
          phase,
          status: response.status(),
          method,
          path: new URL(url).pathname,
          body: `<unavailable: ${error instanceof Error ? error.message : String(error)}>`,
        })))
    })
    const reloadPage = async () => {
      await page.reload({ waitUntil: 'domcontentloaded' })
    }
    await page.goto(`${origin}/auth/callback?code=orphaned-code&state=wrong-state`, { waitUntil: 'domcontentloaded' })
    const failedCallbackDialog = page.getByRole('dialog', { name: '登录 Ternilo' })
    await failedCallbackDialog.waitFor()
    assert.equal(await failedCallbackDialog.getByRole('alert').textContent(), 'OIDC 登录状态校验失败。')
    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: '使用组织账号登录' }).click()
    const tenantPicker = page.getByRole('combobox', { name: '切换空间' })
    try {
      await tenantPicker.waitFor({ timeout: 30_000 })
    } catch (error) {
      throw new Error(`cloud login did not finish; url=${page.url()} body=${(await page.locator('body').textContent()).slice(0, 1_000)} control=${control.diagnostics()}`, { cause: error })
    }
    await tenantPicker.selectOption(cloudTenant.tenant.tenant_id)
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-tenant')), cloudTenant.tenant.tenant_id)

    currentPhase = 'provider-and-credential-settings'
    console.log(`Cloud phase: ${currentPhase}`)
    await page.getByRole('button', { name: '我的模型', exact: true }).click()
    let settingsDialog = page.locator('[data-model-access-shell]')
    await settingsDialog.waitFor()
    assert.equal(await settingsDialog.getByRole('button', { name: '打开配置目录' }).count(), 0)
    assert.equal(await settingsDialog.getByRole('combobox', { name: '账号配置空间', exact: true }).count(), 0)
    assert.equal(await settingsDialog.getByText('primary', { exact: true }).count(), 0, 'platform grants are separate from private Provider configuration')
    assert.equal(await settingsDialog.getByRole('button', { name: /添加 Provider/ }).count(), 2)
    assert.equal(await settingsDialog.getByRole('button', { name: '编辑', exact: true }).count(), 0)
    assert.equal(await settingsDialog.getByRole('button', { name: /删除 Provider/ }).count(), 0)
    await settingsDialog.getByRole('button', { name: /添加 Provider/ }).first().click()
    const providerEditor = settingsDialog.locator('[data-provider-editor="new"]')
    await providerEditor.getByLabel('Provider ID').fill('byok')
    await providerEditor.getByLabel('显示名称', { exact: true }).fill('BYOK Provider')
    await providerEditor.getByLabel('API Key').fill('cloud-browser-byok-secret')
    await providerEditor.getByLabel('API 地址').fill(model.baseUrl)
    await selectChoice(providerEditor.getByLabel('API 协议'), 'openai-chat-completions')
    await providerEditor.locator('[id$="-provider-defaults-context"]').fill('128K')
    await providerEditor.locator('[id$="-provider-defaults-output"]').fill('2048')
    await providerEditor.getByLabel('启用 Provider 默认模型设置 的推理强度').click()
    await providerEditor.getByLabel('high实际推理值', { exact: true }).fill('ultra')
    await providerEditor.getByLabel('模型 ID 1').fill('byok-model')
    await providerEditor.getByLabel('显示名称（可选） 1').fill('BYOK Model')
    await providerEditor.getByRole('button', { name: /添加 Provider/ }).click()
    await settingsDialog.getByText('BYOK Provider', { exact: true }).waitFor()
    assert.equal((await settingsDialog.textContent()).includes('cloud-browser-byok-secret'), false)

    const refreshedProviders = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const identity = await (await fetch('/api/v1/auth/session', { headers: { authorization: `Bearer ${token}` } })).json()
      const tenant = identity.personal_tenant_id
      const response = await fetch('/api/v1/providers', {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      return { status: response.status, body: await response.json() }
    })
    assert.equal(refreshedProviders.status, 200)
    assert.equal(refreshedProviders.body.some(provider => provider.id === 'primary'), false, 'platform upstream stays outside the private Provider inventory')
    const savedByok = refreshedProviders.body.find(provider => provider.id === 'byok')
    assert.equal(savedByok.source, 'user')
    assert.equal(savedByok.api_key_ref, 'TERNILO_PROVIDER_BYOK_API_KEY')
    assert.deepEqual(savedByok.defaults, {
      context_window: 128000,
      max_output_tokens: 2048,
      reasoning: {
        default_effort: 'medium',
        efforts: { low: 'low', medium: 'medium', high: 'ultra' },
      },
    })
    assert.deepEqual(savedByok.models[0].settings, {
      mode: 'inherit',
    })
    assert.equal(JSON.stringify(refreshedProviders).includes('cloud-browser-byok-secret'), false)

    await settingsDialog.getByRole('link', { name: '用户设置', exact: true }).click()
    settingsDialog = page.locator('[data-user-settings]')
    await settingsDialog.getByRole('button', { name: '凭据与登录', exact: true }).click()
    await settingsDialog.getByPlaceholder('MY_SERVICE_TOKEN').fill('CLOUD_BROWSER_SETTING')
    const credentialValue = settingsDialog.locator('input[type="password"]')
    await credentialValue.fill('cloud-browser-setting-secret')
    await settingsDialog.getByRole('button', { name: '保存', exact: true }).click()
    await settingsDialog.getByText('CLOUD_BROWSER_SETTING', { exact: true }).waitFor()
    assert.equal(await credentialValue.inputValue(), '')
    assert.equal((await settingsDialog.textContent()).includes('cloud-browser-setting-secret'), false)

    await settingsDialog.getByRole('button', { name: 'Agent 预设', exact: true }).click()
    for (const name of ['标准模式', 'PTC 模式', '极简模式', '创意模式']) {
      await settingsDialog.getByText(name, { exact: true }).waitFor()
    }
    const cloudSystemPresets = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      const [rosterResponse, ptcResponse, creativeResponse] = await Promise.all([
        fetch('/api/v1/agent-presets', { headers }),
        fetch('/api/v1/agent-presets/ptc', { headers }),
        fetch('/api/v1/agent-presets/creative', { headers }),
      ])
      const [roster, ptc, creative] = await Promise.all([
        rosterResponse.json(), ptcResponse.json(), creativeResponse.json(),
      ])
      if (!rosterResponse.ok || !ptcResponse.ok || !creativeResponse.ok) {
        throw new Error(JSON.stringify({ roster, ptc, creative }))
      }
      return { roster, ptc, creative }
    })
    assert.deepEqual(cloudSystemPresets.roster.presets.slice(0, 4).map(preset => preset.id), [
      'standard', 'ptc', 'minimal', 'creative',
    ])
    assert.equal(cloudSystemPresets.ptc.profile.plugins.find(plugin => plugin.id === 'code-mode')?.config?.mode, 'code')
    assert.equal(cloudSystemPresets.creative.profile.plugins.find(plugin => plugin.id === 'creative-guidance')?.kind, 'ternilo.prompt.section')
    const presetWrite = page.waitForResponse(response =>
      response.url().endsWith('/api/v1/agent-presets') && response.request().method() === 'POST')
    await settingsDialog.getByRole('button', { name: /^复制预设:/ }).first().click()
    const copyDialog = page.getByRole('dialog', { name: /复制预设/ })
    await copyDialog.locator('#preset-copy-id').fill('cloud-browser-preset')
    await copyDialog.getByRole('button', { name: '创建预设' }).click()
    assert.equal((await presetWrite).status(), 201)
    const copiedRoster = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch('/api/v1/agent-presets', {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      return { status: response.status, body: await response.json() }
    })
    assert.equal(copiedRoster.status, 200)
    assert.equal(copiedRoster.body.presets.some(preset => preset.id === 'cloud-browser-preset'), true)
    const presetCard = settingsDialog.locator('.rounded-xl.border.bg-card').filter({ hasText: 'cloud-browser-preset' })
    try {
      await presetCard.waitFor({ timeout: 8_000 })
    } catch (error) {
      throw new Error(`Control preset copy did not refresh: roster=${JSON.stringify(copiedRoster)} body=${(await settingsDialog.textContent()).slice(-2_000)} failed=${JSON.stringify(failedResponses)} control=${control.diagnostics()}`, { cause: error })
    }
    await presetCard.getByRole('button', { name: '设为默认' }).click()
    await presetCard.getByText('默认', { exact: true }).waitFor()

    const unavailableAuthorization = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch('/api/v1/authorizations/begin', {
        method: 'POST',
        headers: {
          authorization: `Bearer ${token}`,
          'x-ternilo-tenant': tenant,
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          key: { space: 'reference', key: 'NO_REGISTERED_FLOW' },
          method: 'device',
          surface_id: 'cloud-browser-settings',
        }),
      })
      return { status: response.status, body: await response.json() }
    })
    assert.equal(unavailableAuthorization.status, 422)
    assert.match(unavailableAuthorization.body.error.message, /no registered authorization flow/)
    await settingsDialog.getByRole('button', { name: '返回工作台' }).click()
    await settingsDialog.waitFor({ state: 'detached' })

    let rejectProjectInventory = true
    const rejectFirstProjectInventory = async route => {
      if (route.request().method() === 'GET' && rejectProjectInventory) {
        await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { message: 'project inventory unavailable' } }) })
        return
      }
      await route.continue()
    }
    await page.route('**/api/v1/projects', rejectFirstProjectInventory)
    await page.getByRole('button', { name: '选择工作文件夹' }).last().click()
    const workspaceDialog = page.getByRole('dialog', { name: '打开工作区' })
    await workspaceDialog.waitFor()
    await workspaceDialog.getByRole('alert').filter({ hasText: 'project inventory unavailable' }).waitFor()
    rejectProjectInventory = false
    await workspaceDialog.getByRole('button', { name: '重试加载' }).click()
    await workspaceDialog.getByRole('button', { name: /托管环境/ }).click()
    await selectProject(page, workspaceDialog, '新建项目')
    await workspaceDialog.getByPlaceholder('项目名称').waitFor()
    await page.unroute('**/api/v1/projects', rejectFirstProjectInventory)
    await workspaceDialog.getByPlaceholder('项目名称').fill('Cloud Project')
    await workspaceDialog.getByRole('button', { name: '仅创建项目' }).click()
    await selectProject(page, workspaceDialog, 'Cloud Project')
    await workspaceDialog.getByLabel('工作区名称').fill('Cloud Workspace')
    await workspaceDialog.getByRole('button', { name: '打开并开始会话' }).click()
    await workspaceDialog.waitFor({ state: 'detached' })
    await page.getByText('Cloud Workspace', { exact: true }).first().waitFor()

    const cloudWorkspace = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch('/api/v1/workspaces', {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const body = await response.json()
      if (!response.ok) throw new Error(`workspace inventory failed: ${response.status} ${JSON.stringify(body)}`)
      const workspace = body.workspaces.find(candidate => candidate.name === 'Cloud Workspace')
      if (!workspace) throw new Error(`Cloud Workspace is absent: ${JSON.stringify(body)}`)
      return { tenant, workspaceId: workspace.workspace_id }
    })
    const cloudWorkspacePath = path.join(
      workspaceRoot,
      createHash('sha256').update(cloudWorkspace.tenant).digest('hex'),
      createHash('sha256').update(cloudWorkspace.workspaceId).digest('hex'),
    )
    const cloudSkillDirectory = path.join(cloudWorkspacePath, '.agents', 'skills', 'cloud-check')
    await mkdir(cloudSkillDirectory, { recursive: true })
    await writeFile(path.join(cloudSkillDirectory, 'SKILL.md'), `---\nname: cloud-check\ndescription: 验证真实 Cloud Skill 数据面\nwhen-to-use: 验证云端执行链时\nuser-invocable: true\n---\n\n# Cloud check\n\nInspect the canonical Cloud workspace and report CLOUD_SKILL_SENTINEL.\n`)
    await writeFile(path.join(cloudWorkspacePath, 'cloud-search-tool.txt'), CLOUD_TOOL_SENTINEL)

    await page.evaluate(() => localStorage.setItem('ternilo.default-permission', 'full_access'))
    const cloudSessionCreate = page.waitForRequest(request => (
      request.url().endsWith('/api/v1/sessions') && request.method() === 'POST'
    ))
    await page.locator('[data-sidebar-new-session]').click()
    assert.equal((await cloudSessionCreate).postDataJSON().permissions, 'workspace_write')
    const prompt = page.getByRole('textbox', { name: '输入任务' })
    await prompt.waitFor()
    const nextModelRequest = (predicate, description, timeoutMs = 30_000) => (
      model.waitForRequest(predicate, description, timeoutMs, model.requestCursor())
    )
    const queueResponseForInput = input => page.waitForResponse(response => {
      if (response.request().method() !== 'POST') return false
      if (!/\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname)) return false
      return response.request().postDataJSON()?.content?.input === input
    })
    const assertSuccessfulCanonicalRun = async (runId, expectedSessionId = null) => {
      const evidence = await page.evaluate(async ({ runId, expectedSessionId }) => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = expectedSessionId || localStorage.getItem('ternilo.current-session') || ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        let latest = []
        for (let attempt = 0; attempt < 300; attempt++) {
          const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
          const body = await response.json()
          if (!response.ok) throw new Error(`canonical run evidence failed: ${response.status} ${JSON.stringify(body)}`)
          latest = body.filter(event => event.run_id === runId)
          if (latest.some(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))) {
            return { events: latest, sessionId, tenant, run: null }
          }
          await new Promise(resolve => setTimeout(resolve, 100))
        }
        const runResponse = await fetch(
          `/api/v1/tenants/${encodeURIComponent(tenant)}/runs/${encodeURIComponent(runId)}`,
          { headers },
        )
        return {
          events: latest,
          sessionId,
          tenant,
          run: { status: runResponse.status, body: await runResponse.json().catch(() => null) },
        }
      }, { runId, expectedSessionId })
      const events = evidence.events
      if (!events.some(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))) {
        throw new Error(`canonical run ${runId} did not terminate: evidence=${JSON.stringify(evidence)} control=${control.diagnostics()} worker=${worker.diagnostics()}`)
      }
      const terminal = events.filter(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))
      assert.equal(terminal.length, 1)
      assert.equal(terminal[0].type, 'turn_finished')
      return events
    }
    const readCloudExtensionEvidence = async () => page.evaluate(async ({ packageId, version, skillName }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      const [inventoryResponse, stateResponse, profileResponse, skillsResponse] = await Promise.all([
        fetch(`/api/v1/extensions?session_id=${encodeURIComponent(sessionId)}`, { headers }),
        fetch('/api/v1/state', { headers }),
        fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/plugins`, { headers }),
        fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/skills`, { headers }),
      ])
      const [inventory, state, profile, skills] = await Promise.all([
        inventoryResponse.json(),
        stateResponse.json(),
        profileResponse.json(),
        skillsResponse.json(),
      ])
      if (!inventoryResponse.ok || !stateResponse.ok || !profileResponse.ok || !skillsResponse.ok) {
        throw new Error(`Cloud Extension evidence failed: ${JSON.stringify({
          inventory: { status: inventoryResponse.status, body: inventory },
          state: { status: stateResponse.status, body: state },
          profile: { status: profileResponse.status, body: profile },
          skills: { status: skillsResponse.status, body: skills },
        })}`)
      }
      const matches = entry => entry?.kind === 'ternilo.extension.package'
        && entry.config?.package_id === packageId
        && entry.config?.version === version
      return {
        inventory,
        sessionId,
        rawMount: state.sessions
          .find(session => session.identity.session_id === sessionId)?.profile_plugins?.find(matches) ?? null,
        effectiveMount: profile.plugins?.find(matches) ?? null,
        skill: skills.skills?.find(skill => skill.name === skillName) ?? null,
      }
    }, {
      packageId: CLOUD_EXTENSION_PACKAGE_ID,
      version: CLOUD_EXTENSION_VERSION,
      skillName: CLOUD_EXTENSION_SKILL_NAME,
    })
    const permissionMenu = page.getByRole('button', { name: '权限: 工作区写入' })
    await permissionMenu.click()
    assert.equal(await page.getByRole('menuitemradio', { name: '完整访问' }).count(), 0)
    await page.keyboard.press('Escape')
    await prompt.fill('/')
    const firstCommandDirectory = page.getByRole('listbox', { name: '指令' })
    try {
      await firstCommandDirectory.getByRole('option', { name: /^\/read\b/ }).waitFor({ timeout: 30_000 })
    } catch (error) {
      const commandDiagnostic = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/commands`, {
          headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
        })
        return {
          sessionId,
          status: response.status,
          body: await response.json().catch(() => null),
        }
      })
      throw new Error(`initial Cloud command catalog is unavailable: catalog=${JSON.stringify(commandDiagnostic)} menu=${JSON.stringify(await firstCommandDirectory.textContent().catch(() => null))} failed_requests=${JSON.stringify(failedRequests)} failed_responses=${JSON.stringify(failedResponses)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    await prompt.fill('')
    await prompt.fill('/skill ')
    const firstSkillDirectory = page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'cloud-check' })
    await firstSkillDirectory.waitFor({ timeout: 30_000 })
    assert.match(await firstSkillDirectory.textContent(), /验证真实 Cloud Skill 数据面/)
    assert.match(await firstSkillDirectory.textContent(), /filesystem/)
    assert.equal((await page.locator('[data-composer-menu]').textContent()).includes('重试'), false)
    await prompt.fill('')
    const modelPicker = page.getByRole('button', { name: /Cloud Model/ })
    assert.match(await modelPicker.textContent(), /Cloud Model/)
    await modelPicker.click()
    await page.getByRole('menuitem', { name: /模型/ }).hover()
    await page.getByRole('menuitem', { name: /BYOK Model/ }).press('Enter')
    await page.getByRole('button', { name: /BYOK Model · 自备模型（BYOK） · medium/ }).waitFor()
    await page.waitForFunction(() => !document.querySelector('[role="menu"]'))
    await page.getByRole('button', { name: /BYOK Model · 自备模型（BYOK） · medium/ }).click()
    const reasoningMenu = page.getByRole('menuitem', { name: /推理强度/ })
    try {
      await reasoningMenu.hover({ timeout: 10_000 })
    } catch (error) {
      throw new Error(`reasoning menu did not open; menus=${JSON.stringify(await page.locator('[role="menu"]').allTextContents())} providers=${JSON.stringify(await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const response = await fetch('/api/v1/providers', { headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant } })
        return response.json()
      }))}`, { cause: error })
    }
    await page.getByRole('menuitem', { name: /^high$/ }).press('Enter')
    await page.getByRole('button', { name: /BYOK Model · 自备模型（BYOK） · high/ }).waitFor()
    await prompt.evaluate(element => {
      const encoded = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
      const bytes = Uint8Array.from(atob(encoded), character => character.charCodeAt(0))
      const transfer = new DataTransfer()
      transfer.items.add(new File([bytes], 'cloud-attachment.png', { type: 'image/png' }))
      element.dispatchEvent(new ClipboardEvent('paste', {
        bubbles: true, cancelable: true, clipboardData: transfer,
      }))
    })
    await page.getByRole('button', { name: '预览 cloud-attachment.webp' }).waitFor()
    await prompt.fill('verify cloud execution')
    await page.getByRole('button', { name: '发送' }).click()
    try {
      await page.getByRole('status').filter({ hasText: '正在运行' }).waitFor({ timeout: 30_000 })
    } catch (error) {
      const failureEvidence = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [events, exported] = await Promise.all([
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
        ])
        return { sessionId, events, exported }
      })
      throw new Error(`cloud run never became observable; body=${(await page.locator('body').textContent()).slice(-2_000)} failure=${JSON.stringify(failureEvidence)} failed_requests=${JSON.stringify(failedRequests)} failed_responses=${JSON.stringify(failedResponses)} model_requests=${JSON.stringify(model.requests)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    try {
      await Promise.race([
        page.locator('article[data-role="assistant"]').filter({ hasText: 'cloud broker ready' })
          .waitFor({ timeout: 60_000 }),
        page.getByRole('alert').filter({ hasText: 'Provider 拒绝访问' })
          .waitFor({ timeout: 60_000 })
          .then(() => { throw new Error('cloud run reported a Provider access failure') }),
      ])
    } catch (error) {
      const failureEvidence = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [events, exported] = await Promise.all([
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
        ])
        return { sessionId, events, exported }
      })
      throw new Error(`cloud run did not finish; body=${(await page.locator('body').textContent()).slice(-2_000)} failure=${JSON.stringify(failureEvidence)} model_requests=${JSON.stringify(model.requests)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })

    currentPhase = 'signed-extension-v1-lifecycle'
    console.log(`Cloud phase: ${currentPhase}`)
    const extensionSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session') ?? '')
    assert.notEqual(extensionSessionId, '')
    await page.getByRole('button', { name: '用户设置' }).click()
    let extensionSettings = page.locator('[data-user-settings]')
    await extensionSettings.getByRole('button', { name: '插件', exact: true }).click()
    await extensionSettings.getByRole('tab', { name: '扩展包' }).click()
    await extensionSettings.locator('#plugin-publisher-file').setInputFiles(path.join(fixtureDirectory, 'publisher.json'))
    await extensionSettings.getByText(CLOUD_EXTENSION_PUBLISHER_ID, { exact: true }).waitFor()
    await extensionSettings.locator('#plugin-bundle-file').setInputFiles(path.join(fixtureDirectory, 'bundle.json'))
    const extensionReview = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await extensionReview.waitFor()
    assert.match(await extensionReview.textContent(), new RegExp(CLOUD_EXTENSION_PACKAGE_ID.replaceAll('.', '\\.')))
    assert.match(
      await extensionReview.locator(`[data-extension-prompt-section-review="${CLOUD_EXTENSION_PROMPT_ID}"]`).textContent(),
      new RegExp(CLOUD_EXTENSION_PROMPT_CONTENT.replaceAll('.', '\\.')),
    )
    assert.match(
      await extensionReview.locator(`[data-extension-skill-review="${CLOUD_EXTENSION_SKILL_NAME}"]`).textContent(),
      new RegExp(CLOUD_EXTENSION_SKILL_CONTENT.replaceAll('.', '\\.')),
    )
    assert.equal(await extensionReview.getByRole('checkbox', { name: /log/ }).isChecked(), true)
    assert.equal(await extensionReview.getByRole('checkbox', { name: /workspace_read/ }).isChecked(), true)
    await extensionReview.getByRole('button', { name: '确认安装' }).click()
    await extensionReview.waitFor({ state: 'detached' })
    const extensionPackageSelector = `[data-extension-package="${CLOUD_EXTENSION_PACKAGE_ID}@${CLOUD_EXTENSION_VERSION}"]`
    let extensionPackage = extensionSettings.locator(extensionPackageSelector)
    await extensionPackage.waitFor()
    assert.equal(await extensionPackage.locator('[data-extension-runtime]').textContent(), 'wasm-component')

    let extensionEvidence = await readCloudExtensionEvidence()
    let installedExtension = extensionEvidence.inventory.extensions.find(extension => (
      extension.manifest.package_id === CLOUD_EXTENSION_PACKAGE_ID
      && extension.manifest.version === CLOUD_EXTENSION_VERSION
    ))
    assert.ok(installedExtension)
    assert.equal(installedExtension.manifest.schema_version, 1)
    assert.equal(installedExtension.manifest.publisher_key_id, CLOUD_EXTENSION_PUBLISHER_ID)
    assert.equal(installedExtension.manifest.source, CLOUD_EXTENSION_SOURCE)
    assert.match(installedExtension.manifest.payload_sha256, /^[a-f0-9]{64}$/)
    assert.deepEqual(installedExtension.granted_capabilities.sort(), ['log', 'workspace_read'])
    assert.equal(installedExtension.manifest.contributions.tools.some(tool => tool.spec.name === 'signed_fixture'), true)
    assert.equal(installedExtension.manifest.contributions.prompt_sections.some(section => (
      section.id === CLOUD_EXTENSION_PROMPT_ID && section.content === CLOUD_EXTENSION_PROMPT_CONTENT
    )), true)
    assert.equal(installedExtension.manifest.contributions.skills.some(skill => (
      skill.name === CLOUD_EXTENSION_SKILL_NAME && skill.content === CLOUD_EXTENSION_SKILL_CONTENT
    )), true)

    await extensionPackage.locator('[data-extension-mount]').click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    extensionEvidence = await readCloudExtensionEvidence()
    assert.deepEqual(extensionEvidence.rawMount?.config, {
      package_id: CLOUD_EXTENSION_PACKAGE_ID,
      version: CLOUD_EXTENSION_VERSION,
      settings: { salutation: 'hello' },
    })
    assert.equal(extensionEvidence.rawMount?.enabled, true)
    assert.equal(extensionEvidence.effectiveMount?.enabled, true)
    assert.equal(extensionEvidence.skill?.description, 'Use a signed Extension fixture for browser acceptance checks.')
    assert.deepEqual(extensionEvidence.skill?.invocation, { model_invocable: true, user_invocable: true })
    await extensionSettings.getByRole('button', { name: '返回工作台' }).click()

    const extensionToolModelRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_EXTENSION_TOOL_TASK)
      && observed.body.tools?.some(tool => tool?.function?.name === 'signed_fixture')
    ), 'signed Cloud Extension v1 Tool request')
    const extensionToolSubmissionResponse = page.waitForResponse(response => (
      response.request().method() === 'POST'
      && response.url().endsWith(`/api/v1/sessions/${encodeURIComponent(extensionSessionId)}/queue`)
    ))
    await prompt.fill(CLOUD_EXTENSION_TOOL_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    const extensionToolSubmissionResponseValue = await extensionToolSubmissionResponse
    if (!extensionToolSubmissionResponseValue.ok()) {
      throw new Error(`Cloud Extension Tool submission failed: ${extensionToolSubmissionResponseValue.status()} ${await extensionToolSubmissionResponseValue.text()}`)
    }
    const extensionToolSubmission = await extensionToolSubmissionResponseValue.json()
    let observedExtensionToolRequest
    try {
      observedExtensionToolRequest = await extensionToolModelRequest
    } catch (error) {
      const failureEvidence = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [state, events, exported] = await Promise.all([
          fetch('/api/v1/state', { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, { headers })
            .then(async response => ({ status: response.status, body: await response.json().catch(() => null) })),
        ])
        return { sessionId, state, events, exported }
      })
      throw new Error(`Cloud Extension v1 Tool run never reached the model; failure=${JSON.stringify(failureEvidence)} failed_requests=${JSON.stringify(failedRequests)} failed_responses=${JSON.stringify(failedResponses)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    assert.equal(observedExtensionToolRequest.body.tools.some(tool => tool?.function?.name === 'signed_fixture'), true)
    assert.match(
      JSON.stringify(observedExtensionToolRequest.body.messages ?? []),
      new RegExp(CLOUD_EXTENSION_PROMPT_CONTENT.replaceAll('.', '\\.')),
    )
    const extensionApproval = page.locator('[data-tool-approval]').filter({ hasText: 'Signed fixture report' })
    await extensionApproval.waitFor({ timeout: 30_000 })
    await extensionApproval.getByRole('button', { name: '允许一次' }).click()
    await page.getByText(/cloud signed extension completed:.*hello from fixture/).waitFor({ timeout: 30_000 })
    await assertSuccessfulCanonicalRun(extensionToolSubmission.run_id)
    const cloudExtensionTool = page.locator('[data-tool-call-id="cloud-extension-v1-tool-call"]')
    await cloudExtensionTool.waitFor({ state: 'attached' })
    if (!await cloudExtensionTool.isVisible()) {
      await cloudExtensionTool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await cloudExtensionTool.waitFor()
    assert.equal(await cloudExtensionTool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await cloudExtensionTool.textContent(), /Signed fixture report.*cloud-browser/)
    const extensionSystemPrompt = page.locator('[data-system-prompt-row]').last()
    await extensionSystemPrompt.locator('[data-disclosure-row]').click()
    assert.match(
      await extensionSystemPrompt.locator('[data-system-prompt-body]').textContent(),
      new RegExp(CLOUD_EXTENSION_PROMPT_CONTENT.replaceAll('.', '\\.')),
    )

    currentPhase = 'signed-extension-v1-skill'
    console.log(`Cloud phase: ${currentPhase}`)
    const extensionSkillModelRequest = nextModelRequest(observed => {
      const latestUser = [...(observed.body.messages ?? [])]
        .reverse()
        .find(message => message?.role === 'user')?.content
      const content = typeof latestUser === 'string' ? latestUser : JSON.stringify(latestUser ?? '')
      return content.includes(CLOUD_EXTENSION_SKILL_TASK)
        && content.includes(`<skill_content name="${CLOUD_EXTENSION_SKILL_NAME}">`)
        && content.includes(CLOUD_EXTENSION_SKILL_CONTENT)
    }, 'signed Cloud Extension v1 Skill request')
    const assistantCountBeforeExtensionSkill = await page.locator('article[data-role="assistant"]').count()
    const extensionSkillSubmissionResponse = queueResponseForInput(CLOUD_EXTENSION_SKILL_TASK)
    await prompt.fill(`/skill ${CLOUD_EXTENSION_SKILL_NAME} ${CLOUD_EXTENSION_SKILL_TASK}`)
    await page.getByRole('button', { name: '发送' }).click()
    const extensionSkillSubmission = await (await extensionSkillSubmissionResponse).json()
    const observedExtensionSkillRequest = await extensionSkillModelRequest
    const serializedExtensionSkillInput = JSON.stringify(observedExtensionSkillRequest.body.messages ?? [])
    assert.match(serializedExtensionSkillInput, new RegExp(CLOUD_EXTENSION_SKILL_CONTENT.replaceAll('.', '\\.')))
    assert.match(serializedExtensionSkillInput, /<user_request>/)
    await page.waitForFunction(expected => (
      document.querySelectorAll('article[data-role="assistant"]').length > expected
      && !document.querySelector('[data-composer-card][data-busy]')
    ), assistantCountBeforeExtensionSkill)
    const extensionSkillEvents = await assertSuccessfulCanonicalRun(extensionSkillSubmission.run_id)
    const extensionSkillUserEvent = extensionSkillEvents.find(event => (
      event.type === 'user_message' && event.source?.skill_name === CLOUD_EXTENSION_SKILL_NAME
    ))
    assert.ok(extensionSkillUserEvent)
    assert.match(extensionSkillUserEvent.display_content, new RegExp(`/skill ${CLOUD_EXTENSION_SKILL_NAME}`))
    assert.equal(extensionSkillUserEvent.display_content.includes(CLOUD_EXTENSION_SKILL_CONTENT), false)
    assert.match(extensionSkillUserEvent.content, new RegExp(CLOUD_EXTENSION_SKILL_CONTENT.replaceAll('.', '\\.')))

    currentPhase = 'signed-extension-v1-lifecycle'
    console.log(`Cloud phase: ${currentPhase}`)
    await page.getByRole('button', { name: '用户设置' }).click()
    extensionSettings = page.locator('[data-user-settings]')
    await extensionSettings.getByRole('button', { name: '插件', exact: true }).click()
    await extensionSettings.getByRole('tab', { name: '扩展包' }).click()
    extensionPackage = extensionSettings.locator(extensionPackageSelector)
    await extensionPackage.getByRole('switch', { name: `停用扩展包 ${CLOUD_EXTENSION_PACKAGE_ID}` }).click()
    await extensionPackage.getByRole('switch', { name: `启用扩展包 ${CLOUD_EXTENSION_PACKAGE_ID}` }).waitFor()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    extensionEvidence = await readCloudExtensionEvidence()
    installedExtension = extensionEvidence.inventory.extensions.find(extension => extension.manifest.package_id === CLOUD_EXTENSION_PACKAGE_ID)
    assert.equal(installedExtension?.enabled, false)
    assert.equal(extensionEvidence.rawMount, null)
    assert.equal(extensionEvidence.effectiveMount, null)
    assert.equal(extensionEvidence.skill, null)

    await extensionPackage.getByRole('switch', { name: `启用扩展包 ${CLOUD_EXTENSION_PACKAGE_ID}` }).click()
    await extensionPackage.getByRole('switch', { name: `停用扩展包 ${CLOUD_EXTENSION_PACKAGE_ID}` }).waitFor()
    await extensionPackage.locator('[data-extension-mount]').click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    extensionEvidence = await readCloudExtensionEvidence()
    assert.equal(extensionEvidence.rawMount?.enabled, true)
    assert.equal(extensionEvidence.effectiveMount?.enabled, true)
    assert.equal(extensionEvidence.skill?.name, CLOUD_EXTENSION_SKILL_NAME)

    await extensionPackage.getByRole('button', { name: `卸载扩展包 ${CLOUD_EXTENSION_PACKAGE_ID}` }).click()
    const uninstallExtension = page.getByRole('dialog', { name: '卸载这个扩展包？' })
    const uninstallResponse = page.waitForResponse(response => (
      response.request().method() === 'DELETE'
      && new URL(response.url()).pathname === `/api/v1/extensions/${CLOUD_EXTENSION_PACKAGE_ID}/${CLOUD_EXTENSION_VERSION}`
    ))
    await uninstallExtension.getByRole('button', { name: '卸载', exact: true }).click()
    assert.equal((await uninstallResponse).status(), 204)
    await extensionPackage.waitFor({ state: 'detached' })
    extensionEvidence = await readCloudExtensionEvidence()
    assert.equal(extensionEvidence.inventory.extensions.some(extension => extension.manifest.package_id === CLOUD_EXTENSION_PACKAGE_ID), false)
    assert.equal(extensionEvidence.rawMount, null)
    assert.equal(extensionEvidence.effectiveMount, null)
    assert.equal(extensionEvidence.skill, null)

    const bundleInput = extensionSettings.locator('#plugin-bundle-file')
    await bundleInput.setInputFiles([])
    await bundleInput.setInputFiles(path.join(fixtureDirectory, 'bundle.json'))
    const reinstallReview = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await reinstallReview.waitFor()
    await reinstallReview.getByRole('button', { name: '确认安装' }).click()
    await reinstallReview.waitFor({ state: 'detached' })
    extensionPackage = extensionSettings.locator(extensionPackageSelector)
    await extensionPackage.waitFor()
    extensionEvidence = await readCloudExtensionEvidence()
    installedExtension = extensionEvidence.inventory.extensions.find(extension => extension.manifest.package_id === CLOUD_EXTENSION_PACKAGE_ID)
    assert.equal(installedExtension?.manifest.schema_version, 1)
    assert.equal(installedExtension?.manifest.payload_sha256.length, 64)
    await extensionPackage.locator('[data-extension-mount]').click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    extensionEvidence = await readCloudExtensionEvidence()
    assert.equal(extensionEvidence.effectiveMount?.enabled, true)
    assert.equal(extensionEvidence.skill?.name, CLOUD_EXTENSION_SKILL_NAME)

    await extensionSettings.getByRole('button', { name: `撤销发布者 ${CLOUD_EXTENSION_PUBLISHER_ID}` }).click()
    const revokeExtensionPublisher = page.getByRole('dialog', { name: '撤销这个发布者？' })
    await revokeExtensionPublisher.getByRole('button', { name: '撤销', exact: true }).click()
    await revokeExtensionPublisher.waitFor({ state: 'detached' })
    await extensionPackage.getByText('已撤销', { exact: true }).waitFor()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    extensionEvidence = await readCloudExtensionEvidence()
    const revokedPublisher = extensionEvidence.inventory.publishers.find(publisher => (
      publisher.trust.key_id === CLOUD_EXTENSION_PUBLISHER_ID
    ))
    const revokedExtension = extensionEvidence.inventory.extensions.find(extension => (
      extension.manifest.package_id === CLOUD_EXTENSION_PACKAGE_ID
      && extension.manifest.version === CLOUD_EXTENSION_VERSION
    ))
    assert.equal(revokedPublisher?.revoked, true)
    assert.equal(revokedExtension?.revoked, true)
    assert.equal(extensionEvidence.rawMount, null)
    assert.equal(extensionEvidence.effectiveMount, null)
    assert.equal(extensionEvidence.skill, null)
    await extensionSettings.getByRole('button', { name: '返回工作台' }).click()
    await extensionSettings.waitFor({ state: 'detached' })
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })

    currentPhase = 'structured-question'
    console.log(`Cloud phase: ${currentPhase}`)
    const questionStartRequest = nextModelRequest(observed => (
      latestUserContent(observed.body).includes(CLOUD_QUESTION_TRIGGER)
    ), 'Cloud structured question request')
    await prompt.fill(CLOUD_QUESTION_TRIGGER)
    await page.getByRole('button', { name: '发送' }).click()
    const requestedQuestion = await questionStartRequest
    assert.equal(requestedQuestion.body.tools?.some(tool => tool?.function?.name === 'ask_user'), true)
    const questionTakeover = page.locator('[data-question-takeover]')
    try {
      await questionTakeover.getByText(CLOUD_QUESTION_ONE, { exact: true }).waitFor({ timeout: 30_000 })
    } catch (error) {
      const diagnostic = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [stateResponse, eventsResponse] = await Promise.all([
          fetch('/api/v1/state', { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
        ])
        return {
          sessionId,
          stateStatus: stateResponse.status,
          state: await stateResponse.json().catch(() => null),
          eventsStatus: eventsResponse.status,
          events: await eventsResponse.json().catch(() => null),
          body: document.body.textContent?.slice(-4_000),
        }
      })
      throw new Error(`Cloud question takeover did not appear: ${JSON.stringify({ diagnostic, failedResponses, failedRequests, control: control.diagnostics(), worker: worker.diagnostics() })}`, { cause: error })
    }
    assert.equal(await questionTakeover.getByText('云端决策', { exact: true }).isVisible(), true)
    assert.equal(await questionTakeover.getByText('Control Cloud', { exact: true }).isVisible(), true)
    const desktopQuestionBox = await questionTakeover.boundingBox()
    assert.equal(desktopQuestionBox.x >= 0 && desktopQuestionBox.x + desktopQuestionBox.width <= 1440, true)

    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForFunction(() => (document.querySelector('.app-sidebar')?.getBoundingClientRect().right ?? 0) <= 0)
    const mobileQuestionBox = await questionTakeover.boundingBox()
    assert.equal(mobileQuestionBox.x >= 6 && mobileQuestionBox.x + mobileQuestionBox.width <= 384, true)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    const firstOption = questionTakeover.getByRole('radio', { name: '先验证' })
    const firstOptionBox = await firstOption.boundingBox()
    assert.equal(firstOptionBox.height >= 40 && firstOptionBox.x >= 0 && firstOptionBox.x + firstOptionBox.width <= 390, true)
    await firstOption.click()

    await questionTakeover.getByText(CLOUD_QUESTION_TWO, { exact: true }).waitFor()
    await questionTakeover.getByRole('checkbox', { name: 'Web' }).click()
    await questionTakeover.getByRole('checkbox', { name: 'Worker' }).click()
    await questionTakeover.getByRole('textbox', { name: '输入回答' }).fill('移动端继续操作')
    await questionTakeover.getByRole('button', { name: '提交' }).click()

    const resumedQuestionRequest = await withTimeout(model.questionResumed, 'Cloud question Provider resume')
    const questionToolOutput = resumedQuestionRequest.body.messages.find(message =>
      message?.role === 'tool' && message?.tool_call_id === 'cloud-question-call')
    assert.deepEqual(JSON.parse(questionToolOutput.content), { answers: [
      { id: 'strategy', selected: ['先验证 (Recommended)'] },
      { id: 'surfaces', selected: ['Web', 'Worker'], custom: '移动端继续操作' },
    ] })
    await page.locator('article[data-role="assistant"]').filter({ hasText: 'cloud question resumed' })
      .waitFor({ timeout: 30_000 })
    await questionTakeover.waitFor({ state: 'detached' })
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })

    const questionEvidence = await page.evaluate(async ({ firstQuestion, secondQuestion }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      if (!response.ok) throw new Error(`question event evidence failed: ${response.status}`)
      const events = await response.json()
      const asked = events.filter(event => event.type === 'user_question_asked'
        && [firstQuestion, secondQuestion].includes(event.question?.question))
      const ids = asked.map(event => event.question.id)
      const answered = events.filter(event => event.type === 'user_question_answered'
        && ids.includes(event.answer?.question_id))
      const runIds = [...new Set([...asked, ...answered].map(event => event.run_id))]
      const finished = events.filter(event => event.type === 'turn_finished' && runIds.includes(event.run_id))
      return { asked, answered, finished, runIds }
    }, { firstQuestion: CLOUD_QUESTION_ONE, secondQuestion: CLOUD_QUESTION_TWO })
    assert.equal(questionEvidence.asked.length, 2)
    assert.equal(questionEvidence.answered.length, 2)
    assert.equal(questionEvidence.runIds.length, 1)
    assert.equal(questionEvidence.finished.length, 1)
    assert.equal(questionEvidence.finished[0].answer, 'cloud question resumed')
    assert.equal(await page.locator('[data-question-lifecycle][data-state="answered"]')
      .filter({ hasText: CLOUD_QUESTION_ONE }).count(), 1)
    assert.equal(await page.locator('[data-question-lifecycle][data-state="answered"]')
      .filter({ hasText: CLOUD_QUESTION_TWO }).count(), 1)
    await page.setViewportSize({ width: 1440, height: 900 })

    currentPhase = 'cloud-agent-team'
    console.log(`Cloud phase: ${currentPhase}`)
    const teamStartRequest = nextModelRequest(observed => (
      latestUserContent(observed.body).includes(CLOUD_TEAM_TASK)
      && observed.body.tools?.some(tool => tool?.function?.name === 'spawn_agent')
    ), 'Cloud Agent Team parent spawn request')
    const childInitialRequest = nextModelRequest(observed => (
      latestUserContent(observed.body).includes(CLOUD_CHILD_INITIAL_TASK)
    ), 'Cloud canonical child initial request')
    await prompt.fill(CLOUD_TEAM_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    await teamStartRequest
    const teamSpawnApproval = page.locator('[data-tool-approval]').filter({ hasText: 'spawn_agent' })
    await teamSpawnApproval.waitFor({ timeout: 30_000 })
    await teamSpawnApproval.getByRole('button', { name: '允许一次' }).click()
    await teamSpawnApproval.waitFor({ state: 'detached' })
    try {
      await childInitialRequest
    } catch (error) {
      const teamFailure = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const stateResponse = await fetch('/api/v1/state', { headers })
        const state = await stateResponse.json().catch(() => null)
        const sessions = Array.isArray(state?.sessions) ? state.sessions : []
        const events = await Promise.all(sessions.map(async session => {
          const sessionId = session.identity?.session_id ?? ''
          const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
          return { sessionId, status: response.status, body: await response.json().catch(() => null) }
        }))
        return { stateStatus: stateResponse.status, state, events }
      })
      throw new Error(`Cloud child did not start; evidence=${JSON.stringify(teamFailure)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    await page.getByText('cloud child spawned', { exact: true }).waitFor({ timeout: 30_000 })
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })

    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    let cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
    await cloudTeam.waitFor()
    let cloudChild = cloudTeam.locator('[data-agent-team-member]').filter({ hasText: 'Cloud child' })
    await cloudChild.waitFor({ timeout: 30_000 })
    try {
      await page.waitForFunction(() => [...document.querySelectorAll('[data-agent-team-member]')]
        .some(member => member.textContent?.includes('Cloud child') && member.getAttribute('data-status') === 'idle'))
    } catch (error) {
      const statusEvidence = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const parentSessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const stateResponse = await fetch('/api/v1/state', { headers })
        const state = await stateResponse.json().catch(() => null)
        const sessions = Array.isArray(state?.sessions) ? state.sessions : []
        const child = sessions.find(session => session.parent_session_id === parentSessionId
          && session.subagent?.subagent_id)
        const eventResults = await Promise.all([parentSessionId, child?.identity?.session_id ?? '']
          .filter(Boolean)
          .map(async sessionId => {
            const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers })
            return { sessionId, status: response.status, events: await response.json().catch(() => null) }
          }))
        return {
          members: [...document.querySelectorAll('[data-agent-team-member]')].map(member => ({
            id: member.getAttribute('data-agent-team-member'),
            status: member.getAttribute('data-status'),
            text: member.textContent,
          })),
          parentSessionId,
          stateStatus: stateResponse.status,
          child,
          eventResults,
        }
      })
      throw new Error(`Cloud child did not settle in Agent Team; evidence=${JSON.stringify(statusEvidence)}`, { cause: error })
    }
    const cloudChildMemberId = await cloudChild.getAttribute('data-agent-team-member')
    assert.ok(cloudChildMemberId)
    assert.match(await cloudChild.textContent(), /in-process/)

    const cloudTeamSessions = await page.evaluate(async memberId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const parentSessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch('/api/v1/state', {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const state = await response.json()
      if (!response.ok) throw new Error(`Cloud Team state failed: ${response.status} ${JSON.stringify(state)}`)
      const child = state.sessions.find(session => session.parent_session_id === parentSessionId
        && session.subagent?.subagent_id === memberId
        && session.subagent?.transcript_kind === 'conversation')
      return { parentSessionId, childSessionId: child?.identity?.session_id ?? '' }
    }, cloudChildMemberId)
    assert.notEqual(cloudTeamSessions.parentSessionId, '')
    assert.notEqual(cloudTeamSessions.childSessionId, '')
    assert.notEqual(cloudTeamSessions.childSessionId, cloudChildMemberId)

    await cloudTeam.locator('#agent-team-tasks-tab').click()
    await cloudTeam.getByRole('button', { name: '新建任务' }).click()
    let cloudTaskForm = cloudTeam.locator('[data-agent-team-task-form="create"]')
    await cloudTaskForm.getByLabel('任务名称').fill(CLOUD_TEAM_TASK_SUBJECT)
    await cloudTaskForm.getByLabel('详细说明').fill('Verify the durable Cloud task board')
    await selectChoice(cloudTaskForm.getByLabel('负责人'), { label: 'Cloud child' })
    await cloudTaskForm.getByRole('button', { name: '保存' }).click()
    let cloudTask = cloudTeam.locator('[data-agent-team-task]').filter({ hasText: CLOUD_TEAM_TASK_SUBJECT })
    await cloudTask.waitFor()
    assert.equal(await cloudTask.getAttribute('data-status'), 'pending')
    await cloudTask.getByRole('button', { name: `编辑任务“${CLOUD_TEAM_TASK_SUBJECT}”` }).click()
    cloudTaskForm = cloudTeam.locator('[data-agent-team-task-form="edit"]')
    await selectChoice(cloudTaskForm.getByLabel('状态'), 'in_progress')
    await cloudTaskForm.getByRole('button', { name: '保存' }).click()
    cloudTask = cloudTeam.locator('[data-agent-team-task]').filter({ hasText: CLOUD_TEAM_TASK_SUBJECT })
    await page.waitForFunction(subject => [...document.querySelectorAll('[data-agent-team-task]')]
      .some(task => task.textContent?.includes(subject) && task.getAttribute('data-status') === 'in_progress'), CLOUD_TEAM_TASK_SUBJECT)

    await cloudTeam.locator('#agent-team-mailbox-tab').click()
    await selectChoice(cloudTeam.getByLabel('收件人'), { label: 'Cloud child' })
    await cloudTeam.getByRole('textbox', { name: '消息', exact: true }).fill(CLOUD_TEAM_MESSAGE)
    await cloudTeam.getByRole('button', { name: '发送', exact: true }).click()
    const cloudOutgoing = cloudTeam.locator('[data-agent-team-mailbox] li').filter({ hasText: CLOUD_TEAM_MESSAGE })
    await cloudOutgoing.waitFor()
    assert.match(await cloudOutgoing.textContent(), /未读/)
    await cloudTeam.getByRole('button', { name: '关闭' }).click()
    await cloudTeam.waitFor({ state: 'detached' })

    let parentSidebarRow = page.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.parentSessionId}"]`)
    let childSidebarRow = page.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.childSessionId}"]`)
    await childSidebarRow.waitFor()
    assert.equal(await childSidebarRow.getAttribute('data-session-depth'), '1')
    assert.equal(await childSidebarRow.getAttribute('data-subagent-session'), '')
    assert.equal(await page.locator(`[data-sidebar-subagent-group="${cloudTeamSessions.parentSessionId}"] [data-session-id="${cloudTeamSessions.childSessionId}"]`).count(), 1)
    let childFold = parentSidebarRow.locator('[data-sidebar-session-children-toggle]')
    assert.equal(await childFold.getAttribute('aria-expanded'), 'true')
    await childFold.click()
    await childSidebarRow.waitFor({ state: 'detached' })
    await childFold.click()
    childSidebarRow = page.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.childSessionId}"]`)
    await childSidebarRow.waitFor()
    await childSidebarRow.locator('[data-sidebar-session-button]').click()
    await page.waitForFunction(childSessionId => localStorage.getItem('ternilo.current-session') === childSessionId, cloudTeamSessions.childSessionId)
    parentSidebarRow = page.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.parentSessionId}"]`)
    await parentSidebarRow.locator('[data-sidebar-session-button]').click()
    await page.waitForFunction(parentSessionId => localStorage.getItem('ternilo.current-session') === parentSessionId, cloudTeamSessions.parentSessionId)
    await prompt.waitFor()
    await prompt.fill('/')
    await page.getByRole('listbox', { name: '指令' }).getByRole('option', { name: /^\/write\b/ }).waitFor({ timeout: 30_000 })
    await prompt.fill('')

    const writeTools = page.locator('[data-tool-call-id]').filter({ hasText: '写入文件' })
    const persistentWriteIndex = await writeTools.count()
    await prompt.fill('/write cloud-persistent.txt survives-worker-restart')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    const persistentWrite = writeTools.nth(persistentWriteIndex)
    await persistentWrite.waitFor({ timeout: 30_000 })
    await persistentWrite.getByRole('button', { name: '展开 写入文件 结果' }).click()
    await persistentWrite.locator('[data-tool-view="diff"]').waitFor()
    assert.match(await persistentWrite.locator('[data-tool-view="diff"]').textContent(), /survives-worker-restart/)

    currentPhase = 'worker-restart-command-catalog'
    console.log(`Cloud phase: ${currentPhase}`)
    await stopProcess(worker)
    if (containerMode) await execute('docker', ['rm', '-f', workerName]).catch(() => {})
    worker = startTrackedWorker()
    await waitForOutput(worker, /Ternilo cloud worker .* ready/)

    let restartedCommandCatalog = null
    let restartedCatalogLast = null
    for (let attempt = 0; attempt < 12 && !restartedCommandCatalog; attempt += 1) {
      if (worker.child.exitCode !== null) {
        throw new Error(`restarted Worker exited before command catalog became ready: ${worker.diagnostics()}`)
      }
      restartedCatalogLast = await page.evaluate(async sessionId => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/commands`, {
          headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
        })
        return { status: response.status, body: await response.json().catch(() => null) }
      }, cloudTeamSessions.parentSessionId)
      if (restartedCatalogLast.status === 200
        && restartedCatalogLast.body?.commands?.some(command => command.name === 'read')) {
        restartedCommandCatalog = restartedCatalogLast.body
      } else {
        await new Promise(resolve => setTimeout(resolve, 50))
      }
    }
    if (!restartedCommandCatalog) {
      throw new Error(`restarted Worker command catalog is unavailable: last=${JSON.stringify(restartedCatalogLast)} control=${control.diagnostics()} worker=${worker.diagnostics()}`)
    }
    assert.equal(restartedCommandCatalog.session_id, cloudTeamSessions.parentSessionId)

    await prompt.fill('/')
    await page.getByRole('listbox', { name: '指令' }).getByRole('option', { name: /^\/read\b/ }).waitFor({ timeout: 30_000 })
    await prompt.fill('')
    const expandReadResult = async (callId, expected) => {
      const readTool = page.locator(`[data-tool-call-id="${callId}"]`)
      try {
        await readTool.waitFor({ timeout: 30_000 })
      } catch (error) {
        const readEvidence = await page.evaluate(async () => {
          const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
          const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
          const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
          const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {
            headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
          })
          return {
            sessionId,
            status: response.status,
            events: await response.json().catch(() => null),
            body: document.body.textContent?.slice(-4_000) ?? '',
          }
        })
        throw new Error(`Cloud read result ${callId} was not rendered; evidence=${JSON.stringify(readEvidence)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
      }
      await readTool.getByRole('button', { name: '展开 读取文件 结果' }).click()
      const result = readTool.locator('[data-tool-view="read"]')
      await result.waitFor()
      assert.match(await result.textContent(), expected)
    }
    const persistentReadResponse = queueResponseForInput('/read cloud-persistent.txt')
    await prompt.fill('/read cloud-persistent.txt')
    await page.getByRole('button', { name: '发送' }).click()
    const persistentReadSubmission = await (await persistentReadResponse).json()
    const persistentReadEvents = await assertSuccessfulCanonicalRun(
      persistentReadSubmission.run_id,
      cloudTeamSessions.parentSessionId,
    )
    const persistentReadCall = persistentReadEvents.find(event => event.type === 'tool_call_started' && event.call?.name === 'read_file')
    assert.ok(persistentReadCall?.call?.id)
    await expandReadResult(persistentReadCall.call.id, /survives-worker-restart/)

    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
    await cloudTeam.waitFor()
    cloudChild = cloudTeam.locator(`[data-agent-team-member="${cloudChildMemberId}"]`)
    await cloudChild.waitFor()
    await cloudTeam.locator('#agent-team-tasks-tab').click()
    cloudTask = cloudTeam.locator('[data-agent-team-task]').filter({ hasText: CLOUD_TEAM_TASK_SUBJECT })
    await cloudTask.waitFor()
    assert.equal(await cloudTask.getAttribute('data-status'), 'in_progress')
    await cloudTeam.locator('#agent-team-mailbox-tab').click()
    await cloudTeam.locator('[data-agent-team-mailbox] li').filter({ hasText: CLOUD_TEAM_MESSAGE }).waitFor()
    await cloudTeam.locator('#agent-team-roster-tab').click()

    const childFollowupRequest = nextModelRequest(observed => (
      latestUserContent(observed.body).includes(CLOUD_CHILD_FOLLOWUP_TASK)
    ), 'addressed Cloud child follow-up request')
    await cloudChild.getByRole('textbox', { name: '后续任务' }).fill(CLOUD_CHILD_FOLLOWUP_TASK)
    await cloudChild.getByRole('button', { name: '发送', exact: true }).click()
    await cloudTeam.waitFor({ state: 'detached' })
    await childFollowupRequest

    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
    await cloudTeam.waitFor()
    cloudChild = cloudTeam.locator(`[data-agent-team-member="${cloudChildMemberId}"]`)
    await page.waitForFunction(memberId => document.querySelector(`[data-agent-team-member="${CSS.escape(memberId)}"]`)?.getAttribute('data-status') === 'running', cloudChildMemberId)
    const childStopStartedAt = Date.now()
    await cloudChild.getByRole('button', { name: '停止', exact: true }).click()
    const childStopClosedAt = await withTimeout(model.childStopClosed, 'Cloud child Stop stream close')
    assert.equal(childStopClosedAt - childStopStartedAt < 5_000, true)
    await cloudTeam.waitFor({ state: 'detached' })

    const childStopEvidence = await page.evaluate(async ({ parentSessionId, childSessionId, followup }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      for (let attempt = 0; attempt < 100; attempt += 1) {
        const [parentResponse, childResponse] = await Promise.all([
          fetch(`/api/v1/sessions/${encodeURIComponent(parentSessionId)}/events`, { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(childSessionId)}/events`, { headers }),
        ])
        const [parentEvents, childEvents] = await Promise.all([parentResponse.json(), childResponse.json()])
        if (!parentResponse.ok || !childResponse.ok) throw new Error(`Cloud child evidence failed: ${JSON.stringify({ parentEvents, childEvents })}`)
        const childInput = childEvents.find(event => event.type === 'user_message' && event.content?.includes(followup))
        const cancelled = childInput
          ? childEvents.filter(event => event.run_id === childInput.run_id && event.type === 'turn_cancelled')
          : []
        if (cancelled.length) {
          return {
            childInput,
            cancelled,
            parentContainsFollowup: parentEvents.some(event => event.type === 'user_message' && event.content?.includes(followup)),
          }
        }
        await new Promise(resolve => setTimeout(resolve, 100))
      }
      throw new Error('addressed Cloud child Stop did not settle')
    }, {
      parentSessionId: cloudTeamSessions.parentSessionId,
      childSessionId: cloudTeamSessions.childSessionId,
      followup: CLOUD_CHILD_FOLLOWUP_TASK,
    })
    assert.equal(childStopEvidence.cancelled.length, 1)
    assert.equal(childStopEvidence.parentContainsFollowup, false)

    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
    await cloudTeam.waitFor()
    cloudChild = cloudTeam.locator(`[data-agent-team-member="${cloudChildMemberId}"]`)
    await cloudChild.getByRole('button', { name: '打开独立会话' }).click()
    await cloudTeam.waitFor({ state: 'detached' })
    await page.waitForFunction(childSessionId => localStorage.getItem('ternilo.current-session') === childSessionId, cloudTeamSessions.childSessionId)
    await page.locator('article[data-role="user"]').filter({ hasText: CLOUD_CHILD_FOLLOWUP_TASK }).waitFor({ timeout: 30_000 })
    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
    await cloudTeam.waitFor()
    await cloudTeam.locator('#agent-team-tasks-tab').click()
    await cloudTeam.locator('[data-agent-team-task]').filter({ hasText: CLOUD_TEAM_TASK_SUBJECT }).waitFor()
    await cloudTeam.locator('#agent-team-mailbox-tab').click()
    const cloudIncoming = cloudTeam.locator('[data-agent-team-mailbox] li[data-unread]').filter({ hasText: CLOUD_TEAM_MESSAGE })
    await cloudIncoming.waitFor()
    await cloudIncoming.getByRole('button', { name: '标为已读' }).click()
    await cloudIncoming.waitFor({ state: 'detached' })
    await cloudTeam.getByRole('button', { name: '关闭' }).click()
    await cloudTeam.waitFor({ state: 'detached' })

    await page.evaluate(parentSessionId => localStorage.setItem('ternilo.current-session', parentSessionId), cloudTeamSessions.parentSessionId)
    await reloadPage()
    await page.getByRole('textbox', { name: '输入任务' }).waitFor({ timeout: 30_000 })

    for (const viewport of [{ width: 390, height: 844 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      await page.getByRole('button', { name: '更多会话操作' }).click()
      await page.getByRole('menuitem', { name: 'Agent Team' }).click()
      cloudTeam = page.getByRole('dialog', { name: 'Agent Team' })
      await cloudTeam.waitFor()
      const geometry = await cloudTeam.evaluate(element => {
        const box = element.getBoundingClientRect()
        return { left: box.left, top: box.top, right: box.right, bottom: box.bottom, width: innerWidth, height: innerHeight }
      })
      assert.equal(geometry.left >= 0 && geometry.top >= 0 && geometry.right <= geometry.width && geometry.bottom <= geometry.height, true, JSON.stringify(geometry))
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
      const closeButton = cloudTeam.getByRole('button', { name: '关闭' })
      await page.waitForFunction(() => {
        const button = document.querySelector('[data-agent-team-panel] > [data-slot="dialog-close"]')
        if (!(button instanceof HTMLElement)) return false
        const box = button.getBoundingClientRect()
        return box.width >= 40 && box.height >= 40
      })
      const closeBox = await closeButton.boundingBox()
      assert.ok(closeBox && closeBox.width >= 40 && closeBox.height >= 40, JSON.stringify({ viewport, closeBox }))
      await closeButton.click()
      await cloudTeam.waitFor({ state: 'detached' })
    }
    await page.setViewportSize({ width: 1440, height: 900 })

    const tenantIsolation = await page.evaluate(async platformSelection => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const request = async (url, options = {}, tenant = null) => {
        const response = await fetch(url, {
          ...options,
          headers: {
            authorization: `Bearer ${token}`,
            ...(tenant ? { 'x-ternilo-tenant': tenant } : {}),
            ...(options.body ? { 'content-type': 'application/json' } : {}),
          },
        })
        const responseBody = await response.json().catch(() => null)
        if (!response.ok) throw new Error(`${options.method ?? 'GET'} ${url}: ${response.status} ${JSON.stringify(responseBody)}`)
        return responseBody
      }
      const tenant = (await request('/api/v1/tenants', {
        method: 'POST', body: JSON.stringify({ slug: 'cloud-isolation', display_name: 'Cloud Isolation' }),
      })).tenant.tenant_id
      const providers = await request('/api/v1/providers', {}, tenant)
      const project = (await request('/api/v1/projects', {
        method: 'POST', body: JSON.stringify({ name: 'Isolated Project' }),
      }, tenant)).project.project_id
      const workspace = (await request('/api/v1/workspaces', {
        method: 'POST', body: JSON.stringify({ project_id: project, name: 'Isolated Workspace', placement: 'cloud' }),
      }, tenant)).workspace.workspace_id
      const session = await request('/api/v1/sessions', {
        method: 'POST', body: JSON.stringify({ workspace_id: workspace, session_id: 'tenant-isolation-probe', permissions: 'workspace_write', model: platformSelection }),
      }, tenant)
      const sessionId = session.identity.session_id
      const runEvents = async runId => {
        for (let attempt = 0; attempt < 300; attempt++) {
          const events = await request(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {}, tenant)
          const current = events.filter(event => event.run_id === runId)
          if (current.some(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))) return current
          await new Promise(resolve => setTimeout(resolve, 100))
        }
        throw new Error(`cloud run ${runId} did not finish`)
      }
      const initialization = await request(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        method: 'POST', body: JSON.stringify({ input: 'initialize isolated workspace' }),
      }, tenant)
      const initialized = await runEvents(initialization.run.run_id)
      if (!initialized.some(event => event.type === 'turn_finished')) throw new Error(`workspace initialization failed: ${JSON.stringify(initialized)}`)

      const read = await request(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        method: 'POST', body: JSON.stringify({ input: '/read cloud-persistent.txt' }),
      }, tenant)
      return { providers, events: await runEvents(read.run.run_id) }
    }, platformSelection)
    assert.equal(tenantIsolation.providers.some(provider => provider.id === 'byok'), false)
    assert.match(JSON.stringify(tenantIsolation.events), /read_file/)
    assert.equal(JSON.stringify(tenantIsolation.events).includes('survives-worker-restart'), false)

    const activeTenantId = await page.evaluate(() => localStorage.getItem('ternilo.current-tenant') ?? '')
    const activeSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session') ?? '')
    assert.notEqual(activeTenantId, '')
    assert.notEqual(activeSessionId, '')
    const tenantDigest = createHash('sha256').update(activeTenantId).digest('hex')
    const currentUsage = containerMode
      ? (await execute('docker', [
          'run', '--rm', '--user', '0:0', '--entrypoint', '/usr/bin/find',
          '--volume', `${workspaceRoot}:/workspaces:ro`, workerImage,
          `/workspaces/${tenantDigest}`, '-type', 'f', '-printf', '%s\n',
        ])).stdout.trim().split('\n').filter(Boolean).reduce((sum, value) => sum + Number(value), 0)
      : (await directoryUsage(path.join(workspaceRoot, tenantDigest))).bytes
    const unconstrainedPolicy = JSON.parse(await readFile(policyPath, 'utf8'))
    const constrainedPolicy = structuredClone(unconstrainedPolicy)
    constrainedPolicy.max_tenant_workspace_bytes = currentUsage + 4_096
    currentPhase = 'workspace-quota-worker-restart'
    console.log(`Cloud phase: ${currentPhase}`)
    await restartWithPolicy(constrainedPolicy)

    const oversizedContent = 'q'.repeat(32_768)
    const quotaFailureCards = page.locator('[data-chat-event="turn_failed"]')
    const quotaFailureCount = await quotaFailureCards.count()
    const quotaRun = await page.evaluate(async ({ tenant, sessionId, input }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        method: 'POST',
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant, 'content-type': 'application/json' },
        body: JSON.stringify({ input }),
      })
      const responseBody = await response.json()
      if (!response.ok) throw new Error(`quota run admission failed: ${response.status} ${JSON.stringify(responseBody)}`)
      return responseBody
    }, { tenant: activeTenantId, sessionId: activeSessionId, input: `/write quota-overflow.txt ${oversizedContent}` })
    const quotaOutcome = await page.evaluate(async ({ tenant, sessionId, runId }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      for (let attempt = 0; attempt < 300; attempt++) {
        const [eventsResponse, runResponse] = await Promise.all([
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
          fetch(`/api/v1/tenants/${encodeURIComponent(tenant)}/runs/${encodeURIComponent(runId)}`, { headers }),
        ])
        const events = await eventsResponse.json()
        const run = (await runResponse.json()).run
        const current = events.filter(event => event.run_id === runId)
        const terminal = current.find(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))
        if (['succeeded', 'failed', 'cancelled', 'indeterminate'].includes(run.state) && terminal) {
          return { events: current, run }
        }
        await new Promise(resolve => setTimeout(resolve, 100))
      }
      throw new Error(`quota run ${runId} did not finish`)
    }, { tenant: activeTenantId, sessionId: activeSessionId, runId: quotaRun.run.run_id })
    assert.equal(quotaOutcome.run.state, 'failed')
    assert.match(quotaOutcome.run.error.message, /workspace byte quota exceeded/)
    assert.equal(quotaOutcome.events.some(event => event.type === 'turn_failed' && /workspace byte quota exceeded/.test(event.message)), true)
    await reloadPage()
    await page.locator(`[data-sidebar-session-row][data-session-id="${activeSessionId}"]`).waitFor({ timeout: 30_000 })
    const quotaFailureCard = quotaFailureCards.nth(quotaFailureCount)
    await quotaFailureCard.waitFor({ timeout: 30_000 })
    assert.match(await quotaFailureCard.textContent(), /workspace byte quota exceeded/)
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })

    const escapedTenant = activeTenantId.replaceAll("'", "''")
    const escapedRun = quotaRun.run.run_id.replaceAll("'", "''")
    const reservation = await execute('docker', [
      'exec', postgres.name, 'psql', '-U', 'postgres', '-d', 'ternilo_cloud_browser_test', '-Atc',
      `SELECT reservation.state FROM control_quota_reservations AS reservation JOIN cloud_runs AS run ON run.tenant_id = reservation.tenant_id AND run.quota_reservation_id = reservation.reservation_id WHERE run.tenant_id = '${escapedTenant}' AND run.run_id = '${escapedRun}'`,
    ])
    assert.equal(reservation.stdout.trim(), 'released')

    currentPhase = 'restore-worker-policy'
    console.log(`Cloud phase: ${currentPhase}`)
    await restartWithPolicy(unconstrainedPolicy)

    const authoritativeStats = await page.evaluate(async ({ tenant, sessionId }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/stats`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const value = await response.json()
      if (!response.ok) throw new Error(`Cloud stats evidence failed: ${response.status} ${JSON.stringify(value)}`)
      return value
    }, { tenant: activeTenantId, sessionId: activeSessionId })
    assert.equal(authoritativeStats.exact_input_tokens, CLOUD_PARENT_STATS_AFTER_TEAM.input)
    assert.equal(authoritativeStats.exact_output_tokens, CLOUD_PARENT_STATS_AFTER_TEAM.output)
    assert.equal(authoritativeStats.cached_input_tokens, CLOUD_PARENT_STATS_AFTER_TEAM.cached)
    const expectedUsageLabel = `输入 ${CLOUD_PARENT_STATS_AFTER_TEAM.input} tok · 输出 ${CLOUD_PARENT_STATS_AFTER_TEAM.output} tok`
    await page.waitForFunction(expected => (
      document.querySelector('.session-stats-line')?.getAttribute('aria-label')?.includes(expected)
    ), expectedUsageLabel)
    const statsLabel = await page.locator('.session-stats-line').getAttribute('aria-label')
    assert.equal(statsLabel.includes(expectedUsageLabel), true)
    assert.equal(statsLabel.includes(`缓存命中 ${CLOUD_PARENT_STATS_AFTER_TEAM.cachePercent}%`), true)

    const modelRequest = await withTimeout(model.requestSeen, 'first Cloud Provider request')
    assert.equal(modelRequest.authorization, 'Bearer cloud-browser-byok-secret')
    assert.equal(modelRequest.body.model, 'byok-model')
    assert.equal(modelRequest.body.max_tokens, 2048)
    assert.equal(modelRequest.body.reasoning_effort, 'ultra')
    assert.equal(modelRequest.body.stream_options.include_usage, true)
    assert.match(JSON.stringify(modelRequest.body.messages), /data:image\/webp;base64,/)

    currentPhase = 'typed-cloud-skill'
    console.log(`Cloud phase: ${currentPhase}`)
    const skillRequestReady = nextModelRequest(observed => {
      const latestUser = [...(observed.body.messages ?? [])]
        .reverse()
        .find(message => message?.role === 'user')?.content
      const content = typeof latestUser === 'string' ? latestUser : JSON.stringify(latestUser ?? '')
      return content.includes(CLOUD_SKILL_TASK)
        && content.includes('<skill_content name="cloud-check">')
        && content.includes('CLOUD_SKILL_SENTINEL')
        && content.includes('<user_request>')
    }, 'typed Cloud Skill model request')
    const assistantCountBeforeSkill = await page.locator('article[data-role="assistant"]').count()
    const skillRunResponse = queueResponseForInput(CLOUD_SKILL_TASK)
    await prompt.fill(`/skill cloud-check ${CLOUD_SKILL_TASK}`)
    await page.getByRole('button', { name: '发送' }).click()
    let skillSubmission
    try {
      skillSubmission = await (await skillRunResponse).json()
    } catch (error) {
      const diagnostic = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') || ''
        const tenant = localStorage.getItem('ternilo.current-tenant') || ''
        const session = localStorage.getItem('ternilo.current-session') || ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const read = async suffix => {
          try {
            const response = await fetch(`/api/v1/sessions/${encodeURIComponent(session)}/${suffix}`, { headers, signal: AbortSignal.timeout(3000) })
            return { status: response.status, body: await response.json() }
          } catch (failure) { return { error: String(failure) } }
        }
        return { session, tenant, commandCatalog: await read('commands'), queue: await read('queue') }
      })
      console.error(`Typed Skill submission diagnostic: ${diagnosticResponseBody(JSON.stringify(diagnostic))}`)
      throw error
    }
    const typedSkillRequest = await skillRequestReady
    assert.equal(typedSkillRequest.authorization, 'Bearer cloud-browser-byok-secret')
    await page.waitForFunction(expected => (
      document.querySelectorAll('article[data-role="assistant"]').length > expected
      && !document.querySelector('[data-composer-card][data-busy]')
    ), assistantCountBeforeSkill)
    const skillUserMessage = page.locator('article[data-role="user"]').last()
    assert.match(await skillUserMessage.textContent(), /\/skill cloud-check/)
    assert.match(await skillUserMessage.textContent(), new RegExp(CLOUD_SKILL_TASK))
    assert.equal((await skillUserMessage.textContent()).includes('CLOUD_SKILL_SENTINEL'), false)
    const skillEvidence = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const events = await response.json()
      if (!response.ok) throw new Error(`typed Skill evidence failed: ${response.status} ${JSON.stringify(events)}`)
      return events.filter(event => event.type === 'user_message'
        && event.display_content?.includes('/skill cloud-check')).at(-1)
    })
    assert.equal(skillEvidence.source.kind, 'submission')
    assert.equal(skillEvidence.source.skill_name, 'cloud-check')
    assert.match(skillEvidence.display_content, new RegExp(CLOUD_SKILL_TASK))
    assert.match(skillEvidence.content, /<skill_content name="cloud-check">/)
    await assertSuccessfulCanonicalRun(skillSubmission.run_id)

    currentPhase = 'cloud-plan-review'
    console.log(`Cloud phase: ${currentPhase}`)
    await prompt.fill('/plan')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByRole('button', { name: '切换到执行模式' }).waitFor({ timeout: 30_000 })
    const planRequestReady = nextModelRequest(observed => (
      latestUserContent(observed.body).includes(CLOUD_PLAN_TASK)
      && observed.body.tools?.some(tool => tool?.function?.name === 'exit_plan_mode')
    ), 'Cloud Plan Review model request')
    await prompt.fill(CLOUD_PLAN_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    await planRequestReady
    const planReview = page.locator('[data-plan-review]')
    try {
      await planReview.waitFor({ timeout: 30_000 })
    } catch (error) {
      const planEvidence = await page.evaluate(async () => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [eventsResponse, stateResponse] = await Promise.all([
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
          fetch('/api/v1/state', { headers }),
        ])
        return {
          sessionId,
          eventsStatus: eventsResponse.status,
          events: await eventsResponse.json().catch(() => null),
          stateStatus: stateResponse.status,
          state: await stateResponse.json().catch(() => null),
          body: document.body.textContent?.slice(-4_000) ?? '',
        }
      })
      throw new Error(`Cloud Plan review did not become visible; evidence=${JSON.stringify(planEvidence)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    await page.setViewportSize({ width: 390, height: 430 })
    const planGeometry = await planReview.evaluate(element => {
      const card = element.getBoundingClientRect()
      const scroller = element.querySelector('[data-plan-review-scroll]')
      return {
        viewportWidth: document.documentElement.clientWidth,
        documentWidth: document.documentElement.scrollWidth,
        card: { left: card.left, right: card.right, top: card.top, bottom: card.bottom },
        scrolls: scroller.scrollHeight > scroller.clientHeight,
        overflow: getComputedStyle(scroller).overflowY,
        buttons: [...element.querySelectorAll('[data-plan-review-action]')].map(button => ({
          label: button.textContent.trim(),
          height: button.getBoundingClientRect().height,
        })),
      }
    })
    assert.equal(planGeometry.documentWidth, planGeometry.viewportWidth)
    assert.equal(planGeometry.card.left >= 0 && planGeometry.card.right <= 390, true)
    assert.equal(planGeometry.card.top >= 0 && planGeometry.card.bottom <= 431, true)
    assert.equal(planGeometry.scrolls, true)
    assert.equal(planGeometry.overflow, 'auto')
    assert.deepEqual(planGeometry.buttons.filter(button => button.height < 40), [])
    const approvalRequests = []
    const recordApprovalRequest = request => approvalRequests.push(`${request.method()} ${new URL(request.url()).pathname}`)
    page.on('request', recordApprovalRequest)
    const planResumeReady = nextModelRequest(observed => (
      observed.body.messages?.at(-1)?.role === 'tool'
      && observed.body.messages.at(-1).tool_call_id === 'cloud-plan-call'
    ), 'approved Cloud Plan resume request')
    await planReview.getByRole('button', { name: '批准计划' }).click()
    await planResumeReady
    await planReview.waitFor({ state: 'detached', timeout: 30_000 })
    await page.getByText('cloud plan approved', { exact: true }).waitFor({ timeout: 30_000 })
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.getByRole('button', { name: '切换到计划模式' }).waitFor({ timeout: 30_000 })
    page.off('request', recordApprovalRequest)
    assert.equal(approvalRequests.some(value => value.startsWith('PATCH ') && value.includes('/sessions/')), false)
    const approvedPlanState = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const current = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch('/api/v1/state', {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const state = await response.json()
      if (!response.ok) throw new Error(`approved Plan state failed: ${response.status} ${JSON.stringify(state)}`)
      return state.sessions.find(session => session.identity.session_id === current)?.mode
    })
    assert.equal(approvedPlanState, 'execute')
    currentPhase = 'cloud-stop-and-send'
    console.log(`Cloud phase: ${currentPhase}`)
    const steerStartResponse = page.waitForResponse(response => {
      if (response.request().method() !== 'POST') return false
      if (!/\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname)) return false
      return response.request().postDataJSON()?.content?.input === CLOUD_STEER_TASK
    })
    const steerStartRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_STEER_TASK)
    ), 'controlled Cloud steering start')
    await prompt.fill(CLOUD_STEER_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    const steerStartSubmission = await (await steerStartResponse).json()
    await steerStartRequest
    await page.getByRole('button', { name: '停止运行', exact: true }).waitFor({ timeout: 30_000 })
    const steeringResponse = page.waitForResponse(response => {
      if (response.request().method() !== 'POST') return false
      if (!/\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname)) return false
      const body = response.request().postDataJSON()
      return body?.delivery === 'steer' && body?.content?.input === CLOUD_STEER_MESSAGE
    })
    await prompt.fill(CLOUD_STEER_MESSAGE)
    const steeredModelRequestReady = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_STEER_MESSAGE)
    ), 'new Provider turn after stopping the previous run')
    await prompt.press('Control+Enter')
    const steeringSubmission = await (await steeringResponse).json()
    if (!['queued', 'running'].includes(steeringSubmission.placement)) {
      const steeringDiagnostic = await page.evaluate(async ({ tenant, sessionId }) => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [stateResponse, eventsResponse, queueResponse, runsResponse] = await Promise.all([
          fetch('/api/v1/state', { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/queue`, { headers }),
          fetch(`/api/v1/tenants/${encodeURIComponent(tenant)}/runs?limit=100`, { headers }),
        ])
        return {
          state: await stateResponse.json().catch(() => null),
          events: await eventsResponse.json().catch(() => null),
          queue: await queueResponse.json().catch(() => null),
          runs: await runsResponse.json().catch(() => null),
        }
      }, { tenant: activeTenantId, sessionId: activeSessionId })
      throw new Error(`strict Cloud steering was not accepted: ${JSON.stringify({ steeringSubmission, steeringDiagnostic, control: control.diagnostics(), worker: worker.diagnostics() })}`)
    }
    const steeredModelRequest = await steeredModelRequestReady
    model.releaseSteer()
    assert.match(JSON.stringify(steeredModelRequest.body.messages), new RegExp(CLOUD_STEER_MESSAGE))
    await page.getByText('cloud steering applied', { exact: true }).waitFor({ timeout: 30_000 })
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    const steeringEvidence = await page.evaluate(async candidateRunId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      const [eventsResponse, runsResponse] = await Promise.all([
        fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
        fetch(`/api/v1/tenants/${encodeURIComponent(tenant)}/runs?limit=100`, { headers }),
      ])
      const events = await eventsResponse.json()
      const runs = await runsResponse.json()
      if (!eventsResponse.ok || !runsResponse.ok) throw new Error(`steering evidence failed: ${JSON.stringify({ events, runs })}`)
      const injected = events.filter(event => event.type === 'user_message'
        && event.source?.kind === 'submission'
        && event.source?.delivery === 'queue'
        && event.content.includes('cloud steering injection'))
      return {
        injected,
        candidateStillExists: runs.runs.some(run => run.run_id === candidateRunId),
        cancelledRuns: events.filter(event => event.type === 'turn_cancelled').map(event => event.run_id),
      }
    }, steeringSubmission.run_id)
    assert.equal(steeringEvidence.injected.length, 1)
    assert.equal(steeringEvidence.injected[0].run_id, steeringSubmission.run_id)
    assert.notEqual(steeringEvidence.injected[0].run_id, steerStartSubmission.run_id)
    assert.equal(steeringEvidence.candidateStillExists, true)
    assert.ok(steeringEvidence.cancelledRuns.includes(steerStartSubmission.run_id))

    currentPhase = 'cloud-fast-stop'
    console.log(`Cloud phase: ${currentPhase}`)
    const stopRunResponse = page.waitForResponse(response => {
      if (response.request().method() !== 'POST') return false
      if (!/\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname)) return false
      return response.request().postDataJSON()?.content?.input === CLOUD_STOP_TASK
    })
    const stopModelRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_STOP_TASK)
    ), 'controlled Cloud Stop stream')
    await prompt.fill(CLOUD_STOP_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    const stopSubmission = await (await stopRunResponse).json()
    await stopModelRequest
    await page.getByText('cloud stop stream remains active', { exact: true }).waitFor({ timeout: 30_000 })
    const stopStartedAt = Date.now()
    await page.getByRole('button', { name: '停止运行' }).click()
    const stopClosedAt = await withTimeout(model.stopClosed, 'Cloud Stop stream close')
    assert.equal(stopClosedAt - stopStartedAt < 5_000, true)
    try {
      await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    } catch (error) {
      const stopDiagnostic = await page.evaluate(async runId => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [stateResponse, eventsResponse, queueResponse, runsResponse] = await Promise.all([
          fetch('/api/v1/state', { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/queue`, { headers }),
          fetch(`/api/v1/tenants/${encodeURIComponent(tenant)}/runs?limit=100`, { headers }),
        ])
        return {
          runId,
          tenant,
          sessionId,
          state: { status: stateResponse.status, body: await stateResponse.json().catch(() => null) },
          events: { status: eventsResponse.status, body: await eventsResponse.json().catch(() => null) },
          queue: { status: queueResponse.status, body: await queueResponse.json().catch(() => null) },
          runs: { status: runsResponse.status, body: await runsResponse.json().catch(() => null) },
          buttons: [...document.querySelectorAll('button')].map(button => button.textContent?.trim()).filter(Boolean),
        }
      }, stopSubmission.run_id)
      throw new Error(`Cloud Stop did not restore the composer: diagnostic=${JSON.stringify(stopDiagnostic)} body=${JSON.stringify((await page.locator('body').textContent()).slice(-3_000))} failed_requests=${JSON.stringify(failedRequests)} failed_responses=${JSON.stringify(failedResponses)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: error })
    }
    const stopEvidence = await page.evaluate(async runId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const events = await response.json()
      if (!response.ok) throw new Error(`Stop evidence failed: ${response.status} ${JSON.stringify(events)}`)
      return events.filter(event => event.run_id === runId && event.type === 'turn_cancelled')
    }, stopSubmission.run_id)
    assert.equal(stopEvidence.length, 1)
    const afterStopRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_AFTER_STOP_TASK)
    ), 'successful Cloud turn after Stop')
    const completedBrokerAnswers = page.locator('article[data-role="assistant"]').filter({ hasText: 'cloud broker ready' })
    const afterStopAnswerIndex = await completedBrokerAnswers.count()
    const afterStopRunResponse = queueResponseForInput(CLOUD_AFTER_STOP_TASK)
    await prompt.fill(CLOUD_AFTER_STOP_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    const afterStopSubmission = await (await afterStopRunResponse).json()
    await afterStopRequest
    await completedBrokerAnswers.nth(afterStopAnswerIndex).waitFor({ timeout: 30_000 })
    await page.locator('button[aria-label="发送"]').waitFor({ timeout: 30_000 })
    await assertSuccessfulCanonicalRun(afterStopSubmission.run_id)

    await prompt.fill('/')
    await page.getByRole('listbox', { name: '指令' }).getByRole('option', { name: /^\/read\b/ }).waitFor({ timeout: 30_000 })
    const searchReadResponse = queueResponseForInput('/read cloud-search-tool.txt')
    await prompt.fill('/read cloud-search-tool.txt')
    await page.locator('button[aria-label="发送"]:not(:disabled)').waitFor({ timeout: 30_000 })
    await page.getByRole('button', { name: '发送' }).click()
    const searchReadSubmission = await (await searchReadResponse).json()
    const searchReadEvents = await assertSuccessfulCanonicalRun(searchReadSubmission.run_id)
    const searchReadCall = searchReadEvents.find(event => event.type === 'tool_call_started' && event.call?.name === 'read_file')
    assert.ok(searchReadCall?.call?.id)
    await expandReadResult(searchReadCall.call.id, new RegExp(CLOUD_TOOL_SENTINEL))
    const searchModelRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_SEARCH_TASK)
    ), 'Cloud canonical content-search fixture turn')
    const searchRunResponse = queueResponseForInput(CLOUD_SEARCH_TASK)
    await prompt.fill(CLOUD_SEARCH_TASK)
    const searchCompletionDeadline = Date.now() + 30_000
    await page.getByRole('button', { name: '发送' }).click()
    const searchSubmission = await (await searchRunResponse).json()
    const searchCanonicalCompletion = assertSuccessfulCanonicalRun(searchSubmission.run_id)
    const searchProjectionCompletion = (async () => {
      await Promise.all([
        searchModelRequest,
        page.getByText(CLOUD_ASSISTANT_SENTINEL, { exact: true }).waitFor({
          timeout: Math.max(1, searchCompletionDeadline - Date.now()),
        }),
      ])
      await page.getByRole('button', { name: '发送' }).waitFor({
        timeout: Math.max(1, searchCompletionDeadline - Date.now()),
      })
    })()
    const searchCompletion = Promise.allSettled([
      searchCanonicalCompletion,
      searchProjectionCompletion,
    ])
    const [searchCanonicalResult, searchComposerResult] = await searchCompletion
    if (searchCanonicalResult.status === 'rejected') {
      throw new Error(`Cloud search canonical run did not finish: run_id=${searchSubmission.run_id} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: searchCanonicalResult.reason })
    }
    if (searchComposerResult.status === 'rejected') {
      const searchDiagnostic = await page.evaluate(async runId => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
        const [stateResponse, eventsResponse, queueResponse, runsResponse] = await Promise.all([
          fetch('/api/v1/state', { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, { headers }),
          fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/queue`, { headers }),
          fetch(`/api/v1/tenants/${encodeURIComponent(tenant)}/runs?limit=100`, { headers }),
        ])
        const [state, events, queue, runs] = await Promise.all([
          stateResponse.json().catch(() => null),
          eventsResponse.json().catch(() => null),
          queueResponse.json().catch(() => null),
          runsResponse.json().catch(() => null),
        ])
        const runEvents = Array.isArray(events) ? events.filter(event => event.run_id === runId) : []
        return {
          runId,
          tenant,
          sessionId,
          state: { status: stateResponse.status, body: state },
          events: { status: eventsResponse.status, body: events },
          queue: { status: queueResponse.status, body: queue },
          runs: { status: runsResponse.status, body: runs },
          latestRunEvent: runEvents.at(-1) ?? null,
          liveState: document.querySelector('[data-sidebar-connection]')?.getAttribute('data-live-state') ?? null,
          physicalLiveSockets: window.__terniloCloudE2eLiveSockets.map(socket => ({
            url: socket.url,
            readyState: socket.readyState,
          })),
          buttons: [...document.querySelectorAll('button')].map(button => button.textContent?.trim()).filter(Boolean),
        }
      }, searchSubmission.run_id)
      throw new Error(`Cloud search completed canonically but did not restore the composer: diagnostic=${JSON.stringify(searchDiagnostic)} body=${JSON.stringify((await page.locator('body').textContent()).slice(-3_000))} failed_requests=${JSON.stringify(failedRequests)} failed_responses=${JSON.stringify(failedResponses)} control=${control.diagnostics()} worker=${worker.diagnostics()}`, { cause: searchComposerResult.reason })
    }
    const assertCloudSearch = async (query, category) => {
      const evidence = await page.evaluate(async ({ query, category }) => {
        const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
        const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
        const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
        const response = await fetch(`/api/v1/session-search?${new URLSearchParams({ query, limit: '100' })}`, {
          headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
        })
        const hits = await response.json()
        if (!response.ok) throw new Error(`Cloud search failed: ${response.status} ${JSON.stringify(hits)}`)
        return {
          sessionId,
          hit: hits.find(hit => hit.session_id === sessionId && hit.category === category && hit.excerpt.includes(query)),
        }
      }, { query, category })
      assert.ok(evidence.hit, `${category} search hit must exist for ${query}`)
      assert.equal(Number.isInteger(evidence.hit.event_seq), true)
      assert.equal(typeof evidence.hit.run_id, 'string')
      assert.notEqual(evidence.hit.run_id, '')
      await page.getByRole('button', { name: '搜索会话' }).click()
      const search = page.getByRole('textbox', { name: '搜索会话' })
      await search.fill(query)
      const result = page.getByRole('tree', { name: '搜索结果' })
        .locator('[data-sidebar-session-row]').filter({ hasText: query }).first()
      await result.waitFor({ timeout: 30_000 })
      assert.match(await result.locator('[data-sidebar-session-snippet]').textContent(), new RegExp(query))
      await result.click()
      assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-session')), evidence.sessionId)
      await page.getByRole('button', { name: '关闭搜索' }).click()
    }
    await assertCloudSearch(CLOUD_REASONING_SENTINEL, 'assistant')
    await assertCloudSearch(CLOUD_ASSISTANT_SENTINEL, 'assistant')
    await assertCloudSearch(CLOUD_TOOL_SENTINEL, 'tool')

    const offlineIdentity = await page.evaluate(() => ({
      token: sessionStorage.getItem('ternilo.oidc.access') ?? '',
      tenant: localStorage.getItem('ternilo.current-tenant') ?? '',
      sessionId: localStorage.getItem('ternilo.current-session') ?? '',
    }))
    assert.notEqual(offlineIdentity.token, '')
    assert.notEqual(offlineIdentity.tenant, '')
    assert.notEqual(offlineIdentity.sessionId, '')
    const offlineInput = `/write ${CLOUD_OFFLINE_FILE} ${CLOUD_OFFLINE_SENTINEL}`
    const offlineRunId = `run_offline_${randomBytes(12).toString('hex')}`
    const offlineUserMessage = page.locator('article[data-role="user"]').filter({ hasText: CLOUD_OFFLINE_SENTINEL })
    const offlineWriteIndex = await writeTools.count()
    assert.equal(await offlineUserMessage.count(), 0)
    const offlineFailedRequestStart = failedRequests.length
    const offlineConsoleErrorStart = consoleErrors.length
    await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
    const currentLiveSocket = [...liveSockets].reverse().find(record => (
      record.closedAt === null
      && record.sent.some(({ frame }) => frame.type === 'subscribe'
        && frame.session_id === offlineIdentity.sessionId)
    ))
    assert.ok(currentLiveSocket, JSON.stringify(liveSockets))
    const lastDeliveredBatch = [...currentLiveSocket.received].reverse().find(({ frame }) => (
      frame.type === 'event_batch' && frame.session_id === offlineIdentity.sessionId
    ))?.frame
    assert.ok(lastDeliveredBatch, JSON.stringify(currentLiveSocket.received))
    const expectedResumeAfterSeq = lastDeliveredBatch.next_seq === 0
      ? undefined
      : lastDeliveredBatch.next_seq - 1
    const offlineFrameBoundary = currentLiveSocket.received.length
    const reconnectSocketBoundary = liveSockets.length
    const backgroundRequestBoundary = backgroundLiveRequests.length
    const abortOfflineApi = route => route.abort('internetdisconnected')
    await page.route('**/api/v1/**', abortOfflineApi)
    currentPhase = 'offline-canonical-recovery'
    console.log(`Cloud phase: ${currentPhase}`)
    // Playwright's network emulation can leave an established WebSocket
    // transport-active. Exercise the production offline handler while the
    // network is still reachable so the native close handshake completes,
    // then isolate new connection attempts.
    await page.evaluate(() => window.dispatchEvent(new Event('offline')))
    let offlineSocketStates = []
    for (let attempt = 0; attempt < 100; attempt++) {
      offlineSocketStates = await page.evaluate(() => window.__terniloCloudE2eLiveSockets
        .map(socket => socket.readyState))
      if (offlineSocketStates.every(state => state === 3)) break
      await page.waitForTimeout(50)
    }
    const offlineCloseEvidence = await page.evaluate(() => ({
      liveState: document.querySelector('[data-sidebar-connection]')?.dataset.liveState ?? null,
      socketStates: window.__terniloCloudE2eLiveSockets.map(socket => socket.readyState),
      closeCalls: window.__terniloCloudE2eCloseCalls,
      networkEvents: window.__terniloCloudE2eNetworkEvents,
    }))
    assert.equal(
      offlineSocketStates.every(state => state === 3),
      true,
      `offline close handshake must reach CLOSED(3): ${JSON.stringify(offlineCloseEvidence)}`,
    )
    await page.context().setOffline(true)
    await page.locator('[data-sidebar-connection][data-live-state="reconnecting"]').waitFor({ timeout: 5_000 })
    await page.waitForFunction(() => navigator.onLine === false)
    await page.waitForTimeout(1_500)
    assert.deepEqual(
      backgroundLiveRequests.slice(backgroundRequestBoundary),
      [],
      'offline live recovery must not fall back to periodic REST metadata/history requests',
    )
    assert.ok(
      liveSockets.length - reconnectSocketBoundary <= 4,
      `live reconnect must back off while offline; observed ${liveSockets.length - reconnectSocketBoundary} attempts`,
    )

    const offlineSubmissionResponse = await fetch(
      `${origin}/api/v1/sessions/${encodeURIComponent(offlineIdentity.sessionId)}/queue`,
      {
        method: 'POST',
        headers: {
          authorization: `Bearer ${offlineIdentity.token}`,
          'x-ternilo-tenant': offlineIdentity.tenant,
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          delivery: 'queue',
          run_id: offlineRunId,
          content: { kind: 'prompt', input: offlineInput },
          references: [],
          attachments: [],
        }),
      },
    )
    const offlineSubmission = await offlineSubmissionResponse.json()
    assert.equal(offlineSubmissionResponse.status, 201, JSON.stringify(offlineSubmission))
    assert.equal(offlineSubmission.run_id, offlineRunId)

    let offlineRunEvents = []
    for (let attempt = 0; attempt < 300; attempt++) {
      const response = await fetch(
        `${origin}/api/v1/sessions/${encodeURIComponent(offlineIdentity.sessionId)}/events`,
        { headers: { authorization: `Bearer ${offlineIdentity.token}`, 'x-ternilo-tenant': offlineIdentity.tenant } },
      )
      const events = await response.json()
      assert.equal(response.status, 200, JSON.stringify(events))
      offlineRunEvents = events.filter(event => event.run_id === offlineRunId)
      if (offlineRunEvents.some(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type))) break
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    const matchingOfflineEvents = predicate => offlineRunEvents.filter(predicate)
    assert.equal(matchingOfflineEvents(event => event.type === 'user_message'
      && event.content?.includes(CLOUD_OFFLINE_SENTINEL)).length, 1)
    assert.equal(matchingOfflineEvents(event => event.type === 'tool_call_started'
      && event.call?.name === 'write_file').length, 1)
    assert.equal(matchingOfflineEvents(event => event.type === 'tool_call_finished'
      && event.name === 'write_file').length, 1)
    assert.equal(matchingOfflineEvents(event => event.type === 'turn_finished').length, 1)
    assert.equal(matchingOfflineEvents(event => event.type === 'turn_failed' || event.type === 'turn_cancelled').length, 0)
    assert.equal(await offlineUserMessage.count(), 0)
    assert.equal(await writeTools.count(), offlineWriteIndex)
    assert.equal(
      currentLiveSocket.received.length,
      offlineFrameBoundary,
      'the disconnected socket must not receive durable events written while the browser is offline',
    )

    await Promise.all([
      page.context().setOffline(false),
      page.unroute('**/api/v1/**', abortOfflineApi),
    ])
    await page.waitForFunction(() => navigator.onLine === true)
    await page.evaluate(() => window.dispatchEvent(new Event('online')))
    await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
    let resumedLiveSocket
    for (let attempt = 0; attempt < 300; attempt++) {
      resumedLiveSocket = liveSockets.slice(reconnectSocketBoundary).find(record => (
        record.received.some(({ frame }) => frame.type === 'ready')
        && record.sent.some(({ frame }) => frame.type === 'subscribe'
          && frame.session_id === offlineIdentity.sessionId)
      ))
      if (resumedLiveSocket) break
      await page.waitForTimeout(100)
    }
    assert.ok(resumedLiveSocket, JSON.stringify(liveSockets.slice(reconnectSocketBoundary)))
    const resumeSubscribe = resumedLiveSocket.sent.find(({ frame }) => (
      frame.type === 'subscribe' && frame.session_id === offlineIdentity.sessionId
    )).frame
    assert.equal(resumeSubscribe.after_seq, expectedResumeAfterSeq)
    await offlineUserMessage.waitFor({ timeout: 30_000 })
    const offlineWrite = writeTools.nth(offlineWriteIndex)
    await offlineWrite.waitFor({ timeout: 30_000 })
    assert.equal(await offlineUserMessage.count(), 1)
    assert.equal(await writeTools.count(), offlineWriteIndex + 1)
    await offlineWrite.getByRole('button', { name: '展开 写入文件 结果' }).click()
    await offlineWrite.locator('[data-tool-view="diff"]').waitFor()
    assert.match(await offlineWrite.locator('[data-tool-view="diff"]').textContent(), new RegExp(CLOUD_OFFLINE_SENTINEL))
    const resumedOfflineEvents = resumedLiveSocket.received
      .flatMap(({ frame }) => frame.type === 'event_batch' ? frame.events : [])
      .filter(event => event.run_id === offlineRunId)
    assert.deepEqual(
      [...new Set(resumedOfflineEvents.map(event => event.seq))],
      resumedOfflineEvents.map(event => event.seq),
      'cursor recovery must not deliver duplicate durable events',
    )
    assert.deepEqual(
      resumedOfflineEvents.map(event => event.seq),
      offlineRunEvents.map(event => event.seq),
      'reconnect must catch up every durable event written while offline',
    )
    await page.waitForTimeout(250)
    const intentionalOfflineFailures = failedRequests.splice(offlineFailedRequestStart)
    assert.equal(intentionalOfflineFailures.every(entry => entry.includes('net::ERR_INTERNET_DISCONNECTED')), true, JSON.stringify(intentionalOfflineFailures))
    const intentionalOfflineConsoleErrors = consoleErrors.splice(offlineConsoleErrorStart)
    assert.equal(intentionalOfflineConsoleErrors.every(entry => (
      /net::ERR_INTERNET_DISCONNECTED|WebSocket connection.*failed/i.test(entry)
    )), true, JSON.stringify(intentionalOfflineConsoleErrors))
    const intentionalOfflineConsoleErrorDetails = consoleErrorDetails.splice(offlineConsoleErrorStart)
    assert.equal(intentionalOfflineConsoleErrorDetails.length, intentionalOfflineConsoleErrors.length)
    assert.equal(intentionalOfflineConsoleErrorDetails.every(entry => (
      entry.phase === 'offline-canonical-recovery'
      && /net::ERR_INTERNET_DISCONNECTED|WebSocket connection.*failed/i.test(entry.text)
    )), true, JSON.stringify(intentionalOfflineConsoleErrorDetails))

    await reloadPage()
    await page.locator(`[data-sidebar-session-row][data-session-id="${offlineIdentity.sessionId}"]`).waitFor({ timeout: 30_000 })
    await offlineUserMessage.waitFor({ timeout: 30_000 })
    await writeTools.nth(offlineWriteIndex).waitFor({ timeout: 30_000 })
    assert.equal(await offlineUserMessage.count(), 1)
    assert.equal(await writeTools.count(), offlineWriteIndex + 1)

    currentPhase = 'workspace-unregister-and-run'
    console.log(`Cloud phase: ${currentPhase}`)
    const cloudWorkspaceRow = page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'Cloud Workspace' })
    await cloudWorkspaceRow.hover()
    await cloudWorkspaceRow.getByRole('button', { name: '工作区“Cloud Workspace”的操作' }).click()
    await page.getByRole('menuitem', { name: '移除工作区' }).click()
    const unregisterWorkspaceDialog = page.getByRole('dialog', { name: '移除工作区' })
    assert.match(await unregisterWorkspaceDialog.textContent(), /已有会话将显示在“未分组”中/)
    await unregisterWorkspaceDialog.getByRole('button', { name: '移除工作区' }).click()
    await unregisterWorkspaceDialog.waitFor({ state: 'detached' })
    await cloudWorkspaceRow.waitFor({ state: 'detached' })
    const ungrouped = page.locator('[data-sidebar-workspace-group]').filter({ hasText: '未分组' })
    await ungrouped.getByText('未分组', { exact: true }).waitFor()
    assert.equal(await ungrouped.locator(`[data-sidebar-session-row][data-session-id="${activeSessionId}"]`).count(), 1)

    const unregisterRunRequest = nextModelRequest(observed => (
      JSON.stringify(observed.body.messages ?? []).includes(CLOUD_UNREGISTER_TASK)
    ), 'Cloud run after Workspace unregister')
    const assistantCountBeforeUnregisterRun = await page.locator('article[data-role="assistant"]').count()
    const unregisterRunResponse = queueResponseForInput(CLOUD_UNREGISTER_TASK)
    await prompt.fill(CLOUD_UNREGISTER_TASK)
    await page.getByRole('button', { name: '发送' }).click()
    const unregisterSubmission = await (await unregisterRunResponse).json()
    await unregisterRunRequest
    await page.waitForFunction(expected => (
      document.querySelectorAll('article[data-role="assistant"]').length > expected
      && !document.querySelector('[data-composer-card][data-busy]')
    ), assistantCountBeforeUnregisterRun)
    await assertSuccessfulCanonicalRun(unregisterSubmission.run_id)

    const rejectedOldWorkspace = await page.evaluate(async workspaceId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch('/api/v1/sessions', {
        method: 'POST',
        headers: {
          authorization: `Bearer ${token}`,
          'x-ternilo-tenant': tenant,
          'content-type': 'application/json',
        },
        body: JSON.stringify({ workspace_id: workspaceId, permissions: 'workspace_write' }),
      })
      return { status: response.status, body: await response.json().catch(() => null) }
    }, cloudWorkspace.workspaceId)
    assert.equal(rejectedOldWorkspace.status, 400)
    assert.match(rejectedOldWorkspace.body.error.message, /workspace/i)

    const clientState = await page.evaluate(() => ({
      html: document.documentElement.outerHTML,
      local: Object.fromEntries(Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index)).filter(Boolean).map(key => [key, localStorage.getItem(key)])),
      session: Object.fromEntries(Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index)).filter(Boolean).map(key => [key, sessionStorage.getItem(key)])),
    }))
    assert.equal(JSON.stringify(clientState).includes('cloud-e2e-provider-secret'), false)
    assert.equal(JSON.stringify(clientState).includes('cloud-browser-byok-secret'), false)
    assert.equal(JSON.stringify(clientState).includes('cloud-browser-setting-secret'), false)
    const exported = await page.evaluate(async () => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const sessionId = localStorage.getItem('ternilo.current-session') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      if (!response.ok) throw new Error(`cloud session export failed: ${response.status}`)
      return response.json()
    })
    assert.equal(JSON.stringify(exported).includes('cloud-e2e-provider-secret'), false)
    assert.equal(JSON.stringify(exported).includes('cloud-browser-byok-secret'), false)
    assert.equal(JSON.stringify(exported).includes('cloud-browser-setting-secret'), false)
    for (const secret of CLOUD_E2E_SECRETS) {
      assert.equal(control.diagnostics().includes(secret), false)
      for (const process of workerProcesses) {
        assert.equal(process.diagnostics().includes(secret), false)
      }
    }

    const csp = (await page.request.get(origin)).headers()['content-security-policy']
    assert.match(csp, /frame-ancestors 'none'/)
    await reloadPage()
    await page.locator(`[data-sidebar-session-row][data-session-id="${activeSessionId}"]`).waitFor({ timeout: 30_000 })
    await page.locator('article[data-role="assistant"]')
      .filter({ hasText: 'cloud broker ready' }).first().waitFor()
    assert.equal(await page.locator('[data-question-lifecycle][data-state="answered"]')
      .filter({ hasText: CLOUD_QUESTION_ONE }).count(), 1)
    assert.equal(await page.locator('[data-question-lifecycle][data-state="answered"]')
      .filter({ hasText: CLOUD_QUESTION_TWO }).count(), 1)
    const restoredImage = page.getByRole('button', { name: '打开图片 cloud-attachment.webp' })
    await restoredImage.waitFor({ timeout: 30_000 })
    await restoredImage.click()
    const restoredPreview = page.getByRole('dialog', { name: '图片预览：cloud-attachment.webp' })
    await restoredPreview.waitFor()
    await restoredPreview.getByRole('button', { name: '关闭图片预览' }).click()

    currentPhase = 'english-mobile-final'
    console.log(`Cloud phase: ${currentPhase}`)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForFunction(() => (document.querySelector('.app-sidebar')?.getBoundingClientRect().right ?? 0) <= 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    if (!await prompt.isVisible()) {
      const mobileState = await page.evaluate(() => Object.fromEntries(
        ['[data-app-frame]', '.app-sidebar', '.conversation-column', '.conversation-scroll', '.composer-seat', '.composer-shell', '.composer-editor']
          .map(selector => {
            const element = document.querySelector(selector)
            if (!element) return [selector, null]
            const rect = element.getBoundingClientRect()
            const style = getComputedStyle(element)
            return [selector, { rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height }, display: style.display, visibility: style.visibility, opacity: style.opacity }]
          }),
      ))
      throw new Error(`mobile composer is not visible: ${JSON.stringify(mobileState)}`)
    }
    const composer = await page.locator('.composer-shell').boundingBox()
    assert.equal(composer.x >= 8 && composer.x + composer.width <= 382, true)

    await page.getByRole('button', { name: '打开侧边栏' }).click()
    const mobileSidebar = page.getByRole('complementary', { name: '会话侧边栏' })
    parentSidebarRow = mobileSidebar.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.parentSessionId}"]`)
    childSidebarRow = mobileSidebar.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.childSessionId}"]`)
    await childSidebarRow.waitFor()
    assert.equal(await childSidebarRow.getAttribute('data-session-depth'), '1')
    childFold = parentSidebarRow.locator('[data-sidebar-session-children-toggle]')
    const mobileFoldBox = await childFold.boundingBox()
    assert.ok(mobileFoldBox && mobileFoldBox.width >= 40 && mobileFoldBox.height >= 40)
    await childFold.click()
    await childSidebarRow.waitFor({ state: 'detached' })
    await childFold.click()
    await mobileSidebar.locator(`[data-sidebar-session-row][data-session-id="${cloudTeamSessions.childSessionId}"]`).waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    await mobileSidebar.getByRole('button', { name: '用户设置' }).click()
    let mobileSettings = page.locator('[data-user-settings]')
    await mobileSettings.getByRole('button', { name: '通用', exact: true }).click()
    assert.equal(await mobileSettings.getByLabel('新会话默认权限').locator('option').filter({ hasText: '完整访问 · 仅本地电脑' }).count(), 1)
    await selectChoice(mobileSettings.getByLabel('语言'), 'en')
    mobileSettings = page.locator('[data-user-settings]')
    await mobileSettings.getByRole('button', { name: 'General', exact: true }).waitFor()
    assert.equal(await mobileSettings.getByLabel('Default permission for new sessions').locator('option').filter({ hasText: 'Full access · local computers only' }).count(), 1)
    await mobileSettings.getByRole('button', { name: 'Back to workbench' }).click()
    await reloadPage()
    const englishPrompt = page.getByRole('textbox', { name: 'Enter task' })
    await englishPrompt.waitFor({ timeout: 30_000 })
    await page.getByRole('button', { name: 'Open sidebar' }).click()
    const englishSidebar = page.getByRole('complementary', { name: 'Conversation sidebar' })
    await englishSidebar.getByText('Ungrouped', { exact: true }).waitFor()
    await englishSidebar.getByRole('button', { name: 'Close sidebar' }).click()
    const englishPermission = page.getByRole('button', { name: 'Session permission: Workspace write' })
    await englishPermission.click()
    assert.equal(await page.getByRole('menuitemradio', { name: 'Full access' }).count(), 0)
    await page.keyboard.press('Escape')

    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'cloud-browser-mobile-en.png'), fullPage: true })
    await page.setViewportSize({ width: 844, height: 390 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    const landscapeComposer = await page.locator('.composer-shell').boundingBox()
    assert.equal(landscapeComposer.x >= 8 && landscapeComposer.x + landscapeComposer.width <= 836, true)

    // The narrow desktop rail hides the connection label until expanded.
    await page.locator('.app-sidebar').getByRole('button', { name: 'Expand sidebar', exact: true }).click()
    await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({
      state: 'visible',
      timeout: 30_000,
    })
    const quietRequestBoundary = backgroundLiveRequests.length
    const quietSocketBoundary = liveSockets.length
    await page.waitForTimeout(30_000)
    assert.deepEqual(
      backgroundLiveRequests.slice(quietRequestBoundary),
      [],
      'settled Cloud Web must not restore periodic REST metadata/history requests',
    )
    assert.equal(liveSockets.length, quietSocketBoundary, 'settled Cloud Web must not reconnect its live socket')
    const physicalLiveSockets = await page.evaluate(() => window.__terniloCloudE2eLiveSockets.map(socket => ({
      url: socket.url,
      readyState: socket.readyState,
    })))
    assert.equal(
      physicalLiveSockets.filter(socket => socket.readyState === 1).length,
      1,
      `settled Cloud Web must keep exactly one physical live socket: ${JSON.stringify(physicalLiveSockets)}`,
    )

    currentPhase = 'final-error-gates'
    console.log(`Cloud phase: ${currentPhase}`)
    await Promise.all([...failedRequestTasks, ...failedResponseTasks])
    const expectedRestartCommandUnavailable = entry => {
      if (entry.phase !== 'worker-restart-command-catalog' || entry.status !== 503 || entry.method !== 'GET'
        || !/^\/api\/v1\/sessions\/[^/]+\/(commands|skills)$/.test(entry.path)) return false
      const error = JSON.parse(entry.body).error
      return error?.code === 'unavailable' && /^cloud child (?:closed before replying to a Session command|Session command timed out)$/.test(error.message)
    }
    const expectedPolicyRestartUnavailable = entry => {
      if (!['workspace-quota-worker-restart', 'restore-worker-policy'].includes(entry.phase)
        || entry.status !== 503 || entry.method !== 'GET') return false
      const catalog = /^\/api\/v1\/sessions\/[^/]+\/(commands|skills)$/.exec(entry.path)?.[1]
      if (!catalog) return false
      const error = JSON.parse(entry.body).error
      const capability = catalog === 'commands' ? 'AddressedSessionCommands' : 'Skills'
      return error?.code === 'unavailable' && error.message === `capability unavailable: no live cloud Worker can serve ${capability}`
    }
    const expectedBaselineFailedResponse = entry => (
      entry.status === 422 && entry.method === 'POST' && entry.path === '/api/v1/authorizations/begin'
    ) || (
      entry.status === 503 && entry.method === 'GET' && entry.path === '/api/v1/projects'
    ) || (
      entry.status === 400 && entry.method === 'POST' && entry.path === '/api/v1/sessions'
    )
    const expectedFailedResponse = entry => (
      expectedBaselineFailedResponse(entry) || expectedRestartCommandUnavailable(entry) || expectedPolicyRestartUnavailable(entry)
    )
    const expectedFailedResponseDetails = failedResponseDetails.filter(expectedFailedResponse)
    const expectedBaselineFailedResponseDetails = expectedFailedResponseDetails.filter(expectedBaselineFailedResponse)
    const restartCommandUnavailable = expectedFailedResponseDetails.filter(expectedRestartCommandUnavailable)
    const policyRestartUnavailable = expectedFailedResponseDetails.filter(expectedPolicyRestartUnavailable)
    assert.ok(policyRestartUnavailable.length <= 2)
    assert.equal(new Set(policyRestartUnavailable.map(entry => entry.phase)).size, policyRestartUnavailable.length)
    const unexpectedFailedResponseDetails = failedResponseDetails.filter(entry => !expectedFailedResponse(entry))
    assert.equal(expectedBaselineFailedResponseDetails.length, 3)
    assert.ok(restartCommandUnavailable.length <= 2)
    assert.equal(new Set(restartCommandUnavailable.map(entry => entry.path)).size, restartCommandUnavailable.length)
    for (const entry of restartCommandUnavailable) {
      const restartCommandError = JSON.parse(entry.body).error
      assert.equal(restartCommandError.code, 'unavailable')
      assert.match(
        restartCommandError.message,
        /^cloud child (?:closed before replying to a Session command|Session command timed out)$/,
      )
    }
    assert.deepEqual(unexpectedFailedResponseDetails, [], JSON.stringify({
      unexpectedFailedResponseDetails,
      control: diagnosticResponseBody(control.diagnostics()),
      workers: workerProcesses.map(process => diagnosticResponseBody(process.diagnostics())),
    }, null, 2))
    const remainingFailedRequestDetails = failedRequestDetails.filter(detail => failedRequests.includes(detail.summary))
    const successfulResponseAborts = remainingFailedRequestDetails.filter(detail => (
      detail.failure === 'net::ERR_ABORTED'
      && detail.responseStatus !== null
      && detail.responseStatus >= 200
      && detail.responseStatus < 300
    ))
    const lifecycleReadAborts = remainingFailedRequestDetails.filter(detail => (
      detail.failure === 'net::ERR_ABORTED'
      && detail.method === 'GET'
      && detail.responseStatus === null
      && detail.lifecycleRequest
    ))
    const unexpectedFailedRequestDetails = remainingFailedRequestDetails.filter(detail => (
      !successfulResponseAborts.includes(detail) && !lifecycleReadAborts.includes(detail)
    ))
    console.log(`Cloud request abort classification: ${JSON.stringify({
      successfulResponseAborts: successfulResponseAborts.length,
      lifecycleReadAborts: lifecycleReadAborts.length,
      unexpected: unexpectedFailedRequestDetails.length,
    })}`)
    assert.deepEqual(unexpectedFailedRequestDetails, [], JSON.stringify({
      failedRequestDetails: remainingFailedRequestDetails,
    }, null, 2))
    const expectedConsoleResponseCounts = new Map()
    for (const response of expectedFailedResponseDetails) {
      const key = `${response.phase}:${response.status}`
      expectedConsoleResponseCounts.set(key, (expectedConsoleResponseCounts.get(key) ?? 0) + 1)
    }
    const expectedConsoleErrors = []
    const unexpectedConsoleErrors = []
    for (const entry of consoleErrorDetails) {
      const status = /status of (\d+)/.exec(entry.text)?.[1]
      const key = `${entry.phase}:${status}`
      const available = expectedConsoleResponseCounts.get(key) ?? 0
      if (status !== undefined && available > 0) {
        expectedConsoleErrors.push(entry)
        expectedConsoleResponseCounts.set(key, available - 1)
      } else {
        unexpectedConsoleErrors.push(entry)
      }
    }
    console.log(`Cloud expected error classification: ${JSON.stringify({
      failedResponses: expectedFailedResponseDetails.length,
      consoleErrors: expectedConsoleErrors.length,
      unexpectedConsoleErrors: unexpectedConsoleErrors.length,
    })}`)
    assert.equal(expectedConsoleErrors.length, expectedFailedResponseDetails.length)
    assert.deepEqual(unexpectedConsoleErrors, [], JSON.stringify({ consoleErrors }))
    assert.deepEqual(pageErrors, [])
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await page.setViewportSize({ width: 1440, height: 900 })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'cloud-browser-desktop-en.png'), fullPage: true })
    }
  } catch (error) {
    console.error(`Cloud failure in ${currentPhase}: ${error instanceof Error ? error.message : String(error)}`)
    console.error(`Server diagnostics: ${diagnosticResponseBody(control?.diagnostics() || '')}`)
    for (const worker of workerProcesses) console.error(`Worker diagnostics: ${diagnosticResponseBody(worker.diagnostics())}`)
    if (page && process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'cloud-browser-failure.png'), fullPage: true }).catch(() => {})
      await writeFile(path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'cloud-browser-failure.txt'), diagnosticResponseBody(await page.locator('body').innerText().catch(() => 'page closed')))
    }
    throw error
  } finally {
    await browser?.close()
    await Promise.all(workerProcesses.map(process => stopProcess(process)))
    await Promise.all(controlProcesses.map(process => stopProcess(process)))
    await model.close()
    await oidc.close()
    await postgres.stop()
    if (containerMode) {
      await execute('docker', ['rm', '-f', workerName, controlName]).catch(() => {})
      const owner = `${process.getuid?.() ?? 0}:${process.getgid?.() ?? 0}`
      await execute('docker', [
        'run', '--rm', '--user', '0:0', '--entrypoint', '/bin/chown',
        '--volume', `${directory}:/cleanup`, workerImage,
        '-R', owner, '/cleanup',
      ]).catch(() => {})
    }
    await rm(directory, { recursive: true, force: true })
  }
})
