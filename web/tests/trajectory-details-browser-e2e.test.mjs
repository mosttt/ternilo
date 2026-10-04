import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
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

async function startModelFixture() {
  let requestCount = 0
  const server = createServer((request, response) => {
    if (request.method === 'GET' && request.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ data: [{ id: 'trajectory-model', name: 'Trajectory Model' }] }))
      return
    }
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      if (body.instructions?.includes('You name software-agent conversations')) {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'Trajectory files' })
          + sse({ type: 'response.completed', response: { status: 'completed', output: [] } }))
        return
      }
      const index = requestCount++
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      if (index === 0) {
        const call = { type: 'function_call', call_id: 'trajectory-write', name: 'write_file', arguments: JSON.stringify({ path: 'trajectory-proof.txt', content: 'durable trajectory image' }) }
        response.end([
          sse({ type: 'response.output_item.added', output_index: 0, item: call }),
          sse({ type: 'response.completed', response: { status: 'completed', output: [call], usage: { input_tokens: 8, output_tokens: 2 } } }),
        ].join(''))
        return
      }
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'durable image complete' }),
        sse({ type: 'response.completed', response: { status: 'completed', output: [], usage: { input_tokens: 4, output_tokens: 3 } } }),
      ].join(''))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
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
  const created = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/sessions')
  await page.locator('[data-sidebar-new-session]').click()
  const session = await (await created).json()
  await page.waitForFunction(sessionId => localStorage.getItem('ternilo.current-session') === sessionId, session.identity.session_id)
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function apiRequest(page, pathValue, method, bodyValue) {
  return page.evaluate(async ({ pathValue, method, bodyValue }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
      body: JSON.stringify(bodyValue),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, bodyValue })
}

async function configureProvider(page, baseUrl) {
  await apiRequest(page, '/credentials', 'POST', { name: 'TERNILO_PROVIDER_TRAJECTORY_API_KEY', value: 'trajectory-key' })
  await apiRequest(page, '/providers', 'POST', {
    id: 'trajectory-fixture', display_name: 'Trajectory Fixture', base_url: baseUrl,
    protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_TRAJECTORY_API_KEY',
    defaults: { context_window: 32_000, max_output_tokens: 2_048 },
    models: [{ id: 'trajectory-model', display_name: 'Trajectory Model', settings: { mode: 'inherit' } }],
    timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 10,
  })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  await apiRequest(page, `/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
    model: { provider: 'named_provider', provider_id: 'trajectory-fixture', model: 'trajectory-model' },
  })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Trajectory Model/ }).waitFor().catch(async cause => {
    const models = await apiRequest(page, `/model-options?session_id=${encodeURIComponent(sessionId)}`, 'GET')
    const providers = await apiRequest(page, '/providers', 'GET')
    throw new Error(`${cause.message}\nmodel options: ${JSON.stringify(models)}\nproviders: ${JSON.stringify(providers)}`)
  })
}

test('Trajectory Details shares durable images, releases Session cache, and synchronizes selection', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-trajectory-details-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  let browser, page
  try {
    const origin = await ternilo.origin
    for (const asset of ['app.js', 'app.css']) {
      const actual = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(actual).digest('hex'), createHash('sha256').update(await readFile(path.join(webRoot, 'dist/assets', asset))).digest('hex'))
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    const networkErrors = []
    const resolveRequests = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') pageErrors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) networkErrors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    page.on('request', request => {
      if (request.method() === 'POST' && request.url().includes('/attachments/resolve')) resolveRequests.push({ url: request.url(), body: request.postDataJSON() })
    })
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureProvider(page, model.baseUrl)
    const originalSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    const originalResolveCount = () => resolveRequests.filter(request => request.url.includes(`/sessions/${encodeURIComponent(originalSessionId)}/attachments/resolve`) && request.body?.attachment?.name === 'durable.webp').length

    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.evaluate(async element => {
      const canvas = document.createElement('canvas')
      canvas.width = 3
      canvas.height = 2
      const context = canvas.getContext('2d')
      context.fillStyle = '#18c6b5'
      context.fillRect(0, 0, canvas.width, canvas.height)
      const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'))
      if (!blob) throw new Error('PNG fixture encoding failed')
        const transfer = new DataTransfer()
        transfer.items.add(new File([blob], 'durable.png', { type: 'image/png' }))
        element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }))
    })
    await page.getByRole('button', { name: '预览 durable.webp' }).waitFor()
    await input.fill('durable trajectory image')
    await page.getByRole('button', { name: '发送' }).click()
    try {
      await page.getByRole('tabpanel', { name: '对话' })
        .getByText('durable image complete', { exact: true })
        .waitFor({ timeout: 10_000 })
    } catch (cause) {
      const exported = await page.evaluate(async () => {
        const sessionId = localStorage.getItem('ternilo.current-session')
        const token = window.__TERNILO_BOOT__?.apiToken
        const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, {
          headers: token ? { authorization: `Bearer ${token}` } : {},
        })
        return response.text()
      })
      throw new Error(`${cause instanceof Error ? cause.message : String(cause)}\npage: ${await page.locator('body').innerText()}\nevents: ${exported}\nserver: ${ternilo.diagnostics()}`)
    }
    const chatImage = page.getByRole('button', { name: '打开图片 durable.webp' })
    await chatImage.waitFor()
    assert.equal(originalResolveCount(), 1)
    assert.equal(await readFile(path.join(workspace, 'trajectory-proof.txt'), 'utf8'), 'durable trajectory image')
    const files = page.locator('[data-workspace-panel]')
    await page.locator('[data-workspace-toggle]').click()
    await files.locator('[data-workspace-entry="trajectory-proof.txt"]').click()
    await files.locator('[data-workspace-preview="trajectory-proof.txt"] pre').filter({ hasText: 'durable trajectory image' }).waitFor()

    // Chat -> Trajectory: the shared fact selects the exact tool record.
    const processControl = page.locator('[data-turn-process]').first()
    if (await processControl.count() && await processControl.getAttribute('aria-expanded') === 'false') await processControl.click()
    await page.locator('[data-tool-call-inspect]').click()
    await page.locator('[data-tool-call-id="trajectory-write"][data-selected="true"]').waitFor()
    await files.waitFor({ state: 'detached' })
    await page.getByRole('complementary', { name: '详情' }).waitFor()
    await page.locator('[data-workspace-toggle]').click()
    await files.locator('[data-workspace-preview="trajectory-proof.txt"] pre').waitFor()
    assert.equal(await page.getByRole('complementary', { name: '详情' }).count(), 0)
    await files.getByRole('button', { name: '关闭文件侧栏', exact: true }).click()
    await page.locator('[data-tool-call-inspect]').click()
    await page.getByRole('tab', { name: '轨迹' }).click()
    const selectedTool = page.locator('[data-trajectory-record][data-kind="tool"][data-selected="true"]')
    await selectedTool.waitFor()
    const trajectoryWidth = await page.locator('[data-trajectory-root]').evaluate(element => {
      const scroll = element.closest('[data-conversation-scroll]')
      if (!(scroll instanceof HTMLElement)) throw new Error('conversation scroll host is missing')
      return {
        trajectory: element.getBoundingClientRect().width,
        available: scroll.clientWidth,
      }
    })
    assert.ok(Math.abs(trajectoryWidth.available - trajectoryWidth.trajectory) <= 2, `trajectory must fill the conversation width: ${JSON.stringify(trajectoryWidth)}`)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)

    // Once the conversation scrollport engages sticky positioning, each
    // trajectory chrome row must begin below the preceding one. A stale
    // hard-coded column offset used to hide the first 13px under the timeline.
    await page.setViewportSize({ width: 1440, height: 420 })
    await page.locator('[data-conversation-scroll]').evaluate(element => {
      element.scrollTop = element.scrollHeight
      element.dispatchEvent(new Event('scroll'))
    })
    const stickyGeometry = await page.evaluate(() => {
      const toolbar = document.querySelector('[data-trajectory-toolbar]')?.getBoundingClientRect()
      const timeline = document.querySelector('[data-trajectory-timeline]')?.getBoundingClientRect()
      const columns = document.querySelector('[data-trajectory-column-head]')?.getBoundingClientRect()
      if (!toolbar || !timeline || !columns) throw new Error('trajectory sticky chrome is missing')
      return {
        toolbarBottom: toolbar.bottom,
        timelineTop: timeline.top,
        timelineBottom: timeline.bottom,
        columnsTop: columns.top,
      }
    })
    assert.ok(stickyGeometry.toolbarBottom <= stickyGeometry.timelineTop + 1, JSON.stringify(stickyGeometry))
    assert.ok(stickyGeometry.timelineBottom <= stickyGeometry.columnsTop + 1, JSON.stringify(stickyGeometry))
    await page.setViewportSize({ width: 1440, height: 900 })

    // A selected record remains explicit under search and hidden-call filters.
    const search = page.getByRole('searchbox', { name: '搜索轨迹' })
    await search.fill('query-with-no-record-match')
    await selectedTool.waitFor()
    assert.equal(await search.inputValue(), 'query-with-no-record-match')
    const calls = page.getByRole('toolbar', { name: '轨迹工具栏' }).getByRole('button', { name: '调用' })
    await calls.click()
    await selectedTool.waitFor()
    await search.fill('')
    await calls.click()

    // Collapsing a selected Turn immediately reveals that selection again.
    const turns = page.getByRole('toolbar', { name: '轨迹工具栏' }).getByRole('button', { name: '轮次' })
    await turns.click()
    await page.locator('[data-trajectory-ledger] button[aria-expanded="true"]').waitFor()
    await selectedTool.waitFor()

    // Trajectory -> Chat: a timeline selection is reflected by the Chat tool tree.
    await page.getByRole('complementary', { name: '详情' }).getByRole('button', { name: '关闭详情' }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileTrajectoryWidth = await page.locator('[data-trajectory-root]').evaluate(element => {
      const scroll = element.closest('[data-conversation-scroll]')
      if (!(scroll instanceof HTMLElement)) throw new Error('conversation scroll host is missing')
      return { trajectory: element.getBoundingClientRect().width, available: scroll.clientWidth }
    })
    assert.ok(mobileTrajectoryWidth.available - mobileTrajectoryWidth.trajectory <= 2)
    const mobileStickyGeometry = await page.evaluate(() => {
      const toolbar = document.querySelector('[data-trajectory-toolbar]')?.getBoundingClientRect()
      const timeline = document.querySelector('[data-trajectory-timeline]')?.getBoundingClientRect()
      if (!toolbar || !timeline) throw new Error('mobile trajectory sticky chrome is missing')
      return { toolbarBottom: toolbar.bottom, timelineTop: timeline.top }
    })
    assert.ok(mobileStickyGeometry.toolbarBottom <= mobileStickyGeometry.timelineTop + 1, JSON.stringify(mobileStickyGeometry))
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)

    const touchContext = await browser.newContext({ locale: 'zh-CN',
      storageState: await page.context().storageState(),
      viewport: { width: 390, height: 844 },
      isMobile: true,
      hasTouch: true,
    })
    await touchContext.addInitScript(sessionId => {
      localStorage.setItem('ternilo.current-session', sessionId)
    }, originalSessionId)
    const touchPage = await touchContext.newPage()
    await touchPage.goto(origin, { waitUntil: 'domcontentloaded' })
    const touchTrajectoryButton = touchPage.getByRole('tab', { name: '轨迹', exact: true })
    await touchTrajectoryButton.waitFor({ timeout: 8_000 }).catch(async cause => {
      const state = await touchPage.evaluate(() => ({
        href: location.href,
        title: document.title,
        text: document.body.innerText.slice(0, 2_000),
        session: localStorage.getItem('ternilo.current-session'),
        boot: Boolean(window.__TERNILO_BOOT__),
      }))
      throw new Error(`touch trajectory did not load: ${JSON.stringify(state)}`, { cause })
    })
    await touchTrajectoryButton.click()
    const touchBlock = touchPage.locator('[data-trajectory-timeline-block][data-kind="tool"]')
    const touchBlockBox = await touchBlock.boundingBox()
    assert.ok(touchBlockBox)
    await touchPage.touchscreen.tap(touchBlockBox.x + touchBlockBox.width / 2, touchBlockBox.y + touchBlockBox.height / 2)
    await touchPage.locator('[data-trajectory-record][data-kind="tool"][data-selected="true"]').waitFor()
    await touchContext.close()

    await page.setViewportSize({ width: 1440, height: 900 })
    await turns.click()
    await page.locator('[data-trajectory-ledger] button[aria-expanded="false"]').waitFor()
    const timelineToolBox = await page.locator('[data-trajectory-timeline-block][data-kind="tool"]').boundingBox()
    assert.ok(timelineToolBox)
    await page.mouse.click(timelineToolBox.x + timelineToolBox.width / 2, timelineToolBox.y + timelineToolBox.height / 2)
    await page.locator('[data-trajectory-ledger] button[aria-expanded="true"]').waitFor()
    await selectedTool.waitFor()
    await page.getByRole('tab', { name: '对话' }).click()
    await page.locator('[data-tool-call-id="trajectory-write"][data-selected="true"]').waitFor()
    assert.equal(originalResolveCount(), 1, 'opening files must not release the active session image cache')

    // Leaving the Session releases its shared cache; returning resolves once again.
    await page.locator('[data-sidebar-new-session]').click()
    await page.locator('[data-new-session-hero]').waitFor()
    await page.locator(`[data-sidebar-session-row][data-session-id="${originalSessionId}"]`).getByRole('button').first().click()
    await page.getByRole('button', { name: '打开图片 durable.webp' }).waitFor()
    assert.equal(originalResolveCount(), 2, JSON.stringify(resolveRequests))

    // The durable user image is available in Trajectory Details without a third read.
    await page.getByRole('tab', { name: '轨迹' }).click()
    await page.locator('[data-trajectory-record][data-kind="user"]').click()
    const details = page.getByRole('complementary', { name: '详情' })
    const detailsImage = details.getByRole('button', { name: '打开图片 durable.webp' })
    await detailsImage.waitFor()
    assert.equal(originalResolveCount(), 2)
    assert.ok(resolveRequests.filter(request => request.body?.attachment?.name === 'durable.webp').every(request => request.url.includes(`/sessions/${encodeURIComponent(originalSessionId)}/attachments/resolve`)), 'old attachments must never be resolved with the newly selected session ID')
    await detailsImage.click()
    await page.getByRole('dialog', { name: '图片预览：durable.webp' }).waitFor()
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(networkErrors, [])
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'trajectory-details.png'), animations: 'disabled' }) }
  } catch (error) {
    if (page && artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'trajectory-details-failure.png') }).catch(() => {}) }
    throw error
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
