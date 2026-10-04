import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, [
    'serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory,
  ], { cwd: repository, stdio: ['ignore', 'pipe', 'pipe'] })
  let diagnostics = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { diagnostics += chunk })
  const origin = new Promise((resolve, reject) => {
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      output += chunk
      const match = output.match(/Ternilo local web: (http:\/\/[^\s]+)/)
      if (match) resolve(match[1])
    })
    child.once('exit', code => reject(new Error(`Ternilo web exited with ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin, diagnostics: () => diagnostics }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

function sse(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

function timeout(promise, label, duration = 15_000) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(
      () => reject(new Error(`timed out waiting for ${label}`)),
      duration,
    )),
  ])
}

async function startControlledModel() {
  const records = []
  const waiters = new Map()
  const pending = new Map()
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const chunks = []
    incoming.on('data', chunk => chunks.push(chunk))
    incoming.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      if (typeof body.instructions === 'string' && body.instructions.includes('You name software-agent conversations')) {
        const title = '队列与引导验收'
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: title }),
          sse({ type: 'response.completed', response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: title }] }],
            usage: { input_tokens: 8, output_tokens: 3 },
          } }),
        ].join(''))
        return
      }
      const index = records.length
      const record = {
        headers: incoming.headers,
        body,
      }
      records.push(record)
      waiters.get(index)?.(record)
      waiters.delete(index)
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-request-id': `queue-steering-${index + 1}`,
      })
      response.write(sse({
        type: 'response.output_text.delta',
        output_index: 0,
        content_index: 0,
        delta: `请求 ${index + 1} 正在处理。`,
      }))
      pending.set(index, response)
      response.once('close', () => pending.delete(index))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    count: () => records.length,
    isPending: index => pending.has(index),
    request(index) {
      if (records[index]) return Promise.resolve(records[index])
      return timeout(new Promise(resolve => waiters.set(index, resolve)), `model request ${index}`)
    },
    release(index) {
      const response = pending.get(index)
      assert.ok(response, `model request ${index} must still be streaming`)
      response.write(sse({
        type: 'response.output_text.delta',
        output_index: 0,
        content_index: 0,
        delta: ` 请求 ${index + 1} 已完成。`,
      }))
      response.end(sse({
        type: 'response.completed',
        response: {
          status: 'completed',
          output: [],
          usage: {
            input_tokens: 20 + index,
            output_tokens: 5,
            input_tokens_details: { cached_tokens: index },
            output_tokens_details: { reasoning_tokens: 0 },
          },
        },
      }))
    },
    async close() {
      for (const response of pending.values()) response.destroy()
      await new Promise((resolve, reject) => {
        server.close(error => error ? reject(error) : resolve())
      })
    },
  }
}

function latestUserText(record) {
  const users = record.body.input.filter(item => item.role === 'user')
  const content = users.at(-1)?.content ?? []
  return content
    .filter(item => item.type === 'input_text')
    .map(item => item.text)
    .join('\n')
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathEditor.fill(workspace)
  await pathEditor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function configureProvider(page, baseUrl) {
  await page.getByRole('button', { name: '设置' }).click()
  const dialog = page.getByRole('dialog', { name: '设置' })
  await dialog.getByRole('button', { name: '模型', exact: true }).click()
  await dialog.getByRole('button', { name: '添加 Provider' }).first().click()
  const editor = dialog.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID', { exact: true }).fill('queue-fixture')
  await editor.getByLabel('显示名称', { exact: true }).fill('Queue Fixture')
  await editor.getByLabel('API 地址', { exact: true }).fill(baseUrl)
  await editor.getByLabel('API Key', { exact: true }).fill('queue-fixture-secret')
  await dialog.getByRole('textbox', { name: '模型 ID 1' }).fill('queue-model')
  await dialog.getByRole('textbox', { name: '显示名称（可选） 1' }).fill('Queue Model')
  await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
  await dialog.getByRole('button', { name: '添加 Provider', exact: true }).last().click()
  await dialog.getByText('Queue Fixture', { exact: true }).waitFor()
  await dialog.getByRole('button', { name: '关闭设置' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.getByRole('button', { name: /Queue Model/ }).waitFor()
}

async function readInbox(page) {
  return page.evaluate(async () => {
    const sessionId = localStorage.getItem('ternilo.current-session')
    if (!sessionId) throw new Error('current session is missing')
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/queue`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
    })
    if (!response.ok) throw new Error(`inbox failed: ${response.status}`)
    return response.json()
  })
}

async function waitForInbox(page, predicate, label, duration = 15_000) {
  const deadline = Date.now() + duration
  let last
  while (Date.now() < deadline) {
    last = await readInbox(page)
    if (predicate(last)) return last
    await page.waitForTimeout(100)
  }
  throw new Error(`timed out waiting for ${label}: ${JSON.stringify(last)}`)
}

function queueResponse(page, input, delivery, action) {
  const response = page.waitForResponse(candidate => {
    if (candidate.request().method() !== 'POST') return false
    if (!/\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(candidate.url()).pathname)) return false
    const body = candidate.request().postDataJSON()
    return body?.delivery === delivery && body?.content?.input === input
  })
  return action().then(() => response)
}

async function submitWithKeyboard(page, input, delivery = 'queue') {
  const editor = page.getByRole('textbox', { name: '输入任务' })
  await editor.fill(input)
  const response = await queueResponse(
    page,
    input,
    delivery,
    () => editor.press(delivery === 'steer' ? 'Control+Enter' : 'Enter'),
  )
  return { status: response.status(), body: await response.json() }
}

function queueRow(page, text) {
  return page.locator('[data-queue-dock] li').filter({ hasText: text })
}

test('queued inputs become separate user messages in one turn; interruption stops before sending the next batch', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-queue-e2e-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const model = await startControlledModel()
  const ternilo = startTernilo(dataDirectory)
  let browser
  let page
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    page = await context.newPage()
    const pageErrors = []
    const consoleErrors = []
    const httpErrors = []
    const observerRequests = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    page.on('response', response => {
      if (response.status() >= 400) httpErrors.push(`${response.status()} ${new URL(response.url()).pathname}`)
    })
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureProvider(page, model.baseUrl)

    const observer = await context.newPage()
    observer.on('pageerror', error => pageErrors.push(error.message))
    observer.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    observer.on('request', request => observerRequests.push({
      method: request.method(),
      path: new URL(request.url()).pathname,
    }))
    await observer.goto(origin, { waitUntil: 'domcontentloaded' })
    await observer.getByRole('textbox', { name: '输入任务' }).waitFor()
    await observer.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor()
    observerRequests.length = 0

    const initial = 'A: keep this turn running'
    const queuedFirst = 'B: add the first requirement'
    const queuedSecond = 'C: add a second requirement'
    const editedSecond = 'C: edited requirement'
    const userMessages = record => record.body.input.filter(item => item.role === 'user')
      .map(item => item.content.filter(part => part.type === 'input_text').map(part => part.text).join('\n'))
    const finish = () => waitForInbox(page, inbox => !inbox.active_run_id && inbox.items.length === 0, 'empty batch queue')
    const interrupted = async (index, nextIndex) => {
      const request = await model.request(nextIndex)
      await page.waitForTimeout(150)
      assert.equal(model.isPending(index), false, 'old model request must stop before the next batch')
      return request
    }

    const initialResult = await submitWithKeyboard(page, initial)
    assert.equal(initialResult.status, 201)
    const request0 = await model.request(0)
    assert.equal(latestUserText(request0), initial)
    assert.equal(request0.headers.authorization, 'Bearer queue-fixture-secret')
    await page.getByRole('button', { name: '停止运行', exact: true }).waitFor()
    assert.equal(await page.locator('[data-queue-dock]').count(), 0, 'running A is not repeated above the composer')

    await submitWithKeyboard(page, queuedFirst)
    await queueRow(observer, queuedFirst).waitFor()
    await submitWithKeyboard(page, queuedSecond)
    await queueRow(page, queuedSecond).waitFor()
    assert.equal(await page.locator('[data-current-task]').count(), 0)
    await queueRow(page, queuedSecond).getByRole('button', { name: '编辑排队消息' }).click()
    const queueEditor = page.getByRole('textbox', { name: '编辑排队消息' })
    await queueEditor.fill(editedSecond)
    await page.getByRole('button', { name: '保存排队消息' }).click()
    await queueRow(page, editedSecond).waitFor()
    await submitWithKeyboard(page, 'remove this pending input')
    await queueRow(page, 'remove this pending input').getByRole('button', { name: '删除排队消息' }).click()
    await queueRow(page, 'remove this pending input').waitFor({ state: 'detached' })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await queueRow(page, editedSecond).waitFor()
    assert.equal(model.count(), 1, 'B and C wait for the entire A turn')

    model.release(0)
    const request1 = await model.request(1)
    assert.deepEqual(userMessages(request1), [initial, queuedFirst, editedSecond])
    const lastInputs = request1.body.input.slice(-2)
    assert.deepEqual(lastInputs.map(input => input.role), ['user', 'user'], 'B and C are separate API messages, not joined strings')
    const runningBatch = await readInbox(page)
    assert.ok(runningBatch.active_run_id)
    assert.ok(runningBatch.items.every(item => item.placement === 'running'))
    await page.locator('[data-role="user"]').filter({ hasText: queuedFirst }).waitFor()
    await page.locator('[data-role="user"]').filter({ hasText: editedSecond }).waitFor()
    await page.locator('[data-queue-dock]').waitFor({ state: 'detached' })
    await submitWithKeyboard(page, 'D: arrived during BC')
    assert.equal(model.count(), 2)
    model.release(1)
    const request2 = await model.request(2)
    assert.equal(latestUserText(request2), 'D: arrived during BC')
    model.release(2)
    await finish()
    await page.getByRole('button', { name: '发送', exact: true }).waitFor()

    await submitWithKeyboard(page, 'A2: interrupt this request')
    await model.request(3)
    const active = await waitForInbox(page, inbox => inbox.active_run_id, 'active A2')
    await submitWithKeyboard(page, 'B2: first pending input')
    await submitWithKeyboard(page, 'C2: second pending input')
    const sendAll = page.waitForResponse(response => response.request().method() === 'POST' && /\/queue\/[^/]+\/steer$/.test(new URL(response.url()).pathname))
    await page.getByRole('textbox', { name: '输入任务' }).press('Control+Enter')
    assert.equal((await sendAll).status(), 200)
    const request4 = await interrupted(3, 4)
    assert.deepEqual(userMessages(request4).slice(-2), ['B2: first pending input', 'C2: second pending input'])
    const next = await waitForInbox(page, inbox => inbox.active_run_id && inbox.active_run_id !== active.active_run_id, 'new BC turn')
    assert.notEqual(next.active_run_id, active.active_run_id)
    await page.locator('[data-queue-dock]').waitFor({ state: 'detached' })
    const urgent = await submitWithKeyboard(page, 'E: new immediate input', 'steer')
    assert.equal(urgent.status, 201)
    const request5 = await interrupted(4, 5)
    assert.equal(latestUserText(request5), 'E: new immediate input')
    model.release(5)
    await finish()
    await page.getByRole('button', { name: '发送', exact: true }).waitFor()

    await submitWithKeyboard(page, 'A3: stop without sending')
    await model.request(6)
    await submitWithKeyboard(page, 'F: preserve pending input')
    await submitWithKeyboard(page, 'G: preserve another input')
    await page.getByRole('button', { name: '停止运行', exact: true }).click()
    await waitForInbox(page, inbox => inbox.paused && !inbox.active_run_id && inbox.items.length === 2, 'paused queue')
    await page.waitForTimeout(200)
    assert.equal(model.count(), 7, 'ordinary Stop must not submit the pending batch')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await queueRow(page, 'G: preserve another input').waitFor()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('[data-app-frame][data-mobile="true"]').waitFor()
    await page.keyboard.press('Escape')
    await page.locator('[data-app-frame][data-mobile="true"]:not([data-mobile-sidebar-open])').waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    const dock = page.locator('[data-queue-dock]')
    const dockBox = await dock.boundingBox()
    assert.ok(dockBox && dockBox.x >= 0 && dockBox.x + dockBox.width <= 390)
    for (const button of await dock.locator('[data-slot="button"]:visible').all()) {
      const box = await button.boundingBox()
      assert.ok(box && box.width >= 40 && box.height >= 40)
    }
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'queue-mobile.png'), animations: 'disabled' })
    }
    await dock.getByRole('button', { name: '发送全部', exact: true }).click()
    const request7 = await interrupted(6, 7)
    assert.deepEqual(userMessages(request7).slice(-2), ['F: preserve pending input', 'G: preserve another input'])
    model.release(7)
    await finish()
    assert.equal(model.count(), 8)
    await page.locator('[data-queue-dock]').waitFor({ state: 'detached' })
    assert.equal(await page.locator('[data-current-task]').count(), 0)

    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(httpErrors, [])
    assert.equal(ternilo.diagnostics().includes('queue-fixture-secret'), false)
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page?.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'queue-failure.png') }).catch(() => {})
    }
    console.error(ternilo.diagnostics())
    throw error
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
