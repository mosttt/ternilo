import { selectChoice } from './browser-select-fixture.mjs'
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
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')
const axePath = path.join(webRoot, 'node_modules', 'axe-core', 'axe.min.js')

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

function releaseGate() {
  let release
  const promise = new Promise(resolve => { release = resolve })
  return { promise, release }
}

function observePage(page, observations) {
  page.on('websocket', socket => observations.webSockets?.push(socket.url()))
  page.on('request', request => observations.requests?.push({
    method: request.method(),
    url: request.url(),
    at: Date.now(),
  }))
  page.on('pageerror', error => observations.pageErrors.push(error.message))
  page.on('console', message => {
    if (message.type() === 'error') observations.consoleErrors.push(message.text())
  })
  page.on('requestfailed', request => {
    if (request.failure()?.errorText === 'net::ERR_ABORTED' && request.method() === 'GET' && /\/sessions\/[^/]+\/workspace$/.test(new URL(request.url()).pathname)) return
    observations.failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText ?? 'unknown error'}`)
  })
  page.on('response', response => {
    if (response.status() >= 400) {
      observations.failedResponses.push(`${response.request().method()} ${response.url()}: ${response.status()}`)
    }
  })
}

async function startModelFixture() {
  const received = []
  const waiters = new Map()
  const firstAnswer = {
    output: releaseGate(),
    update: releaseGate(),
    completion: releaseGate(),
  }
  const longTail = Array.from(
    { length: 80 },
    (_, index) => `段落 ${index + 1}：验证长会话滚动、中文排版和输入框固定。`,
  )
  const firstAnswerOpenFence = [
    'Provider **路由成功**，并正确解析兼容端点。',
    '',
    '- [x] 流式 Markdown',
    '- [x] `null` 集合兼容',
    '',
    '| 项目 | 状态 |',
    '| --- | --- |',
    '| 缓存统计 | 可用 |',
    '',
    '```rust',
    'fn main() { println!("Ternilo"); }',
    'let partial = 1;',
  ].join('\n')
  const firstAnswerAfterFence = [
    '```',
    '',
    '$$x^2 + y^2$$',
  ].join('\n')
  const firstAnswerCodeTail = '// streamed tail keeps completed lines mounted'

  const server = createServer((incoming, response) => {
    if (incoming.method === 'GET' && incoming.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ data: [
        { id: 'model-a', name: 'Remote Model A' },
        { id: 'model-discovered', name: 'Discovered Model' },
      ] }))
      return
    }
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
        const title = 'Provider 路由与滚动验收'
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
      const record = {
        headers: incoming.headers,
        body,
      }
      const index = received.push(record) - 1
      waiters.get(index)?.(record)
      waiters.delete(index)

      const answerChunks = index === 0
        ? [
            `${firstAnswerOpenFence}\n`,
            `${firstAnswerCodeTail}\n`,
            `${firstAnswerAfterFence}\n\n`,
            ...Array.from({ length: 10 }, (_, group) => `${longTail.slice(group * 8, group * 8 + 8).join('\n\n')}\n\n`),
          ]
        : ['第二轮完成，发送新任务后已回到会话底部。']
      const reasoningChunks = index === 0 ? [
        sse({
          type: 'response.reasoning_summary_text.delta',
          delta: '先检查 Provider 路由与模型参数。',
        }),
        sse({
          type: 'response.reasoning_summary_text.delta',
          delta: '\n再验证 Markdown、统计与滚动契约。',
        }),
      ] : []
      const outputChunks = answerChunks.map(content => sse({
        type: 'response.output_text.delta',
        output_index: 0,
        content_index: 0,
        delta: content,
      }))
      const completed = sse({
        type: 'response.completed',
        response: {
          status: 'completed',
          output: index === 0 ? [{
            type: 'reasoning',
            summary: [{
              type: 'summary_text',
              text: '先检查 Provider 路由与模型参数。\n再验证 Markdown、统计与滚动契约。',
            }],
          }] : [],
          usage: {
            input_tokens: 120,
            output_tokens: 40,
            input_tokens_details: { cached_tokens: 60 },
            output_tokens_details: { reasoning_tokens: index === 0 ? 7 : 0 },
          },
        },
      })
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-request-id': `browser-provider-${index + 1}`,
      })
      if (index !== 0) {
        ;[...outputChunks, completed].forEach((chunk, chunkIndex, stream) => {
          setTimeout(() => {
            response.write(chunk)
            if (chunkIndex === stream.length - 1) response.end()
          }, 100 + chunkIndex * 80)
        })
        return
      }
      void (async () => {
        reasoningChunks.forEach(chunk => response.write(chunk))
        await firstAnswer.output.promise
        response.write(outputChunks[0])
        await firstAnswer.update.promise
        response.write(outputChunks[1])
        await firstAnswer.completion.promise
        outputChunks.slice(2).forEach((chunk, chunkIndex, remaining) => {
          setTimeout(() => {
            response.write(chunk)
            if (chunkIndex === remaining.length - 1) {
              response.write(completed)
              response.end()
            }
          }, 1_800 + chunkIndex * 180)
        })
      })()
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    request(index) {
      if (received[index]) return Promise.resolve(received[index])
      return new Promise(resolve => waiters.set(index, resolve))
    },
    releaseFirstOutput() { firstAnswer.output.release() },
    releaseFirstStreamingUpdate() { firstAnswer.update.release() },
    releaseFirstCompletion() { firstAnswer.completion.release() },
    close: () => new Promise((resolve, reject) => {
      firstAnswer.output.release()
      firstAnswer.update.release()
      firstAnswer.completion.release()
      server.close(error => error ? reject(error) : resolve())
    }),
  }
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  const directoryList = path => dialog.getByRole('list', { name: `目录 ${path}`, exact: true })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  await dialog.getByRole('textbox', { name: '编辑文件夹路径' }).fill(workspace)
  await dialog.getByRole('textbox', { name: '编辑文件夹路径' }).press('Enter')
  await directoryList(workspace).waitFor()
  assert.equal(await dialog.getByRole('list').count(), 2)
  assert.equal(await dialog.getByRole('button', { name: /\.hidden/ }).count(), 0)
  await dialog.getByRole('button', { name: '显示隐藏目录' }).click()
  assert.equal(await dialog.getByRole('button', { name: /\.hidden/ }).isVisible(), true)
  await directoryList(workspace).getByRole('button', { name: /crates/ }).click()
  await directoryList(`${workspace}/crates`).getByRole('button', { name: /nested/ }).waitFor()
  await dialog.getByRole('button', { name: path.basename(workspace), exact: true }).click()
  await directoryList(workspace).waitFor()

  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathInput = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathInput.fill(`${workspace}/do`)
  assert.equal(await directoryList(workspace).getByRole('button', { name: /docs/ }).isVisible(), true)
  assert.equal(await directoryList(workspace).getByRole('button', { name: /crates/ }).count(), 0)
  await pathInput.fill(`${workspace}/missing-prefix`)
  assert.equal(await directoryList(workspace).getByRole('button', { name: /crates/ }).isVisible(), true)
  await pathInput.fill(workspace)
  await pathInput.press('Enter')

  for (const viewport of [{ width: 390, height: 844 }, { width: 390, height: 430 }, { width: 844, height: 390 }]) {
    await page.setViewportSize(viewport)
    const geometry = await dialog.evaluate((element) => ({
      documentWidth: document.documentElement.scrollWidth,
      viewportWidth: document.documentElement.clientWidth,
      dialog: element.getBoundingClientRect().toJSON(),
      buttons: [...element.querySelectorAll('button')]
        .filter(button => button.getClientRects().length > 0)
        .map(button => ({ label: button.getAttribute('aria-label') || button.textContent?.trim(), height: button.getBoundingClientRect().height })),
    }))
    assert.equal(geometry.documentWidth, geometry.viewportWidth, `directory picker overflowed at ${viewport.width}x${viewport.height}`)
    assert.ok(geometry.dialog.left >= 0 && geometry.dialog.right <= viewport.width)
    assert.deepEqual(geometry.buttons.filter(button => button.height < 40), [])
  }
  await page.setViewportSize({ width: 1440, height: 900 })
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.getByPlaceholder('描述你想要构建的内容', { exact: true }).waitFor()
}

async function configureProvider(page, baseUrl) {
  await page.getByRole('button', { name: '设置' }).click()
  const dialog = page.getByRole('dialog', { name: '设置' })
  await dialog.getByRole('button', { name: '模型', exact: true }).click()
  await dialog.getByRole('button', { name: '添加 Provider' }).first().click()
  await dialog.getByLabel('Provider ID', { exact: true }).fill('browser-fixture')
  await dialog.getByLabel('显示名称', { exact: true }).fill('Browser Fixture')
  await dialog.getByLabel('API 地址', { exact: true }).fill(baseUrl)
  await dialog.getByLabel('API Key', { exact: true }).fill('direct-browser-secret')
  await dialog.getByRole('textbox', { name: '模型 ID 1' }).fill('model-a')
  await dialog.getByRole('textbox', { name: '显示名称（可选） 1' }).fill('Model A')
  await dialog.locator('[id$="-provider-defaults-context"]').fill('128K')
  await dialog.locator('[id$="-provider-defaults-output"]').fill('8K')
  await dialog.locator('section[aria-label="Provider 默认模型设置"]').getByRole('switch').click()
  await dialog.getByRole('textbox', { name: 'high实际推理值', exact: true }).fill('ultra')
  await dialog.getByRole('button', { name: '模型详细设置 1' }).click()
  assert.equal(await dialog.getByLabel('上下文窗口来源').getAttribute('data-choice-value'), 'automatic')
  await dialog.locator('[data-model-settings="model-a"] [data-effective-reasoning]').filter({ hasText: '默认 medium' }).waitFor()
  const firstDefaultPersisted = page.waitForResponse(response =>
    response.request().method() === 'PUT' && new URL(response.url()).pathname === '/api/v1/default-model')
  await dialog.getByRole('button', { name: '添加 Provider', exact: true }).last().click()
  assert.equal((await firstDefaultPersisted).status(), 200)
  await dialog.getByRole('status').filter({ hasText: '已保存 Browser Fixture' }).waitFor()
  await dialog.getByRole('button', { name: '编辑' }).click()
  assert.equal(await dialog.getByLabel('API Key', { exact: true }).inputValue(), '')
  assert.match(await dialog.getByLabel('API Key', { exact: true }).getAttribute('placeholder'), /已配置/)
  assert.equal((await page.content()).includes('direct-browser-secret'), false)
  const clientStorage = await page.evaluate(() => {
    const snapshot = storage => Object.fromEntries(
      Array.from({ length: storage.length }, (_, index) => storage.key(index))
        .filter(key => key !== null)
        .map(key => [key, storage.getItem(key)]),
    )
    return { local: snapshot(localStorage), session: snapshot(sessionStorage) }
  })
  assert.equal(JSON.stringify(clientStorage).includes('direct-browser-secret'), false)
  const providers = await page.evaluate(async () => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch('/api/v1/providers', {
      headers: token ? { authorization: `Bearer ${token}` } : {},
    })
    if (!response.ok) throw new Error(`provider inventory failed: ${response.status}`)
    return response.json()
  })
  assert.equal(JSON.stringify(providers).includes('direct-browser-secret'), false)
  await dialog.locator('[data-provider-editor="browser-fixture"] summary').filter({ hasText: '自定义设置' }).click()
  await dialog.getByRole('button', { name: '获取可用模型' }).click()
  const discovery = page.getByRole('dialog', { name: '选择要添加的模型' })
  await discovery.waitFor()
  assert.equal(await discovery.getByText('Discovered Model', { exact: true }).count(), 1)
  assert.equal(await discovery.getByRole('checkbox', { name: /model-a/ }).isChecked(), true)
  assert.equal(await discovery.getByRole('checkbox', { name: /model-discovered/ }).isChecked(), true)
  await discovery.getByRole('button', { name: '应用所选' }).click()
  assert.equal(await dialog.getByRole('textbox', { name: '模型 ID 2' }).inputValue(), 'model-discovered')
  assert.equal(await dialog.getByRole('textbox', { name: '显示名称（可选） 1' }).inputValue(), 'Model A')
  await dialog.getByRole('button', { name: '关闭 Provider 编辑器' }).click()

  await dialog.getByRole('button', { name: '插件', exact: true }).click()
  const pluginRows = dialog.getByRole('switch')
  await pluginRows.first().waitFor()
  assert.equal(await pluginRows.count() > 0, true)
  const filesPlugin = dialog.locator('[data-plugin-id="local-files"]')
  const pluginDescription = await filesPlugin.locator('button span.block').first().textContent()
  assert.equal((pluginDescription?.trim().length ?? 0) > 8, true)
  await filesPlugin.getByRole('button', { name: '展开: local-files' }).click()
  const maxReadBytes = filesPlugin.getByLabel(/Max read bytes/i)
  const inheritedValue = await maxReadBytes.inputValue()
  await maxReadBytes.fill('3145728')
  assert.equal(await filesPlugin.getByText('未保存', { exact: true }).count(), 1)
  await filesPlugin.getByRole('button', { name: '放弃修改' }).click()
  assert.equal(await maxReadBytes.inputValue(), inheritedValue)
  await maxReadBytes.fill('3145728')
  const pluginSave = page.waitForResponse(response => response.request().method() === 'PATCH' && /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname))
  await filesPlugin.getByRole('button', { name: '保存', exact: true }).click()
  assert.equal((await pluginSave).status(), 200)
  await filesPlugin.getByText('会话覆盖', { exact: true }).waitFor()
  await filesPlugin.getByRole('button', { name: '展开: local-files' }).click()
  const pluginReset = page.waitForResponse(response => response.request().method() === 'PATCH' && /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname))
  await filesPlugin.getByRole('button', { name: '移除会话覆盖' }).click()
  assert.equal((await pluginReset).status(), 200)
  await filesPlugin.getByText('会话覆盖', { exact: true }).waitFor({ state: 'detached' })

  await dialog.getByRole('tab', { name: '插件列表' }).click()
  await dialog.getByRole('searchbox', { name: '搜索插件' }).fill('ternilo.files.local')
  assert.equal(Number(await dialog.locator('[data-plugin-count]').textContent()), 1)
  await dialog.locator('[data-plugin-kind="ternilo.files.local"]').getByRole('button').click()
  assert.match(await dialog.locator('[data-plugin-kind="ternilo.files.local"]').textContent(), /提供服务/)
  await dialog.getByRole('tab', { name: '扩展包' }).click()
  assert.match(await dialog.locator('[role="tabpanel"]:visible').textContent(), /WASM|扩展/)

  await dialog.getByRole('button', { name: '关闭设置' }).click()
  await dialog.waitFor({ state: 'detached' })

  const modelButton = page.getByRole('button', { name: /Model A/ })
  const [mainBox, scrollBox, heroBox, modelBox] = await Promise.all([
    page.locator('#conversation-main').boundingBox(),
    page.locator('.conversation-scroll').boundingBox(),
    page.locator('[data-new-session-hero]').boundingBox(),
    modelButton.boundingBox(),
  ])
  assert.ok(mainBox && scrollBox && heroBox && modelBox)
  assert.equal(scrollBox.height > mainBox.height * 0.9, true, JSON.stringify({ mainBox, scrollBox }))
  assert.equal(heroBox.y >= mainBox.y && heroBox.y + heroBox.height <= mainBox.y + mainBox.height, true, JSON.stringify({ mainBox, heroBox }))
  assert.equal(modelBox.y >= mainBox.y && modelBox.y + modelBox.height <= mainBox.y + mainBox.height, true, JSON.stringify({ mainBox, modelBox }))
  assert.equal(await modelButton.evaluate(element => {
    const rect = element.getBoundingClientRect()
    const x = rect.left + rect.width / 2
    const y = rect.top + rect.height / 2
    const hit = document.elementFromPoint(x, y)
    return hit !== null && (hit === element || element.contains(hit))
  }), true)
  await modelButton.click()
  await page.getByRole('menuitem', { name: /推理强度/ }).hover()
  const defaultPersisted = page.waitForResponse(response =>
    response.request().method() === 'PUT' && new URL(response.url()).pathname === '/api/v1/default-model')
  await page.getByRole('menuitem', { name: /^high$/ }).press('Enter')
  assert.equal((await defaultPersisted).status(), 200)

  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Model A.*high/ }).waitFor()
  const inherited = await page.evaluate(async () => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const headers = {
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      'content-type': 'application/json',
    }
    const currentSessionId = localStorage.getItem('ternilo.current-session')
    if (!currentSessionId) throw new Error('current session is missing')
    const defaultResponse = await fetch(`/api/v1/default-model?session_id=${encodeURIComponent(currentSessionId)}`, { headers })
    if (!defaultResponse.ok) throw new Error(`default model read failed: ${defaultResponse.status}`)
    const stateResponse = await fetch('/api/v1/state', { headers })
    const state = await stateResponse.json()
    const workspaceId = state.workspaces[0]?.workspace_id
    if (!workspaceId) throw new Error('workspace is missing')
    const createResponse = await fetch('/api/v1/sessions', {
      method: 'POST', headers, body: JSON.stringify({ workspace_id: workspaceId }),
    })
    if (!createResponse.ok) throw new Error(`new Session failed: ${createResponse.status}`)
    const created = await createResponse.json()
    const deleteResponse = await fetch(`/api/v1/sessions/${encodeURIComponent(created.identity.session_id)}`, {
      method: 'DELETE', headers,
    })
    await deleteResponse.arrayBuffer()
    return { defaultModel: await defaultResponse.json(), sessionModel: created.model }
  })
  assert.deepEqual(inherited.defaultModel, {
    provider: 'named_provider', provider_id: 'browser-fixture', model: 'model-a', reasoning_effort: 'high',
  })
  assert.deepEqual(inherited.sessionModel, inherited.defaultModel)
}

async function dragSeparator(page, name, dx) {
  const handle = page.getByRole('separator', { name })
  const box = await handle.boundingBox()
  assert.ok(box)
  const x = box.x + box.width / 2
  const y = box.y + Math.min(120, box.height / 2)
  await page.mouse.move(x, y)
  await page.mouse.down()
  await page.mouse.move(x + dx, y, { steps: 6 })
  await page.mouse.up()
}

test('model picker persists the host default across reload and new Session creation', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-default-model-e2e-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 800 }, serviceWorkers: 'block' })
    const observations = { pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [] }
    observePage(page, observations)
    await page.goto(origin, { waitUntil: 'networkidle' })
    await page.evaluate(async workspacePath => {
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      const headers = {
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        'content-type': 'application/json',
      }
      const workspaceResponse = await fetch('/api/v1/workspaces', {
        method: 'POST', headers, body: JSON.stringify({ path: workspacePath }),
      })
      const workspace = await workspaceResponse.json()
      const sessionResponse = await fetch('/api/v1/sessions', {
        method: 'POST', headers, body: JSON.stringify({ workspace_id: workspace.workspace_id }),
      })
      const session = await sessionResponse.json()
      const providerResponse = await fetch('/api/v1/providers', {
        method: 'POST', headers, body: JSON.stringify({
          id: 'default-browser', display_name: 'Default Browser',
          base_url: 'http://127.0.0.1:9/v1', protocol: 'openai-responses', api_key_ref: null,
          defaults: { context_window: 128000, max_output_tokens: 8192 },
          models: [{ id: 'model-a', display_name: 'Model A', settings: { mode: 'inherit' } }],
          timeout_ms: 30000, max_attempts: 2, retry_base_delay_ms: 100,
        }),
      })
      if (!workspaceResponse.ok || !sessionResponse.ok || !providerResponse.ok) {
        throw new Error('default model browser fixture initialization failed')
      }
      localStorage.setItem('ternilo.current-workspace', workspace.workspace_id)
      localStorage.setItem('ternilo.current-session', session.identity.session_id)
    }, workspace)
    await page.reload({ waitUntil: 'domcontentloaded' })

    await page.getByRole('button', { name: /默认模型/ }).click()
    await page.getByRole('menuitem', { name: /^模型/ }).hover()
    const defaultSaved = page.waitForResponse(response => response.request().method() === 'PUT'
      && response.url().includes('/api/v1/default-model?'))
    await page.getByRole('menuitem', { name: /Model A/ }).press('Enter')
    await page.getByRole('button', { name: /Model A/ }).waitFor()
    const savedResponse = await defaultSaved
    assert.equal(savedResponse.ok(), true)
    assert.deepEqual(await savedResponse.json(), {
      provider: 'named_provider', provider_id: 'default-browser', model: 'model-a',
    })

    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: /Model A/ }).waitFor()
    const inherited = await page.evaluate(async () => {
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      const headers = {
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        'content-type': 'application/json',
      }
      const currentSessionId = localStorage.getItem('ternilo.current-session')
      const workspaceId = localStorage.getItem('ternilo.current-workspace')
      const defaultResponse = await fetch(`/api/v1/default-model?session_id=${encodeURIComponent(currentSessionId)}`, { headers })
      const createResponse = await fetch('/api/v1/sessions', {
        method: 'POST', headers, body: JSON.stringify({ workspace_id: workspaceId }),
      })
      const created = await createResponse.json()
      const deleteResponse = await fetch(`/api/v1/sessions/${encodeURIComponent(created.identity.session_id)}`, {
        method: 'DELETE', headers,
      })
      await deleteResponse.arrayBuffer()
      return { defaultModel: await defaultResponse.json(), sessionModel: created.model }
    })
    assert.deepEqual(inherited.defaultModel, {
      provider: 'named_provider', provider_id: 'default-browser', model: 'model-a',
    })
    assert.deepEqual(inherited.sessionModel, inherited.defaultModel)
    assert.deepEqual(observations, { pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [] })
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})

test('shared workbench supports provider streaming, trajectory, stable scrolling and mobile layout', { timeout: 240_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-web-e2e-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  await Promise.all([
    mkdir(path.join(workspace, 'docs')),
    mkdir(path.join(workspace, 'crates', 'nested'), { recursive: true }),
    mkdir(path.join(workspace, '.hidden')),
  ])
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' })
    await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin })
    const page = await context.newPage()
    const observations = {
      pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [], requests: [], webSockets: [],
    }
    observePage(page, observations)
    await page.goto(origin, { waitUntil: 'networkidle' })

    const manifest = await page.request.get(`${origin}/manifest.webmanifest`)
    assert.equal(manifest.ok(), true)
    assert.match(manifest.headers()['content-type'], /application\/manifest\+json/)
    assert.equal((await manifest.json()).display, 'standalone')
    assert.equal((await page.request.get(`${origin}/service-worker.js`)).ok(), true)

    await chooseWorkspace(page, workspace)
    const blankSessionRow = page.locator('[data-sidebar-session-row]').first()
    assert.equal(await blankSessionRow.getByText('新会话', { exact: true }).isVisible(), true)
    assert.equal(await blankSessionRow.locator('[data-sidebar-session-time]').count(), 0)
    assert.equal(await blankSessionRow.getByRole('button', { name: /的操作$/ }).count(), 0)
    const newSessionHero = page.locator('[data-new-session-hero]')
    await newSessionHero.waitFor()
    await newSessionHero.getByText('让想法，动起来', { exact: true }).waitFor()
    assert.equal(await newSessionHero.getByText('预览版', { exact: true }).count(), 0)
    assert.equal(await page.getByRole('button', { name: '切换新会话工作区' }).isVisible(), true)
    assert.equal(await page.getByRole('button', { name: /新会话 Agent/ }).isVisible(), true)
    await page.getByPlaceholder('描述你想要构建的内容', { exact: true }).waitFor()
    assert.equal(await page.locator('.session-header').isVisible(), false)
    const modelOnboarding = page.locator('[data-model-onboarding]')
    assert.equal(await modelOnboarding.count(), 1)
    const onboardingInput = page.getByRole('textbox', { name: '输入任务' })
    await onboardingInput.fill('配置模型前保留草稿')
    assert.equal(await page.getByRole('button', { name: '发送' }).isDisabled(), true)
    assert.equal(await onboardingInput.inputValue(), '配置模型前保留草稿')
    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    assert.equal(await page.getByRole('button', { name: '发送' }).evaluate(button => button.getBoundingClientRect().height >= 40), true)
    await page.setViewportSize({ width: 1440, height: 900 })
    await onboardingInput.fill('')
    await page.getByRole('button', { name: '视图选项' }).click()
    assert.equal(await page.getByText('分组方式', { exact: true }).isVisible(), true)
    assert.equal(await page.getByText('排序方式', { exact: true }).isVisible(), true)
    for (const label of ['按工作区', '单列表', '手动排序', '最近更新']) {
      assert.equal(await page.getByRole('menuitem', { name: label }).isVisible(), true)
    }
    const flatViewOption = page.getByRole('menuitem', { name: '单列表' })
    await flatViewOption.click()
    await flatViewOption.waitFor({ state: 'detached' })
    assert.equal(await page.locator('[data-sidebar-workspace-header]').getByText('会话', { exact: true }).isVisible(), true)
    await page.getByRole('button', { name: '视图选项' }).click()
    const manualOrderOption = page.getByRole('menuitem', { name: '手动排序' })
    await manualOrderOption.click()
    await manualOrderOption.waitFor({ state: 'detached' })
    await page.getByRole('button', { name: '视图选项' }).click()
    const workspaceViewOption = page.getByRole('menuitem', { name: '按工作区' })
    await workspaceViewOption.click()
    await workspaceViewOption.waitFor({ state: 'detached' })
    await page.getByRole('button', { name: '视图选项' }).click()
    const recentOrderOption = page.getByRole('menuitem', { name: '最近更新' })
    await recentOrderOption.click()
    await recentOrderOption.waitFor({ state: 'detached' })
    await configureProvider(page, model.baseUrl)
    await page.getByRole('button', { name: /Model A/ }).waitFor()
    assert.equal(await page.locator('.composer-shell').getByRole('button', { name: /会话权限/ }).isVisible(), true)
    assert.equal(await page.locator('.composer-shell').getByRole('button', { name: /Agent 预设/ }).count(), 0)

    const initialSidebarWidth = await page.locator('.app-sidebar').evaluate(element => element.parentElement.getBoundingClientRect().width)
    assert.equal(Math.abs(initialSidebarWidth - 280) < 1, true, `initial sidebar width: ${initialSidebarWidth}`)
    await dragSeparator(page, '调整侧边栏宽度', 60)
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 340) < 1)
    observations.requests.length = 0
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-sidebar-session-row]').first().waitFor()
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 340) < 1)
    await page.getByRole('button', { name: /Model A/ }).waitFor()
    const reloadInventoryReads = observations.requests.filter(request => {
      if (request.method !== 'GET') return false
      const url = new URL(request.url)
      return url.pathname === '/api/v1/providers' || url.pathname === '/api/v1/credentials'
    })
    assert.equal(reloadInventoryReads.filter(request => new URL(request.url).pathname === '/api/v1/providers').length, 1)
    assert.equal(reloadInventoryReads.filter(request => new URL(request.url).pathname === '/api/v1/credentials').length, 1)
    assert.equal(reloadInventoryReads.every(request => new URL(request.url).searchParams.get('session_id')), true)
    await dragSeparator(page, '调整侧边栏宽度', -60)
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 280) < 1)

    const input = page.getByRole('textbox', { name: '输入任务' })
    const submissionRoute = '**/api/v1/sessions/*/queue'
    let releaseFirstAdmission
    let observeFirstAdmission
    let observeFirstAdmissionContinued
    const firstAdmissionStarted = new Promise(resolve => { observeFirstAdmission = resolve })
    const firstAdmissionGate = new Promise(resolve => { releaseFirstAdmission = resolve })
    const firstAdmissionContinued = new Promise(resolve => { observeFirstAdmissionContinued = resolve })
    const delayFirstAdmission = async route => {
      if (route.request().method() !== 'POST') return route.continue()
      observeFirstAdmission()
      await firstAdmissionGate
      await route.continue()
      observeFirstAdmissionContinued()
    }
    await page.route(submissionRoute, delayFirstAdmission)
    await input.fill('验证 Provider、Markdown、统计和流式兼容性')
    await page.getByRole('button', { name: '发送' }).click()
    await firstAdmissionStarted
    const firstEcho = page.locator('[data-submission-echo]')
    await firstEcho.waitFor()
    assert.match(await firstEcho.textContent(), /验证 Provider、Markdown、统计和流式兼容性/)
    assert.equal(await input.inputValue(), '')
    assert.equal(await input.isEditable(), true)
    releaseFirstAdmission()
    await firstAdmissionContinued
    await page.unroute(submissionRoute, delayFirstAdmission)
    await page.getByRole('button', { name: '停止运行' }).waitFor()
    assert.equal(await input.isEditable(), true)
    assert.equal(await page.getByRole('button', { name: '停止运行' }).isVisible(), true)
    await newSessionHero.waitFor({ state: 'detached' })
    assert.equal(await input.getAttribute('placeholder'), 'Enter 排队；Ctrl/⌘ + Enter 停止并发送全部')
    const headerActions = page.locator('.session-header-actions')
    assert.equal(await headerActions.isVisible(), true)
    assert.equal(await headerActions.getByRole('button', { name: /Agent 预设/ }).isVisible(), true)
    assert.equal(await headerActions.getByRole('button', { name: '更多会话操作' }).isVisible(), true)
    await headerActions.getByRole('button', { name: '更多会话操作' }).click()
    await page.getByRole('menuitem', { name: '切换到计划模式', exact: true }).waitFor()
    await page.keyboard.press('Escape')

    const firstRequest = await model.request(0)
    assert.equal(firstRequest.headers.authorization, 'Bearer direct-browser-secret')
    assert.equal(firstRequest.body.model, 'model-a')
    assert.equal(firstRequest.body.stream, true)
    assert.equal(firstRequest.body.input.at(-1).content[0].text, '验证 Provider、Markdown、统计和流式兼容性')
    assert.equal(firstRequest.body.reasoning.effort, 'ultra')
    assert.equal(firstRequest.body.store, false)
    const systemPromptTrigger = page.getByRole('button', { name: /系统提示词/ }).first()
    await systemPromptTrigger.waitFor()
    assert.equal(await systemPromptTrigger.getAttribute('aria-expanded'), 'false')
    assert.equal(await page.locator('[data-system-prompt-body]').count(), 0)
    await systemPromptTrigger.click()
    assert.equal(await page.locator('[data-system-prompt-body]').first().textContent(), firstRequest.body.instructions)
    await firstEcho.waitFor({ state: 'detached' })
    assert.equal(await page.locator('article[data-role="user"]', { hasText: '验证 Provider、Markdown、统计和流式兼容性' }).count(), 1)

    const streamingThink = page.locator('[data-reasoning-row][data-state="running"]')
    await streamingThink.waitFor()
    await page.getByRole('status').filter({ hasText: '正在深入思考…'}).waitFor()
    assert.match(await streamingThink.textContent(), /思考/)
    await streamingThink.getByText('再验证 Markdown、统计与滚动契约。', { exact: true }).waitFor()
    assert.match(await streamingThink.textContent(), /再验证 Markdown、统计与滚动契约/)
    model.releaseFirstOutput()
    const streamingFence = page.locator('.markdown-streaming code.language-rust')
    const firstStreamingLine = streamingFence.locator('[data-streaming-line="0"]')
    await firstStreamingLine.waitFor()
    const firstStreamingLineHandle = await firstStreamingLine.elementHandle()
    model.releaseFirstStreamingUpdate()
    await streamingFence.getByText('// streamed tail keeps completed lines mounted', { exact: true }).waitFor()
    assert.equal(await firstStreamingLineHandle.evaluate((line) => line === document.querySelector('[data-streaming-line="0"]')), true)
    model.releaseFirstCompletion()

    const exported = await page.evaluate(async () => {
      const sessionId = localStorage.getItem('ternilo.current-session')
      if (!sessionId) throw new Error('current session is missing')
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/export`, {
        headers: token ? { authorization: `Bearer ${token}` } : {},
      })
      if (!response.ok) throw new Error(`session export failed: ${response.status}`)
      return response.json()
    })
    assert.equal(JSON.stringify(exported).includes('direct-browser-secret'), false)
    assert.equal(ternilo.diagnostics().includes('direct-browser-secret'), false)

    const scroll = page.locator('.conversation-scroll')
    await page.getByText('段落 16：验证长会话滚动、中文排版和输入框固定。', { exact: true }).waitFor()
    await page.waitForFunction(() => {
      const element = document.querySelector('.conversation-scroll')
      return element && element.scrollHeight - element.scrollTop - element.clientHeight < 8
    })
    await scroll.evaluate(element => {
      element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: -320 }))
      element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight - 320)
      element.dispatchEvent(new Event('scroll'))
    })
    await page.getByRole('button', { name: '回到底部' }).waitFor()
    const readerTop = await scroll.evaluate(element => element.scrollTop)
    await page.getByText('段落 64：验证长会话滚动、中文排版和输入框固定。', { exact: true }).waitFor()
    assert.equal(Math.abs(await scroll.evaluate(element => element.scrollTop) - readerTop) < 2, true)
    await page.getByRole('button', { name: '回到底部', exact: true }).click()
    await page.getByText('段落 80：验证长会话滚动、中文排版和输入框固定。', { exact: true }).waitFor()
    await page.waitForFunction(() => {
      const element = document.querySelector('.conversation-scroll')
      return element && element.scrollHeight - element.scrollTop - element.clientHeight < 8
    })

    const answer = page.locator('article[data-role="assistant"]').last()
    const firstTurnTail = page.locator('[data-turn-tail="1"]')
    await answer.getByText('路由成功', { exact: true }).waitFor({ timeout: 30_000 })
    assert.match(await answer.locator('code.language-rust').textContent(), /streamed tail keeps completed lines mounted/)
    assert.equal(await answer.locator('code.language-rust span[style*="--shiki-token-"]').count() > 0, true)
    await page.getByRole('button', { name: '发送' }).waitFor()
    assert.equal(await answer.locator('strong').textContent(), '路由成功')
    assert.equal(await answer.locator('table td').first().textContent(), '缓存统计')
    assert.equal(await answer.locator('code.language-rust').count(), 1)
    assert.equal(await answer.locator('math').count() > 0, true)
    const processControl = page.locator('[data-turn-process="1"]')
    await processControl.waitFor()
    assert.equal(await processControl.getAttribute('aria-expanded'), 'false')
    assert.match(await processControl.textContent(), /已思考/)
    const completedThinkSeat = page.locator('[data-turn-inline-reasoning]')
    assert.equal(await completedThinkSeat.getAttribute('hidden'), '')
    await processControl.focus()
    await processControl.press('Enter')
    const completedThink = completedThinkSeat.locator('[data-reasoning-row][data-state="complete"]')
    await completedThink.waitFor({ state: 'visible' })
    assert.equal(await completedThink.locator('[data-disclosure-row]').getAttribute('aria-expanded'), 'false')
    await completedThink.locator('[data-disclosure-row]').click()
    assert.match(await completedThink.locator('[data-reasoning-body]').textContent(), /先检查 Provider 路由与模型参数/)
    assert.match(await completedThink.locator('[data-reasoning-body]').textContent(), /再验证 Markdown、统计与滚动契约/)
    await answer.locator('.markdown-copy').click()
    await answer.locator('.markdown-copy[aria-label="已复制"]').waitFor()
    assert.match(await page.evaluate(() => navigator.clipboard.readText()), /println!/)
    await firstTurnTail.getByRole('button', { name: '复制' }).last().click()
    await firstTurnTail.getByRole('button', { name: '已复制' }).waitFor()
    assert.match(await page.evaluate(() => navigator.clipboard.readText()), /Provider \*\*路由成功\*\*/)
    assert.equal(await firstTurnTail.getByRole('button', { name: '在新对话中分支' }).isVisible(), true)

    const turnUsage = page.locator('[data-turn-usage]').first()
    assert.match(await turnUsage.locator('summary').textContent(), /160 tok/)
    assert.match(await turnUsage.locator('summary').textContent(), /缓存命中率 50%/)
    await turnUsage.locator('summary').click()
    assert.match(await turnUsage.locator('[data-turn-usage-details]').textContent(), /未缓存输入60 tok/)
    assert.match(await turnUsage.locator('[data-turn-usage-details]').textContent(), /缓存读取60 tok/)
    assert.match(await turnUsage.locator('[data-turn-usage-details]').textContent(), /输出40 tok（其中推理 7）/)
    const turnMetrics = page.locator('[data-turn-tail="1"] [data-turn-metrics]')
    assert.match(await turnMetrics.textContent(), /用时/)
    assert.match(await turnMetrics.textContent(), /首 token/)
    assert.match(await turnMetrics.textContent(), /tok\/s/)

    await page.getByRole('button', { name: '设置' }).click()
    let settingsDialog = page.getByRole('dialog', { name: '设置' })
    await selectChoice(settingsDialog.getByLabel('已完成轮次显示方式'), 'normal')
    await settingsDialog.getByRole('button', { name: '关闭设置' }).click()
    assert.equal(await page.locator('[data-turn-process="1"]').count(), 0)
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.transcript-view')), 'normal')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()
    await page.getByText('路由成功', { exact: true }).waitFor()
    assert.equal(await page.locator('[data-turn-process="1"]').count(), 0)
    await page.getByRole('button', { name: '设置' }).click()
    settingsDialog = page.getByRole('dialog', { name: '设置' })
    await selectChoice(settingsDialog.getByLabel('已完成轮次显示方式'), 'compact')
    await settingsDialog.getByRole('button', { name: '关闭设置' }).click()
    await page.locator('[data-turn-process="1"]').waitFor()

    const stats = page.locator('.session-stats-line')
    await stats.waitFor()
    const statsLabel = await stats.getAttribute('aria-label')
    assert.match(statsLabel, /1 轮/)
    assert.match(statsLabel, /LLM/)
    assert.match(statsLabel, /首 token/)
    assert.match(statsLabel, /tok\/s/)
    assert.match(statsLabel, /缓存命中 50%/)
    assert.match(statsLabel, /输入 120 tok · 输出 40 tok/)
    assert.match(statsLabel, /推理 7 tok/)

    const statsFailurePattern = '**/api/v1/sessions/*/stats'
    const failStats = route => route.fulfill({
      status: 503,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'fixture_stats_unavailable', message: '统计暂不可用' } }),
    })
    await page.route(statsFailurePattern, failStats)

    assert.equal(await scroll.evaluate(element => element.scrollHeight > element.clientHeight), true)
    const headerY = (await page.locator('.session-header').boundingBox()).y
    const composerY = (await page.locator('.composer-shell').boundingBox()).y
    await scroll.evaluate(element => {
      element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: -element.scrollHeight }))
      element.scrollTop = 0
      element.dispatchEvent(new Event('scroll'))
    })
    await page.waitForTimeout(80)
    assert.equal(Math.abs((await page.locator('.session-header').boundingBox()).y - headerY) < 1, true)
    assert.equal(Math.abs((await page.locator('.composer-shell').boundingBox()).y - composerY) < 1, true)

    const chatTabBox = await page.getByRole('tab', { name: '对话' }).boundingBox()
    const trajectoryTabBox = await page.getByRole('tab', { name: '轨迹' }).boundingBox()
    const tabGap = trajectoryTabBox.x - (chatTabBox.x + chatTabBox.width)
    assert.ok(tabGap >= 0 && tabGap <= 40, `view tabs overlap or separate: ${tabGap}px`)
    assert.ok(Math.abs(chatTabBox.y - trajectoryTabBox.y) < 1, 'view tabs must share a row')
    assert.ok(chatTabBox.height >= 24 && trajectoryTabBox.height >= 24, 'view tab targets are too small')
    assert.equal(await page.getByRole('tab', { name: '对话' }).getAttribute('aria-selected'), 'true')
    assert.equal(await page.locator('[data-width-handle]').count(), 2)
    await page.getByRole('tab', { name: '轨迹' }).click()
    assert.equal(await page.getByRole('tab', { name: '轨迹' }).getAttribute('aria-selected'), 'true')
    assert.equal(await page.getByRole('tab', { name: '对话' }).getAttribute('aria-selected'), 'false')
    assert.equal(await page.locator('[data-width-handle]').count(), 0, 'chat width handles must not intercept trajectory controls')
    await page.getByRole('toolbar', { name: '轨迹工具栏' }).waitFor()
    assert.equal(await scroll.evaluate(element => element.scrollTop), 0)
    const timeline = page.locator('[data-trajectory-timeline]')
    assert.equal(await timeline.isVisible(), true)
    const initialTimelineScale = await timeline.locator('[data-trajectory-timeline-scale]').textContent()
    await timeline.getByRole('button', { name: '放大时间轴' }).click()
    assert.notEqual(await timeline.locator('[data-trajectory-timeline-scale]').textContent(), initialTimelineScale)
    await timeline.getByRole('button', { name: '复位时间轴' }).click()
    assert.equal(await timeline.locator('[data-trajectory-timeline-scale]').textContent(), initialTimelineScale)
    const durationModeButton = page.getByRole('toolbar', { name: '轨迹工具栏' }).locator('button[aria-pressed]').first()
    const initialTimelineLabel = await timeline.getAttribute('aria-label')
    const alternateTimelinePrefix = initialTimelineLabel.startsWith('完整时间轴') ? '压缩空闲轴' : '完整时间轴'
    await durationModeButton.click()
    await page.waitForFunction(prefix => document.querySelector('[data-trajectory-timeline]')?.getAttribute('aria-label')?.startsWith(prefix), alternateTimelinePrefix)
    await durationModeButton.click()
    await page.waitForFunction(label => document.querySelector('[data-trajectory-timeline]')?.getAttribute('aria-label') === label, initialTimelineLabel)
    const timelinePlot = timeline.locator('[data-trajectory-timeline-plot]')
    const timelinePlotBox = await timelinePlot.boundingBox()
    // Select on the empty tool lane: model blocks intentionally consume pointer-down
    // so clicking a record opens its details instead of beginning a range gesture.
    const timelineRangeY = timelinePlotBox.y + timelinePlotBox.height * 0.84
    await page.mouse.move(timelinePlotBox.x + timelinePlotBox.width * 0.2, timelineRangeY)
    await page.mouse.down()
    await page.mouse.move(timelinePlotBox.x + timelinePlotBox.width * 0.8, timelineRangeY)
    await page.mouse.up()
    await timeline.locator('[data-trajectory-timeline-range]').waitFor()
    await timeline.press('Escape')
    assert.equal(await timeline.locator('[data-trajectory-timeline-range]').count(), 0)
    await timeline.getByRole('button', { name: '切换到平移' }).click()
    assert.equal(await timeline.getByRole('button', { name: '切换到范围选择' }).isVisible(), true)
    await timeline.getByRole('button', { name: '切换到范围选择' }).click()
    const trajectoryLedger = page.locator('[data-trajectory-ledger]')
    assert.match(await trajectoryLedger.textContent(), /第 1 轮|Turn 1/)
    assert.match(await trajectoryLedger.textContent(), /120/)
    assert.match(await trajectoryLedger.textContent(), /40/)
    const reasoningRecord = page.locator('[data-trajectory-record]').filter({
      has: page.locator('[data-metric="推理"]', { hasText: '7' }),
    })
    assert.equal(await reasoningRecord.count(), 1)
    await reasoningRecord.click()
    const details = page.getByRole('complementary', { name: '详情' })
    await details.waitFor()
    assert.match(await details.textContent(), /计时与用量/)
    assert.match(await details.textContent(), /推理 token\s*7/)
    assert.match(await details.textContent(), /思考/)
    assert.match(await details.textContent(), /先检查 Provider 路由与模型参数/)
    assert.match(await details.textContent(), /模型输出/)
    assert.match(await details.textContent(), /Provider \*\*路由成功\*\*/)
    await page.waitForFunction(() => Math.abs(document.querySelector('.details-panel').parentElement.getBoundingClientRect().width - 360) < 1)
    await page.setViewportSize({ width: 1250, height: 900 })
    await page.waitForFunction(() => Math.abs(document.querySelector('.details-panel').parentElement.getBoundingClientRect().width - 330) < 1)
    await page.setViewportSize({ width: 1219, height: 900 })
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-details-overlay'))
    await page.waitForFunction(() => Math.abs(document.querySelector('.details-panel').parentElement.getBoundingClientRect().width - innerWidth) < 1)
    assert.equal(await page.getByRole('separator', { name: '调整详情栏宽度' }).count(), 0)
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.waitForFunction(() => Math.abs(document.querySelector('.details-panel').parentElement.getBoundingClientRect().width - 360) < 1)
    assert.match(await details.textContent(), /计时与用量/)
    await details.getByRole('button', { name: '关闭详情' }).click()

    await page.getByRole('tab', { name: '对话' }).click()
    assert.equal(await page.locator('[data-width-handle]').count(), 2, 'returning to chat preserves reading-width controls')
    assert.equal(await scroll.evaluate(element => element.scrollTop), 0)
    await input.fill('从顶部发出第二轮，并自动回到底部')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(1)
    await page.getByText('第二轮完成，发送新任务后已回到会话底部。', { exact: true }).waitFor({ timeout: 30_000 })
    await page.waitForFunction(() => {
      const element = document.querySelector('.conversation-scroll')
      return element && element.scrollHeight - element.scrollTop - element.clientHeight < 8
    })
    assert.equal(await page.locator('[data-system-prompt-row]').count(), 1)
    const turnMarks = page.locator('[data-turn-navigator-mark]')
    await page.getByRole('button', { name: '跳转到第 2 轮' }).waitFor()
    assert.equal(await turnMarks.count(), 2)
    await page.getByRole('button', { name: '跳转到第 1 轮' }).hover()
    assert.match(await page.locator('[data-turn-navigator-preview]').textContent(), /验证 Provider、Markdown、统计和流式兼容性/)
    const bottomBeforeNavigation = await scroll.evaluate(element => element.scrollTop)
    await page.getByRole('button', { name: '跳转到第 1 轮' }).click()
    await page.waitForFunction(previous => document.querySelector('.conversation-scroll').scrollTop < previous, bottomBeforeNavigation)
    assert.equal(await page.getByRole('button', { name: '跳转到第 1 轮' }).getAttribute('aria-current'), 'true')
    await page.getByRole('button', { name: '跳转到第 2 轮' }).click()
    assert.equal(await page.getByRole('button', { name: '跳转到第 2 轮' }).getAttribute('aria-current'), 'true')
    const bottomBeforeKeyboardNavigation = await scroll.evaluate(element => element.scrollTop)
    await page.getByRole('button', { name: '跳转到第 1 轮' }).press('Enter')
    await page.waitForFunction(previous => document.querySelector('.conversation-scroll').scrollTop < previous, bottomBeforeKeyboardNavigation)
    assert.equal(await page.getByRole('button', { name: '跳转到第 1 轮' }).getAttribute('aria-current'), 'true')
    const feedbackPattern = '**/api/v1/sessions/*/feedback'
    const acceptFeedbackWithoutLiveMutation = route => route.fulfill({ status: 204 })
    await page.route(feedbackPattern, acceptFeedbackWithoutLiveMutation)
    const statsBeforeFailedReload = await stats.getAttribute('aria-label')
    assert.match(statsBeforeFailedReload, /2 轮/)
    await page.getByRole('button', { name: '好回答' }).last().click()
    const metadataWarning = page.getByRole('alert').filter({ hasText: '部分会话信息未更新' })
    await metadataWarning.waitFor()
    assert.match(await metadataWarning.textContent(), /统计暂不可用/)
    assert.equal(await stats.getAttribute('aria-label'), statsBeforeFailedReload)
    await page.unroute(feedbackPattern, acceptFeedbackWithoutLiveMutation)
    await page.unroute(statsFailurePattern, failStats)

    let releaseFailedAdmission
    let observeFailedAdmission
    let observeFailedAdmissionFinished
    const failedAdmissionStarted = new Promise(resolve => { observeFailedAdmission = resolve })
    const failedAdmissionGate = new Promise(resolve => { releaseFailedAdmission = resolve })
    const failedAdmissionFinished = new Promise(resolve => { observeFailedAdmissionFinished = resolve })
    const rejectAdmission = async route => {
      if (route.request().method() !== 'POST') return route.continue()
      observeFailedAdmission()
      await failedAdmissionGate
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'fixture_admission_failed', message: '提交暂不可用' } }),
      })
      observeFailedAdmissionFinished()
    }
    await page.route(submissionRoute, rejectAdmission)
    await input.fill('失败后应恢复的草稿')
    await page.getByRole('button', { name: '发送' }).click()
    await failedAdmissionStarted
    await page.locator('[data-submission-echo]', { hasText: '失败后应恢复的草稿' }).waitFor()
    assert.equal(await input.inputValue(), '')
    releaseFailedAdmission()
    await failedAdmissionFinished
    await page.waitForFunction(() => document.querySelector('textarea[aria-label="输入任务"]')?.value === '失败后应恢复的草稿')
    assert.equal(await page.locator('[data-submission-echo]', { hasText: '失败后应恢复的草稿' }).count(), 0)
    await page.unroute(submissionRoute, rejectAdmission)
    await input.fill('')

    await input.evaluate(element => {
      const encoded = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
      const bytes = Uint8Array.from(atob(encoded), character => character.charCodeAt(0))
      const transfer = new DataTransfer()
      transfer.items.add(new File([bytes], 'clipboard.png', { type: 'image/png' }))
      const event = new Event('paste', { bubbles: true, cancelable: true })
      Object.defineProperty(event, 'clipboardData', { value: transfer })
      element.dispatchEvent(event)
    })
    const draftImage = page.getByRole('button', { name: '预览 clipboard.webp' })
    await draftImage.waitFor()
    await draftImage.click()
    const draftPreview = page.getByRole('dialog', { name: '图片预览：clipboard.webp' })
    await draftPreview.waitFor()
    await draftPreview.getByRole('button', { name: '关闭图片预览' }).evaluate(button => button.click())
    await draftPreview.waitFor({ state: 'detached' })
    assert.equal(await page.getByRole('button', { name: '发送' }).isEnabled(), true)
    await page.getByRole('button', { name: '发送' }).click()
    const imageRequest = await model.request(2)
    const imageContent = imageRequest.body.input.at(-1).content
    assert.deepEqual(imageContent.map(part => part.type), ['input_image'])
    assert.match(imageContent[0].image_url, /^data:image\/webp;base64,/)
    const historicalImage = page.getByRole('button', { name: '打开图片 clipboard.webp' }).last()
    await historicalImage.waitFor({ timeout: 30_000 })
    await historicalImage.click()
    const historicalPreview = page.getByRole('dialog', { name: '图片预览：clipboard.webp' })
    await historicalPreview.waitFor()
    await historicalPreview.getByRole('button', { name: '关闭图片预览' }).evaluate(button => button.click())
    await historicalPreview.waitFor({ state: 'detached' })
    await page.getByText('第二轮完成，发送新任务后已回到会话底部。', { exact: true }).last().waitFor()

    await firstTurnTail.getByRole('button', { name: '在新对话中分支' }).click()
    await page.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 2)
    await page.locator('.session-title-row h1').filter({ hasText: /\(1\)$/ }).waitFor()
    await page.waitForTimeout(1_000)
    await page.waitForFunction(() => document.querySelectorAll('article[data-role="assistant"]').length === 1)
    assert.equal(await page.getByText('第二轮完成，发送新任务后已回到会话底部。', { exact: true }).count(), 0)
    const messageForkRow = page.locator('[data-sidebar-session-row]').filter({ has: page.locator('[data-sidebar-session-active]') })
    await messageForkRow.hover()
    await messageForkRow.getByRole('button', { name: /的操作$/ }).click()
    const [archivedMessageFork] = await Promise.all([
      page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname.endsWith('/archive')),
      page.getByRole('menuitem', { name: '归档会话' }).click(),
    ])
    assert.equal(archivedMessageFork.status(), 200, await archivedMessageFork.text())
    await page.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 1)

    const sourceRow = page.locator('[data-sidebar-session-row]').first()
    await sourceRow.hover()
    await sourceRow.getByRole('button', { name: /的操作$/ }).click()
    const sessionMenuLabels = await page.getByRole('menuitem').allTextContents()
    assert.deepEqual(sessionMenuLabels.map(label => label.trim()), ['重命名', '分叉会话', '归档会话'])
    await page.getByRole('menuitem', { name: '重命名' }).click()
    const renameDialog = page.getByRole('dialog', { name: '重命名会话' })
    await renameDialog.getByRole('textbox', { name: '会话名称' }).fill('浏览器会话')
    await renameDialog.getByRole('button', { name: '重命名' }).click()
    await renameDialog.waitFor({ state: 'detached' })
    await sourceRow.getByText('浏览器会话', { exact: true }).waitFor()
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话')
    assert.equal(await page.locator('.session-title-row h1').evaluate(element => element.ondblclick), null)

    await sourceRow.hover()
    await sourceRow.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '分叉会话' }).click()
    await page.locator('[data-sidebar-session-row]').nth(1).waitFor({ timeout: 10_000 })
    assert.equal(await page.locator('[data-sidebar-session-row]').count(), 2)
    await page.getByText('浏览器会话 (1)', { exact: true }).first().waitFor()
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话 (1)')
    await page.waitForFunction(() => document.querySelectorAll('article[data-role="assistant"]').length >= 2)

    const forkRow = page.locator('[data-sidebar-session-row]').filter({ hasText: '浏览器会话 (1)' })
    await page.locator('[data-sidebar-session-row]').filter({ has: page.getByText('浏览器会话', { exact: true }) }).hover()
    await page.locator('[data-radix-popper-content-wrapper] strong').filter({ hasText: /^浏览器会话$/ }).waitFor()
    await forkRow.hover()
    await forkRow.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '归档会话' }).click()
    await page.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 1)
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-sidebar-session-row]').first().waitFor()
    assert.equal(await page.locator('[data-sidebar-session-row]').count(), 1)
    assert.equal(await page.getByText('浏览器会话 (1)', { exact: true }).count(), 0)

    const searchFailurePattern = '**/api/v1/session-search?*'
    const failSearch = route => route.fulfill({
      status: 503,
      contentType: 'application/json',
      body: JSON.stringify({ error: { code: 'fixture_search_unavailable', message: '搜索索引暂不可用' } }),
    })
    await page.route(searchFailurePattern, failSearch)
    await page.getByRole('button', { name: '搜索会话' }).click()
    await page.getByRole('textbox', { name: '搜索会话' }).fill('故障注入查询')
    const searchAlert = page.getByRole('alert').filter({ hasText: '搜索失败' })
    await searchAlert.waitFor()
    assert.match(await searchAlert.textContent(), /搜索索引暂不可用/)
    await page.unroute(searchFailurePattern, failSearch)
    await searchAlert.getByRole('button', { name: '重试' }).click()
    await searchAlert.waitFor({ state: 'detached' })
    await page.getByText('没有匹配的会话', { exact: true }).waitFor()

    const searchIdentity = await page.evaluate(async () => {
      const sessionId = localStorage.getItem('ternilo.current-session')
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      const response = await fetch('/api/v1/state', { headers: token ? { authorization: `Bearer ${token}` } : {} })
      if (!response.ok) throw new Error(`state failed: ${response.status}`)
      const state = await response.json()
      const session = state.sessions.find(candidate => candidate.identity.session_id === sessionId)
      if (!session) throw new Error('current session missing from state')
      return { sessionId, workspaceId: session.workspace_id, title: session.title, updatedAt: session.updated_at_ms }
    })
    let releaseSlowSearch
    let markSlowSearchStarted
    const slowSearchGate = new Promise(resolve => { releaseSlowSearch = resolve })
    const slowSearchStarted = new Promise(resolve => { markSlowSearchStarted = resolve })
    const raceSearch = async route => {
      const query = new URL(route.request().url()).searchParams.get('query')
      if (query !== 'race-slow' && query !== 'race-latest') return route.continue()
      if (query === 'race-slow') {
        markSlowSearchStarted()
        await slowSearchGate
      }
      const excerpt = query === 'race-slow' ? 'STALE_RACE_SENTINEL' : 'LATEST_RACE_SENTINEL'
      try {
        await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([{
          session_id: searchIdentity.sessionId,
          workspace_id: searchIdentity.workspaceId,
          title: searchIdentity.title,
          updated_at_ms: searchIdentity.updatedAt,
          event_seq: 1,
          occurred_at_ms: searchIdentity.updatedAt,
          run_id: null,
          category: null,
          excerpt,
        }]) })
      } catch (error) {
        if (query !== 'race-slow') throw error
      }
    }
    await page.route(searchFailurePattern, raceSearch)
    const searchBox = page.getByRole('textbox', { name: '搜索会话' })
    await searchBox.fill('race-slow')
    await slowSearchStarted
    await searchBox.fill('race-latest')
    await page.locator('[data-sidebar-session-snippet]', { hasText: 'LATEST_RACE_SENTINEL' }).waitFor()
    releaseSlowSearch()
    await page.waitForTimeout(250)
    assert.equal(await page.getByText('STALE_RACE_SENTINEL', { exact: true }).count(), 0)
    assert.equal(await page.getByText('LATEST_RACE_SENTINEL', { exact: true }).count(), 1)
    await page.unroute(searchFailurePattern, raceSearch)

    await page.getByRole('textbox', { name: '搜索会话' }).fill('Markdown')
    const indexedSearchResult = page.getByRole('tree', { name: '搜索结果' }).locator('[data-sidebar-session-row]')
    await indexedSearchResult.waitFor()
    assert.match(await indexedSearchResult.locator('[data-sidebar-session-snippet]').textContent(), /Markdown/)
    assert.match(await indexedSearchResult.textContent(), /浏览器会话/)
    await page.getByRole('button', { name: '关闭搜索' }).click()

    await page.addScriptTag({ path: axePath })
    const serious = await page.evaluate(async () => {
      const result = await window.axe.run(document, { resultTypes: ['violations'] })
      return result.violations
        .filter(item => item.impact === 'serious' || item.impact === 'critical')
        .map(item => ({ id: item.id, targets: item.nodes.map(node => node.target) }))
    })
    assert.deepEqual(serious, [])

    await page.setViewportSize({ width: 980, height: 900 })
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 56) < 1)
    await page.getByRole('button', { name: '展开侧边栏' }).click()
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 280) < 1)
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').parentElement.getBoundingClientRect().width - 280) < 1)

    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    assert.equal(await headerActions.getByRole('button', { name: '更多会话操作' }).isVisible(), true)
    assert.equal(await headerActions.getByRole('button', { name: /Agent 预设/ }).isVisible(), false)
    await headerActions.getByRole('button', { name: '更多会话操作' }).click()
    assert.equal(await page.getByRole('menuitem').filter({ has: page.getByText('标准模式', { exact: true }) }).isVisible(), true)
    assert.equal(await page.getByRole('menuitem', { name: '切换到计划模式' }).isVisible(), true)
    assert.equal(await page.getByRole('menuitem', { name: '选择工作文件夹' }).isVisible(), true)
    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
    await page.waitForFunction(() => (document.querySelector('.app-sidebar')?.getBoundingClientRect().x ?? -1) >= -1)
    assert.equal(Math.abs((await sidebar.boundingBox()).x) < 1, true)
    const mobileSessionRow = sidebar.locator('[data-sidebar-session-row]').first()
    await mobileSessionRow.getByRole('button', { name: /的操作$/ }).click()
    assert.equal(await page.getByRole('menuitem', { name: '分叉会话' }).isVisible(), true)
    assert.equal(await page.getByRole('menuitem', { name: '归档会话' }).isVisible(), true)
    await page.keyboard.press('Escape')
    await sidebar.getByRole('button', { name: '关闭侧边栏' }).click()
    await page.getByRole('tab', { name: '轨迹', exact: true }).click()
    await page.getByRole('toolbar', { name: '轨迹工具栏' }).waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    const mobileTrajectoryRecord = page.locator('[data-trajectory-record]').first()
    for (const label of ['输入', '输出', '推理', '缓存']) {
      assert.equal(await mobileTrajectoryRecord.getByText(new RegExp(`^${label}\\s+(?:\\d|—)`)).isVisible(), true)
    }
    for (const button of await page.locator('[data-trajectory-timeline-controls] button:visible').all()) {
      const box = await button.boundingBox()
      assert.equal(box.width >= 40 && box.height >= 40, true)
    }
    await page.getByRole('tab', { name: '对话', exact: true }).click()
    const composerBox = await page.locator('.composer-shell').boundingBox()
    assert.equal(composerBox.x >= 8 && composerBox.x + composerBox.width <= 382, true)
    for (const button of await page.locator('.composer-toolbar button:visible').all()) {
      const box = await button.boundingBox()
      assert.equal(box.width >= 40 && box.height >= 40, true)
    }

    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await sidebar.getByRole('button', { name: '设置' }).click()
    const mobileSettings = page.getByRole('dialog', { name: '设置' })
    await mobileSettings.waitFor()
    const mobileSettingsBox = await mobileSettings.boundingBox()
    assert.ok(mobileSettingsBox)
    assert.equal(mobileSettingsBox.x >= 5 && mobileSettingsBox.x + mobileSettingsBox.width <= 385, true)
    assert.equal(await mobileSettings.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await mobileSettings.getByRole('button', { name: '模型', exact: true }).click()
    assert.equal(await mobileSettings.locator('[data-settings-content]').evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await mobileSettings.getByRole('button', { name: '插件', exact: true }).click()
    assert.equal(await mobileSettings.getByRole('tab', { name: '插件配置' }).isVisible(), true)
    await mobileSettings.getByRole('button', { name: '关闭设置' }).click()
    await mobileSettings.waitFor({ state: 'detached' })
    if (await page.locator('[data-app-frame][data-mobile-sidebar-open]').count()) {
      await sidebar.getByRole('button', { name: '关闭侧边栏' }).click()
    }

    await input.focus()
    await page.setViewportSize({ width: 390, height: 430 })
    await page.waitForFunction(() => Math.abs(document.querySelector('[data-app-frame]').getBoundingClientRect().height - window.innerHeight) < 1)
    const keyboardComposerBox = await page.locator('.composer-shell').boundingBox()
    assert.equal(keyboardComposerBox.y >= 0 && keyboardComposerBox.y + keyboardComposerBox.height <= 430, true)
    assert.equal(await input.isVisible(), true)
    await page.setViewportSize({ width: 740, height: 390 })
    await page.waitForFunction(() => document.documentElement.scrollWidth === window.innerWidth)
    assert.equal((await page.locator('.composer-shell').boundingBox()).y >= 0, true)
    await page.setViewportSize({ width: 390, height: 844 })

    await input.fill('浏览器会话的未发送草稿')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await input.waitFor()
    assert.equal(await input.inputValue(), '浏览器会话的未发送草稿')
    const draftedSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await sidebar.getByRole('button', { name: '新会话', exact: true }).click()
    await page.waitForFunction(previous => localStorage.getItem('ternilo.current-session') !== previous, draftedSessionId)
    await page.locator('[data-new-session-hero]').waitFor()
    assert.equal(await page.locator('.session-header').isVisible(), false)
    assert.equal(await page.getByText('让想法，动起来', { exact: true }).isVisible(), true)
    assert.equal(await page.getByRole('textbox', { name: '输入任务' }).getAttribute('placeholder'), '描述你想要构建的内容')
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    assert.equal(await input.inputValue(), '')
    await input.fill('第二个会话的独立草稿')
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await sidebar.locator('[data-sidebar-session-row]').filter({ hasText: '浏览器会话' }).locator('[data-sidebar-session-button]').click()
    assert.equal(await input.inputValue(), '浏览器会话的未发送草稿')

    await page.setViewportSize({ width: 1440, height: 900 })
    const workspaceRow = page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'workspace' }).first()
    await workspaceRow.hover()
    const workspacePathCard = page.getByRole('button', { name: `复制工作区完整路径：${workspace}` })
    await workspacePathCard.waitFor()
    await page.getByText(/^创建于 \d{4}年\d{1,2}月\d{1,2}日 \d{2}:\d{2}$/).waitFor()
    await workspacePathCard.click()
    await page.getByRole('status').filter({ hasText: '已复制完整路径' }).waitFor()
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), workspace)

    await workspaceRow.hover()
    await workspaceRow.getByRole('button', { name: /工作区.*的操作$/ }).click()
    assert.deepEqual(
      (await page.getByRole('menuitem').allTextContents()).map(label => label.trim()),
      ['查看位置', '重命名', '移除工作区'],
    )
    await page.getByRole('menuitem', { name: '重命名' }).click()
    const workspaceRenameDialog = page.getByRole('dialog', { name: '重命名工作区' })
    await workspaceRenameDialog.getByRole('textbox', { name: '工作区名称' }).fill('浏览器工作区')
    await workspaceRenameDialog.getByRole('textbox', { name: '工作区名称' }).press('Enter')
    await workspaceRenameDialog.waitFor({ state: 'detached' })
    await page.getByText('浏览器工作区', { exact: true }).first().waitFor()

    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByText('浏览器工作区', { exact: true }).first().waitFor()
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话')
    assert.equal(await input.inputValue(), '浏览器会话的未发送草稿')

    const renamedWorkspaceRow = page.locator('[data-sidebar-workspace-row]').filter({ hasText: '浏览器工作区' }).first()
    await renamedWorkspaceRow.hover()
    await renamedWorkspaceRow.getByRole('button', { name: '工作区“浏览器工作区”的操作' }).click()
    await page.getByRole('menuitem', { name: '移除工作区' }).click()
    const removeWorkspaceDialog = page.getByRole('dialog', { name: '移除工作区' })
    assert.match(await removeWorkspaceDialog.textContent(), /目录、其中的文件和会话日志都会保留/)
    await removeWorkspaceDialog.getByRole('button', { name: '移除工作区' }).click()
    await removeWorkspaceDialog.waitFor({ state: 'detached' })
    await page.getByText('未分组', { exact: true }).waitFor()
    assert.equal(await page.getByText('浏览器工作区', { exact: true }).count(), 0)
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话')
    assert.equal(await input.inputValue(), '浏览器会话的未发送草稿')

    const ungroupedSource = page.locator('[data-sidebar-session-row]').filter({ hasText: '浏览器会话' }).first()
    await ungroupedSource.hover()
    await ungroupedSource.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '分叉会话' }).click()
    await page.getByText('浏览器会话 (1)', { exact: true }).first().waitFor()
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话 (1)')
    await page.waitForFunction(() => document.querySelectorAll('article[data-role="assistant"]').length >= 2)

    observations.webSockets.length = 0
    // The composer loads skills after its command catalog; Live readiness alone
    // does not mean this initial HTTP dependency chain has completed.
    const restoredSkillCatalog = page.waitForResponse(response => response.request().method() === 'GET'
      && /\/sessions\/[^/]+\/skills$/.test(new URL(response.url()).pathname) && response.status() === 200)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByText('未分组', { exact: true }).waitFor()
    await page.getByText('浏览器会话 (1)', { exact: true }).first().waitFor()
    assert.equal(await page.locator('.session-title-row h1').textContent(), '浏览器会话 (1)')
    await page.waitForFunction(() => document.querySelectorAll('article[data-role="assistant"]').length >= 2)
    await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
    await restoredSkillCatalog

    observations.requests.length = 0
    await page.waitForTimeout(30_000)
    const idleApiReads = observations.requests.filter(request => request.method === 'GET' && new URL(request.url).pathname.startsWith('/api/v1/'))
    assert.deepEqual(idleApiReads, [], JSON.stringify(idleApiReads, null, 2))
    assert.equal(observations.webSockets.length, 1, JSON.stringify(observations.webSockets, null, 2))
    assert.equal(new URL(observations.webSockets[0]).pathname, '/api/v1/live')

    const ungroupedGroup = page.locator('[data-sidebar-workspace-group]').filter({ hasText: '未分组' }).last()
    const ungroupedToggle = ungroupedGroup.locator('[data-sidebar-workspace-button]')
    assert.equal(await ungroupedToggle.getAttribute('aria-expanded'), 'true')
    await ungroupedToggle.click()
    await page.waitForTimeout(250)
    assert.equal(await ungroupedToggle.getAttribute('aria-expanded'), 'false')
    assert.equal(await ungroupedGroup.locator('[data-sidebar-session-row]').count(), 0)
    await ungroupedToggle.click()
    assert.equal(await ungroupedToggle.getAttribute('aria-expanded'), 'true')

    await ungroupedGroup.getByRole('button', { name: '未分组的操作' }).click()
    await page.getByRole('menuitem', { name: /删除全部会话/ }).click()
    const deleteUngroupedDialog = page.getByRole('dialog', { name: '删除未分组会话？' })
    assert.match(await deleteUngroupedDialog.textContent(), /永久删除“未分组”中的 \d+ 个会话及其日志/)
    await deleteUngroupedDialog.getByRole('button', { name: '全部删除' }).click()
    await page.waitForFunction(() => document.querySelectorAll('[data-sidebar-workspace-group]').length === 0)
    assert.equal(await page.getByText('未分组', { exact: true }).count(), 0)
    const intentionalFailures = observations.failedResponses.filter(failure => (
      failure.endsWith(': 503') && (
        failure.includes('/stats') || failure.includes('/queue') || failure.includes('/session-search?')
      )
    ))
    for (const path of ['/stats', '/queue', '/session-search?']) {
      assert.equal(intentionalFailures.some(failure => failure.includes(path)), true, JSON.stringify(observations, null, 2))
    }
    assert.deepEqual(
      observations.failedResponses.filter(failure => !intentionalFailures.includes(failure)),
      [],
      JSON.stringify(observations, null, 2),
    )
    assert.equal(
      observations.consoleErrors.every(message => message.includes('Failed to load resource') && message.includes('503')),
      true,
      JSON.stringify(observations, null, 2),
    )
    assert.equal(observations.consoleErrors.length, intentionalFailures.length, JSON.stringify(observations, null, 2))
    const lifecycleAborts = observations.failedRequests.filter(failure => (
      failure.endsWith('net::ERR_ABORTED') && failure.startsWith('GET ') && (
        failure.includes('/session-search?query=race-slow')
        || /\/sessions\/[^/]+\/(?:skills|commands)(?:\?|:)/.test(failure)
      )
    ))
    assert.equal(lifecycleAborts.some(failure => failure.includes('/session-search?query=race-slow')), true, JSON.stringify(observations, null, 2))
    assert.deepEqual(
      observations.failedRequests.filter(failure => !lifecycleAborts.includes(failure)),
      [],
      JSON.stringify(observations, null, 2),
    )
    assert.deepEqual(observations.pageErrors, [], JSON.stringify(observations, null, 2))
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
