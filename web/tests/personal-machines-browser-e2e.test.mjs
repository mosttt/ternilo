import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectProject, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
async function until(read, ready, label) {
  const deadline = Date.now() + 30_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}

async function modelFixture() {
  const calls = []
  const server = createServer(async (request, response) => {
    if (request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    const write = body.tools?.some(tool => tool.function?.name === 'write_file')
      && !body.messages.some(message => message.role === 'tool')
    const content = `${body.model} task completed`
    const identity = { id: `fixture-${calls.length}`, model: body.model, created: 1 }
    const usage = { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 }
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage }))
      return
    }
    const delta = write ? { role: 'assistant', tool_calls: [{ index: 0, id: 'write-proof', type: 'function', function: {
      name: 'write_file', arguments: JSON.stringify({ path: 'machine-proof.txt', content: body.model }),
    } }] } : { role: 'assistant', content }
    const sse = value => `data: ${JSON.stringify({ ...identity, object: 'chat.completion.chunk', ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(sse({ choices: [{ index: 0, delta, finish_reason: null }] })
      + sse({ choices: [{ index: 0, delta: {}, finish_reason: write ? 'tool_calls' : 'stop' }] })
      + sse({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

async function enroll(page, server, id, directory, environment, upstream, processes) {
  await page.goto(`${server.origin}/settings/computers`)
  await page.getByLabel('电脑 ID', { exact: true }).fill(id)
  await page.getByRole('button', { name: '生成启动命令', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '启动 Ternilo Node' })
  const command = await dialog.locator('[data-node-launch-command]').textContent()
  const token = /--token "([^"\s]+)"/.exec(command)?.[1]
  assert.ok(token)
  await dialog.getByRole('button', { name: '我已保存，关闭', exact: true }).click()
  const origin = `http://127.0.0.1:${await freePort()}`
  const args = ['serve', '--data-dir', path.join(directory, id), '--listen', new URL(origin).host,
    '--node-id', id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
  const env = { ...environment, TERNILO_LOCAL_TOKEN: token }
  const node = startProcess(nodeBinary, args, env)
  processes.push(node)
  await waitForHttp(origin, node)
  const html = await (await fetch(origin)).text()
  const localToken = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
  await serverRequest(origin, '/providers', { token: localToken, body: {
    id: 'machine-model', display_name: `${id} provider`, base_url: upstream.baseUrl,
    protocol: 'openai-chat-completions', api_key_ref: null,
    defaults: { context_window: 32_000, max_output_tokens: 2_048 },
    models: [{ id: `${id}-model`, display_name: `${id} model`, settings: { mode: 'inherit' } }],
    timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 25,
  } })
  return { id, origin, args, env, process: node, token: localToken }
}

async function activate(page, locator) {
  if (page.viewportSize().width <= 639) await locator.tap()
  else await locator.click()
}

async function openPicker(page) {
  const menu = page.getByRole('button', { name: '打开侧边栏', exact: true })
  if (await menu.isVisible()) await activate(page, menu)
  await activate(page, page.getByRole('button', { name: '添加工作区', exact: true }))
  const dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
  await dialog.waitFor()
  return dialog
}
async function chooseDirectory(page, dialog, node, directory) {
  await selectChoice(dialog.getByLabel('运行电脑', { exact: true }), node.id)
  await activate(page, dialog.getByRole('button', { name: '选择文件夹', exact: true }))
  const browser = page.getByRole('dialog', { name: `选择 ${node.id} 上的工作文件夹`, exact: true })
  await activate(page, browser.getByRole('button', { name: '编辑文件夹路径', exact: true }))
  const editor = browser.getByRole('textbox', { name: '编辑文件夹路径', exact: true })
  await editor.fill(directory)
  await editor.press('Enter')
  await browser.getByRole('list', { name: `目录 ${directory}`, exact: true }).waitFor()
  await activate(page, browser.getByRole('button', { name: '选择此文件夹', exact: true }))
  await dialog.waitFor()
  assert.equal(await dialog.locator('[data-selected-directory]').textContent(), directory)
  assert.equal(await dialog.getByLabel('工作区名称', { exact: true }).inputValue(), path.basename(directory))
}
async function verifyLayout(page, dialog) {
  await dialog.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished)))
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth))
  const bounds = await dialog.boundingBox()
  const viewport = page.viewportSize()
  assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width + 1 && bounds.y + bounds.height <= viewport.height + 1)
}
async function runTask(page, node, directory) {
  await activate(page, page.locator('button[title="配置模型"]'))
  await activate(page, page.getByRole('menuitem', { name: /^模型/ }))
  await activate(page, page.getByRole('menuitem', { name: new RegExp(`^${node.id} model`) }))
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(`Create the proof file on ${node.id}`)
  await activate(page, page.getByRole('button', { name: '发送', exact: true }))
  await page.locator('article[data-role="assistant"]').filter({ hasText: `${node.id}-model task completed` }).waitFor()
  assert.equal(await readFile(path.join(directory, 'machine-proof.txt'), 'utf8'), `${node.id}-model`)
}

test('personal Server opens projects on two real machines without a Worker and preserves selection across recovery', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-personal-machines-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  const processes = []
  let browser, upstream, page
  const errors = [], consoleErrors = [], offlineChecks = [], offlineReads = []
  let offlineTarget
  try {
    upstream = await modelFixture()
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 960 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push({ message: message.text(), url: message.location().url }) })
    page.on('response', response => {
      if (response.status() < 400) return
      const url = new URL(response.url())
      const configurationRead = ['/api/v1/catalog', '/api/v1/agent-presets', '/api/v1/providers', '/api/v1/credentials'].includes(url.pathname)
        && (url.searchParams.get('session_id') === offlineTarget?.session || url.searchParams.get('workspace_id') === offlineTarget?.workspace)
      const commandRead = offlineTarget && url.pathname === `/api/v1/sessions/${offlineTarget.session}/commands`
      if (offlineTarget && response.status() === 503 && response.request().method() === 'GET' && (configurationRead || commandRead)) {
        offlineReads.push(response.url())
        offlineChecks.push(response.json().then(body => assert.equal(body.error.code, 'unavailable')))
      } else errors.push(`HTTP ${response.status()} ${url.pathname}`)
    })
    page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(request.failure()?.errorText) })
    await page.goto(server.origin)
    for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await page.request.get(`${server.origin}/assets/${asset}`)).body())
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'), `embedded ${asset} matches the tested source`)
    }
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('dialog').waitFor({ state: 'hidden' })
    const empty = await openPicker(page)
    await empty.getByRole('button', { name: '接入电脑', exact: true }).click()
    await page.locator('[data-my-computers]').waitFor()
    const nodes = []
    for (const id of ['laptop', 'vps']) nodes.push(await enroll(page, server, id, directory, environment, upstream, processes))
    const headers = { token: server.owner.session.access_token }
    // The authenticated browser selects its personal space; use that same scope for assertions.
    headers.tenantId = await page.evaluate(() => localStorage.getItem('ternilo.current-tenant'))
    await until(() => serverRequest(server.origin, '/execution-targets', headers), result => result.executors.filter(item => item.connected).length === 2, 'two machines connect')
    await page.goto(server.origin)
    const records = []
    for (const [index, node] of nodes.entries()) {
      const folder = path.join(directory, node.id, 'work', index ? 'another-project' : 'first-project')
      await mkdir(folder, { recursive: true })
      if (index === 1) await page.setViewportSize({ width: 390, height: 844 })
      const dialog = await openPicker(page)
      await chooseDirectory(page, dialog, node, folder)
      if (index === 0) {
        await selectProject(page, dialog, '新建项目')
        await dialog.getByLabel('项目名称', { exact: true }).fill('Personal development')
        await verifyLayout(page, dialog)
        await page.screenshot({ path: path.join(artifacts, 'workspace-desktop.png') })
      } else {
        for (const width of [390, 320]) {
          await page.setViewportSize({ width, height: 844 })
          await verifyLayout(page, dialog)
          await page.screenshot({ path: path.join(artifacts, `workspace-${width}.png`) })
        }
      }
      await activate(page, dialog.getByRole('button', { name: index ? '打开并开始会话' : '创建项目并开始会话', exact: true }))
      await dialog.waitFor({ state: 'hidden' })
      const id = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
      assert.ok(id, 'folder confirmation opens a session directly')
      const state = await serverRequest(server.origin, '/state', headers)
      const session = state.sessions.find(item => item.identity.session_id === id)
      const workspace = state.workspaces.find(item => item.workspace_id === session.workspace_id)
      assert.equal(workspace.node_id, node.id)
      assert.equal(workspace.title, path.basename(folder))
      const location = await serverRequest(server.origin, `/workspaces/${workspace.workspace_id}/location`, headers)
      assert.equal(location.path, folder)
      await runTask(page, node, folder)
      records.push({ node, folder, session, workspace })
      await page.setViewportSize({ width: 1440, height: 960 })
    }
    assert.equal(records[0].workspace.project_id, records[1].workspace.project_id, 'the existing project can contain workspaces on different computers')
    assert.notEqual(records[0].session.identity.session_id, records[1].session.identity.session_id)
    await page.reload()
    await page.locator('article[data-role="assistant"]').filter({ hasText: 'vps-model task completed' }).waitFor()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Keep this draft on the VPS')
    offlineTarget = { session: records[1].session.identity.session_id, workspace: records[1].workspace.workspace_id }
    await stopProcess(nodes[1].process)
    await until(() => serverRequest(server.origin, '/execution-targets', headers), result => result.executors.find(item => item.executor_id === 'vps')?.connected === false, 'VPS goes offline')
    await page.reload()
    await page.locator('article[data-role="assistant"]').filter({ hasText: 'vps-model task completed' }).waitFor()
    const restarted = startProcess(nodeBinary, nodes[1].args, nodes[1].env)
    processes.push(restarted)
    await waitForHttp(nodes[1].origin, restarted)
    await until(() => serverRequest(server.origin, '/execution-targets', headers), result => result.executors.find(item => item.executor_id === 'vps')?.connected, 'VPS reconnects')
    await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
    assert.equal(await page.getByRole('textbox', { name: '输入任务', exact: true }).inputValue(), 'Keep this draft on the VPS')
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-session')), records[1].session.identity.session_id)
    for (const record of records) assert.equal(await readFile(path.join(record.folder, 'machine-proof.txt'), 'utf8'), `${record.node.id}-model`)
    assert.ok(upstream.calls.some(call => call.model === 'laptop-model'))
    assert.ok(upstream.calls.some(call => call.model === 'vps-model'))
    await Promise.all(offlineChecks)
    const unmatchedReads = [...offlineReads]
    for (const error of consoleErrors) {
      const index = unmatchedReads.indexOf(error.url)
      if (index >= 0 && error.message === 'Failed to load resource: the server responded with a status of 503 (Service Unavailable)') unmatchedReads.splice(index, 1)
      else errors.push(error.message)
    }
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'personal-machines-result.json'), JSON.stringify({ sessions: records.map(item => item.session.identity.session_id), machines: nodes.map(item => item.id), expectedOfflineReads: offlineReads.length, errors }, null, 2))
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'personal-machines-failure.png'), fullPage: true }).catch(() => {})
    error.message += `\n${processes.map(item => item.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const item of processes.reverse()) await stopProcess(item)
    await upstream?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
