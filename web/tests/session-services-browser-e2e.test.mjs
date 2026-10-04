import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  throw new Error('Timed out: ' + label)
}

const mcpSource = String.raw`
import { appendFileSync, existsSync, writeFileSync } from 'node:fs'
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'
import path from 'node:path'
const directory = process.env.SERVICE_FIXTURE_DIRECTORY
const file = name => path.join(directory, name)
if (process.argv[2] === 'writer') {
  writeFileSync(file('writer-pid'), String(process.pid))
  process.on('SIGTERM', () => {})
  setInterval(() => {
    if (existsSync(file('fixture-stop'))) process.exit(0)
    try { appendFileSync(file('heartbeat'), 'tick\n') } catch { process.exit(0) }
  }, 20)
} else {
  appendFileSync(file('starts'), 'start\n')
  writeFileSync(file('server-pid'), String(process.pid))
  spawn(process.execPath, [fileURLToPath(import.meta.url), 'writer'], { stdio: 'ignore' })
  setInterval(() => { if (existsSync(file('fixture-stop'))) process.exit(0) }, 20)
  const input = createInterface({ input: process.stdin })
  input.on('close', () => process.exit(0))
  input.on('line', line => {
    const request = JSON.parse(line)
    if (request.id === undefined) return
    let result
    if (request.method === 'initialize') result = {
      protocolVersion: request.params.protocolVersion,
      capabilities: { tools: {} }, serverInfo: { name: 'service-fixture', version: '1' },
    }
    else if (request.method === 'tools/list') result = { tools: [{
      name: 'echo', description: 'Return fixture text',
      inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] },
    }] }
    else result = { content: [{ type: 'text', text: 'Fixture response' }] }
    process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n')
  })
}
`

function complete(response, text) {
  const output = [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }]
  response.writeHead(200, { 'content-type': 'text/event-stream' })
  response.end('data: ' + JSON.stringify({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text }) + '\n\n'
    + 'data: ' + JSON.stringify({ type: 'response.completed', response: { status: 'completed', output, usage: { input_tokens: 20, output_tokens: 10 } } }) + '\n\n')
}

async function liveProcess(pid) {
  if (!pid) return false
  try { process.kill(pid, 0) } catch (error) { if (error.code === 'ESRCH') return false; throw error }
  if (process.platform === 'linux') {
    const stat = await readFile('/proc/' + pid + '/stat', 'utf8').catch(error => { if (error.code === 'ENOENT') return ''; throw error })
    return stat !== '' && !['Z', 'X'].includes(stat.slice(stat.lastIndexOf(')') + 2).split(' ')[0])
  }
  return true
}

test('background services retain state across tasks and explicit stop releases the shared directory', {
  timeout: 150_000, skip: process.platform === 'win32',
}, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-services-browser-'))
  const workspacePath = path.join(directory, 'shared')
  const serviceDirectory = path.join(workspacePath, 'service')
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(workspacePath)
  await mkdir(serviceDirectory)
  await mkdir(artifacts, { recursive: true })
  const script = path.join(serviceDirectory, 'fixture.mjs')
  await writeFile(script, mcpSource)
  const errors = [], assetHashes = {}, calls = []
  let local, browser, ownerPage, otherPage
  const bytes = name => readFile(path.join(serviceDirectory, name)).then(value => value.length).catch(error => { if (error.code === 'ENOENT') return 0; throw error })
  const pid = name => readFile(path.join(serviceDirectory, name), 'utf8').then(value => Number(value)).catch(error => { if (error.code === 'ENOENT') return 0; throw error })
  const starts = async () => await bytes('starts') / 'start\n'.length
  const model = createServer((request, response) => {
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => { void (async () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      const title = body.instructions?.includes('You name software-agent conversations')
      const input = body.input?.filter(item => item.role === 'user').at(-1)?.content
        ?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      if (!title) calls.push({ input, tools: body.tools?.map(tool => tool.name) ?? [],
        heartbeat: await bytes('heartbeat'), writerRunning: await liveProcess(await pid('writer-pid')) })
      complete(response, title ? 'Service fixture' : 'Completed: ' + input)
    })().catch(error => { errors.push('fixture: ' + error.message); response.writeHead(500); response.end() }) })
  })
  await new Promise(resolve => model.listen(0, '127.0.0.1', resolve))
  try {
    const origin = 'http://127.0.0.1:' + await freePort()
    local = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
    await waitForHttp(origin, local)
    for (const name of ['app.js', 'app.css']) {
      const response = await fetch(origin + '/assets/' + name)
      assert.equal(response.status, 200)
      const digest = value => createHash('sha256').update(value).digest('hex')
      assetHashes[name] = digest(Buffer.from(await response.arrayBuffer()))
      assert.equal(assetHashes[name], digest(await readFile(path.join(repository, 'web/dist/assets', name))))
    }
    const html = await (await fetch(origin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const api = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const workspace = await api('/workspaces', { body: { path: workspacePath } })
    await api('/credentials', { body: { name: 'SERVICES_FIXTURE_KEY', value: 'fixture-key' } })
    await api('/providers', { body: {
      id: 'services-fixture', display_name: 'Services fixture', base_url: 'http://127.0.0.1:' + model.address().port + '/v1',
      protocol: 'openai-responses', api_key_ref: 'SERVICES_FIXTURE_KEY',
      defaults: { context_window: 128000, max_output_tokens: 4096 },
      models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 50,
    } })
    const sessions = []
    for (const title of ['Service owner', 'Directory waiter']) {
      const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const id = session.identity.session_id
      await api('/sessions/' + id, { method: 'PATCH', body: {
        title, permissions: 'full_access', model: { provider: 'named_provider', provider_id: 'services-fixture', model: 'fixture' },
      } })
      sessions.push(id)
    }
    // Empty sessions use the welcome screen without the conversation header.
    const history = await api('/sessions/' + sessions[0] + '/turns', { body: { input: '/code "service-catalog-history"' } })
    assert.match(history.answer, /service-catalog-history/)
    assert.deepEqual(calls, [], 'the direct history command must not make a model request')
    await api('/sessions/' + sessions[0], { method: 'PATCH', body: { profile_plugins: [{
      id: 'fixture-mcp', kind: 'ternilo.mcp.stdio', enabled: true,
      config: { server_name: 'fixture', command: process.execPath, args: [script],
        env: { SERVICE_FIXTURE_DIRECTORY: serviceDirectory }, startup_timeout_ms: 5000, tool_call_timeout_ms: 5000 },
    }] } })
    const servicePath = '/sessions/' + sessions[0] + '/services'
    const service = async () => (await api(servicePath)).find(item => item.id === 'mcp:fixture')
    assert.equal((await service()).status, 'idle')
    await api('/sessions/' + sessions[0] + '/commands')
    await api('/sessions/' + sessions[0] + '/skills')
    assert.equal(await starts(), 0, 'cold catalog requests must not start MCP')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    async function pageFor(id) {
      const page = await context.newPage()
      page.on('pageerror', error => errors.push('page: ' + error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push('console: ' + message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push('HTTP ' + response.status() + ' ' + new URL(response.url()).pathname) })
      page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push('request: ' + request.url() + ' ' + request.failure()?.errorText) })
      await page.goto(origin)
      const group = page.locator('[data-sidebar-workspace-group]').filter({ has: page.locator('[data-sidebar-workspace-title]', { hasText: 'shared' }) })
      const toggle = group.locator('[data-sidebar-workspace-button]')
      await toggle.waitFor()
      if (await toggle.getAttribute('aria-expanded') !== 'true') await toggle.click()
      const more = group.locator('[role="treeitem"] > button[aria-expanded="false"]:not([data-sidebar-workspace-button]):not([data-sidebar-session-button])')
      if (await more.count()) await more.click()
      await page.locator('[data-sidebar-session-row][data-session-id="' + id + '"] [data-sidebar-session-button]').click()
      return page
    }
    const idle = id => until(() => api('/sessions/' + id + '/queue'), inbox => !inbox.active_run_id, 'session settles: ' + id)
    async function send(page, input) {
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(input)
      await page.getByRole('button', { name: '发送', exact: true }).click()
    }
    async function completed(page, id, input) {
      await page.locator('article[data-role="assistant"]').filter({ hasText: 'Completed: ' + input }).waitFor()
      await idle(id)
    }
    async function openServices() {
      await ownerPage.getByRole('button', { name: '更多会话操作', exact: true }).click()
      await ownerPage.getByRole('menuitem', { name: '后台服务', exact: true }).click()
      const dialog = ownerPage.getByRole('dialog', { name: '后台服务', exact: true })
      await dialog.locator('[data-session-service="mcp:fixture"]').waitFor()
      await dialog.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished)))
      return dialog
    }
    async function action(dialog, name, status) {
      const response = ownerPage.waitForResponse(value => value.request().method() === 'POST'
        && new URL(value.url()).pathname.endsWith('/' + (name === '启动' ? 'start' : 'stop')))
      await dialog.locator('[data-session-service="mcp:fixture"]').getByRole('button', { name, exact: true }).click()
      assert.equal((await response).ok(), true)
      await dialog.locator('[data-session-service="mcp:fixture"] [data-status="' + status + '"]').waitFor()
      assert.equal((await service()).status, status)
    }
    ownerPage = await pageFor(sessions[0])
    otherPage = await pageFor(sessions[1])
    let dialog = await openServices()
    await dialog.locator('[data-status="idle"]').waitFor()
    await dialog.getByRole('button', { name: '刷新', exact: true }).click()
    assert.equal(await starts(), 0, 'opening or refreshing the service dialog must not spawn a process')
    await ownerPage.keyboard.press('Escape')
    await dialog.waitFor({ state: 'hidden' })
    let initialPid = 0
    for (const input of ['service-round-one', 'service-round-two']) {
      await send(ownerPage, input)
      await completed(ownerPage, sessions[0], input)
      assert.equal((await service()).status, 'running')
      assert.equal(await starts(), 1, 'one MCP instance must survive both completed tasks')
      if (initialPid) assert.equal(await pid('server-pid'), initialPid)
      else initialPid = await pid('server-pid')
      assert.ok(initialPid > 0)
      assert.equal(calls.find(call => call.input === input).tools.includes('mcp__fixture__echo'), true)
    }
    await until(() => bytes('heartbeat'), length => length >= 20, 'the MCP descendant is writing after the turn')
    await send(otherPage, 'wait-for-service-release')
    const waiting = otherPage.locator('[role="status"]').filter({ hasText: '等待目录空闲' }).first()
    await waiting.waitFor()
    assert.equal(calls.some(call => call.input === 'wait-for-service-release'), false)
    dialog = await openServices()
    await dialog.locator('[data-status="running"]').waitFor()
    await ownerPage.screenshot({ path: path.join(artifacts, 'services-running-desktop.png'), animations: 'disabled' })
    await otherPage.screenshot({ path: path.join(artifacts, 'services-directory-wait-desktop.png'), animations: 'disabled' })
    await action(dialog, '停止', 'stopped')
    await completed(otherPage, sessions[1], 'wait-for-service-release')
    await waiting.waitFor({ state: 'hidden' })
    const resumed = calls.find(call => call.input === 'wait-for-service-release')
    if (process.platform === 'linux') assert.equal(resumed.writerRunning, false, 'the writer must stop before another session enters the model')
    await new Promise(resolve => setTimeout(resolve, 120))
    assert.equal(await bytes('heartbeat'), resumed.heartbeat, 'the stopped MCP descendant must not write after directory handoff')
    await ownerPage.keyboard.press('Escape')
    await dialog.waitFor({ state: 'hidden' })
    await send(ownerPage, 'manual-stop-stays-stopped')
    await completed(ownerPage, sessions[0], 'manual-stop-stays-stopped')
    assert.equal(await starts(), 1)
    assert.equal((await service()).status, 'stopped')
    assert.equal(calls.find(call => call.input === 'manual-stop-stays-stopped').tools.includes('mcp__fixture__echo'), false)
    await ownerPage.setViewportSize({ width: 390, height: 844 })
    dialog = await openServices()
    const bounds = await dialog.boundingBox()
    assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= 390 && bounds.y + bounds.height <= 844)
    assert.equal(await ownerPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    const startButton = dialog.locator('[data-session-service="mcp:fixture"]').getByRole('button', { name: '启动', exact: true })
    const startBounds = await startButton.boundingBox()
    assert.ok(startBounds.height >= 40, 'mobile start target height: ' + startBounds.height)
    await action(dialog, '启动', 'running')
    assert.equal(await starts(), 2)
    assert.notEqual(await pid('server-pid'), initialPid)
    await ownerPage.screenshot({ path: path.join(artifacts, 'services-running-mobile.png'), animations: 'disabled' })
    await action(dialog, '停止', 'stopped')
    const stoppedBytes = await bytes('heartbeat')
    await new Promise(resolve => setTimeout(resolve, 120))
    assert.equal(await bytes('heartbeat'), stoppedBytes)
    await ownerPage.screenshot({ path: path.join(artifacts, 'services-stopped-mobile.png'), animations: 'disabled' })
    assert.deepEqual(calls.map(call => call.input), [
      'service-round-one', 'service-round-two', 'wait-for-service-release', 'manual-stop-stays-stopped',
    ], 'catalog reads and service controls must not submit model requests')
    assert.deepEqual(errors, [])
  } catch (error) {
    await ownerPage?.screenshot({ path: path.join(artifacts, 'services-failure.png'), animations: 'disabled' }).catch(() => {})
    process.stderr.write(local?.diagnostics() ?? '')
    throw error
  } finally {
    await writeFile(path.join(serviceDirectory, 'fixture-stop'), '')
    await writeFile(path.join(artifacts, 'services-observations.json'), JSON.stringify({ assetHashes, errors, calls, starts: await starts() }, null, 2))
    await browser?.close()
    await stopProcess(local)
    model.closeAllConnections()
    await new Promise(resolve => model.close(resolve))
    await rm(directory, { recursive: true, force: true })
  }
})
