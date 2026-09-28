import assert from 'node:assert/strict'
import { createHash, randomBytes } from 'node:crypto'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'

import {
  base64Url,
  freePort,
  repository,
  startOidcServer,
  startPostgres,
  startProcess,
  stopProcess,
  waitForHttp,
} from '../../../web/tests/platform-e2e-fixture.mjs'

const controlBinary = path.join(repository, 'target', 'debug', 'ternilo-server')
const nodeBinary = path.join(repository, 'target', 'debug', 'ternilo')
const workerPolicy = path.join(repository, 'deploy', 'docker', 'worker-policy.json')

async function issueAccessToken(oidc, origin) {
  const verifier = base64Url(randomBytes(32))
  const challenge = base64Url(createHash('sha256').update(verifier).digest())
  const redirectUri = `${origin}/auth/callback`
  const authorize = new URL('/authorize', oidc.issuer)
  authorize.search = new URLSearchParams({
    response_type: 'code',
    client_id: 'ternilo-control-node-core',
    redirect_uri: redirectUri,
    scope: 'openid profile email',
    state: 'control-node-core-state',
    code_challenge: challenge,
    code_challenge_method: 'S256',
  })
  const authorization = await fetch(authorize, { redirect: 'manual' })
  assert.equal(authorization.status, 302)
  const callback = new URL(authorization.headers.get('location'))
  assert.equal(callback.searchParams.get('state'), 'control-node-core-state')

  const token = await fetch(new URL('/token', oidc.issuer), {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'authorization_code',
      client_id: 'ternilo-control-node-core',
      redirect_uri: redirectUri,
      code: callback.searchParams.get('code'),
      code_verifier: verifier,
    }),
  })
  assert.equal(token.status, 200)
  return (await token.json()).access_token
}

async function request(origin, route, { token, tenant, method = 'GET', body } = {}) {
  const headers = {}
  if (token) headers.authorization = `Bearer ${token}`
  if (tenant) headers['x-ternilo-tenant'] = tenant
  if (body !== undefined) headers['content-type'] = 'application/json'
  const response = await fetch(new URL(route, origin), {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await response.text()
  return {
    status: response.status,
    value: text ? JSON.parse(text) : null,
  }
}

async function expectRequest(origin, route, options, status) {
  const response = await request(origin, route, options)
  assert.equal(response.status, status, JSON.stringify(response.value))
  return response.value
}

async function waitFor(predicate, label, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs
  let lastError
  while (Date.now() < deadline) {
    try {
      if (await predicate()) return
    } catch (error) {
      lastError = error
    }
    await new Promise(resolve => setTimeout(resolve, 200))
  }
  throw new Error(`timed out waiting for ${label}`, { cause: lastError })
}

async function waitForExecutor(origin, token, tenant, executorId, connected) {
  await waitFor(async () => {
    const response = await request(origin, '/api/v1/execution-targets', { token, tenant })
    return response.status === 200 && response.value.executors.some(executor => (
      executor.executor_id === executorId && executor.connected === connected
    ))
  }, `executor ${executorId} connected=${connected}`)
}

async function localBoot(nodeOrigin) {
  const response = await fetch(nodeOrigin)
  assert.equal(response.status, 200)
  const html = await response.text()
  const marker = 'window.__TERNILO_BOOT__ = '
  const encoded = html.split(marker)[1]?.split(';</script>')[0]
  assert.ok(encoded, 'Node local boot payload is missing')
  return JSON.parse(encoded)
}

function eventStats(events) {
  const sequences = events.map(event => event.seq)
  return {
    count: sequences.length,
    unique: new Set(sequences).size,
    last: sequences.at(-1),
  }
}

test('Control and Node complete enrollment, RPC, offline recovery, and shutdown without a browser', {
  timeout: 180_000,
}, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-control-node-core-'))
  const nodeData = path.join(temporary, 'node-data')
  const workspacePath = path.join(temporary, 'workspace')
  const cloudWorkspaces = path.join(temporary, 'cloud-workspaces')
  await Promise.all([
    mkdir(nodeData),
    mkdir(workspacePath),
    mkdir(cloudWorkspaces),
  ])

  const postgres = await startPostgres({
    prefix: 'ternilo-control-node-core',
    database: 'ternilo_control_node_core_test',
  })
  const oidc = await startOidcServer({
    audience: 'ternilo-control-node-core',
    subject: 'control-node-core-owner',
    email: 'control-node-core@example.com',
    name: 'Control Node Core Owner',
  })
  const controlPort = await freePort()
  const nodePort = await freePort()
  const origin = `http://127.0.0.1:${controlPort}`
  const nodeOrigin = `http://127.0.0.1:${nodePort}`
  const controlEnvironment = {
    TERNILO_DATABASE_URL: postgres.url,
    TERNILO_MIGRATION_DATABASE_URL: postgres.url,
    TERNILO_SECRET_MASTER_KEY: Buffer.alloc(32, 23).toString('base64'),
    TERNILO_MODEL_API_KEY: 'unused-control-node-core-key',
  }
  const controlArguments = [
    'platform',
    '--listen', `127.0.0.1:${controlPort}`,
    '--public-url', origin,
    '--oidc-issuer', oidc.issuer,
    '--oidc-audience', 'ternilo-control-node-core',
    '--oidc-client-id', 'ternilo-control-node-core',
    '--allow-insecure-oidc',
    '--worker-policy', workerPolicy,
    '--workspace-root', cloudWorkspaces,
  ]
  const controls = []
  const nodes = []
  const startControl = () => {
    const process = startProcess(controlBinary, controlArguments, controlEnvironment)
    controls.push(process)
    return process
  }
  const executorId = 'core-node'
  const startNode = credential => {
    const process = startProcess(nodeBinary, ['serve',
      '--gateway-url', `${origin.replace('http://', 'ws://')}/api/v1/executors/connect`,
      '--allow-insecure-gateway',
      '--node-id', executorId,
      '--data-dir', nodeData,
      '--listen', `127.0.0.1:${nodePort}`,
    ], { TERNILO_LOCAL_TOKEN: credential })
    nodes.push(process)
    return process
  }

  let control = startControl()
  let node
  try {
    await waitForHttp(`${origin}/health`, control)
    const token = await issueAccessToken(oidc, origin)
    const actor = await expectRequest(origin, '/api/v1/me', { token }, 200)
    const createdTenant = await expectRequest(origin, '/api/v1/tenants', {
      token,
      method: 'POST',
      body: { slug: 'control-node-core', display_name: 'Control Node Core' },
    }, 201)
    const tenant = createdTenant.tenant.tenant_id
    const projects = await expectRequest(origin, '/api/v1/projects', { token, tenant }, 200)
    const project = projects.projects[0]
    assert.ok(project?.project_id)

    const enrollment = await expectRequest(
      origin,
      `/api/v1/tenants/${encodeURIComponent(tenant)}/enrollments`,
      {
        token,
        tenant,
        method: 'POST',
        body: { executor_id: executorId, project_id: project.project_id, ttl_seconds: 600 },
      },
      201,
    )
    const consumed = await expectRequest(origin, '/api/v1/enrollments/consume', {
      method: 'POST',
      body: { token: enrollment.enrollment.token },
    }, 200)
    const credential = consumed.credential.token
    assert.ok(credential)

    node = startNode(credential)
    await waitForHttp(nodeOrigin, node)
    await waitForExecutor(origin, token, tenant, executorId, true)

    const workspace = await expectRequest(origin, '/api/v1/workspaces', {
      token,
      tenant,
      method: 'POST',
      body: {
        project_id: project.project_id,
        name: 'Core Node Workspace',
        placement: 'local_node',
        executor_id: executorId,
        path: workspacePath,
      },
    }, 201)
    const workspaceId = workspace.workspace.workspace_id
    const session = await expectRequest(origin, '/api/v1/sessions', {
      token,
      tenant,
      method: 'POST',
      body: {
        workspace_id: workspaceId,
        session_id: 'core-public-session',
        agent_id: null,
        agent_preset: null,
        permissions: null,
      },
    }, 201)
    const publicSessionId = session.identity.session_id
    assert.equal(publicSessionId, 'core-public-session')

    const firstTurn = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/turns`,
      {
        token,
        tenant,
        method: 'POST',
        body: { input: '/code "control-node-core-ready"', run_id: 'core-first', attachments: [] },
      },
      200,
    )
    assert.equal(JSON.parse(firstTurn.answer).result, 'control-node-core-ready')
    const firstEvents = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/events`,
      { token, tenant },
      200,
    )
    assert.ok(firstEvents.some(event => (
      event.type === 'turn_finished' && JSON.parse(event.answer).result === 'control-node-core-ready'
    )))
    assert.equal(eventStats(firstEvents).count, eventStats(firstEvents).unique)

    const boot = await localBoot(nodeOrigin)
    const localState = await expectRequest(nodeOrigin, '/api/v1/state', {
      token: boot.apiToken,
    }, 200)
    assert.equal(localState.sessions.length, 1)
    const nodeSessionId = localState.sessions[0].identity.session_id
    assert.notEqual(nodeSessionId, publicSessionId)

    const mapping = (await postgres.query(
      "SELECT session_id, node_session_id, owner_user_id, workspace_id, last_event_seq " +
      "FROM control_edge_sessions",
    )).split('|')
    assert.deepEqual(mapping.slice(0, 4), [
      publicSessionId,
      nodeSessionId,
      actor.user_id,
      workspaceId,
    ])
    assert.equal(Number(mapping[4]), firstEvents.at(-1).seq)

    await stopProcess(control)
    assert.equal(control.child.exitCode, 0, control.diagnostics())
    const offlineLocalTurn = await expectRequest(
      nodeOrigin,
      `/api/v1/sessions/${encodeURIComponent(nodeSessionId)}/turns`,
      {
        token: boot.apiToken,
        method: 'POST',
        body: {
          input: '/code "control-restart-offline"',
          run_id: 'core-offline',
          attachments: [],
        },
      },
      200,
    )
    assert.equal(JSON.parse(offlineLocalTurn.answer).result, 'control-restart-offline')

    control = startControl()
    await waitForHttp(`${origin}/health`, control)
    await waitForExecutor(origin, token, tenant, executorId, true)
    await waitFor(async () => Number(await postgres.query(
      "SELECT count(*) FROM control_edge_events " +
      "WHERE event_json::text LIKE '%control-restart-offline%'",
    )) > 0, 'Control reconnect cursor catch-up')

    const recoveredEvents = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/events`,
      { token, tenant },
      200,
    )
    assert.equal(recoveredEvents.filter(event => (
      event.type === 'turn_finished' && JSON.parse(event.answer).result === 'control-restart-offline'
    )).length, 1)
    const recoveredStats = eventStats(recoveredEvents)
    assert.equal(recoveredStats.count, recoveredStats.unique)

    await stopProcess(node)
    assert.equal(node.child.exitCode, 0, node.diagnostics())
    await waitForExecutor(origin, token, tenant, executorId, false)
    const cachedWhileOffline = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/events`,
      { token, tenant },
      200,
    )
    assert.deepEqual(cachedWhileOffline, recoveredEvents)

    const offlineStarted = Date.now()
    const offlineMutation = await request(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/turns`,
      {
        token,
        tenant,
        method: 'POST',
        body: { input: '/code "must-not-run"', run_id: 'offline-rejected', attachments: [] },
      },
    )
    assert.equal(offlineMutation.status, 503, JSON.stringify(offlineMutation.value))
    assert.equal(offlineMutation.value.error.code, 'unavailable')
    assert.match(offlineMutation.value.error.message, /node is offline/i)
    assert.ok(Date.now() - offlineStarted < 2_000)

    const cachedCount = Number(await postgres.query('SELECT count(*) FROM control_edge_events'))
    node = startNode(credential)
    await waitForHttp(nodeOrigin, node)
    await waitForExecutor(origin, token, tenant, executorId, true)
    await new Promise(resolve => setTimeout(resolve, 500))
    const replayedCount = Number(await postgres.query('SELECT count(*) FROM control_edge_events'))
    assert.equal(replayedCount, cachedCount, 'cursor replay must not duplicate cached events')

    const reconnectedTurn = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/turns`,
      {
        token,
        tenant,
        method: 'POST',
        body: {
          input: '/code "node-process-reconnected"',
          run_id: 'core-reconnected',
          attachments: [],
        },
      },
      200,
    )
    assert.equal(JSON.parse(reconnectedTurn.answer).result, 'node-process-reconnected')
    const finalEvents = await expectRequest(
      origin,
      `/api/v1/sessions/${encodeURIComponent(publicSessionId)}/events`,
      { token, tenant },
      200,
    )
    assert.equal(finalEvents.filter(event => (
      event.type === 'turn_finished' && JSON.parse(event.answer).result === 'node-process-reconnected'
    )).length, 1)
    assert.equal(eventStats(finalEvents).count, eventStats(finalEvents).unique)

    await stopProcess(node)
    assert.equal(node.child.exitCode, 0, node.diagnostics())
    await stopProcess(control)
    assert.equal(control.child.exitCode, 0, control.diagnostics())
  } catch (error) {
    const diagnostics = [
      ...controls.map((process, index) => `control ${index + 1}:\n${process.diagnostics()}`),
      ...nodes.map((process, index) => `node ${index + 1}:\n${process.diagnostics()}`),
    ].join('\n')
    throw new Error(`${error instanceof Error ? error.message : String(error)}\n${diagnostics}`, {
      cause: error,
    })
  } finally {
    for (const process of nodes) await stopProcess(process)
    for (const process of controls) await stopProcess(process)
    await oidc.close()
    await postgres.stop()
    await rm(temporary, { recursive: true, force: true })
  }
})
