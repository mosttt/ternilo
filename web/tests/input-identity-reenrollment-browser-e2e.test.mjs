import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const OLD_MESSAGE = 'verified-before-reenrollment'
const OLD_HOLD = 'held-before-reenrollment'
const OLD_PENDING = 'pending-before-reenrollment'
const DIRECT_MESSAGE = 'verified-direct-after-reenrollment'
const NEW_MESSAGE = 'verified-browser-after-reenrollment'
const DIRECT_RUN = 'reenrollment-direct-run'

async function until(read, ready, label) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

async function modelFixture() {
  const held = new Map(), seen = new Set(), requests = []
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') { response.writeHead(404); response.end(); return }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      requests.push(body)
      const user = body.input?.filter(item => item.role === 'user').at(-1)
      const input = user?.content?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      const title = body.instructions?.includes('You name software-agent conversations')
      const text = title ? 'Re-enrolled identity' : `Re-enrollment fixture: ${input}`
      const finish = () => {
        if (response.destroyed) return
        const sse = event => `data: ${JSON.stringify(event)}\n\n`
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text })
          + sse({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }], usage: { input_tokens: 20, output_tokens: 10 } } }))
      }
      const marker = [OLD_HOLD, DIRECT_MESSAGE].find(marker => input.includes(marker) && !seen.has(marker))
      if (!title && marker) { seen.add(marker); held.set(marker, finish) }
      else finish()
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests,
    isHeld: marker => held.has(marker),
    release(marker) { held.get(marker)?.(); held.delete(marker) },
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }),
  }
}

async function assertAssets(origin, label, observations) {
  observations.assets[label] = {}
  for (const name of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${name}`)
    assert.equal(response.status, 200)
    const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
    const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', name))).digest('hex')
    assert.equal(served, expected, `${label} embeds current ${name}`)
    observations.assets[label][name] = served
  }
}

async function login(page, origin, account, tenantId, sessionId) {
  await page.goto(origin)
  await page.getByLabel('用户名', { exact: true }).fill(account.username)
  await page.getByLabel('密码', { exact: true }).fill(account.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await selectSpace(page, tenantId)
  await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
  await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
}

async function submit(page, input) {
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(input)
  const response = page.waitForResponse(response => response.request().method() === 'POST' && /\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname))
  await page.getByRole('button', { name: '发送', exact: true }).click()
  const accepted = await response
  assert.equal(accepted.status(), 201)
  return accepted.json()
}

function messages(events, text) { return events.filter(event => event.type === 'user_message' && event.content === text) }
function row(page, text) { return page.locator('article[data-role="user"]').filter({ hasText: text }) }
async function label(container, text) { await container.locator('[data-input-identity-label]').filter({ hasText: new RegExp(`^${text}$`) }).waitFor() }
function assertUnknown(value) { assert.equal(value.provenance?.author?.kind === 'account', false, 'unverified historical input must not claim a platform account') }

async function discover(request, executorId) {
  const state = await until(() => request('/state'), state => {
    const workspace = state.workspaces.find(workspace => workspace.node_id === executorId)
    return workspace && state.sessions.some(session => session.workspace_id === workspace.workspace_id)
  }, `discover ${executorId}`)
  const workspace = state.workspaces.find(workspace => workspace.node_id === executorId)
  return state.sessions.find(session => session.workspace_id === workspace.workspace_id).identity.session_id
}

function observe(page, observations) {
  page.on('pageerror', error => observations.errors.push(`page: ${error.message}`))
  page.on('console', message => { if (message.type() === 'error') observations.errors.push(`console: ${message.text()}`) })
  page.on('response', response => {
    const url = new URL(response.url())
    if (url.pathname.startsWith('/api/')) observations.responses.push({ method: response.request().method(), path: url.pathname, status: response.status() })
    if (response.status() >= 400) observations.errors.push(`HTTP ${response.status()} ${url.pathname}`)
  })
  page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) observations.errors.push(`request: ${request.url()} ${request.failure()?.errorText}`) })
}

test('re-enrolled Node keeps old account inputs readable without asserting an unverified identity', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-identity-reenrollment-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const observations = { assets: {}, errors: [], responses: [] }
  const model = await modelFixture()
  let dataRoot = path.join(directory, 'node')
  let currentComputerId
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const isolatedEnvironment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
  let application, node, browser, page, directOutcome
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const token = application.owner.session.access_token
    const account = application.owner.session.user
    const author = { kind: 'account', ...account }
    const ownerRequest = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const team = (await ownerRequest('/tenants', { body: { slug: 'reenrollment-team', display_name: 'Re-enrollment team' } })).tenant
    const request = (resource, options = {}) => ownerRequest(resource, { tenantId: team.tenant_id, ...options })
    const project = (await request('/projects')).projects[0]
    async function startNode(name) {
      const enrollment = (await ownerRequest(`/tenants/${team.tenant_id}/my-computer-enrollments`, { body: { name, project_id: project.project_id, ttl_seconds: 600 } })).enrollment
      const executorId = enrollment.executor_id
      currentComputerId = executorId
      const credential = (await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      node = startProcess(binary, ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', dataRoot, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway', '--node-id', executorId], { ...isolatedEnvironment, TERNILO_LOCAL_TOKEN: credential.token })
      await waitForHttp(nodeOrigin, node)
      await assertAssets(nodeOrigin, executorId, observations)
      const html = await (await fetch(nodeOrigin)).text()
      const boot = html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)
      assert.ok(boot, 'Local exposes its authenticated browser bootstrap')
      const apiToken = JSON.parse(boot[1]).apiToken
      return (resource, options = {}) => serverRequest(nodeOrigin, resource, { token: apiToken, ...options })
    }
    await assertAssets(origin, 'server', observations)
    let local = await startNode('identity-before')
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await local('/workspaces', { body: { path: workspacePath } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localSessionId = session.identity.session_id
    await local('/credentials', { body: { name: 'REENROLLMENT_MODEL_KEY', value: 'fixture-key' } })
    await local('/providers', { body: { id: 'reenrollment-fixture', display_name: 'Re-enrollment model', base_url: model.baseUrl, protocol: 'openai-responses', api_key_ref: 'REENROLLMENT_MODEL_KEY', defaults: { context_window: 128000, max_output_tokens: 4096 }, models: [{ id: 'identity-model', settings: { mode: 'inherit' } }], timeout_ms: 60000, max_attempts: 1, retry_base_delay_ms: 50 } })
    await local(`/sessions/${localSessionId}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'reenrollment-fixture', model: 'identity-model' } } })
    const oldSessionId = await discover(request, currentComputerId)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const contextOptions = { viewport: { width: 1440, height: 960 }, permissions: ['clipboard-read', 'clipboard-write'], serviceWorkers: 'block' }
    let context = await browser.newContext(contextOptions)
    page = await context.newPage(); observe(page, observations)
    await login(page, origin, application.owner, team.tenant_id, oldSessionId)
    const oldAccepted = await submit(page, OLD_MESSAGE)
    assert.deepEqual(oldAccepted.provenance.author, author)
    await until(() => request(`/sessions/${oldSessionId}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0, 'first verified input completes')
    await label(row(page, OLD_MESSAGE), '你')
    const held = await submit(page, OLD_HOLD)
    await until(async () => model.isHeld(OLD_HOLD), Boolean, 'old model call is held')
    const pending = await submit(page, OLD_PENDING)
    assert.deepEqual(pending.provenance.author, author)
    await request(`/sessions/${oldSessionId}/turns/${held.run_id}`, { method: 'DELETE' })
    await until(() => request(`/sessions/${oldSessionId}/queue`), inbox => inbox.paused && !inbox.active_run_id && inbox.items.length === 1, 'cancel leaves the accepted tail paused')
    model.release(OLD_HOLD)
    await context.close()
    await stopProcess(node); node = undefined
    await ownerRequest(`/tenants/${team.tenant_id}/my-computers/${currentComputerId}`, { method: 'DELETE' })
    const copiedRoot = path.join(directory,'independent-node')
    await cp(dataRoot,copiedRoot,{ recursive:true,filter: source => !source.endsWith('node-authorizations.json') && !source.includes(`${path.sep}runtime`) && !source.endsWith('.writer.lock') })
    dataRoot = copiedRoot
    local = await startNode('identity-after')
    const newSessionId = await discover(request, currentComputerId)
    assert.notEqual(newSessionId, oldSessionId, 'new executor receives an independent public mapping')
    const history = await until(() => request(`/sessions/${newSessionId}/events`), events => messages(events, OLD_MESSAGE).length === 1, 'historical output is readable through the new route')
    for (const event of history.filter(event => event.type === 'user_message')) assertUnknown(event)
    const queue = await request(`/sessions/${newSessionId}/queue`)
    assert.equal(queue.paused, true)
    assert.equal(queue.items.length, 1)
    assert.equal(queue.items[0].id, pending.id)
    assert.equal(queue.items[0].content.input, OLD_PENDING)
    assertUnknown(queue.items[0])
    const localQueue = await local(`/sessions/${localSessionId}/queue`)
    assert.deepEqual(localQueue.items[0].provenance.author, author, 'public projection does not rewrite retained Local data')
    const exported = await request(`/sessions/${newSessionId}/export`)
    assert.equal(messages(exported.events, OLD_MESSAGE).length, 1)
    for (const event of exported.events.filter(event => event.type === 'user_message')) assertUnknown(event)
    context = await browser.newContext(contextOptions)
    page = await context.newPage(); observe(page, observations)
    await login(page, origin, application.owner, team.tenant_id, newSessionId)
    await label(row(page, OLD_MESSAGE), '身份未记录')
    const pendingRow = page.locator(`[data-queued-submission="${pending.id}"]`)
    await label(pendingRow, '身份未记录')
    await row(page, OLD_MESSAGE).locator('[data-input-identity]').click()
    const unknown = page.getByRole('dialog', { name: '提交者', exact: true })
    await unknown.getByText('这条输入未记录提交者，无法确定是谁发送的。', { exact: true }).waitFor()
    assert.equal(await unknown.locator('[data-input-account-id]').count(), 0)
    await page.keyboard.press('Escape')
    await page.screenshot({ path: path.join(artifacts, 'reenrollment-unverified-history-and-queue.png'), animations: 'disabled' })
    directOutcome = request(`/sessions/${newSessionId}/turns`, { body: { run_id: DIRECT_RUN, input: DIRECT_MESSAGE } })
    void directOutcome.catch(() => {})
    await until(async () => model.isHeld(DIRECT_MESSAGE), Boolean, 'new direct turn reaches the controlled Provider')
    await label(row(page, DIRECT_MESSAGE), '你')
    const steeringResponse = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname.endsWith(`/queue/${pending.id}/steer`))
    await page.getByRole('button', { name: '停止并发送全部', exact: true }).click()
    const steered = await steeringResponse
    assert.equal(steered.status(), 200)
    assertUnknown(await steered.json())
    model.release(DIRECT_MESSAGE)
    await assert.rejects(directOutcome, /cancel/i)
    await until(() => request(`/sessions/${newSessionId}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0, 'interrupted turn and pending batch finish')
    const interruptedHistory = await request(`/sessions/${newSessionId}/export`)
    const direct = messages(interruptedHistory.events, DIRECT_MESSAGE).find(event => event.run_id === DIRECT_RUN)
    const retained = messages(interruptedHistory.events, OLD_PENDING).find(event => event.run_id !== DIRECT_RUN)
    assert.ok(direct && retained, 'the retained input starts a new turn after the direct run stops')
    assert.deepEqual(direct.provenance.author, author)
    assertUnknown(retained)
    await until(() => request(`/sessions/${newSessionId}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0, 'direct turn and retained steering finish')
    await label(row(page, OLD_PENDING), '身份未记录')
    const current = await submit(page, NEW_MESSAGE)
    assert.equal(current.provenance.input_id, current.id)
    assert.deepEqual(current.provenance.author, author)
    await until(() => request(`/sessions/${newSessionId}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0, 'new verified browser input completes')
    await page.reload()
    await label(row(page, OLD_MESSAGE), '身份未记录')
    await label(row(page, OLD_PENDING), '身份未记录')
    await label(row(page, NEW_MESSAGE), '你')
    const finalExport = await request(`/sessions/${newSessionId}/export`)
    for (const text of [OLD_MESSAGE, OLD_HOLD, OLD_PENDING]) {
      const old = messages(finalExport.events, text)
      assert.equal(old.length, 1)
      assertUnknown(old[0])
    }
    assert.deepEqual(messages(finalExport.events, NEW_MESSAGE)[0].provenance.author, author)
    const original = messages(await local(`/sessions/${localSessionId}/events`), OLD_MESSAGE)[0]
    assert.deepEqual(original.provenance, oldAccepted.provenance, 'original local authors remain intact after replay and steering')
    await page.setViewportSize({ width: 390, height: 844 })
    await row(page, NEW_MESSAGE).locator('[data-input-identity]').click()
    const identity = page.getByRole('dialog', { name: '提交者', exact: true })
    assert.equal(await identity.locator('[data-input-account-id]').textContent(), account.user_id)
    await identity.getByRole('button', { name: '复制账号 ID', exact: true }).click()
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), account.user_id)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await page.screenshot({ path: path.join(artifacts, 'reenrollment-current-identity-mobile.png'), animations: 'disabled' })
    assert.deepEqual(observations.errors, [])
    observations.sessions = { old: oldSessionId, current: newSessionId, local: localSessionId }
    observations.inputs = { old: oldAccepted.id, retained: pending.id, current: current.id }
  } catch (error) {
    if (page && !page.isClosed()) await page.screenshot({ path: path.join(artifacts, 'reenrollment-failure.png'), animations: 'disabled' }).catch(() => {})
    process.stderr.write(`${application?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}\n`)
    throw error
  } finally {
    model.release(OLD_HOLD); model.release(DIRECT_MESSAGE)
    await writeFile(path.join(artifacts, 'reenrollment-observations.json'), JSON.stringify({ ...observations, modelRequests: model.requests.length }, null, 2))
    await browser?.close()
    await stopProcess(node)
    await stopProcess(application)
    await directOutcome?.catch(() => {})
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
