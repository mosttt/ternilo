import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const thoughts = ['先读取工作区中的文件，再核对工具返回内容。', '文件读取成功，现在整理结果。']
const sse = value => `data: ${JSON.stringify(value)}\n\n`
const reasoningItem = text => ({ type: 'reasoning', summary: [], content: [{ type: 'reasoning_text', text }] })

async function modelFixture() {
  const requests = []
  let release
  const server = createServer(async (incoming, response) => {
    if (incoming.method === 'GET' && incoming.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ data: [{ id: 'deepseek-fixture', name: 'DeepSeek fixture', protocol: 'deepseek-responses', context_window: 1048576, max_output_tokens: 393216 }] }))
      return
    }
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') { response.writeHead(404).end(); return }
    const chunks = []
    for await (const chunk of incoming) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    if (body.instructions?.includes('You name software-agent conversations')) {
      response.end(sse({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', content: [{ type: 'output_text', text: 'DeepSeek reasoning' }] }] } }))
      return
    }
    const index = requests.push(body) - 1
    const thought = thoughts[Math.min(index, 1)]
    const item = reasoningItem(thought)
    response.write(sse({ type: 'response.reasoning_text.delta', output_index: 0, content_index: 0, delta: thought }))
    const finish = () => {
      if (response.destroyed || response.writableEnded) return
      for (const value of [
        { type: 'response.reasoning_text.done', output_index: 0, content_index: 0, text: thought },
        { type: 'response.content_part.done', output_index: 0, content_index: 0, part: item.content[0] },
        { type: 'response.output_item.done', output_index: 0, item },
      ]) response.write(sse(value))
      const output = [item]
      if (index === 0) {
        const tool = { type: 'function_call', call_id: 'read-fixture', name: 'read_file', arguments: JSON.stringify({ path: 'fixture.txt' }) }
        output.push(tool)
        response.write(sse({ type: 'response.output_item.done', output_index: 1, item: tool }))
      } else {
        output.push({ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: '验证完成，文件内容已读取。' }] })
        response.write(sse({ type: 'response.output_text.delta', output_index: 1, content_index: 0, delta: '验证完成，文件内容已读取。' }))
      }
      response.end(sse({ type: 'response.completed', response: { status: 'completed', output, usage: { input_tokens: 30, output_tokens: 20, output_tokens_details: { reasoning_tokens: 12 } } } }))
    }
    if (index === 0) release = finish
    else finish()
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests,
    release: () => release?.(),
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }),
  }
}

async function api(page, endpoint, method = 'GET', body) {
  return page.evaluate(async ({ endpoint, method, body }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${endpoint}`, {
      method, headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { endpoint, method, body })
}

async function screenshot(page, name) {
  const directory = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (!directory) return
  await mkdir(directory, { recursive: true })
  await page.screenshot({ path: path.join(directory, `${name}.png`), fullPage: true })
}

async function assertFits(page, locator) {
  const box = await locator.boundingBox()
  assert.ok(box && box.x >= 0 && box.x + box.width <= page.viewportSize().width + 1)
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
}

test('DeepSeek protocol selection, live reasoning, tool history and persisted mobile disclosure', { timeout: 120_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-deepseek-browser-'))
  const workspace = path.join(directory, 'workspace')
  await mkdir(workspace)
  await writeFile(path.join(workspace, 'fixture.txt'), 'Verified fixture content')
  const model = await modelFixture()
  const origin = `http://127.0.0.1:${await freePort()}`
  const app = startProcess(process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
    'serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data'),
  ], { XDG_STATE_HOME: path.join(directory, 'state') })
  let browser, page
  const errors = []
  try {
    await waitForHttp(origin, app)
    for (const asset of ['app.js', 'app.css']) {
      const actual = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
      const expected = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(actual).digest('hex'), createHash('sha256').update(expected).digest('hex'))
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 850 }, hasTouch: true })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
    page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(request.failure()?.errorText) })
    await page.goto(origin)
    const workspaceRecord = await api(page, '/workspaces', 'POST', { path: workspace })
    const session = await api(page, '/sessions', 'POST', { workspace_id: workspaceRecord.workspace_id, agent_preset: 'standard' })
    const sessionId = session.identity.session_id
    await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), sessionId)
    await page.reload()
    await page.getByRole('button', { name: '设置', exact: true }).click()
    const settings = page.getByRole('dialog', { name: '设置', exact: true })
    await settings.getByRole('button', { name: '模型', exact: true }).click()
    await settings.getByRole('button', { name: '添加 Provider', exact: true }).first().click()
    const editor = settings.locator('[data-provider-editor="new"]')
    await editor.getByLabel('Provider ID', { exact: true }).fill('deepseek-test')
    await editor.getByLabel('显示名称', { exact: true }).fill('DeepSeek test')
    await editor.getByLabel('API 地址', { exact: true }).fill(model.baseUrl)
    const protocol = editor.getByLabel('API 协议', { exact: true })
    assert.equal(await protocol.getAttribute('data-choice-value'), 'openai-responses')
    await selectChoice(protocol, 'deepseek-responses')
    await editor.locator('[id$="-provider-defaults-context"]').fill('128K')
    await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
    await editor.getByRole('switch', { name: '启用 Provider 默认模型设置 的推理强度' }).click()
    await editor.getByRole('checkbox', { name: 'max', exact: true }).check()
    await selectChoice(editor.locator('[id$="-provider-defaults-default-effort"]'), 'max')
    await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
    const discovery = page.getByRole('dialog', { name: '选择要添加的模型' })
    await discovery.getByRole('button', { name: '应用所选', exact: true }).click()
    assert.equal(await editor.getByLabel('模型 ID 1', { exact: true }).inputValue(), 'deepseek-fixture')
    assert.equal(await editor.getByLabel('模型 ID 2', { exact: true }).count(), 0)
    await editor.getByRole('button', { name: '模型详细设置 1', exact: true }).click()
    const fields = editor.locator('[data-model-settings="deepseek-fixture"]')
    assert.equal(await fields.getByLabel('上下文窗口来源').innerText(), '自动 · 上游提供')
    assert.equal(await fields.getByLabel('推理强度', { exact: true }).innerText(), '自动 · Provider 默认')
    await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
    assert.equal((await api(page, '/providers')).find(provider => provider.id === 'deepseek-test').protocol, 'deepseek-responses')
    await settings.getByRole('button', { name: '关闭设置', exact: true }).click()
    await api(page, `/sessions/${sessionId}`, 'PATCH', { model: { provider: 'named_provider', provider_id: 'deepseek-test', model: 'deepseek-fixture' } })
    await page.reload()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('请读取 fixture.txt 并报告结果')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    const live = page.locator('[data-reasoning-row][data-state="running"]')
    await live.waitFor()
    for (const width of [1280, 390, 320]) {
      await page.setViewportSize({ width, height: 850 })
      const toggle = live.locator('[data-disclosure-row]')
      assert.equal(await toggle.getAttribute('aria-expanded'), 'false')
      await toggle.click()
      assert.equal(await live.locator('[data-reasoning-body]').textContent(), thoughts[0])
      await assertFits(page, live)
      await screenshot(page, `deepseek-live-${width}`)
      await toggle.click()
      assert.equal(await live.locator('[data-reasoning-body]').count(), 0)
    }
    model.release()
    await page.getByText('验证完成，文件内容已读取。', { exact: true }).waitFor()
    await page.locator('[data-reasoning-row][data-state="running"]').waitFor({ state: 'detached' })
    assert.equal(model.requests.length, 2)
    assert.ok(model.requests.every(request => request.max_output_tokens === 393216 && request.reasoning?.effort === 'max'), JSON.stringify(model.requests.map(request => ({ reasoning: request.reasoning, output: request.max_output_tokens }))))
    const history = model.requests[1].input
    const thoughtIndex = history.findIndex(item => item.type === 'reasoning')
    assert.ok(thoughtIndex >= 0)
    assert.deepEqual(history[thoughtIndex], reasoningItem(thoughts[0]))
    assert.equal(history[thoughtIndex + 1].type, 'function_call')
    assert.equal(history[thoughtIndex + 2].type, 'function_call_output')
    assert.match(history[thoughtIndex + 2].output, /Verified fixture content/)
    await page.reload()
    const turnProcess = page.locator('[data-turn-process]')
    await turnProcess.first().waitFor()
    if (await turnProcess.first().getAttribute('aria-expanded') === 'false') await turnProcess.first().click()
    const rows = page.locator('[data-reasoning-row][data-state="complete"]')
    await rows.nth(1).waitFor()
    assert.equal(await rows.count(), 2)
    for (let index = 0; index < 2; index++) {
      const row = rows.nth(index)
      const toggle = row.locator('[data-disclosure-row]')
      if (await toggle.getAttribute('aria-expanded') === 'false') await toggle.click()
      assert.equal(await row.locator('[data-reasoning-body]').textContent(), thoughts[index])
      await assertFits(page, row)
    }
    await screenshot(page, 'deepseek-history-320')
    assert.deepEqual(errors, [])
    t.diagnostic('Verified separate protocol, two real model steps, full reasoning history, live 1280/390/320px disclosure and persisted reload.')
  } catch (error) {
    if (page) await screenshot(page, 'deepseek-failure').catch(() => {})
    throw new Error(`${error.message}\nApp: ${app.diagnostics()}\nBrowser: ${JSON.stringify(errors)}\nModel requests: ${model.requests.length}`, { cause: error })
  } finally {
    model.release()
    await browser?.close()
    await stopProcess(app)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
