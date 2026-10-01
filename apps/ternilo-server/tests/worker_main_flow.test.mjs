import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { createHash, randomBytes } from 'node:crypto'
import { createServer } from 'node:http'
import { chmod, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { promisify } from 'node:util'
import { freePort, repository, stopProcess, waitForHttp } from '../../../web/tests/platform-e2e-fixture.mjs'

const execute = promisify(execFile)
const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY || path.join(repository, 'target/debug/ternilo-server')
const workerBinary = process.env.TERNILO_E2E_WORKER_BINARY || path.join(repository, 'target/debug/ternilo-worker')
const cleanEnvironment = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('TERNILO_')))
const pause = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds))

function start(binary, args, environment = {}) {
  const child = spawn(binary, args, { cwd: repository, env: { ...cleanEnvironment, ...environment }, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = ''
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  return { child, diagnostics: () => output }
}

async function ready(worker) {
  for (let attempt = 0; attempt < 300; attempt++) {
    if (worker.child.exitCode !== null) throw new Error(`Worker exited before readiness: ${worker.diagnostics()}`)
    if (/Ternilo cloud worker .* ready/.test(worker.diagnostics())) return
    await pause(100)
  }
  throw new Error(`Worker did not register: ${worker.diagnostics()}`)
}

async function modelFixture() {
  const requests = []
  const pending = new Set()
  const server = createServer(async (request, response) => {
    if (request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    let text = ''
    for await (const part of request) text += part
    const body = JSON.parse(text)
    requests.push({ body, authorization: request.headers.authorization })
    response.writeHead(200, { 'content-type': 'text/event-stream', 'x-request-id': `worker-canary-${requests.length}` })
    response.write('data: {"choices":[{"delta":{"content":"broker "}}]}\n\n')
    if (JSON.stringify(body.messages.at(-1)).includes('keep this canary running')) {
      pending.add(response)
      response.once('close', () => pending.delete(response))
      return
    }
    await pause(30)
    response.end([
      'data: {"choices":[{"delta":{"content":"ready"}}]}\n\n',
      'data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":2}}}\n\n',
      'data: [DONE]\n\n',
    ].join(''))
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { requests, pending, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: async () => {
    for (const response of pending) response.destroy()
    server.closeAllConnections()
    await new Promise(resolve => server.close(resolve))
  } }
}

test('Server and Worker run with independent roots, private broker keys and persistent storage', { timeout: 180_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-worker-flow-'))
  const model = await modelFixture()
  const serverConfig = path.join(directory, 'server/config.json')
  const workerData = path.join(directory, 'worker')
  const workspaceRoot = path.join(workerData, 'workspaces')
  const policyPath = path.join(directory, 'policy.json')
  const operatorKey = randomBytes(24).toString('hex')
  const userKey = randomBytes(24).toString('hex')
  const password = randomBytes(24).toString('hex')
  const origin = `http://127.0.0.1:${await freePort()}`
  const policy = JSON.parse(await readFile(path.join(repository, 'deploy/docker/worker-policy.json'), 'utf8'))
  policy.minimum_workspace_free_bytes = 0
  policy.model_routes = { primary: {
    base_url: model.baseUrl, protocol: 'openai-chat-completions',
    defaults: { context_window: 128000, max_output_tokens: 2048 },
    models: [{ id: 'cloud-model', display_name: 'Cloud Model', settings: { mode: 'inherit' } }],
    api_key_env: 'TERNILO_TEST_OPERATOR_KEY', timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 100,
  } }
  await writeFile(policyPath, JSON.stringify(policy))
  const database = process.env.TERNILO_WORKER_E2E_DATABASE_URL
  const migration = process.env.TERNILO_WORKER_E2E_MIGRATION_URL
  const initArgs = ['init', '--config-dir', path.dirname(serverConfig), '--non-interactive', '--owner-username', 'owner', '--listen', new URL(origin).host]
  if (database) initArgs.push('--database-url', database)
  if (migration) initArgs.push('--migration-database-url', migration)
  const workers = []
  let server
  let token
  let tenant
  const request = async (endpoint, body, method = body === undefined ? 'GET' : 'POST') => {
    const response = await fetch(`${origin}/api/v1${endpoint}`, {
      method, headers: { 'content-type': 'application/json', ...(token && { authorization: `Bearer ${token}` }), ...(tenant && { 'x-ternilo-tenant': tenant }) },
      ...(body !== undefined && { body: JSON.stringify(body) }),
    })
    const value = response.status === 204 ? null : await response.json()
    assert.ok(response.ok, `${method} ${endpoint}: ${response.status} ${JSON.stringify(value)}`)
    return value
  }
  const launch = () => { const process = start(workerBinary, ['serve', '--config-dir', workerData]); workers.push(process); return process }
  let worker
  try {
    await execute(serverBinary, initArgs, { cwd: repository, env: { ...cleanEnvironment, TERNILO_SERVER_OWNER_EMAIL: "owner@example.test", TERNILO_SERVER_OWNER_PASSWORD: password } })
    server = start(serverBinary, ['serve', '--config-dir', path.dirname(serverConfig), '--managed-execution-enabled', '--worker-policy', policyPath], { TERNILO_TEST_OPERATOR_KEY: operatorKey })
    await waitForHttp(`${origin}/readyz`, server)
    const login = await request('/auth/login', { username: 'owner', password })
    token = login.access_token
    tenant = login.personal_tenant_id
    const grant = await request('/admin/workers', { worker_id: `worker-${randomBytes(6).toString('hex')}` })
    await execute(workerBinary, ['init', '--config-dir', workerData, '--server-url', origin, '--workspace-root', workspaceRoot,
      '--sandbox', process.env.TERNILO_WORKER_E2E_SANDBOX || 'process', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'],
    { cwd: repository, env: { ...cleanEnvironment, TERNILO_WORKER_TOKEN: grant.token } })
    const configuration = JSON.parse(await readFile(path.join(workerData, 'config.json'), 'utf8'))
    assert.doesNotMatch(JSON.stringify(configuration), /database_url|secret_master_key|host_database/)
    worker = launch()
    await ready(worker)
    const registered = (await request('/admin/workers')).find(record => record.worker_id === grant.worker_id)
    assert.equal(registered.online, true)
    assert.equal(registered.storage_id, grant.storage_id)
    t.diagnostic('Worker registered using only its scoped Server credential')

    const { workspace } = await request('/workspaces', { project_id: login.personal_project_id, name: 'Persistent Worker', placement: 'cloud' })
    const session = await request('/sessions', { workspace_id: workspace.workspace_id })
    const sessionId = session.identity.session_id
    const waitForRun = async submission => {
      for (let attempt = 0; attempt < 600; attempt++) {
        const { run: record } = await request(`/tenants/${tenant}/runs/${submission.run_id}`)
        if (['succeeded', 'failed', 'cancelled', 'indeterminate'].includes(record.state)) {
          assert.equal(record.state, 'succeeded', `run failed: ${JSON.stringify(record.error)}; ${worker.diagnostics()}`)
          return record
        }
        if (worker.child.exitCode !== null) throw new Error(`Worker exited during the run: ${worker.diagnostics()}`)
        await pause(100)
      }
      throw new Error(`run did not complete: ${worker.diagnostics()}`)
    }
    const run = async (input, attachments = []) => waitForRun(await request(`/sessions/${sessionId}/queue`, { delivery: 'queue', content: { kind: 'prompt', input }, attachments }))
    await run('confirm the operator broker')
    assert.ok(model.requests.some(value => value.authorization === `Bearer ${operatorKey}`))
    await request('/credentials', { name: 'CANARY_BYOK', value: userKey })
    await request('/providers', { ...policy.model_routes.primary, api_key_env: undefined, api_key_ref: 'CANARY_BYOK', id: 'private', display_name: 'Private Provider' })
    await request(`/sessions/${sessionId}`, { model: { provider: 'named_provider', provider_id: 'private', model: 'cloud-model' } }, 'PATCH')
    await run('confirm the private owner broker')
    assert.ok(model.requests.some(value => value.authorization === `Bearer ${userKey}`))
    const usage = await request(`/tenants/${tenant}/model-usage`)
    assert.ok(usage.ledger.some(entry => entry.provider === 'primary'))
    assert.ok(usage.ledger.some(entry => entry.provider === 'private'))
    for (const entry of usage.ledger) {
      assert.equal(entry.input_tokens, 12)
      assert.equal(entry.output_tokens, 4)
      assert.equal(entry.cached_input_tokens, 2)
      assert.equal(entry.user_id, login.user.user_id)
      assert.equal(entry.reservation_state, 'committed')
    }
    t.diagnostic('Operator and BYOK models streamed through Server with authoritative usage')

    await run('/write durable.txt survives-worker-restart')
    const digest = value => createHash('sha256').update(value).digest('hex')
    const physicalWorkspace = path.join(workspaceRoot, digest(tenant), digest(workspace.workspace_id))
    assert.match(await readFile(path.join(physicalWorkspace, 'durable.txt'), 'utf8'), /survives-worker-restart/)
    assert.ok(!(await readdir(path.join(directory, 'server'))).includes(digest(tenant)))
    const marker = await readFile(path.join(workspaceRoot, '.ternilo-storage.json'), 'utf8')
    const candidates = await request(`/sessions/${sessionId}/references?directory=&query=`)
    assert.match(JSON.stringify(candidates), /durable\.txt/)
    const attachmentContent = `attachment restored through Server ${randomBytes(8).toString('hex')}`
    const upload = { name: 'restore-proof.txt', media_type: 'text/plain', content: attachmentContent }
    await run('Read the attached restore proof', [upload])
    const uploadedEvents = await request(`/sessions/${sessionId}/events`)
    const retained = uploadedEvents.flatMap(event => event.attachments || []).find(attachment => attachment.name === upload.name)
    assert.ok(retained?.content.startsWith('ternilo-attachment://sha256/'), 'the Server keeps the canonical attachment reference')
    const attachmentPath = path.join(physicalWorkspace, '.ternilo/attachments/objects', digest(attachmentContent))
    assert.equal(await readFile(attachmentPath, 'utf8'), attachmentContent)

    await stopProcess(worker)
    // Remove only this fixture's cached object; its authoritative Server copy and workspace files remain.
    await rm(attachmentPath)
    const replacementRoot = path.join(directory, 'empty-replacement-volume')
    const wrongVolume = start(workerBinary, ['serve', '--config-dir', workerData, '--workspace-root', replacementRoot])
    workers.push(wrongVolume)
    for (let attempt = 0; attempt < 100 && wrongVolume.child.exitCode === null; attempt++) await pause(100)
    assert.equal(wrongVolume.child.exitCode, 1, `an empty replacement must be refused: ${wrongVolume.diagnostics()}`)
    assert.match(wrongVolume.diagnostics(), /data root differs from its registered persistent volume/)
    const replacementMarker = JSON.parse(await readFile(path.join(replacementRoot, '.ternilo-storage.json'), 'utf8'))
    assert.notEqual(replacementMarker.root_id, JSON.parse(marker).root_id)
    assert.equal(await readFile(path.join(workspaceRoot, '.ternilo-storage.json'), 'utf8'), marker)
    t.diagnostic('The original credential could not bind an empty replacement volume')

    worker = launch()
    await ready(worker)
    assert.equal(await readFile(path.join(workspaceRoot, '.ternilo-storage.json'), 'utf8'), marker)
    const restoredRequestStart = model.requests.length
    await run('Read the restored attachment again', [retained])
    assert.equal(await readFile(attachmentPath, 'utf8'), attachmentContent)
    assert.ok(model.requests.slice(restoredRequestStart).some(value => JSON.stringify(value.body.messages).includes(attachmentContent)), 'the real child resolves the downloaded object into the model input')
    t.diagnostic('A canonical historical attachment was downloaded from Server and restored by the real child')
    await run('/read durable.txt')
    const events = await request(`/sessions/${sessionId}/events`)
    assert.match(JSON.stringify(events), /survives-worker-restart/)
    assert.equal(new Set(events.map(event => event.seq)).size, events.length)
    t.diagnostic('Files, directed reference browsing and event history survived Worker restart')

    if (!database || database.startsWith('sqlite:')) {
      const snapshot = path.join(directory, 'online snapshot.sqlite3')
      await execute(serverBinary, ['admin', 'backup-sqlite', '--config-dir', path.dirname(serverConfig), '--output', snapshot], { cwd: repository, env: cleanEnvironment })
      const snapshotBytes = await readFile(snapshot)
      assert.equal(snapshotBytes.subarray(0, 15).toString(), 'SQLite format 3')
      await assert.rejects(execute(serverBinary, ['admin', 'backup-sqlite', '--config-dir', path.dirname(serverConfig), '--output', snapshot], { cwd: repository, env: cleanEnvironment }))
      assert.deepEqual(await readFile(snapshot), snapshotBytes)
      assert.equal((await fetch(`${origin}/readyz`)).status, 200, 'online backup leaves Server available')
      t.diagnostic('The online SQLite backup command preserved a consistent snapshot and kept Server running')
    }

    const instance = await request('/admin/instance')
    await request('/admin/instance', { mode: 'multi_user', revision: instance.revision }, 'PATCH')
    const invitation = await request('/admin/invitations', { role: 'member' })
    const member = await request('/auth/invitations/accept', { token: invitation.token, username: 'member', password: randomBytes(24).toString('hex') })
    for (const method of ['GET', 'PATCH']) {
      const forbidden = await fetch(`${origin}/api/v1/admin/execution`, {
        method, headers: { authorization: `Bearer ${member.access_token}`, 'content-type': 'application/json' },
        ...(method === 'PATCH' && { body: JSON.stringify({ claims_paused: true }) }),
      })
      assert.equal(forbidden.status, 403, 'ordinary members cannot inspect or pause instance execution')
      assert.equal((await forbidden.json()).error.code, 'policy_denied')
    }
    const paused = await request('/admin/execution', { claims_paused: true }, 'PATCH')
    assert.deepEqual(paused, { claims_paused: true, active_runs: 0, active_commands: 0 })
    const queued = await request(`/sessions/${sessionId}/queue`, { delivery: 'queue', content: { kind: 'prompt', input: 'resume after maintenance' } })
    await pause(300)
    const { run: waiting } = await request(`/tenants/${tenant}/runs/${queued.run_id}`)
    assert.equal(waiting.state, 'queued', 'maintenance leaves accepted work queued without starting a child')
    assert.equal((await request('/admin/execution')).active_runs, 0)
    const resumed = await request('/admin/execution', { claims_paused: false }, 'PATCH')
    assert.equal(resumed.claims_paused, false)
    await waitForRun(queued)
    t.diagnostic('Maintenance paused new claims, exposed quiescence and resumed the same queued run')

    const held = await request(`/sessions/${sessionId}/queue`, { delivery: 'queue', content: { kind: 'prompt', input: 'keep this canary running' } })
    for (let attempt = 0; attempt < 200 && model.pending.size === 0; attempt++) await pause(100)
    assert.equal(model.pending.size, 1)
    await request('/admin/workers/' + grant.worker_id, undefined, 'DELETE')
    for (let attempt = 0; attempt < 100 && model.pending.size > 0; attempt++) await pause(100)
    assert.equal(model.pending.size, 0, 'revocation closes the in-flight provider request')
    const revoked = (await request('/admin/workers')).find(record => record.worker_id === grant.worker_id)
    assert.equal(revoked.online, false)
    assert.ok(revoked.revoked_at_ms)
    const { run: heldRun } = await request(`/tenants/${tenant}/runs/${held.run_id}`)
    assert.notEqual(heldRun.state, 'succeeded')
    for (const secret of [operatorKey, userKey, grant.token]) {
      assert.equal(workers.some(process => process.diagnostics().includes(secret)), false)
      assert.equal(JSON.stringify(events).includes(secret), false)
    }
    t.diagnostic('Revoking one Worker stopped its model channel without exposing credentials')
  } finally {
    await Promise.all(workers.map(stopProcess))
    await stopProcess(server)
    await model.close()
    if (process.env.TERNILO_WORKER_E2E_KEEP === '1') {
      await chmod(directory, 0o700)
      t.diagnostic(`Temporary test data retained at ${directory}`)
    } else await rm(directory, { recursive: true, force: true })
  }
})
