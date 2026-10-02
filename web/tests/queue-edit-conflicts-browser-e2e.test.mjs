import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

test('Server accounts and Local preserve queue drafts, reject stale revisions and recover explicitly', { timeout: 180_000 }, async context => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-queue-edit-'))
  const processes = [], pages = [], pending = new Set()
  const responseTasks = [], pageAccounts = new Map(), requests = new Map()
  const observations = { errors: [], console: [], network: [], requestFailures: [], screenshots: [], checks: [], assets: [], processes: [], transitions: [] }
  const capture = async (page, name) => {
    if (!process.env.TERNILO_E2E_ARTIFACT_DIR) return
    await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
    const screenshot = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${name}.png`)
    await page.screenshot({ path: screenshot, fullPage: true })
    observations.screenshots.push(screenshot)
  }
  let browser, model
  context.after(async () => {
    for (const [index, page] of pages.entries()) {
      if (!page.isClosed()) await capture(page, `final-page-${index}`).catch(error => observations.errors.push(error.message))
    }
    await browser?.close()
    await Promise.all(responseTasks)
    for (const process of processes.reverse()) {
      await stopProcess(process)
      observations.processes.push({ pid: process.child.pid, exitCode: process.child.exitCode, signalCode: process.child.signalCode })
    }
    if (model) await new Promise(resolve => { model.closeAllConnections(); model.close(resolve) })
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await writeFile(path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'queue-edit-conflicts.json'), JSON.stringify(observations, null, 2))
      await writeFile(path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'queue-processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    }
    await rm(directory, { recursive: true, force: true })
  })
  const environment = { XDG_STATE_HOME: path.join(directory, 'state'), XDG_CONFIG_HOME: path.join(directory, 'config'), XDG_DATA_HOME: path.join(directory, 'data') }
  const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, environment })
  processes.push(server)
  let tenantId = server.owner.session.personal_tenant_id
  const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
  const registration = await owner('/admin/registration')
  await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
  const credentials = { username: 'queue-member', email: 'queue-member@example.test', password: 'queue-member-password' }
  const { session: account } = await serverRequest(server.origin, '/auth/register', { body: credentials })
  const { tenant } = await owner('/tenants', { body: { slug: 'queue-team', display_name: 'Queue team' } })
  tenantId = tenant.tenant_id
  await owner(`/tenants/${tenantId}/members/${account.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
  const member = (resource, options = {}) => serverRequest(server.origin, resource, { token: account.access_token, tenantId, ...options })
  const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'queue-node', project_id: null, ttl_seconds: 600 } })
  const enrolledComputerId = enrolled.enrollment.executor_id
  const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
  const localOrigin = `http://127.0.0.1:${await freePort()}`
  const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
    'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'local'), '--node-id', enrolledComputerId,
    '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
  ], { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
  processes.push(node)
  await waitForHttp(localOrigin, node)
  const html = await (await fetch(localOrigin)).text()
  const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
  const local = (resource, options = {}) => serverRequest(localOrigin, resource, { token, ...options })
  for (const origin of [server.origin, localOrigin]) {
    for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      observations.assets.push({ origin, asset, sha256: createHash('sha256').update(served).digest('hex') })
    }
  }
  model = createServer(async (request, response) => {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const payload = JSON.parse(Buffer.concat(chunks).toString())
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    const frame = value => `data: ${JSON.stringify(value)}\n\n`
    if (payload.messages.some(message => message.role === 'system' && String(message.content).includes('You name software-agent conversations'))) {
      response.end(frame({ choices: [{ delta: { content: 'Queue conflicts' }, finish_reason: 'stop' }] }) + 'data: [DONE]\n\n')
      return
    }
    pending.add(response)
    response.once('close', () => pending.delete(response))
    response.write(frame({ choices: [{ delta: { content: 'Holding the active task.' } }] }))
  })
  await new Promise(resolve => model.listen(0, '127.0.0.1', resolve))
  await local('/providers', { body: { id: 'queue-proof', display_name: 'Queue proof', base_url: `http://127.0.0.1:${model.address().port}/v1`, protocol: 'openai-chat-completions',
    defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'queue-model', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25 } })
  const folder = path.join(directory, 'workspace')
  await mkdir(folder)
  const workspace = await local('/workspaces', { body: { path: folder } })
  const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
  const localId = session.identity.session_id
  await local(`/sessions/${localId}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'queue-proof', model: 'queue-model' } } })
  const state = await until(() => owner('/state'), state => state.sessions.length === 1, 'Node session sync')
  const serverId = state.sessions[0].identity.session_id
  const sharedPath = `/sessions/${serverId}/sharing/user/${account.user.user_id}`
  await owner(sharedPath, { method: 'PUT', body: { view: true, submit: true } })
  const serverQueue = `/sessions/${serverId}/queue`, localQueue = `/sessions/${localId}/queue`
  await owner(serverQueue, { body: { content: { kind: 'prompt', input: 'Hold queue' } } })
  await until(() => pending.size, size => size === 1, 'active model request')
  const accepted = await member(serverQueue, { body: { content: { kind: 'prompt', input: 'Shared original' } } })
  const revision = accepted.updated_at_ms
  const outcomes = await Promise.allSettled([
    owner(`${serverQueue}/${accepted.id}`, { method: 'PATCH', body: { input: 'Owner race', expected_updated_at_ms: revision } }),
    member(`${serverQueue}/${accepted.id}`, { method: 'PATCH', body: { input: 'Member race', expected_updated_at_ms: revision } }),
  ])
  assert.equal(outcomes.filter(outcome => outcome.status === 'fulfilled').length, 1)
  assert.match(outcomes.find(outcome => outcome.status === 'rejected').reason.message, /409/)
  const winner = outcomes.find(outcome => outcome.status === 'fulfilled').value
  assert.ok(winner.updated_at_ms > revision)
  assert.deepEqual(winner.provenance, accepted.provenance)
  assert.deepEqual((await local(localQueue)).items.find(item => item.id === winner.id), winner)
  observations.checks.push({ name: 'same-revision-race', passed: true, revision, winnerRevision: winner.updated_at_ms })
  for (const [client, queue] of [[owner, serverQueue], [local, localQueue]]) {
    await assert.rejects(() => client(`${queue}/${winner.id}`, { method: 'PATCH', body: { input: 'Unconditional edit' } }), /400/)
  }
  observations.checks.push({ name: 'missing-revision-local-and-server', passed: true })
  browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
  const open = async (origin, sessionId, login) => {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    pages.push(page)
    page.setDefaultTimeout(15_000)
    page.on('pageerror', error => observations.errors.push(error.message))
    const pageId = pages.length - 1
    page.on('console', message => observations.console.push({ pageId, atMs: Date.now(), type: message.type(), text: message.text(), location: message.location() }))
    page.on('request', request => {
      const url = new URL(request.url())
      const record = {
        requestId: observations.network.length, pageId, method: request.method(), path: url.pathname, query: url.search,
        startedAtMs: Date.now(), phase: observations.phase, tenant: request.headers()['x-ternilo-tenant'] ?? null,
        tokenFingerprint: fingerprint(request.headers().authorization ?? ''),
      }
      requests.set(request, record)
      observations.network.push(record)
    })
    page.on('response', response => {
      const record = requests.get(response.request())
      Object.assign(record, { status: response.status(), receivedAtMs: Date.now() })
      if (record.path === '/api/v1/auth/login' && response.ok()) {
        responseTasks.push(response.json().then(session => {
          pageAccounts.set(page, session)
          record.account = { userId: session.user.user_id, username: session.user.username, personalTenant: session.personal_tenant_id, tokenFingerprint: fingerprint(`Bearer ${session.access_token}`) }
        }).catch(error => { record.bodyError = error.message }))
      } else if (response.status() >= 400) {
        responseTasks.push(response.json().then(body => { record.errorBody = body }).catch(error => { record.bodyError = error.message }))
      }
    })
    page.on('requestfinished', request => {
      const record = requests.get(request)
      Object.assign(record, { finishedAtMs: Date.now(), timing: request.timing() })
    })
    page.on('requestfailed', request => {
      const record = requests.get(request)
      Object.assign(record, { failedAtMs: Date.now(), failure: request.failure()?.errorText })
      observations.requestFailures.push(record)
    })
    await page.goto(origin)
    if (login) {
      await page.getByLabel('用户名', { exact: true }).fill(login.username)
      await page.getByLabel('密码', { exact: true }).fill(login.password)
      await page.getByRole('button', { name: '登录', exact: true }).click()
      await page.getByRole('dialog').waitFor({ state: 'hidden' })
      await selectSpace(page, tenantId)
    }
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).locator('button').first().click()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Independent composer draft')
    return page
  }
  const ownerPage = await open(server.origin, serverId, server.owner)
  const memberPage = await open(server.origin, serverId, credentials)
  const localPage = await open(localOrigin, localId)
  await recoverConflict(ownerPage, serverQueue, member, serverQueue, winner.id, 'load', page => capture(page, 'server-owner-conflict'))
  observations.checks.push({ name: 'server-owner-explicit-load', passed: true })
  await recoverConflict(memberPage, serverQueue, owner, serverQueue, winner.id, 'cancel', page => capture(page, 'server-member-conflict'))
  observations.checks.push({ name: 'server-member-explicit-cancel', passed: true })
  await recoverConflict(localPage, localQueue, owner, serverQueue, winner.id, 'load', page => capture(page, 'local-conflict'))
  observations.checks.push({ name: 'local-explicit-load', passed: true })
  const otherSession = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
  const sessionDraft = await recoverConflict(localPage, localQueue, owner, serverQueue, winner.id, 'retain', page => capture(page, 'before-session-switch'))
  await selectSession(localPage, otherSession.identity.session_id)
  assert.equal(await localPage.getByRole('textbox', { name: '编辑排队消息', exact: true }).count(), 0)
  assert.equal((await localPage.locator('body').innerText()).includes(sessionDraft), false)
  await selectSession(localPage, localId)
  await localPage.locator(`[data-queued-submission="${winner.id}"]`).waitFor()
  assert.equal(await localPage.getByRole('textbox', { name: '编辑排队消息', exact: true }).count(), 0)
  await capture(localPage, 'after-session-switch')
  observations.checks.push({ name: 'session-switch-isolation', passed: true })
  const accountDraft = await recoverConflict(ownerPage, serverQueue, member, serverQueue, winner.id, 'retain', page => capture(page, 'before-account-switch'))
  await Promise.all(responseTasks)
  const outgoingAccount = pageAccounts.get(ownerPage)
  assert.equal(outgoingAccount.user.user_id, server.owner.session.user.user_id)
  observations.phase = 'account-signout'
  observations.transitions.push({ event: 'signout-start', atMs: Date.now(), pageId: 0, userId: outgoingAccount.user.user_id, tokenFingerprint: fingerprint(`Bearer ${outgoingAccount.access_token}`), personalTenant: outgoingAccount.personal_tenant_id, sharedTenant: tenantId, sharedSession: serverId })
  await ownerPage.getByRole('button', { name: '退出登录', exact: true }).click()
  await ownerPage.getByLabel('用户名', { exact: true }).waitFor()
  observations.transitions.push({ event: 'signin-form-visible', atMs: Date.now(), pageId: 0 })
  const revoked = await fetch(`${server.origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${outgoingAccount.access_token}` } })
  observations.revokedSession = { atMs: Date.now(), status: revoked.status, body: await revoked.json() }
  assert.equal(revoked.status, 401)
  await ownerPage.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await ownerPage.getByLabel('密码', { exact: true }).fill(credentials.password)
  observations.phase = 'account-signin'
  observations.transitions.push({ event: 'signin-start', atMs: Date.now(), pageId: 0, username: credentials.username })
  await ownerPage.getByRole('button', { name: '登录', exact: true }).click()
  await ownerPage.getByRole('dialog').waitFor({ state: 'hidden' })
  await Promise.all(responseTasks)
  const incomingAccount = pageAccounts.get(ownerPage)
  assert.equal(incomingAccount.user.user_id, account.user.user_id)
  observations.transitions.push({ event: 'signin-complete', atMs: Date.now(), pageId: 0, userId: incomingAccount.user.user_id, tokenFingerprint: fingerprint(`Bearer ${incomingAccount.access_token}`), personalTenant: incomingAccount.personal_tenant_id })
  await selectSpace(ownerPage, tenantId)
  await selectSession(ownerPage, serverId)
  await ownerPage.locator(`[data-queued-submission="${winner.id}"]`).waitFor()
  assert.equal(await ownerPage.getByRole('textbox', { name: '编辑排队消息', exact: true }).count(), 0)
  assert.equal((await ownerPage.locator('body').innerText()).includes(accountDraft), false)
  await capture(ownerPage, 'after-account-switch')
  observations.checks.push({ name: 'account-switch-isolation', passed: true })
  observations.phase = 'permission-and-removal'
  const current = (await owner(serverQueue)).items.find(item => item.id === winner.id)
  await owner(sharedPath, { method: 'PUT', body: { view: true, submit: false } })
  await assert.rejects(() => member(`${serverQueue}/${winner.id}`, { method: 'PATCH', body: { input: 'No permission', expected_updated_at_ms: current.updated_at_ms } }), /403/)
  assert.deepEqual((await owner(serverQueue)).items.find(item => item.id === winner.id), current)
  observations.checks.push({ name: 'revoked-submit-permission', passed: true })
  await rejectDeleted(localPage, localQueue, owner, serverQueue, winner.id, page => capture(page, 'deleted-draft-retained'))
  observations.checks.push({ name: 'deleted-draft-retained-and-cancelled', passed: true })
  await Promise.all(responseTasks)
  assert.deepEqual(observations.errors, [])
  const revocation = auditAccountSwitch(observations, server.origin)
  observations.accountSwitchAudit = revocation
  observations.checks.push({ name: 'scoped-revocation-and-account-request-boundary', passed: true })
  const failed = observations.network.filter(response => response.status >= 400 && !revocation.requestIds.includes(response.requestId))
  assert.deepEqual(failed.map(response => response.status), [409, 409, 409, 409, 409, 400])
  assert.ok(failed.every(response => response.method === 'PATCH' && response.path.includes('/queue/')))
  const consoleErrors = observations.console.filter((message, index) => message.type === 'error' && !revocation.consoleIndices.includes(index))
  for (const message of consoleErrors) assert.match(message.text, /Failed to load resource: the server responded with a status of (409|400)/)
  assert.equal(consoleErrors.length, failed.length)
  observations.completed = true
  context.diagnostic('Verified real HTTP conflicts, three independent browser contexts, explicit recovery and retained drafts; no application build performed.')
})

function fingerprint(authorization) {
  return authorization ? createHash('sha256').update(authorization).digest('hex') : null
}

function auditAccountSwitch(observations, origin) {
  const outgoing = observations.transitions.find(transition => transition.event === 'signout-start')
  const signinStart = observations.transitions.find(transition => transition.event === 'signin-start')
  const incoming = observations.transitions.find(transition => transition.event === 'signin-complete')
  const logout = observations.network.find(request => request.pageId === outgoing.pageId
    && request.path === '/api/v1/auth/logout' && request.startedAtMs >= outgoing.atMs)
  const login = observations.network.find(request => request.pageId === incoming.pageId
    && request.path === '/api/v1/auth/login' && request.startedAtMs >= signinStart.atMs)
  assert.equal(logout.status, 204)
  assert.equal(logout.method, 'POST')
  assert.equal(logout.tokenFingerprint, outgoing.tokenFingerprint)
  assert.equal(login.status, 200)
  assert.equal(login.account.userId, incoming.userId)
  assert.notEqual(incoming.tokenFingerprint, outgoing.tokenFingerprint)
  const expired = { error: { code: 'policy_denied', message: 'browser session is invalid or expired' } }
  assert.deepEqual(observations.revokedSession.body, expired)
  const revoked = observations.network.filter(request => request.status === 401)
  const seenTargets = new Set(), consoleIndices = []
  for (const request of revoked) {
    assert.equal(request.pageId, outgoing.pageId)
    assert.equal(request.method, 'GET')
    assert.equal(request.tokenFingerprint, outgoing.tokenFingerprint)
    assert.ok(request.startedAtMs >= logout.startedAtMs && request.startedAtMs <= logout.receivedAtMs, 'only requests already in flight at logout completion may be revoked')
    assert.ok(request.receivedAtMs >= logout.receivedAtMs && request.receivedAtMs < signinStart.atMs, 'revoked replies must precede the next login')
    assert.deepEqual(request.errorBody, expired)
    assert.equal(seenTargets.has(request.path), false, 'no repeated revoked requests')
    seenTargets.add(request.path)
    if (request.path === '/api/v1/model-options') {
      assert.equal(request.query, `?limit=25&session_id=${outgoing.sharedSession}`)
      assert.equal(request.tenant, outgoing.sharedTenant)
    } else {
      assert.ok(['/api/v1/providers', '/api/v1/credentials'].includes(request.path))
      assert.equal(request.query, '')
      assert.equal(request.tenant, outgoing.personalTenant)
    }
    const matches = observations.console.flatMap((message, index) => message.type === 'error'
      && message.pageId === outgoing.pageId && message.location.url === `${origin}${request.path}${request.query}`
      && message.atMs >= request.startedAtMs && message.atMs < signinStart.atMs
      && message.text === 'Failed to load resource: the server responded with a status of 401 (Unauthorized)'
      ? [index] : [])
    assert.equal(matches.length, 1, 'each revoked response must match exactly one console diagnostic')
    consoleIndices.push(matches[0])
  }
  const incomingRequests = observations.network.filter(request => request.pageId === incoming.pageId
    && request.startedAtMs >= login.receivedAtMs && request.path.startsWith('/api/v1/'))
  assert.ok(incomingRequests.length > 0)
  for (const request of incomingRequests) {
    assert.equal(request.tokenFingerprint, incoming.tokenFingerprint, 'new account never sends the previous credential')
    assert.ok(request.tenant === null || [incoming.personalTenant, outgoing.sharedTenant].includes(request.tenant), 'new account never addresses the previous personal space')
    const sessionId = new URLSearchParams(request.query).get('session_id') ?? request.path.match(/^\/api\/v1\/sessions\/([^/]+)/)?.[1]
    if (sessionId) assert.equal(sessionId, outgoing.sharedSession, 'only the explicitly shared session may be read after switching accounts')
  }
  const lateOutgoing = observations.network.filter(request => request.pageId === outgoing.pageId
    && request.tokenFingerprint === outgoing.tokenFingerprint && request.receivedAtMs >= login.receivedAtMs)
  assert.deepEqual(lateOutgoing, [], 'previous account responses do not cross the new login boundary')
  return { requestIds: revoked.map(request => request.requestId), consoleIndices, incomingRequestsChecked: incomingRequests.length, lateOutgoingResponses: lateOutgoing.length }
}

async function selectSession(page, id) {
  await page.locator(`[data-sidebar-session-row][data-session-id="${id}"]`).locator('button').first().click()
  await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
}

async function recoverConflict(page, queue, competitor, competitorQueue, id, recovery, capture) {
  const row = page.locator(`[data-queued-submission="${id}"]`)
  const original = (await competitor(competitorQueue)).items.find(item => item.id === id)
  await row.filter({ hasText: original.content.input }).waitFor()
  await row.getByRole('button', { name: '编辑排队消息', exact: true }).click()
  const editor = row.getByRole('textbox', { name: '编辑排队消息', exact: true })
  const draft = `Draft for ${queue} ${recovery}`
  await editor.fill(draft)
  let competing, patchCount = 0
  const resource = `/api/v1${queue}/${id}`
  const matcher = url => url.pathname === resource
  await page.route(matcher, async route => {
    if (route.request().method() !== 'PATCH') return route.continue()
    patchCount += 1
    const body = route.request().postDataJSON()
    competing = await competitor(`${competitorQueue}/${id}`, { method: 'PATCH', body: { input: `Competing revision ${body.expected_updated_at_ms}`, expected_updated_at_ms: body.expected_updated_at_ms } })
    assert.ok(competing.updated_at_ms > body.expected_updated_at_ms)
    await route.continue()
  })
  const rejected = page.waitForResponse(response => new URL(response.url()).pathname === resource && response.request().method() === 'PATCH')
  await row.getByRole('button', { name: '保存排队消息', exact: true }).click()
  assert.equal((await rejected).status(), 409)
  await row.getByRole('alert').filter({ hasText: '已被修改' }).waitFor()
  await page.unroute(matcher)
  assert.equal(await editor.inputValue(), draft)
  assert.equal(await row.getByRole('button', { name: '保存排队消息', exact: true }).isDisabled(), true)
  assert.equal(patchCount, 1)
  assert.deepEqual((await competitor(competitorQueue)).items.find(item => item.id === id), competing)
  await capture(page)
  if (recovery === 'retain') return draft
  if (recovery === 'load') {
    await row.getByRole('button', { name: '加载新版本', exact: true }).click()
    await until(() => editor.inputValue(), value => value === competing.content.input, 'explicit latest revision')
    await editor.fill('Resolved ' + draft)
    const saved = page.waitForResponse(response => new URL(response.url()).pathname === resource && response.request().method() === 'PATCH')
    await row.getByRole('button', { name: '保存排队消息', exact: true }).click()
    const response = await saved
    assert.equal(response.status(), 200)
    assert.equal(response.request().postDataJSON().expected_updated_at_ms, competing.updated_at_ms)
    const updated = await response.json()
    assert.ok(updated.updated_at_ms > competing.updated_at_ms)
    assert.equal(updated.content.input, 'Resolved ' + draft)
  } else {
    await row.getByRole('button', { name: '取消编辑', exact: true }).click()
  }
  await editor.waitFor({ state: 'detached' })
  assert.equal(await page.getByRole('textbox', { name: '输入任务', exact: true }).inputValue(), 'Independent composer draft')
}

async function rejectDeleted(page, queue, competitor, competitorQueue, id, capture) {
  const row = page.locator(`[data-queued-submission="${id}"]`)
  await row.getByRole('button', { name: '编辑排队消息', exact: true }).click()
  const editor = row.getByRole('textbox', { name: '编辑排队消息', exact: true })
  await editor.fill('Draft survives removal')
  const resource = `/api/v1${queue}/${id}`
  const matcher = url => url.pathname === resource
  await page.route(matcher, async route => {
    if (route.request().method() === 'PATCH') await competitor(`${competitorQueue}/${id}`, { method: 'DELETE' })
    await route.continue()
  })
  const rejected = page.waitForResponse(response => new URL(response.url()).pathname === resource && response.request().method() === 'PATCH')
  await row.getByRole('button', { name: '保存排队消息', exact: true }).click()
  assert.equal((await rejected).status(), 400)
  await row.getByRole('alert').filter({ hasText: '无法保存' }).waitFor()
  assert.equal(await editor.inputValue(), 'Draft survives removal')
  assert.equal(await row.getByRole('button', { name: '保存排队消息', exact: true }).isDisabled(), true)
  await capture(page)
  await row.getByRole('button', { name: '取消编辑', exact: true }).click()
  await row.waitFor({ state: 'detached' })
  await page.unroute(matcher)
}
