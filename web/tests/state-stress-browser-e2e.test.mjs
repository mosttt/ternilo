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
const binary = path.join(repository, 'target', 'debug', 'ternilo')
const submissions = 9

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
  })
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

function sse(payload) { return `data: ${JSON.stringify(payload)}\n\n` }

async function startStreamingModel() {
  let requests = 0
  let completed = 0
  const timers = new Set()
  const server = createServer((request, response) => {
    if (request.method === 'GET' && request.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ data: [{ id: 'stress-model', name: 'Stress Model' }] }))
      return
    }
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const body = []
    request.on('data', chunk => body.push(chunk))
    request.on('end', () => {
      const parsed = JSON.parse(Buffer.concat(body).toString('utf8'))
      if (typeof parsed.instructions === 'string' && parsed.instructions.includes('You name software-agent conversations')) {
        const title = '流式状态压力验收'
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
      const index = requests++
      const chunkCount = index === 0 ? 700 : 36
      const delay = index === 0 ? 6 : 3
      let cursor = 0
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-request-id': `state-stress-${index}`,
      })
      const timer = setInterval(() => {
        if (response.destroyed) {
          clearInterval(timer)
          timers.delete(timer)
          return
        }
        if (cursor < chunkCount) {
          response.write(sse({
            type: 'response.output_text.delta', output_index: 0, content_index: 0,
            delta: `stream-${index}-${cursor} ${'x'.repeat(28)}\n`,
          }))
          cursor += 1
          return
        }
        clearInterval(timer)
        timers.delete(timer)
        response.end(sse({
          type: 'response.completed',
          response: {
            status: 'completed', output: [],
            usage: { input_tokens: 40 + index, output_tokens: chunkCount * 8 },
          },
        }))
        completed += 1
      }, delay)
      timers.add(timer)
      response.once('close', () => {
        if (cursor < chunkCount) {
          clearInterval(timer)
          timers.delete(timer)
        }
      })
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    requests: () => requests,
    completed: () => completed,
    close: async () => {
      for (const timer of timers) clearInterval(timer)
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
    },
  }
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const editor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await editor.fill(workspace)
  await editor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function apiRequest(page, pathValue, method, bodyValue) {
  return page.evaluate(async ({ pathValue, method, bodyValue }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
      body: bodyValue === undefined ? undefined : JSON.stringify(bodyValue),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, bodyValue })
}

async function configureProvider(page, baseUrl) {
  await apiRequest(page, '/credentials', 'POST', { name: 'TERNILO_PROVIDER_STRESS_API_KEY', value: 'stress-key' })
  await apiRequest(page, '/providers', 'POST', {
    id: 'stress-fixture', display_name: 'Stress Fixture', base_url: baseUrl,
    protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_STRESS_API_KEY',
    defaults: { context_window: 128_000, max_output_tokens: 32_000 },
    models: [{ id: 'stress-model', display_name: 'Stress Model', settings: { mode: 'inherit' } }],
    timeout_ms: 120_000, max_attempts: 1, retry_base_delay_ms: 10,
  })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  await apiRequest(page, `/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
    model: { provider: 'named_provider', provider_id: 'stress-fixture', model: 'stress-model' },
  })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Stress Model/ }).waitFor()
}

async function eventually(check, label, timeout = 60_000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    if (await check()) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`timed out waiting for ${label}`)
}

async function memorySnapshot(page, cdp) {
  await cdp.send('HeapProfiler.collectGarbage')
  const { metrics } = await cdp.send('Performance.getMetrics')
  const heap = metrics.find(metric => metric.name === 'JSHeapUsedSize')?.value
  assert.equal(typeof heap, 'number', 'Chromium did not expose JSHeapUsedSize')
  return page.evaluate(heapBytes => ({
    heapBytes,
    elements: document.querySelectorAll('*').length,
    textNodes: document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT).nextNode()
      ? (() => {
          const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT)
          let count = 0
          while (walker.nextNode()) count += 1
          return count
        })()
      : 0,
  }), heap)
}

test('Local Salvo stays interactive and bounded under rapid queueing, long streaming, and view churn', { timeout: 150_000 }, async t => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-state-stress-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const model = await startStreamingModel()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    const consoleErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()) })
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureProvider(page, model.baseUrl)

    const cdp = await page.context().newCDPSession(page)
    await cdp.send('Performance.enable')
    const baseline = await memorySnapshot(page, cdp)
    await page.evaluate(() => {
      const frame = document.querySelector('[data-app-frame]')
      const center = document.querySelector('[data-app-center-column]')
      const composer = document.querySelector('[data-composer-input]')
      if (!frame || !center || !composer) throw new Error('stress identity roots missing')
      frame.setAttribute('data-stress-frame', 'stable')
      center.setAttribute('data-stress-center', 'stable')
      composer.setAttribute('data-stress-composer', 'stable')
    })

    let input = page.getByRole('textbox', { name: '输入任务' })
    await input.fill('stress-input-0')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByRole('button', { name: '停止运行' }).waitFor()
    await page.waitForFunction(() => document.body.innerText.includes('stream-0-4'))

    for (let index = 1; index < submissions; index += 1) {
      input = page.getByRole('textbox', { name: '输入任务' })
      assert.equal(await input.isEditable(), true, `composer locked before queued submission ${index}`)
      await input.fill(`stress-input-${index}`)
      await page.getByRole('button', { name: '发送' }).click()
      await page.waitForFunction(() => document.querySelector('[data-composer-input]')?.value === '')
    }

    for (let cycle = 0; cycle < 5; cycle += 1) {
      await page.getByRole('tab', { name: '轨迹' }).click()
      await page.getByRole('toolbar', { name: '轨迹工具栏' }).waitFor()
      assert.equal(await page.locator('[data-trajectory-state="ready"]').count(), 1)
      const record = page.locator('[data-trajectory-record]').first()
      if (await record.count()) {
        await record.click()
        const details = page.getByRole('complementary', { name: '详情' })
        await details.waitFor()
        await details.getByRole('button', { name: '关闭详情' }).click()
      }
      await page.getByRole('tab', { name: '对话' }).click()
      await page.getByRole('button', { name: '设置' }).click()
      const settings = page.getByRole('dialog', { name: '设置' })
      await settings.getByRole('button', { name: '关闭设置' }).click()
      await page.getByRole('button', { name: '收起侧边栏' }).click()
      await page.getByRole('button', { name: '展开侧边栏' }).click()
    }

    await eventually(() => model.completed() === submissions, `${submissions} streamed responses`)
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    await page.waitForFunction(expected => (
      document.querySelectorAll('article[data-role="user"]').length === expected
      && !document.querySelector('[data-composer-card][data-busy]')
    ), submissions)

    input = page.getByRole('textbox', { name: '输入任务' })
    assert.equal(await input.isEditable(), true)
    await input.fill('composer remains editable after stress')
    assert.equal(await input.inputValue(), 'composer remains editable after stress')
    await input.fill('')
    assert.equal(model.requests(), submissions)

    const identity = await page.evaluate(() => ({
      frame: document.querySelector('[data-app-frame]')?.getAttribute('data-stress-frame'),
      center: document.querySelector('[data-app-center-column]')?.getAttribute('data-stress-center'),
      composer: document.querySelector('[data-composer-input]')?.getAttribute('data-stress-composer'),
    }))
    assert.deepEqual(identity, { frame: 'stable', center: 'stable', composer: 'stable' })
    assert.equal(await page.locator('[data-session-state="ready"]').count(), 1)
    assert.equal(await page.locator('[data-workspace-browser-state="ready"]').count(), 1)

    const final = await memorySnapshot(page, cdp)
    const heapLimit = Math.max(baseline.heapBytes * 2.5, baseline.heapBytes + 32 * 1024 * 1024)
    assert.equal(final.heapBytes <= heapLimit, true, `heap exceeded bound: ${JSON.stringify({ baseline, final, heapLimit })}`)
    assert.equal(final.elements <= baseline.elements + 4_000, true, `DOM element growth exceeded bound: ${JSON.stringify({ baseline, final })}`)
    assert.equal(final.textNodes <= baseline.textNodes + 5_000, true, `DOM text growth exceeded bound: ${JSON.stringify({ baseline, final })}`)
    t.diagnostic(`bounded state: ${JSON.stringify({ submissions, baseline, final, heapLimit })}`)
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
  } catch (cause) {
    throw new Error(`${cause instanceof Error ? cause.message : String(cause)}\nserver=${ternilo.diagnostics()}\nmodel=${JSON.stringify({ requests: model.requests(), completed: model.completed() })}`, { cause })
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
