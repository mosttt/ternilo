import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { openModels, closeModels } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const result = await read()
    if (ready(result)) return result
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('Fixture state did not settle')
}

const svg = '<svg xmlns="http://www.w3.org/2000/svg">\n  <text x="10" y="20">literal $value `quoted`</text>\n</svg>'

test('copying, regeneration after success or failure, and cancellable timeouts work on desktop and touch', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-clipboard-timeout-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = []
  let browser, page, model
  const errors = []
  let modelCalls = 0
  let stall = false
  try {
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, ...options })
    const tenant = server.owner.session.personal_tenant_id
    const enrolled = await owner(`/tenants/${tenant}/my-computer-enrollments`, { body: { executor_id: 'clipboard-machine', project_id: null, ttl_seconds: 600 } })
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const app = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'local'), '--node-id', 'clipboard-machine',
      '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(app)
    await waitForHttp(localOrigin, app)
    const html = await (await fetch(localOrigin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const local = (resource, options = {}) => serverRequest(localOrigin, resource, { token, ...options })
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    await writeFile(path.join(folder, 'context.txt'), 'Keep this file reference when regenerating.')
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    model = createServer(async (request, response) => {
      if (request.method === 'GET' && request.url === '/v1/models') {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ data: [{ id: 'timeout-model' }] }))
        return
      }
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      modelCalls += 1
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      const frame = value => `data: ${JSON.stringify(value)}\n\n`
      const content = stall ? 'Partial answer remains readable.' : `\`\`\`svg\n${svg}\n\`\`\``
      response.write(frame({ choices: [{ delta: { content }, finish_reason: null }] }))
      if (!stall) response.end(frame({ choices: [{ delta: {}, finish_reason: 'stop' }] }) + frame({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 20 } }) + 'data: [DONE]\n\n')
    })
    await new Promise(resolve => model.listen(0, '127.0.0.1', resolve))
    await local('/providers', { body: { id: 'timeout-proof', display_name: 'Timeout proof', base_url: `http://127.0.0.1:${model.address().port}/v1`, protocol: 'openai-chat-completions',
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'timeout-model', settings: { mode: 'inherit' } }], timeout_ms: 500, max_attempts: 3, retry_base_delay_ms: 25 } })
    await local(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'timeout-proof', model: 'timeout-model' } } })
    const prompt = `Generate this SVG:\n${svg}`
    await local(`/sessions/${session.identity.session_id}/queue`, { body: {
      content: { kind: 'prompt', input: prompt },
      attachments: [{ name: 'note.txt', media_type: 'text/plain', content: 'Keep this attachment when regenerating.' }],
      references: [{ kind: 'file', path: 'context.txt', file_kind: 'file' }],
    } })
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'))
    const state = await until(() => owner('/state', { tenantId: tenant }), state => state.sessions.length === 1)
    const serverSession = state.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
      args: ['--host-resolver-rules=MAP clipboard.ternilo.test 127.0.0.1', '--no-proxy-server'] })
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page = await context.newPage()
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    const insecureOrigin = server.origin.replace('127.0.0.1', 'clipboard.ternilo.test')
    await page.goto(insecureOrigin)
    assert.equal(await page.evaluate(() => isSecureContext), false)
    assert.equal(await page.evaluate(() => typeof navigator.clipboard), 'undefined')
    for (const asset of ['app.js', 'app.css']) {
      const served = await (await page.request.get(`${server.origin}/assets/${asset}`)).body()
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('dialog').waitFor({ state: 'hidden' })
    await page.locator(`[data-sidebar-session-row][data-session-id="${serverSession}"]`).locator('button').first().click()
    const userCopy = page.locator('[data-message-actions="user"]').getByRole('button', { name: '复制', exact: true })
    const codeCopy = page.locator('.markdown-copy')
    await codeCopy.waitFor()
    const reader = await context.newPage()
    await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: localOrigin })
    await reader.goto(localOrigin)
    const copied = async (locator, text, touch = false) => {
      await page.bringToFront()
      if (touch) await locator.tap(); else await locator.click()
      await reader.bringToFront()
      assert.equal(await reader.evaluate(() => navigator.clipboard.readText()), text)
    }
    await copied(userCopy, prompt)
    await copied(codeCopy, svg)
    await page.bringToFront()
    const originalEvents = await local(`/sessions/${session.identity.session_id}/events`)
    const originalInput = originalEvents.find(event => event.type === 'user_message')
    const draft = page.getByRole('textbox', { name: '输入任务' })
    await draft.fill('Unsent draft must survive regeneration')
    const regenerated = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith(`/sessions/${serverSession}/queue`))
    await page.locator('[data-message-actions="user"]').first().getByRole('button', { name: '重新生成', exact: true }).click()
    const regeneratedResponse = await regenerated
    assert.ok(regeneratedResponse.ok())
    const request = regeneratedResponse.request().postDataJSON()
    assert.deepEqual(request.content, { kind: 'prompt', input: prompt })
    assert.equal(request.delivery, 'queue')
    assert.notEqual(request.run_id, originalInput.run_id)
    assert.deepEqual(request.attachments, originalInput.attachments)
    assert.deepEqual(request.references, originalInput.references)
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'))
    const repeatedInputs = (await local(`/sessions/${session.identity.session_id}/events`)).filter(event => event.type === 'user_message')
    assert.equal(repeatedInputs.length, 2)
    assert.equal(repeatedInputs[0].seq, originalInput.seq)
    assert.equal(repeatedInputs[1].content, originalInput.content)
    assert.equal(await draft.inputValue(), 'Unsent draft must survive regeneration')
    await page.setViewportSize({ width: 390, height: 844 })
    await copied(codeCopy.first(), svg, true)
    await page.screenshot({ path: path.join(artifacts, 'clipboard-http-mobile.png') })
    modelCalls = 0
    stall = true
    await local(`/sessions/${session.identity.session_id}/queue`, { body: { content: { kind: 'prompt', input: 'Demonstrate a stalled stream' } } })
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'))
    await page.bringToFront()
    await page.reload()
    await page.getByText('模型请求超时', { exact: true }).waitFor()
    await page.getByText('Partial answer remains readable.', { exact: true }).waitFor()
    assert.equal(modelCalls, 1, 'partial output is never automatically generated again')
    await page.screenshot({ path: path.join(artifacts, 'stream-timeout-mobile.png') })
    stall = false
    const retry = page.locator('[data-role="user"]').filter({ hasText: 'Demonstrate a stalled stream' }).getByRole('button', { name: '重新生成', exact: true })
    const retryBounds = await retry.boundingBox()
    assert.ok(retryBounds && retryBounds.width >= 40 && retryBounds.height >= 40)
    const retried = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith(`/sessions/${serverSession}/queue`))
    await retry.tap()
    assert.ok((await retried).ok())
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'))
    const retriedEvents = await local(`/sessions/${session.identity.session_id}/events`)
    assert.equal(retriedEvents.filter(event => event.type === 'user_message' && event.content === 'Demonstrate a stalled stream').length, 2)
    assert.equal(retriedEvents.filter(event => event.type === 'turn_failed').length, 1)
    assert.equal(modelCalls, 2, 'manual retry makes a fresh request after the failed stream')
    await page.screenshot({ path: path.join(artifacts, 'regenerate-mobile.png') })
    stall = true
    await reader.bringToFront()
    const settings = await openModels(reader)
    await settings.getByRole('button', { name: '编辑', exact: true }).click()
    await settings.locator('summary').filter({ hasText: '自定义设置' }).click()
    await settings.locator('summary').filter({ hasText: '请求与重试' }).click()
    await settings.getByLabel('单次请求总时限（ms）', { exact: true }).fill('0')
    await settings.getByRole('button', { name: '保存', exact: true }).click()
    await until(() => local('/providers'), providers => providers[0]?.timeout_ms === 0)
    const discovered = await local('/providers/discover', { body: { provider_id: 'timeout-proof', timeout_ms: 0 } })
    assert.ok(discovered.some(model => model.id === 'timeout-model'))
    await closeModels(reader)
    await local(`/sessions/${session.identity.session_id}/queue`, { body: { content: { kind: 'prompt', input: 'Wait until I stop this request' } } })
    await page.bringToFront()
    const stop = page.getByRole('button', { name: '停止运行', exact: true })
    await stop.waitFor()
    assert.equal(await page.getByRole('button', { name: '重新生成', exact: true }).first().isDisabled(), true)
    await page.waitForTimeout(750)
    assert.ok((await local(`/sessions/${session.identity.session_id}/queue`)).active_run_id, 'zero timeout exceeds the earlier 500 ms limit')
    await stop.tap()
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), queue => !queue.active_run_id)
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'clipboard-timeout-failure.png'), fullPage: true }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    model?.closeAllConnections()
    if (model) await new Promise(resolve => model.close(resolve))
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
