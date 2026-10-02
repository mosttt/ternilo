import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { homedir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { verifyNodeServices } from './node-services-fixture.mjs'
import { verifyWorkspaceLocation } from './workspace-location-fixture.mjs'

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
  let release, held = false
  const records = []
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') { response.writeHead(404); response.end(); return }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      records.push(body)
      const user = body.input?.filter(item => item.role === 'user').at(-1)
      const input = user?.content?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      const title = body.instructions?.includes('You name software-agent conversations')
      const text = title ? 'Shared identity' : `Identity fixture: ${input}`
      const finish = () => {
        if (response.destroyed) return
        const sse = event => `data: ${JSON.stringify(event)}\n\n`
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text })
          + sse({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }], usage: { input_tokens: 20, output_tokens: 10 } } }))
      }
      if (!title && !held && input.includes('owner-hold-task')) { held = true; release = finish }
      else finish()
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, records, held: () => held, release: () => { release?.(); release = undefined }, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
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

function message(page, text) {
  return page.locator('article[data-role="user"]').filter({ hasText: text })
}

async function assertLabel(container, expected) {
  await container.locator('[data-input-identity-label]').filter({ hasText: new RegExp(`^${expected}$`) }).waitFor()
}

async function submit(page, input) {
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(input)
  const response = page.waitForResponse(response => response.request().method() === 'POST' && /\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname))
  await page.getByRole('button', { name: '发送', exact: true }).click()
  const accepted = await response
  assert.equal(accepted.status(), 201)
  return accepted.json()
}

async function inspectIdentity(page, container, account, keyboard = false) {
  assert.equal((await container.textContent()).includes(account.user_id), false, 'the ID is not shown inline')
  const trigger = container.locator('[data-input-identity]')
  if (keyboard) { await trigger.focus(); await page.keyboard.press('Enter') }
  else await trigger.click()
  const details = page.getByRole('dialog', { name: '提交者', exact: true })
  await details.getByText(account.username, { exact: true }).waitFor()
  assert.equal(await details.locator('[data-input-account-id]').textContent(), account.user_id)
  await details.getByRole('button', { name: '复制账号 ID', exact: true }).click()
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), account.user_id)
  const geometry = await details.boundingBox()
  assert.ok(geometry && geometry.x >= 0 && geometry.x + geometry.width <= page.viewportSize().width)
  return details
}

test('shared Node input keeps each submitter visible in chat, pending queue and steering across two accounts', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(homedir(), '.ternilo-input-identity-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const model = await modelFixture()
  let application, node, browser, owner, member, localRequest, localSessionId
  const errors = [], assetHashes = {}
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const token = application.owner.session.access_token
    const ownerRequest = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const invitation = await ownerRequest('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const collaborator = { username: `member-${'long-name-'.repeat(5)}actor`, password: 'identity-member-password' }
    const memberIdentity = await serverRequest(origin, '/auth/invitations/accept', { body: { token: invitation.token, email: `${collaborator.username}@example.test`, ...collaborator } })
    const team = (await ownerRequest('/tenants', { body: { slug: 'identity-team', display_name: 'Identity team' } })).tenant
    await ownerRequest(`/tenants/${team.tenant_id}/members/${memberIdentity.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const project = (await ownerRequest('/projects', { tenantId: team.tenant_id })).projects[0]
    const enrollment = (await ownerRequest(`/tenants/${team.tenant_id}/my-computer-enrollments`, { body: { name: 'identity-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const enrolledComputerId = enrollment.executor_id
    const credential = (await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    node = startProcess(process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway', '--node-id', enrolledComputerId], { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    await waitForHttp(nodeOrigin, node)
    for (const [endpoint, baseUrl] of Object.entries({ server: origin, local: nodeOrigin })) {
      assetHashes[endpoint] = {}
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${baseUrl}/assets/${asset}`)
        assert.equal(response.status, 200)
        const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
        const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
        assert.equal(served, expected, `${endpoint} embeds current ${asset}`)
        assetHashes[endpoint][asset] = served
      }
    }
    const html = await (await fetch(nodeOrigin)).text()
    const nodeToken = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    localRequest = (resource, options = {}) => serverRequest(nodeOrigin, resource, { token: nodeToken, ...options })
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await localRequest('/workspaces', { body: { path: workspacePath } })
    const localSession = await localRequest('/sessions', { body: { workspace_id: workspace.workspace_id } })
    localSessionId = localSession.identity.session_id
    await localRequest('/credentials', { body: { name: 'IDENTITY_MODEL_KEY', value: 'fixture-key' } })
    await localRequest('/providers', { body: { id: 'identity-fixture', display_name: 'Identity model', base_url: model.baseUrl, protocol: 'openai-responses', api_key_ref: 'IDENTITY_MODEL_KEY', defaults: { context_window: 128000, max_output_tokens: 4096 }, models: [{ id: 'identity-model', settings: { mode: 'inherit' } }], timeout_ms: 60000, max_attempts: 1, retry_base_delay_ms: 50 } })
    await localRequest(`/sessions/${localSessionId}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'identity-fixture', model: 'identity-model' } } })
    await localRequest(`/sessions/${localSessionId}/queue`, { body: { content: { kind: 'prompt', input: 'local-origin-task' } } })
    await until(() => localRequest(`/sessions/${localSessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'local task completion')
    const state = await until(() => ownerRequest('/state', { tenantId: team.tenant_id }), state => state.sessions.length === 1, 'Node session discovery')
    const sessionId = state.sessions[0].identity.session_id
    await ownerRequest(`/sessions/${sessionId}/sharing/user/${memberIdentity.user.user_id}`, { tenantId: team.tenant_id, method: 'PUT', body: { view: true, submit: true, stop: true, configure: false } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const contexts = await Promise.all([0, 1].map(() => browser.newContext({ viewport: { width: 1440, height: 960 }, permissions: ['clipboard-read', 'clipboard-write'], serviceWorkers: 'block' })))
    ;[owner, member] = await Promise.all(contexts.map(context => context.newPage()))
    for (const page of [owner, member]) {
      page.on('pageerror', error => errors.push(`page: ${error.message}`))
      page.on('console', message => { if (message.type() === 'error') errors.push(`console: ${message.text()}`) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push(`request: ${request.url()} ${request.failure()?.errorText}`) })
    }
    await login(owner, origin, application.owner, team.tenant_id, sessionId)
    await login(member, origin, collaborator, team.tenant_id, sessionId)
    const memberInput = member.locator('[data-composer-input]')
    await memberInput.fill('/')
    const memberMenu = member.locator('[data-composer-menu]')
    await memberMenu.getByRole('option').first().waitFor()
    for (const command of ['/model', '/permission', '/plan']) {
      assert.equal(await memberMenu.locator('code').filter({ hasText: new RegExp(`^${command}$`) }).count(), 0)
    }
    await memberInput.press('Escape')
    await memberInput.fill('')
    await assert.rejects(serverRequest(origin, `/sessions/${sessionId}`, {
      token: memberIdentity.access_token, tenantId: team.tenant_id, method: 'PATCH', body: { mode: 'plan' },
    }), /403/)
    await verifyWorkspaceLocation({ owner, member, ownerRequest, tenantId: team.tenant_id, memberIdentity,
      workspace: state.workspaces.find(value => value.workspace_id === state.sessions[0].workspace_id),
      localWorkspace: workspace, workspacePath, nodeOrigin, configPath: application.configPath, artifacts })
    await assertLabel(message(owner, 'local-origin-task'), '本机用户')
    await assertLabel(message(member, 'local-origin-task'), '本机用户')
    const first = await submit(owner, 'owner-hold-task')
    assert.deepEqual(first.provenance.author, { kind: 'account', ...application.owner.session.user })
    assert.equal(JSON.stringify(first.provenance).includes(application.owner.email), false)
    await until(async () => model.held(), held => held, 'held model request')
    await assertLabel(message(owner, 'owner-hold-task'), '你')
    await assertLabel(message(member, 'owner-hold-task'), application.owner.username)
    await inspectIdentity(member, message(member, 'owner-hold-task'), application.owner.session.user, true)
    await member.keyboard.press('Escape')
    await owner.getByRole('textbox', { name: '输入任务', exact: true }).fill('owner-private-draft')
    const second = await submit(member, 'member-steering-task')
    assert.deepEqual(second.provenance.author, { kind: 'account', ...memberIdentity.user })
    assert.equal(JSON.stringify(second.provenance).includes(memberIdentity.email), false)
    assert.equal(await owner.getByRole('textbox', { name: '输入任务', exact: true }).inputValue(), 'owner-private-draft')
    await assertLabel(owner.locator(`[data-queued-submission="${second.id}"]`), collaborator.username)
    await assertLabel(member.locator(`[data-queued-submission="${second.id}"]`), '你')
    assert.equal(await member.locator('[data-current-task]').count(), 0)
    await member.getByRole('button', { name: '停止并发送全部', exact: true }).click()
    model.release()
    await assertLabel(message(owner, 'member-steering-task'), collaborator.username)
    await assertLabel(message(member, 'member-steering-task'), '你')
    await until(() => ownerRequest(`/sessions/${sessionId}/queue`, { tenantId: team.tenant_id }), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'shared turn completion')
    assert.equal(await message(owner, 'member-steering-task').count(), 1)
    assert.equal(await message(member, 'member-steering-task').count(), 1)
    await owner.screenshot({ path: path.join(artifacts, 'input-identities-desktop.png'), animations: 'disabled' })
    for (const width of [390, 320]) {
      await owner.setViewportSize({ width, height: 844 })
      const details = await inspectIdentity(owner, message(owner, 'member-steering-task'), memberIdentity.user)
      assert.equal(await owner.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await owner.screenshot({ path: path.join(artifacts, `input-identity-${width}.png`), animations: 'disabled' })
      await owner.keyboard.press('Escape')
      await details.waitFor({ state: 'hidden' })
    }
    await member.reload()
    await assertLabel(message(member, 'member-steering-task'), '你')
    await assertLabel(message(member, 'owner-hold-task'), application.owner.username)
    await verifyNodeServices({ origin, owner, member, ownerRequest, localRequest,
      tenantId: team.tenant_id, memberIdentity, sessionId, localSessionId, workspacePath, artifacts, model })
    assert.deepEqual(errors, [])
  } catch (error) {
    if (localRequest && localSessionId) {
      const diagnostics = await Promise.allSettled(['queue', 'events', 'services'].map(resource => localRequest(`/sessions/${localSessionId}/${resource}`)))
      await writeFile(path.join(artifacts, 'local-startup-diagnostics.json'), JSON.stringify(diagnostics, null, 2))
    }
    await owner?.screenshot({ path: path.join(artifacts, 'input-identity-owner-failure.png'), animations: 'disabled' }).catch(() => {})
    await member?.screenshot({ path: path.join(artifacts, 'input-identity-member-failure.png'), animations: 'disabled' }).catch(() => {})
    process.stderr.write(`${application?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}\n`)
    throw error
  } finally {
    model.release()
    await writeFile(path.join(artifacts, 'input-identity-observations.json'), JSON.stringify({ assetHashes, errors, modelRequests: model.records.length }, null, 2))
    await browser?.close()
    await stopProcess(node)
    await stopProcess(application)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
