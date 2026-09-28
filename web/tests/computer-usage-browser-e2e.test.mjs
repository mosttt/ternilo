import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { body, freePort, initializeServer, json, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function upstreamFixture() {
  let calls = 0
  const server = createServer(async (request, response) => {
    if (request.url !== '/v1/chat/completions') return json(response, 404, {})
    const input = JSON.parse(await body(request))
    calls++
    response.setHeader('x-request-id', `device-upstream-${calls}`)
    if (calls === 1) return json(response, 503, { error: { message: 'private-upstream-error' }, usage: { prompt_tokens: 7 } })
    const content = 'Device usage complete.'
    const usage = { prompt_tokens: 11, completion_tokens: 5, prompt_tokens_details: { cached_tokens: 3 }, completion_tokens_details: { reasoning_tokens: 2 } }
    const identity = { id: `usage-${calls}`, model: input.model, created: 1 }
    if (!input.stream) return json(response, 200, { ...identity, choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })
    const sse = value => `data: ${JSON.stringify({ ...identity, object: 'chat.completion.chunk', ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(sse({ choices: [{ index: 0, delta: { role: 'assistant', content }, finish_reason: null }] })
      + sse({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })
      + sse({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls: () => calls,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('device usage survives offline replay, excludes fork copies and preserves shared submitters without charging Server budgets', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-computer-usage-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  const upstream = await upstreamFixture()
  let browser, page, node
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, environment })
    processes.push(server)
    const token = server.owner.session.access_token
    const team = (await serverRequest(server.origin, '/tenants', { token, body: { slug: 'usage-team', display_name: 'Usage team' } })).tenant
    const tenantId = team.tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token, tenantId, ...options })
    const project = (await owner('/projects')).projects[0]
    const enrollment = (await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { executor_id: 'usage-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await owner('/enrollments/consume', { body: { token: enrollment.token } })).credential
    const origin = `http://127.0.0.1:${await freePort()}`
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data'), '--node-id', 'usage-node', '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const env = { ...environment, TERNILO_LOCAL_TOKEN: credential.token }
    const startNode = async () => {
      node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, env)
      processes.push(node); await waitForHttp(origin, node)
      return localApi(origin)
    }
    let local = await startNode()
    for (const target of [origin, server.origin]) for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${target}/assets/${asset}`)).arrayBuffer())
      const expected = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(expected).digest('hex'))
    }
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    await local('/providers', { body: { id: 'usage-provider', display_name: 'Usage Provider', base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 32000, max_output_tokens: 1024 }, models: [{ id: 'usage-model', settings: { mode: 'inherit' } }], timeout_ms: 5000, max_attempts: 2, retry_base_delay_ms: 1 } })
    await local(`/sessions/${sessionId}`, { method: 'PATCH', body: { title: 'Device usage source', model: { provider: 'named_provider', provider_id: 'usage-provider', model: 'usage-model' } } })
    // Workbench discovery establishes the public mapping; unmapped local journals stay private.
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'source mapped')
    const publicId = state.sessions[0].identity.session_id
    const run = async (request, id, runId) => {
      await request(`/sessions/${id}/queue`, { body: { run_id: runId, content: { kind: 'prompt', input: `private-prompt-${runId}` } } })
      await until(() => request(`/sessions/${id}/events`), events => events.some(event => event.run_id === runId && event.type === 'turn_finished'), `${runId} completed`)
    }
    const reportPath = '/model-computers/usage-node/usage'
    const report = count => until(() => owner(reportPath), value => value.observations.length === count && value.observations.every(record => record.finished_at_ms !== null), `${count} usage observations`)
    await run(local, sessionId, 'online-task')
    const localEvents = await local(`/sessions/${sessionId}/events`)
    const starts = localEvents.filter(event => event.type === 'provider_usage_started')
    assert.equal(starts.length, 2, JSON.stringify(localEvents))
    assert.ok(starts.every(event => event.source_session_id === sessionId), JSON.stringify(starts))
    let usage = await report(2)
    assert.equal(usage.source, 'device_reported')
    const failed = usage.observations.find(record => record.error_code)
    assert.equal(failed.usage.input_tokens, 7)
    assert.equal(failed.usage.output_tokens, null)
    assert.equal(usage.observations.find(record => !record.error_code).usage.cached_input_tokens, 3)
    await stopProcess(server)
    await run(local, sessionId, 'offline-task')
    await stopProcess(node)
    local = await startNode()
    const restarted = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config', server.configPath], environment)
    processes.push(restarted); await waitForHttp(`${server.origin}/readyz`, restarted)
    usage = await report(3)
    await stopProcess(node)
    local = await startNode()
    await until(() => owner(`/tenants/${tenantId}/my-computers`), value => value.executors.some(value => value.connected), 'Node reconnected')
    assert.equal((await report(3)).observations.length, 3)
    const fork = await local(`/sessions/${sessionId}/fork`, { body: {} })
    const forkId = fork.identity.session_id
    await until(() => owner('/state'), value => value.sessions.length === 2, 'fork mapped')
    assert.equal((await owner(reportPath)).observations.length, 3, 'copied journal records are not new calls')
    await local(`/sessions/${forkId}`, { method: 'PATCH', body: { title: 'Device usage fork' } })
    await run(local, forkId, 'fork-task')
    await report(4)
    const invitation = await owner('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const member = await serverRequest(server.origin, '/auth/invitations/accept', { body: { token: invitation.token, username: 'usage-member', email: 'usage-member@example.test', password: 'usage-member-password' } })
    await owner(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    await owner(`/sessions/${publicId}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: { view: true, submit: true, stop: false, configure: false } })
    const asMember = (resource, options = {}) => serverRequest(server.origin, resource, { token: member.access_token, tenantId, ...options })
    assert.ok((await asMember(`/sessions/${publicId}/history`)).events.length > 0)
    await assert.rejects(asMember(reportPath), /403/)
    await run(asMember, publicId, 'shared-task')
    usage = await report(5)
    assert.deepEqual(usage.observations.find(record => record.run_id === 'shared-task').input_author, { kind: 'account', ...member.user })
    assert.equal(upstream.calls(), 5)
    for (const secret of ['private-prompt-', 'private-upstream-error', 'raw_usage', sessionId]) assert.equal(JSON.stringify(usage).includes(secret), false, secret)
    assert.equal((await owner('/model-access/usage')).request_count, 0)
    await stopProcess(node)
    await until(() => owner(`/tenants/${tenantId}/my-computers`), value => value.executors.every(value => !value.connected), 'Node offline')
    assert.equal((await report(5)).observations.length, 5)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page = await context.newPage()
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    const usageUrl = `${server.origin}/models?tab=usage&usage_source=device&space=${tenantId}&computer=usage-node`
    await page.goto(usageUrl)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-computer-usage-record]').nth(4).waitFor()
    assert.equal(await page.locator('[data-computer-usage-record]').count(), 5)
    await page.getByText('usage-member', { exact: false }).waitFor()
    for (const width of [1280, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await page.screenshot({ path: path.join(artifacts, `computer-usage-${width}.png`), fullPage: true })
      await page.locator('[data-computer-usage-record]').first().scrollIntoViewIfNeeded()
      await page.screenshot({ path: path.join(artifacts, `computer-usage-records-${width}.png`) })
    }
    await page.reload()
    await page.locator('[data-computer-usage-record]').nth(4).waitFor()
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'computer-usage-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await upstream.close()
    await rm(directory, { recursive: true, force: true })
  }
})
