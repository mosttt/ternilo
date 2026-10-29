import assert from 'node:assert/strict'
import { Agent, createServer, request as httpRequest } from 'node:http'
import { connect } from 'node:net'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function modelFixture() {
  const calls = []
  const server = createServer(async (request, response) => {
    assert.equal(request.url, '/v1/responses')
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    const emit = value => response.write(`data: ${JSON.stringify(value)}\n\n`)
    const complete = text => {
      emit({ type: 'response.output_text.delta', delta: text })
      emit({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', content: [{ type: 'output_text', text }] }], usage: { input_tokens: 30, output_tokens: 12 } } })
      response.end()
    }
    if (body.instructions?.includes('You name software-agent conversations')) { complete('Gateway fixture'); return }
    assert.ok(body.tools.length)
    assert.ok(body.tools.every(tool => tool.strict === false), 'Responses preserves optional tool arguments')
    const call = { body, complete, closed: false }
    calls.push(call)
    response.on('close', () => { call.closed = true })
    emit({ type: 'response.reasoning_summary_text.delta', delta: 'Reviewing the task.' })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { calls, url: `http://127.0.0.1:${server.address().port}/v1`, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

async function proxyFixture(origin) {
  const target = new URL(origin), sockets = new Set(), models = new Set()
  const track = socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)) }
  const server = createServer((request, response) => {
    const client = request.socket
    const model = request.method === 'POST' && request.url === '/internal/node/v1/model'
    if (model) models.add(client)
    const upstream = httpRequest(new URL(request.url, origin), { method: request.method, headers: request.headers }, reply => {
      response.writeHead(reply.statusCode, reply.headers)
      response.flushHeaders()
      reply.on('error', () => response.destroy())
      reply.pipe(response)
    })
    upstream.on('error', () => response.destroy())
    response.on('close', () => { if (model) models.delete(client); upstream.destroy() })
    request.pipe(upstream)
  })
  server.on('connection', track)
  server.on('upgrade', (request, client, head) => {
    const upstream = connect(Number(target.port), target.hostname)
    track(upstream)
    const headers = request.rawHeaders.reduce((text, value, index) => text + (index % 2 ? `${value}\r\n` : `${value}: `), '')
    upstream.write(`${request.method} ${request.url} HTTP/${request.httpVersion}\r\n${headers}\r\n`)
    if (head.length) upstream.write(head)
    client.on('error', () => upstream.destroy()); upstream.on('error', () => client.destroy())
    client.on('close', () => upstream.destroy())
    upstream.on('close', () => client.destroy())
    client.pipe(upstream); upstream.pipe(client)
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { url: `ws://127.0.0.1:${server.address().port}/api/v1/executors/connect`,
    interrupt: () => { assert.ok(models.size); for (const socket of models) socket.destroy() },
    close: () => new Promise(resolve => { for (const socket of sockets) socket.destroy(); server.close(resolve) }) }
}

test('gateway proxy identifies model streams after a metadata request reuses the connection', { timeout: 10000 }, async () => {
  const upstream = createServer((request, response) => {
    request.resume()
    if (request.url === '/internal/node/v1/model/web-access') response.end('{}')
    else { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.flushHeaders() }
  })
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
  const proxy = await proxyFixture(`http://127.0.0.1:${upstream.address().port}`)
  const agent = new Agent({ keepAlive: true, maxSockets: 1 })
  const post = resource => new Promise((resolve, reject) => {
    const request = httpRequest(new URL(resource, proxy.url.replace('ws:', 'http:')), { agent, method: 'POST', headers: { 'content-length': 2 } }, resolve)
    request.on('error', reject); request.end('{}')
  })
  try {
    const metadata = await post('/internal/node/v1/model/web-access')
    const socket = metadata.socket
    for await (const _chunk of metadata) { /* Drain the reusable connection. */ }
    const stream = await post('/internal/node/v1/model')
    assert.equal(stream.socket, socket)
    stream.on('error', () => {})
    const aborted = new Promise(resolve => stream.once('aborted', resolve))
    proxy.interrupt()
    await aborted
  } finally {
    agent.destroy(); await proxy.close()
    upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve))
  }
})

test('closing Server browser preserves execution; model disconnect links to the real request record', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-gateway-'))
  const processes = [], errors = []
  let browser, model, proxy, page
  try {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
    model = await modelFixture()
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user' }); processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(server.origin, url, { token: server.owner.session.access_token, tenantId, ...options })
    await request('/providers', { body: { id: 'gateway-fixture', display_name: 'Gateway fixture', base_url: model.url, protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture-model', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 10 } })
    await request('/default-model', { method: 'PUT', body: { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'gateway-fixture', model: 'fixture-model' } })
    const { enrollment } = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'Gateway computer', ttl_seconds: 600 } })
    const { credential } = await request('/enrollments/consume', { body: { token: enrollment.token } })
    proxy = await proxyFixture(server.origin)
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrollment.executor_id, '--gateway-url', proxy.url, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state') }); processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin), folder = path.join(directory, 'workspace'); await mkdir(folder)
    await local('/workspaces', { body: { path: folder } })
    const state = await until(() => request('/state'), value => value.workspaces.length === 1, 'workspace mapping')
    const session = await request('/sessions', { body: { workspace_id: state.workspaces[0].workspace_id } })
    const resource = `/sessions/${session.identity.session_id}`
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    let context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 820 }, serviceWorkers: 'block' })
    const open = async () => {
      const page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      await page.goto(server.origin)
      await page.getByLabel('用户名', { exact: true }).or(page.locator('[data-input-bar]')).first().waitFor()
      if (await page.getByLabel('用户名', { exact: true }).isVisible()) {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"]`).click()
      return page
    }
    page = await open()
    const submit = async text => { await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(text); await page.getByRole('button', { name: '发送', exact: true }).click() }
    await submit('Continue after closing this browser')
    await until(() => model.calls.length, length => length === 1, 'first model request')
    await page.getByText('Reviewing the task.', { exact: false }).waitFor()
    await page.close()
    assert.equal(context.pages().length, 0)
    await browser.close()
    // Allow a complete gateway cancellation-monitor interval before finishing the model.
    await new Promise(resolve => setTimeout(resolve, 2500))
    assert.equal(model.calls[0].closed, false)
    model.calls[0].complete('Completed while the browser was closed.')
    await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'turn_finished'), 'background completion')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 820 }, serviceWorkers: 'block' })
    page = await open()
    await page.getByText('Completed while the browser was closed.', { exact: true }).waitFor()
    await submit('Interrupt only the model transport')
    await until(() => model.calls.length, length => length === 2, 'second model request')
    // Receiving a gateway heartbeat must already have delivered its correlation header.
    await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'assistant_reasoning_delta').length >= 2, 'second reasoning output')
    proxy.interrupt()
    const events = await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'turn_failed'), 'interrupted model turn')
    const error = events.findLast(event => event.type === 'turn_failed')
    const id = error.message.match(/Server request ID: (mrq_[A-Za-z0-9_-]+)$/)?.[1]
    assert.ok(id, error.message)
    const ledger = await until(() => request(`/model-access/requests?query=${id}`), page => page.requests.some(row => row.request_id === id && row.state !== 'pending'), 'settled interrupted request')
    const record = ledger.requests.find(row => row.request_id === id)
    assert.equal(record.error_code, 'client_disconnected')
    assert.equal(record.attempts[0].error_code, 'client_disconnected')
    assert.equal(record.accounted_tokens, null, 'missing upstream usage is not zero')
    assert.equal(model.calls.length, 2, 'no automatic replay after an ambiguous interruption')
    for (const [locale, title, action, reason] of [
      ['zh', '与 Server 的模型连接中断', '查看请求记录', '执行端与 Server 的模型连接已关闭'],
      ['en', 'Model connection to Server interrupted', 'View request record', 'The executor’s model connection to Server closed'],
      ['ko', 'Server 모델 연결이 끊겼습니다', '요청 기록 보기', '실행기와 Server의 모델 연결이 닫혀'],
    ]) {
      await page.evaluate(locale => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.theme', 'dark') }, locale)
      await page.setViewportSize({ width: locale === 'en' ? 1280 : 390, height: 844 })
      await page.goto(server.origin)
      await page.locator('[data-chat-event="turn_failed"]').getByText(title, { exact: true }).click()
      const viewRecord = page.getByRole('button', { name: action, exact: true })
      await viewRecord.waitFor()
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `model-error-${locale}.png`), animations: 'disabled' })
      await viewRecord.click()
      await page.locator(`[data-model-request="${id}"]`).waitFor()
      assert.match(page.url(), new RegExp(`request=${id}`))
      assert.equal(await page.locator('[role="search"] input').inputValue(), id)
      await page.getByText(reason, { exact: false }).last().waitFor()
      await page.locator(`[data-model-request="${id}"]`).scrollIntoViewIfNeeded()
      await page.locator(`[data-model-request="${id}"] summary`).click()
      assert.ok(await page.locator(`[data-model-request="${id}"] details`).getAttribute('open') !== null)
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `model-request-${locale}.png`) })
      }
    }
    const runCommand = async input => {
      const submitted = await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input } } })
      const events = await until(() => request(`${resource}/events`), events => events.some(event => event.run_id === submitted.run_id && ['turn_finished', 'turn_failed'].includes(event.type)), 'command finished')
      assert.ok(!events.some(event => event.run_id === submitted.run_id && event.type === 'turn_failed'))
      assert.ok(!events.some(event => event.run_id === submitted.run_id && event.type === 'code_dispatch_finished' && event.output.is_error))
    }
    await runCommand('/code tools::write_file(#{ path: "package/empty.txt", content: "" })')
    assert.equal((await readFile(path.join(folder, 'package/empty.txt'))).length, 0)
    for (const text of ['工程包含ui说明', 'independent ui token', '模型Ternilo与한국어검색결과']) await runCommand(`/code ${JSON.stringify(text)}`)
    const hits = await request('/session-search?query=ui&limit=50')
    assert.ok(hits.some(hit => hit.excerpt.includes('工程包含ui说明')))
    assert.ok(hits.some(hit => hit.excerpt.includes('independent ui token')))
    const korean = await request(`/session-search?query=${encodeURIComponent('검색')}&limit=50`)
    assert.ok(korean.some(hit => hit.excerpt.includes('한국어검색결과')))
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(`${server.origin}/files?session_id=${encodeURIComponent(session.identity.session_id)}&kind=generated`)
    const row = page.locator('[data-file-results] [data-file-id]').first()
    await row.waitFor()
    await row.getByRole('button').first().click()
    await page.locator('[data-file-preview] pre').waitFor()
    assert.equal(await page.locator('[data-file-preview] pre').textContent(), '')
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    const download = page.waitForEvent('download')
    await row.getByRole('button', { name: /下载.*empty\.txt/ }).click()
    assert.equal((await readFile(await (await download).path())).length, 0)
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'empty-file.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR && page && !page.isClosed()) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await proxy?.close(); await model?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
