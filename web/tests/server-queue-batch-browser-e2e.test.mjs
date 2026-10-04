import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('Queue batch did not settle')
}

test('Server batches retain authors and references, stop reasoning timers, and replace any batch input when editing or regenerating', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-server-batch-'))
  const processes = []
  const requests = []
  const pending = new Map()
  const errors = []
  let browser, model
  const frame = payload => `data: ${JSON.stringify(payload)}\n\n`
  const complete = response => response.end(frame({ choices: [{ delta: {}, finish_reason: 'stop' }] }) + 'data: [DONE]\n\n')
  try {
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const tenant = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: tenant, ...options })
    const enrollment = await owner(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'batch-machine', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrollment.enrollment.token } })
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
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    await writeFile(path.join(folder, 'context.txt'), 'Independent C reference sentinel.')
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localSession = session.identity.session_id
    model = createServer(async (request, response) => {
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      if (body.messages.some(message => message.role === 'system' && String(message.content).includes('You name software-agent conversations'))) {
        response.write(frame({ choices: [{ delta: { content: 'Server queue batch' } }] }))
        complete(response)
        return
      }
      const index = requests.length
      requests.push(body)
      pending.set(index, response)
      response.once('close', () => pending.delete(index))
      response.write(frame({ choices: [{ delta: index === 0
        ? { reasoning_content: 'Unfinished reasoning before stop.' }
        : { content: `Partial response ${index}.` } }] }))
    })
    await new Promise(resolve => model.listen(0, '127.0.0.1', resolve))
    await local('/providers', { body: { id: 'batch-proof', display_name: 'Batch proof', base_url: `http://127.0.0.1:${model.address().port}/v1`, protocol: 'openai-chat-completions',
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'batch-model', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25 } })
    await local(`/sessions/${localSession}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'batch-proof', model: 'batch-model' } } })
    const state = await until(() => owner('/state'), value => value.sessions.length === 1)
    const remoteSession = state.sessions[0].identity.session_id
    await owner(`/sessions/${remoteSession}/queue`, { body: { content: { kind: 'prompt', input: 'A: original task' } } })
    await until(() => requests, value => value.length === 1)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    const page = await context.newPage()
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    for (const origin of [server.origin, localOrigin]) {
      for (const asset of ['app.js', 'app.css']) {
        const served = await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer()
        assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      }
    }
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('dialog').waitFor({ state: 'hidden' })
    await page.locator(`[data-sidebar-session-row][data-session-id="${remoteSession}"]`).locator('button').first().click()
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()
    await page.locator('[data-reasoning-row][data-state="running"]').waitFor()
    assert.equal(await page.locator('[data-queue-dock]').count(), 0)
    await owner(`/sessions/${remoteSession}/queue`, { body: {
      content: { kind: 'prompt', input: 'B: from the account' },
      attachments: [{ name: 'B.txt', media_type: 'text/plain', content: 'Independent B attachment sentinel.' }],
    } })
    await local(`/sessions/${localSession}/queue`, { body: {
      content: { kind: 'prompt', input: 'C: from the computer' },
      references: [{ kind: 'file', path: 'context.txt', file_kind: 'file' }],
    } })
    await local(`/sessions/${localSession}/queue`, { body: { content: { kind: 'prompt', input: 'D: later in the same batch' } } })
    await page.locator('[data-queued-submission]').filter({ hasText: 'C: from the computer' }).waitFor()
    const editor = page.getByRole('textbox', { name: '输入任务' })
    await editor.fill('Unsent independent draft')
    const stopped = page.waitForResponse(response => response.request().method() === 'POST' && /\/queue\/[^/]+\/steer$/.test(new URL(response.url()).pathname))
    await page.getByRole('button', { name: '停止并发送全部', exact: true }).click()
    assert.equal((await stopped).status(), 200)
    await until(() => requests, value => value.length === 2)
    await until(() => pending.has(0), value => !value)
    const users = requests[1].messages.filter(message => message.role === 'user')
    const first = users.findIndex(message => JSON.stringify(message.content).includes('B: from the account'))
    const second = users.findIndex(message => JSON.stringify(message.content).includes('C: from the computer'))
    assert.ok(first >= 0 && second > first, 'B and C must be distinct user messages in FIFO order')
    assert.ok(JSON.stringify(requests[1]).includes('Independent B attachment sentinel.'))
    assert.ok(JSON.stringify(requests[1]).includes('Independent C reference sentinel.'))
    await page.locator('[data-role="user"]').filter({ hasText: 'B: from the account' }).waitFor()
    await page.locator('[data-role="user"]').filter({ hasText: 'C: from the computer' }).waitFor()
    await page.locator('[data-queue-dock]').waitFor({ state: 'detached' })
    const reasoning = page.locator('[data-reasoning-row][data-state="complete"]').filter({ hasText: 'Unfinished reasoning before stop.' })
    await reasoning.waitFor()
    const stoppedDuration = await reasoning.locator('[data-reasoning-duration]').innerText()
    await page.waitForTimeout(350)
    assert.equal(await reasoning.locator('[data-reasoning-duration]').innerText(), stoppedDuration)
    assert.equal(await page.locator('[data-reasoning-row][data-state="running"]').count(), 0)
    assert.equal(await editor.inputValue(), 'Unsent independent draft')
    complete(pending.get(1))
    await until(() => local(`/sessions/${localSession}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0)
    const historical = page.locator('[data-chat-turn]').first()
    const latest = page.locator('[data-chat-turn]').last()
    const opacity = locator => locator.evaluate(element => getComputedStyle(element).opacity)
    await editor.focus()
    await page.mouse.move(5, 5)
    await until(() => opacity(historical.locator('[data-message-actions="user"]').first()), value => value === '0')
    assert.deepEqual(await latest.locator('[data-message-actions="user"]').evaluateAll(elements => elements.map(element => getComputedStyle(element).opacity)), ['1', '1', '1'])
    assert.equal(await opacity(latest.locator('[data-turn-tail] footer')), '1')
    await historical.locator('[data-role="user"]').hover()
    await until(() => opacity(historical.locator('[data-message-actions="user"]').first()), value => value === '1')
    const latestTime = latest.locator('[data-message-actions="user"] time').first()
    assert.equal(await opacity(latestTime), '1')
    await page.mouse.move(5, 5)
    await historical.locator('[data-message-actions="user"] button').first().focus()
    await until(() => opacity(historical.locator('[data-message-actions="user"]').first()), value => value === '1')
    await page.setViewportSize({ width: 390, height: 844 })
    await editor.focus()
    await page.mouse.move(5, 5)
    assert.equal(await opacity(historical.locator('[data-message-actions="user"]').first()), '1')
    assert.equal(await opacity(historical.locator('[data-turn-tail] footer')), '1')
    await page.setViewportSize({ width: 1280, height: 900 })
    const events = (await local(`/sessions/${localSession}/events`)).filter(event => event.type === 'user_message')
    assert.deepEqual(events.map(event => event.content), ['A: original task', 'B: from the account', 'C: from the computer', 'D: later in the same batch'])
    assert.equal(events[1].run_id, events[2].run_id)
    assert.equal(events[1].provenance.author.kind, 'account')
    assert.equal(events[2].provenance.author.kind, 'local')
    assert.equal(events[1].attachments[0].name, 'B.txt')
    assert.equal(events[2].references[0].path, 'context.txt')
    await page.getByRole('button', { name: '停止运行', exact: true }).waitFor({ state: 'hidden' })
    const message = text => page.locator('[data-role="user"]').filter({ hasText: text })
    for (const text of ['B: from the account', 'C: from the computer', 'D: later in the same batch']) {
      assert.equal(await message(text).getByRole('button', { name: '编辑消息', exact: true }).count(), 1)
      assert.equal(await message(text).getByRole('button', { name: '重新生成', exact: true }).count(), 1)
    }
    const replacement = page.waitForRequest(request => request.method() === 'POST'
      && /\/queue$/.test(new URL(request.url()).pathname) && request.postDataJSON()?.content?.kind === 'regenerate')
    await message('C: from the computer').getByRole('button', { name: '编辑消息', exact: true }).click()
    await page.getByRole('textbox', { name: '编辑消息内容', exact: true }).fill('C: edited from the account')
    await page.getByRole('button', { name: '保存并重新生成', exact: true }).click()
    const replacementBody = (await replacement).postDataJSON()
    assert.equal(replacementBody.content.target_seq, events[2].seq)
    assert.deepEqual(replacementBody.references, events[2].references)
    await until(() => requests, values => values.length === 3)
    const editedContext = JSON.stringify(requests[2].messages)
    assert.ok(editedContext.includes('B: from the account'))
    assert.ok(editedContext.includes('C: edited from the account'))
    assert.ok(!editedContext.includes('C: from the computer'))
    assert.ok(!editedContext.includes('D: later in the same batch'))
    assert.ok(!editedContext.includes('Partial response 1.'))
    complete(pending.get(2))
    await until(() => local(`/sessions/${localSession}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0)
    await page.getByRole('button', { name: '停止运行', exact: true }).waitFor({ state: 'hidden' })
    await message('C: edited from the account').waitFor()
    assert.equal(await message('C: from the computer').count(), 0)
    assert.equal(await message('D: later in the same batch').count(), 0)
    assert.equal(await message('B: from the account').count(), 1)
    assert.equal(await page.locator('[data-execution-phase]').count(), 0, 'the retained prefix must not reopen its old timer')
    await message('C: edited from the account').getByRole('button', { name: '重新生成', exact: true }).click()
    await until(() => requests, values => values.length === 4)
    assert.equal(requests[3].messages.filter(message => message.role === 'user'
      && JSON.stringify(message.content).includes('C: edited from the account')).length, 1)
    assert.ok(!JSON.stringify(requests[3].messages).includes('Partial response 2.'))
    complete(pending.get(3))
    await until(() => local(`/sessions/${localSession}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0)
    await page.reload()
    await message('C: edited from the account').waitFor()
    assert.equal(await message('B: from the account').count(), 1)
    assert.equal(await message('C: edited from the account').count(), 1)
    assert.equal(await message('D: later in the same batch').count(), 0)
    assert.equal(await page.locator('[data-reasoning-row][data-state="running"]').count(), 0)
    assert.equal(await editor.inputValue(), 'Unsent independent draft')
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'server-batch.png') })
    }
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    for (const response of pending.values()) response.destroy()
    if (model) await new Promise(resolve => model.close(resolve))
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
